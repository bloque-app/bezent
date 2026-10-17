#![cfg_attr(not(feature = "std"), no_std, no_main)]
// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later

//! # bezent contract
//!
//! bezent's on-chain component on the issuing network (SPEC §7.F `CTR-HOME`; PLAN `DEC-6`): one ink!
//! contract on Kreivo that mints, redeems and refund-mints every bridged asset, with bounds kept per
//! asset. It holds its mint and destroy authority through the community dedicated to it, which is
//! the Issuer and Admin of each bridged asset (`REQ-HN-10`, `DEC-7`), and it uses that authority
//! through Kreivo's assets API (`assets__mint`, `assets__burn`, kreivo#512).
//!
//! - **Mint** ([`mint`](bezent::Bezent::mint)): the delivery role mints to the order's destination,
//!   with an on-chain reference used by one mint, forever (`INV-2`), within the asset's per-transfer
//!   maximum, mint window and supply ceiling (`INV-3`).
//! - **Redeem** ([`redeem`](bezent::Bezent::redeem)): anyone. The contract pulls the amount from the
//!   caller into its own account (out of the caller's approval if it has one, which lets a holder
//!   redeem everything; otherwise keeping the caller's account alive), checks its own balance rose
//!   by that amount, and destroys it there
//!   in the same call. It never burns from any account but its own (`REQ-HN-8`, `INV-4`). The
//!   redemption names one of the backends its asset declares and records the payout instructions
//!   or their commitment (`REQ-RQ-9`, `REQ-OT-4`).
//!   It is keyed by its redeemer and reference: mint references and each redeemer's references are
//!   separate namespaces, so knowing a reference lets no one consume or block another's (`REQ-ID-4`).
//! - **Refund mint** ([`refund_mint`](bezent::Bezent::refund_mint)): the delivery role mints, once,
//!   to a redemption's redeemer, inside the mint window (`INV-8`, `REQ-OT-6`, `REQ-RF-4`). Proving
//!   the payout was not executed is the core's job; the contract only enforces once.
//! - **Pauses** per asset and direction, per asset, per direction or all: the pause role or
//!   governance pauses, only governance unpauses (`REQ-GV-2`, `AC-I2.2`).
//! - **Bounds**: lowering takes effect at once (pause role or governance); raising is scheduled by
//!   governance and applied by anyone after the raise delay (`REQ-GV-3`). The delay itself changes
//!   only through the same delay, and is never zero nor above 28 days of blocks
//!   (at Kreivo's ~2.07 s block time, see `BLOCK_TIME_MS`).
//! - **Roles**: delivery, pause and governance, always three different accounts, none of them the
//!   all-zero account or the contract's own (`REQ-HN-9`). Governance replaces delivery and pause at
//!   once; a new governance account waits the raise delay and must accept, so a mistyped account
//!   cannot lock governance out (`REQ-GV-3`).
//! - **Ordered events**: every event carries a contract-wide sequence number that increases by one
//!   per event, and every mint, redemption and refund mint carries its reference (`REQ-HN-7`).
//!
//! A refused message returns an [`Error`](types::Error), and ink! reverts everything it did,
//! including what Kreivo's assets API moved.

pub mod kreivo;
pub mod types;

#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;

#[ink::contract(env = crate::kreivo::KreivoEnvironment)]
pub mod bezent {
    use crate::kreivo::{AssetsCall, MultiAddress, RuntimeCall};
    use crate::types::*;
    use ink::prelude::vec::Vec;
    use ink::storage::{traits::ManualKey, Mapping};

    /// The contract's storage. Every mapping has a fixed root key, so the layout survives code
    /// upgrades and an observer can compute any key without the contract's metadata.
    #[ink(storage)]
    pub struct Bezent {
        /// Delivery, pause and governance.
        roles: Roles,
        /// How many blocks a raise of bounds, or a change of this delay, waits (`REQ-GV-3`).
        raise_delay: BlockNumber,
        /// A change of the raise delay, waiting for the current delay.
        scheduled_delay: Option<ScheduledDelay>,
        /// A change of the governance account, waiting for the raise delay and its acceptance.
        scheduled_governance: Option<ScheduledGovernance>,
        /// The next event's sequence number.
        event_seq: EventSeq,
        /// Each known asset's bounds. An asset without bounds is unknown.
        bounds: Mapping<AssetId, Bounds, ManualKey<0x0000_0001>>,
        /// The backends each known asset's redemptions may name (`REQ-OT-4`). Changed only
        /// through the delayed path, with the bounds.
        backends: Mapping<AssetId, Vec<Backend>, ManualKey<0x0000_0008>>,
        /// Raises waiting for the delay, per asset.
        scheduled_raises: Mapping<AssetId, ScheduledRaise, ManualKey<0x0000_0002>>,
        /// Each asset's sliding mint window (`INV-3`).
        windows: Mapping<AssetId, Window, ManualKey<0x0000_0003>>,
        /// Each known asset's supply: what existed when it was added (its initial supply), plus
        /// what the contract minted, less what it destroyed.
        supply: Mapping<AssetId, Balance, ManualKey<0x0000_0004>>,
        /// The scopes that are paused.
        pauses: Mapping<PauseScope, bool, ManualKey<0x0000_0005>>,
        /// Every on-chain reference a mint has used. Never cleared (`INV-2`). Mints only: no
        /// caller but the delivery role can add to it (`REQ-ID-4`).
        mint_references: Mapping<Reference, (), ManualKey<0x0000_0006>>,
        /// Every redemption, by its redeemer and reference. Never removed. A redemption's
        /// reference is its redeemer's own: no other caller can take or collide with it
        /// (`REQ-ID-4`), and the same redeemer cannot use it twice (`INV-2`, `AC-B5.2`).
        redemptions: Mapping<(AccountId, Reference), Redemption, ManualKey<0x0000_0007>>,
    }

    /// An amount was minted to an order's destination.
    #[ink(event)]
    #[derive(Debug, PartialEq, Eq)]
    pub struct Minted {
        /// The event's position in the contract's order.
        pub seq: EventSeq,
        /// The mint's on-chain reference.
        #[ink(topic)]
        pub reference: Reference,
        /// The asset.
        pub asset: AssetId,
        /// The destination.
        pub to: AccountId,
        /// The amount.
        pub amount: Balance,
    }

    /// An amount was taken from the redeemer and destroyed.
    #[ink(event)]
    #[derive(Debug, PartialEq, Eq)]
    pub struct Redeemed {
        /// The event's position in the contract's order.
        pub seq: EventSeq,
        /// The redemption's on-chain reference, scoped by its redeemer.
        #[ink(topic)]
        pub reference: Reference,
        /// Who redeemed.
        #[ink(topic)]
        pub redeemer: AccountId,
        /// The asset.
        pub asset: AssetId,
        /// The amount destroyed.
        pub amount: Balance,
        /// The backend that pays it out.
        pub backend: Backend,
        /// The payout instructions or their commitment.
        pub payout: Payout,
    }

    /// A redemption's redeemer was refunded by a refund mint.
    #[ink(event)]
    #[derive(Debug, PartialEq, Eq)]
    pub struct RefundMinted {
        /// The event's position in the contract's order.
        pub seq: EventSeq,
        /// The redemption's on-chain reference, scoped by its redeemer.
        #[ink(topic)]
        pub reference: Reference,
        /// The redeemer.
        #[ink(topic)]
        pub to: AccountId,
        /// The asset.
        pub asset: AssetId,
        /// The amount.
        pub amount: Balance,
    }

    /// A scope was paused.
    #[ink(event)]
    #[derive(Debug, PartialEq, Eq)]
    pub struct Paused {
        /// The event's position in the contract's order.
        pub seq: EventSeq,
        /// What was paused.
        pub scope: PauseScope,
        /// Who paused it.
        pub by: AccountId,
    }

    /// A scope was unpaused.
    #[ink(event)]
    #[derive(Debug, PartialEq, Eq)]
    pub struct Unpaused {
        /// The event's position in the contract's order.
        pub seq: EventSeq,
        /// What was unpaused.
        pub scope: PauseScope,
    }

    /// An asset's bounds took effect.
    #[ink(event)]
    #[derive(Debug, PartialEq, Eq)]
    pub struct BoundsSet {
        /// The event's position in the contract's order.
        pub seq: EventSeq,
        /// The asset.
        pub asset: AssetId,
        /// Its bounds.
        pub bounds: Bounds,
    }

    /// The backends an asset's redemptions may name took effect.
    #[ink(event)]
    #[derive(Debug, PartialEq, Eq)]
    pub struct BackendsSet {
        /// The event's position in the contract's order.
        pub seq: EventSeq,
        /// The asset.
        pub asset: AssetId,
        /// Its backends.
        pub backends: Vec<Backend>,
    }

    /// An asset was added, with the supply that already existed of it (`INV-3`).
    #[ink(event)]
    #[derive(Debug, PartialEq, Eq)]
    pub struct SupplyCounted {
        /// The event's position in the contract's order.
        pub seq: EventSeq,
        /// The asset.
        pub asset: AssetId,
        /// Its supply when added, counted against its ceiling.
        pub supply: Balance,
    }

    /// A raise of an asset's bounds, or a change of its backends, was scheduled.
    #[ink(event)]
    #[derive(Debug, PartialEq, Eq)]
    pub struct RaiseScheduled {
        /// The event's position in the contract's order.
        pub seq: EventSeq,
        /// The asset.
        pub asset: AssetId,
        /// The configuration it sets.
        pub config: AssetConfig,
        /// The first block it may be applied at.
        pub due: BlockNumber,
    }

    /// A scheduled raise was cancelled by a lowering.
    #[ink(event)]
    #[derive(Debug, PartialEq, Eq)]
    pub struct RaiseCancelled {
        /// The event's position in the contract's order.
        pub seq: EventSeq,
        /// The asset.
        pub asset: AssetId,
    }

    /// The roles changed.
    #[ink(event)]
    #[derive(Debug, PartialEq, Eq)]
    pub struct RolesSet {
        /// The event's position in the contract's order.
        pub seq: EventSeq,
        /// The roles.
        pub roles: Roles,
    }

    /// A change of the raise delay was scheduled.
    #[ink(event)]
    #[derive(Debug, PartialEq, Eq)]
    pub struct DelayScheduled {
        /// The event's position in the contract's order.
        pub seq: EventSeq,
        /// The delay it sets.
        pub delay: BlockNumber,
        /// The first block it may be applied at.
        pub due: BlockNumber,
    }

    /// A change of the governance account was scheduled.
    #[ink(event)]
    #[derive(Debug, PartialEq, Eq)]
    pub struct GovernanceScheduled {
        /// The event's position in the contract's order.
        pub seq: EventSeq,
        /// The incoming governance account.
        pub governance: AccountId,
        /// The first block it may accept at.
        pub due: BlockNumber,
    }

    /// The raise delay changed.
    #[ink(event)]
    #[derive(Debug, PartialEq, Eq)]
    pub struct DelaySet {
        /// The event's position in the contract's order.
        pub seq: EventSeq,
        /// The delay.
        pub delay: BlockNumber,
    }

    impl Bezent {
        /// Instantiate with three distinct roles, the raise delay, and the assets it starts with,
        /// each with its bounds, the backends its redemptions may name, and the supply that
        /// already exists of it (issued before its Issuer and Admin moved to the contract's
        /// community), which the supply counter starts from (`INV-3`).
        /// Refused with a raise delay of zero (`REQ-GV-3` needs a delay) or above
        /// [`MAX_RAISE_DELAY`].
        #[ink(constructor)]
        pub fn new(
            roles: Roles,
            raise_delay: BlockNumber,
            assets: Vec<(AssetId, AssetConfig)>,
        ) -> Result<Self, Error> {
            if !roles.are_distinct() {
                return Err(Error::RolesNotDistinct);
            }
            if !roles.are_usable(&Self::env().account_id()) {
                return Err(Error::InvalidRoleAccount);
            }
            check_delay(raise_delay)?;
            for (_, config) in &assets {
                config.check()?;
            }
            let mut contract = Self {
                roles,
                raise_delay,
                scheduled_delay: None,
                scheduled_governance: None,
                event_seq: 0,
                bounds: Mapping::new(),
                backends: Mapping::new(),
                scheduled_raises: Mapping::new(),
                windows: Mapping::new(),
                supply: Mapping::new(),
                pauses: Mapping::new(),
                mint_references: Mapping::new(),
                redemptions: Mapping::new(),
            };
            let seq = contract.next_seq();
            contract.env().emit_event(RolesSet { seq, roles });
            for (asset, config) in assets {
                contract.configure(asset, config, true);
            }
            Ok(contract)
        }

        // -----------------------------------------------------------------------------------
        // Value
        // -----------------------------------------------------------------------------------

        /// Mint `amount` of `asset` to `to`, with an on-chain reference. Delivery role only.
        ///
        /// Refused when a mint used the reference before (`INV-2`, `AC-D2.2`; redemptions keep their
        /// own references, `REQ-ID-4`); the amount is zero or above the per-transfer maximum, the mint window or the
        /// supply ceiling (`INV-3`); the asset or its mint direction is paused (`AC-I2.1`); or the
        /// asset is unknown.
        #[ink(message)]
        pub fn mint(
            &mut self,
            asset: AssetId,
            to: AccountId,
            amount: Balance,
            reference: Reference,
        ) -> Result<(), Error> {
            self.ensure_caller(self.roles.delivery)?;
            let bounds = self.bounds.get(asset).ok_or(Error::UnknownAsset)?;
            self.ensure_running(asset, Direction::Mint)?;
            if self.mint_references.contains(reference) {
                return Err(Error::ReferenceUsed);
            }
            if amount == 0 {
                return Err(Error::ZeroAmount);
            }
            if amount > bounds.per_transfer_max {
                return Err(Error::AboveTransferMaximum);
            }
            let window = self.window_after(asset, &bounds, amount)?;
            let supply = self.supply_after(asset, &bounds, amount)?;

            self.mint_through_kreivo(asset, amount, to)?;

            self.windows.insert(asset, &window);
            self.supply.insert(asset, &supply);
            self.mint_references.insert(reference, &());
            let seq = self.next_seq();
            self.env().emit_event(Minted {
                seq,
                reference,
                asset,
                to,
                amount,
            });
            Ok(())
        }

        /// Redeem `amount` of `asset`: take it from the caller and destroy it in this call,
        /// recording the backend, the payout instructions (or their commitment) and the reference.
        /// Anyone may call.
        ///
        /// The order is the safety argument (`REQ-HN-8`): pull from the caller into the contract,
        /// check the contract's own balance rose by exactly `amount`, then `assets__burn` from the
        /// contract's own account. Nothing is ever burnt from the caller. The pull uses the caller's
        /// approval of the contract when it covers `amount` (`Assets::transfer_approved` through
        /// `call_runtime`, which may empty the caller's account, so a holder redeems everything by
        /// batching `approve_transfer` and `redeem`); otherwise `assets__deposit`, which keeps the
        /// caller's account alive. Both pulls are checked the same way.
        ///
        /// Refused when the asset or its redeem direction is paused; the amount is below the
        /// asset's redemption minimum; the asset does not declare `backend` (`REQ-OT-4`);
        /// `payout` does not fit `backend` (a non-zero 20-byte address for an EVM backend, a
        /// 32-byte commitment for an attested one, `REQ-RQ-9`); the caller used the reference for a
        /// redemption before (`AC-B5.2`); or the asset is unknown. A redemption is keyed by its
        /// redeemer and reference, so the same reference used by another redeemer, or by a mint,
        /// is another key (`REQ-ID-4`).
        #[ink(message)]
        pub fn redeem(
            &mut self,
            asset: AssetId,
            amount: Balance,
            backend: Backend,
            payout: Vec<u8>,
            reference: Reference,
        ) -> Result<(), Error> {
            let bounds = self.bounds.get(asset).ok_or(Error::UnknownAsset)?;
            self.ensure_running(asset, Direction::Redeem)?;
            if amount < bounds.redeem_minimum {
                return Err(Error::BelowMinimum);
            }
            if !self
                .backends
                .get(asset)
                .unwrap_or_default()
                .contains(&backend)
            {
                return Err(Error::BackendNotDeclared);
            }
            let payout = Payout::fit(&backend, &payout).ok_or(Error::PayoutDoesNotFitBackend)?;
            let redeemer = self.env().caller();
            if self.redemptions.contains((redeemer, reference)) {
                return Err(Error::ReferenceUsed);
            }

            let me = self.env().account_id();
            let before = self.env().extension().balance(asset, me);
            if !self.pull_approved(asset, redeemer, amount) {
                let moved = self
                    .env()
                    .extension()
                    .deposit(asset, amount)
                    .map_err(Error::Kreivo)?;
                if moved != amount {
                    return Err(Error::DepositMismatch);
                }
            }
            let after = self.env().extension().balance(asset, me);
            if after.checked_sub(before) != Some(amount) {
                return Err(Error::DepositMismatch);
            }
            let burnt = self
                .env()
                .extension()
                .burn(asset, amount, me)
                .map_err(Error::Kreivo)?;
            if burnt < amount {
                return Err(Error::BurnShort);
            }

            let supply = self.supply.get(asset).unwrap_or(0).saturating_sub(burnt);
            self.supply.insert(asset, &supply);
            self.redemptions.insert(
                (redeemer, reference),
                &Redemption {
                    redeemer,
                    asset,
                    amount,
                    backend,
                    payout,
                    refunded: false,
                },
            );
            let seq = self.next_seq();
            self.env().emit_event(Redeemed {
                seq,
                reference,
                redeemer,
                asset,
                amount,
                backend,
                payout,
            });
            Ok(())
        }

        /// Refund a redemption whose payout the core proved not executed: mint its amount to its
        /// redeemer, once, tied to its redeemer and reference (`INV-8`, `REQ-OT-6`, `REQ-RF-4`).
        /// Delivery role only.
        ///
        /// Refused when there is no such redemption; it was refunded before; or the mint would
        /// exceed the mint window (it counts against it, `REQ-RF-4`) or the supply ceiling
        /// (`INV-3`). A pause does not stop it: the refund completes a withdrawal already in flight
        /// (`REQ-GV-1`, F-006 NOTES).
        #[ink(message)]
        pub fn refund_mint(
            &mut self,
            redeemer: AccountId,
            redemption_reference: Reference,
        ) -> Result<(), Error> {
            self.ensure_caller(self.roles.delivery)?;
            let key = (redeemer, redemption_reference);
            let mut redemption = self.redemptions.get(key).ok_or(Error::NoSuchRedemption)?;
            if redemption.refunded {
                return Err(Error::AlreadyRefunded);
            }
            let asset = redemption.asset;
            let bounds = self.bounds.get(asset).ok_or(Error::UnknownAsset)?;
            let window = self.window_after(asset, &bounds, redemption.amount)?;
            let supply = self.supply_after(asset, &bounds, redemption.amount)?;

            self.mint_through_kreivo(asset, redemption.amount, redemption.redeemer)?;

            self.windows.insert(asset, &window);
            self.supply.insert(asset, &supply);
            redemption.refunded = true;
            self.redemptions.insert(key, &redemption);
            let seq = self.next_seq();
            self.env().emit_event(RefundMinted {
                seq,
                reference: redemption_reference,
                to: redemption.redeemer,
                asset,
                amount: redemption.amount,
            });
            Ok(())
        }

        // -----------------------------------------------------------------------------------
        // Pauses
        // -----------------------------------------------------------------------------------

        /// Pause a scope. The pause role or governance.
        #[ink(message)]
        pub fn pause(&mut self, scope: PauseScope) -> Result<(), Error> {
            let by = self.env().caller();
            if by != self.roles.pause && by != self.roles.governance {
                return Err(Error::Unauthorised);
            }
            self.pauses.insert(scope, &true);
            let seq = self.next_seq();
            self.env().emit_event(Paused { seq, scope, by });
            Ok(())
        }

        /// Unpause a scope. Governance only (`REQ-GV-2`, `AC-I2.2`).
        #[ink(message)]
        pub fn unpause(&mut self, scope: PauseScope) -> Result<(), Error> {
            self.ensure_caller(self.roles.governance)?;
            self.pauses.remove(scope);
            let seq = self.next_seq();
            self.env().emit_event(Unpaused { seq, scope });
            Ok(())
        }

        // -----------------------------------------------------------------------------------
        // Bounds
        // -----------------------------------------------------------------------------------

        /// Lower a known asset's bounds, at once. The pause role or governance. Refused if any
        /// field would loosen (`NotALowering`). A raise scheduled for the asset is cancelled, so a
        /// lowering made in an incident is not undone by a raise decided before it. A longer
        /// window stalls the asset's mints for a while (see [`Window::lengthen`], `INV-3`).
        #[ink(message)]
        pub fn lower_bounds(&mut self, asset: AssetId, bounds: Bounds) -> Result<(), Error> {
            let caller = self.env().caller();
            if caller != self.roles.pause && caller != self.roles.governance {
                return Err(Error::Unauthorised);
            }
            let current = self.bounds.get(asset).ok_or(Error::UnknownAsset)?;
            if !bounds.is_valid() {
                return Err(Error::InvalidBounds);
            }
            if !bounds.is_no_looser_than(&current) {
                return Err(Error::NotALowering);
            }
            self.bounds.insert(asset, &bounds);
            self.stall_if_lengthened(asset, Some(current), &bounds);
            if self.scheduled_raises.contains(asset) {
                self.scheduled_raises.remove(asset);
                let seq = self.next_seq();
                self.env().emit_event(RaiseCancelled { seq, asset });
            }
            let seq = self.next_seq();
            self.env().emit_event(BoundsSet { seq, asset, bounds });
            Ok(())
        }

        /// Schedule new bounds and backends for an asset, applicable after the raise delay
        /// (`REQ-GV-3`). Governance only. An unknown asset becomes known this way, its supply
        /// counter starting from `config.initial_supply` (`INV-3`); for a known asset that must be
        /// zero, since no administrative path edits a counter (`REQ-GV-5`). A new schedule
        /// replaces the one before and restarts the delay.
        #[ink(message)]
        pub fn schedule_raise(&mut self, asset: AssetId, config: AssetConfig) -> Result<(), Error> {
            self.ensure_caller(self.roles.governance)?;
            config.check()?;
            if config.initial_supply != 0 && self.bounds.contains(asset) {
                return Err(Error::SupplyOfAKnownAsset);
            }
            let due = self
                .env()
                .block_number()
                .checked_add(self.raise_delay)
                .ok_or(Error::Overflow)?;
            self.scheduled_raises.insert(
                asset,
                &ScheduledRaise {
                    config: config.clone(),
                    due,
                },
            );
            let seq = self.next_seq();
            self.env().emit_event(RaiseScheduled {
                seq,
                asset,
                config,
                due,
            });
            Ok(())
        }

        /// Apply a scheduled raise once its delay has passed. Anyone may call.
        #[ink(message)]
        pub fn apply_raise(&mut self, asset: AssetId) -> Result<(), Error> {
            let raise = self
                .scheduled_raises
                .get(asset)
                .ok_or(Error::NoRaiseScheduled)?;
            if self.env().block_number() < raise.due {
                return Err(Error::RaiseNotDue);
            }
            self.scheduled_raises.remove(asset);
            let added = !self.bounds.contains(asset);
            self.configure(asset, raise.config, added);
            Ok(())
        }

        /// Schedule a new raise delay, applicable after the current one. Governance only. Refused
        /// if it is zero (`REQ-GV-3` needs a delay) or above [`MAX_RAISE_DELAY`] (it would freeze
        /// every delayed path).
        #[ink(message)]
        pub fn schedule_raise_delay(&mut self, delay: BlockNumber) -> Result<(), Error> {
            self.ensure_caller(self.roles.governance)?;
            check_delay(delay)?;
            let due = self
                .env()
                .block_number()
                .checked_add(self.raise_delay)
                .ok_or(Error::Overflow)?;
            self.scheduled_delay = Some(ScheduledDelay { delay, due });
            let seq = self.next_seq();
            self.env().emit_event(DelayScheduled { seq, delay, due });
            Ok(())
        }

        /// Apply a scheduled raise delay once the current delay has passed. Anyone may call.
        #[ink(message)]
        pub fn apply_raise_delay(&mut self) -> Result<(), Error> {
            let scheduled = self.scheduled_delay.ok_or(Error::NoDelayScheduled)?;
            if self.env().block_number() < scheduled.due {
                return Err(Error::DelayNotDue);
            }
            self.scheduled_delay = None;
            self.raise_delay = scheduled.delay;
            let seq = self.next_seq();
            self.env().emit_event(DelaySet {
                seq,
                delay: scheduled.delay,
            });
            Ok(())
        }

        // -----------------------------------------------------------------------------------
        // Roles
        // -----------------------------------------------------------------------------------

        /// Replace the delivery and pause roles, at once. Governance only. Refused unless the
        /// three are different accounts (`REQ-HN-9`), none the all-zero account or the contract's
        /// own; and if `roles.governance` is not the current governance account: that change goes
        /// through [`schedule_governance`](Self::schedule_governance).
        #[ink(message)]
        pub fn set_roles(&mut self, roles: Roles) -> Result<(), Error> {
            self.ensure_caller(self.roles.governance)?;
            if roles.governance != self.roles.governance {
                return Err(Error::GovernanceChangeIsDelayed);
            }
            if !roles.are_distinct() {
                return Err(Error::RolesNotDistinct);
            }
            if !roles.are_usable(&self.env().account_id()) {
                return Err(Error::InvalidRoleAccount);
            }
            self.roles = roles;
            let seq = self.next_seq();
            self.env().emit_event(RolesSet { seq, roles });
            Ok(())
        }

        /// Schedule a new governance account, which may accept after the raise delay
        /// (`REQ-GV-3`). Governance only. A new schedule replaces the one before and restarts the
        /// delay; scheduling the current account withdraws a change. Refused for the all-zero
        /// account, the contract's own, or the delivery or pause account (`REQ-HN-9`).
        #[ink(message)]
        pub fn schedule_governance(&mut self, governance: AccountId) -> Result<(), Error> {
            self.ensure_caller(self.roles.governance)?;
            if !Roles::is_usable(&governance, &self.env().account_id()) {
                return Err(Error::InvalidRoleAccount);
            }
            if governance == self.roles.delivery || governance == self.roles.pause {
                return Err(Error::RolesNotDistinct);
            }
            let due = self
                .env()
                .block_number()
                .checked_add(self.raise_delay)
                .ok_or(Error::Overflow)?;
            self.scheduled_governance = Some(ScheduledGovernance { governance, due });
            let seq = self.next_seq();
            self.env().emit_event(GovernanceScheduled {
                seq,
                governance,
                due,
            });
            Ok(())
        }

        /// Accept a scheduled governance change once the delay has passed. Only the incoming
        /// governance account may call: accepting proves it can act, so governance never passes
        /// to an account that cannot. Refused if delivery or pause has since become that account.
        #[ink(message)]
        pub fn accept_governance(&mut self) -> Result<(), Error> {
            let scheduled = self
                .scheduled_governance
                .ok_or(Error::NoGovernanceScheduled)?;
            self.ensure_caller(scheduled.governance)?;
            if self.env().block_number() < scheduled.due {
                return Err(Error::GovernanceNotDue);
            }
            let roles = Roles {
                governance: scheduled.governance,
                ..self.roles
            };
            if !roles.are_distinct() {
                return Err(Error::RolesNotDistinct);
            }
            self.scheduled_governance = None;
            self.roles = roles;
            let seq = self.next_seq();
            self.env().emit_event(RolesSet { seq, roles });
            Ok(())
        }

        // -----------------------------------------------------------------------------------
        // Reads
        // -----------------------------------------------------------------------------------

        /// The roles.
        #[ink(message)]
        pub fn roles(&self) -> Roles {
            self.roles
        }

        /// The governance change scheduled, if any.
        #[ink(message)]
        pub fn scheduled_governance(&self) -> Option<ScheduledGovernance> {
            self.scheduled_governance
        }

        /// An asset's bounds, if it is known.
        #[ink(message)]
        pub fn bounds(&self, asset: AssetId) -> Option<Bounds> {
            self.bounds.get(asset)
        }

        /// The backends an asset's redemptions may name; none if it is unknown.
        #[ink(message)]
        pub fn backends(&self, asset: AssetId) -> Vec<Backend> {
            self.backends.get(asset).unwrap_or_default()
        }

        /// The raise scheduled for an asset, if any.
        #[ink(message)]
        pub fn scheduled_raise(&self, asset: AssetId) -> Option<ScheduledRaise> {
            self.scheduled_raises.get(asset)
        }

        /// The raise delay, in blocks, and the change of it scheduled, if any.
        #[ink(message)]
        pub fn raise_delay(&self) -> (BlockNumber, Option<ScheduledDelay>) {
            (self.raise_delay, self.scheduled_delay)
        }

        /// Whether exactly this scope is paused. A mint or redemption is stopped when any scope
        /// covering it is.
        #[ink(message)]
        pub fn paused(&self, scope: PauseScope) -> bool {
            self.pauses.contains(scope)
        }

        /// A redemption, by its redeemer and reference.
        #[ink(message)]
        pub fn redemption(&self, redeemer: AccountId, reference: Reference) -> Option<Redemption> {
            self.redemptions.get((redeemer, reference))
        }

        /// Whether a mint has used this reference.
        #[ink(message)]
        pub fn mint_reference_used(&self, reference: Reference) -> bool {
            self.mint_references.contains(reference)
        }

        /// What counts against an asset's mint window now, refund mints included: what was
        /// minted in the window ending at this block, plus what shares a bucket with it (see
        /// [`Window`]). `Balance::MAX` while a longer window stalls the asset's mints.
        #[ink(message)]
        pub fn minted_in_window(&self, asset: AssetId) -> Balance {
            match (self.bounds.get(asset), self.windows.get(asset)) {
                (Some(bounds), Some(window)) => {
                    window.counted(self.env().block_number(), bounds.window_blocks)
                }
                _ => 0,
            }
        }

        /// An asset's supply as the contract counts it: its initial supply, plus what the
        /// contract minted, less what it destroyed.
        #[ink(message)]
        pub fn supply(&self, asset: AssetId) -> Balance {
            self.supply.get(asset).unwrap_or(0)
        }

        /// The sequence number the next event will carry; also how many events there have been.
        #[ink(message)]
        pub fn event_seq(&self) -> EventSeq {
            self.event_seq
        }

        // -----------------------------------------------------------------------------------
        // Internals
        // -----------------------------------------------------------------------------------

        /// Set an asset's bounds and backends, already checked, and emit both. An asset being
        /// `added` also starts its supply counter from its initial supply. A longer window stalls
        /// the asset's mints for a while (see [`Window::lengthen`], `INV-3`).
        fn configure(&mut self, asset: AssetId, config: AssetConfig, added: bool) {
            let AssetConfig {
                bounds,
                backends,
                initial_supply,
            } = config;
            let current = self.bounds.get(asset);
            self.bounds.insert(asset, &bounds);
            self.stall_if_lengthened(asset, current, &bounds);
            self.backends.insert(asset, &backends);
            let seq = self.next_seq();
            self.env().emit_event(BoundsSet { seq, asset, bounds });
            let seq = self.next_seq();
            self.env().emit_event(BackendsSet {
                seq,
                asset,
                backends,
            });
            if added {
                self.supply.insert(asset, &initial_supply);
                let seq = self.next_seq();
                self.env().emit_event(SupplyCounted {
                    seq,
                    asset,
                    supply: initial_supply,
                });
            }
        }

        fn ensure_caller(&self, role: AccountId) -> Result<(), Error> {
            if self.env().caller() == role {
                Ok(())
            } else {
                Err(Error::Unauthorised)
            }
        }

        /// Refuses when any scope covering `asset` in `direction` is paused.
        fn ensure_running(&self, asset: AssetId, direction: Direction) -> Result<(), Error> {
            let covering = [
                PauseScope::All,
                PauseScope::Direction(direction),
                PauseScope::Asset(asset),
                PauseScope::AssetDirection(asset, direction),
            ];
            if covering.iter().any(|scope| self.pauses.contains(scope)) {
                Err(Error::Paused)
            } else {
                Ok(())
            }
        }

        /// When `bounds` make the asset's window longer than `current` did, stall the window
        /// until no mint it dropped is inside the new one (`INV-3`, [`Window::lengthen`]).
        fn stall_if_lengthened(
            &mut self,
            asset: AssetId,
            current: Option<Bounds>,
            bounds: &Bounds,
        ) {
            let lengthened =
                current.is_some_and(|current| bounds.window_blocks > current.window_blocks);
            if let (true, Some(mut window)) = (lengthened, self.windows.get(asset)) {
                if window.lengthen(self.env().block_number(), bounds.window_blocks) {
                    self.windows.insert(asset, &window);
                }
            }
        }

        /// The asset's sliding mint window after minting `amount` more now, or `AboveWindow`
        /// (`INV-3`). Changing the bounds keeps what the window counts.
        fn window_after(
            &self,
            asset: AssetId,
            bounds: &Bounds,
            amount: Balance,
        ) -> Result<Window, Error> {
            self.windows
                .get(asset)
                .unwrap_or_default()
                .after(self.env().block_number(), bounds, amount)
                .ok_or(Error::AboveWindow)
        }

        /// The asset's supply after minting `amount` more, or `AboveCeiling`.
        fn supply_after(
            &self,
            asset: AssetId,
            bounds: &Bounds,
            amount: Balance,
        ) -> Result<Balance, Error> {
            self.supply
                .get(asset)
                .unwrap_or(0)
                .checked_add(amount)
                .filter(|supply| *supply <= bounds.supply_ceiling)
                .ok_or(Error::AboveCeiling)
        }

        /// Pull `amount` of `asset` from `owner` into the contract out of `owner`'s approval of the
        /// contract (`Assets::transfer_approved`, `keep_alive: false`). Whether it was dispatched;
        /// a refused dispatch changes nothing. The caller checks what arrived.
        fn pull_approved(&self, asset: AssetId, owner: AccountId, amount: Balance) -> bool {
            let call = RuntimeCall::Assets(AssetsCall::TransferApproved {
                id: asset,
                owner: MultiAddress::Id(owner),
                destination: MultiAddress::Id(self.env().account_id()),
                amount,
            });
            // ink!'s off-chain engine cannot dispatch runtime calls; the unit tests hand the
            // encoded call to the model of Kreivo instead (`crate::mock::call_runtime`).
            #[cfg(not(test))]
            {
                self.env().call_runtime(&call).is_ok()
            }
            #[cfg(test)]
            {
                crate::mock::call_runtime(&scale::Encode::encode(&call))
            }
        }

        fn mint_through_kreivo(
            &self,
            asset: AssetId,
            amount: Balance,
            to: AccountId,
        ) -> Result<(), Error> {
            let minted = self
                .env()
                .extension()
                .mint(asset, amount, to)
                .map_err(Error::Kreivo)?;
            if minted == amount {
                Ok(())
            } else {
                Err(Error::MintMismatch)
            }
        }

        fn next_seq(&mut self) -> EventSeq {
            let seq = self.event_seq;
            self.event_seq = seq.saturating_add(1);
            seq
        }
    }
}
