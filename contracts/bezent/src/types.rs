// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The types the contract stores, takes, returns and emits.

// `parity-scale-codec`'s derive casts each enum variant's index to `u8` with the variant's own
// span, which `cargo contract build`'s `cast_possible_truncation` check then reports against these
// type definitions. Nothing else in this module casts.
#![allow(clippy::cast_possible_truncation)]

pub use crate::kreivo::{AssetsApiError, KreivoApiError};
use ink::prelude::vec::Vec;

/// An account on Kreivo (`AccountId32`).
pub type AccountId = ink::primitives::AccountId;
/// An amount, in the asset's smallest unit. Kreivo's `Balance` is `u128`.
pub type Balance = u128;
/// Kreivo's own block number.
pub type BlockNumber = u32;
/// An on-chain reference: a fixed, one-to-one digest of a bridge reference (SPEC §2). The contract
/// never interprets it; it only refuses one it has seen before (`INV-2`).
pub type Reference = [u8; 32];
/// A position in the contract's event order, from zero, increasing by one per event (`REQ-HN-7`).
pub type EventSeq = u64;

/// A bridged asset's identity on Kreivo: a local asset (`virto_common::FungibleAssetLocation::Here`).
///
/// Only local assets can be minted and burnt by a contract on Kreivo (kreivo#512,
/// `Config::MintableAssets`), so the contract's asset id has only that variant. It encodes byte for
/// byte as Kreivo's `Here(id)`; a test proves it against Kreivo's own crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[ink::scale_derive(Encode, Decode, TypeInfo)]
#[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
pub enum AssetId {
    /// An asset issued on Kreivo, by its index in `pallet-assets`.
    #[codec(index = 0)]
    Here(u32),
}

/// A direction of the contract's value flow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[ink::scale_derive(Encode, Decode, TypeInfo)]
#[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
pub enum Direction {
    /// Mints: deposits' mints and refund mints are both new supply, but only the first is stopped
    /// by a pause (see `refund_mint`).
    Mint,
    /// Redemptions.
    Redeem,
}

/// What a pause covers (SPEC §7.F: per asset, per direction, or all; plan: one asset and
/// direction).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[ink::scale_derive(Encode, Decode, TypeInfo)]
#[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
pub enum PauseScope {
    /// Every asset, both directions.
    All,
    /// One direction, of every asset.
    Direction(Direction),
    /// One asset, both directions.
    Asset(AssetId),
    /// One asset, one direction.
    AssetDirection(AssetId, Direction),
}

/// An asset's bounds (SPEC §7.F, `INV-3`). An asset with no bounds is unknown to the contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[ink::scale_derive(Encode, Decode, TypeInfo)]
#[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
pub struct Bounds {
    /// The most supply the contract may have outstanding of the asset.
    pub supply_ceiling: Balance,
    /// The most the contract may mint of the asset in one window, refund mints included
    /// (`REQ-RF-4`).
    pub window_amount: Balance,
    /// The length of the mint window, in Kreivo blocks: no span of this many consecutive blocks
    /// mints more than `window_amount` (`INV-3`). The window slides; see [`Window`]. Never zero.
    pub window_blocks: BlockNumber,
    /// The most one mint may carry.
    pub per_transfer_max: Balance,
    /// The least one redemption may carry. Never zero (`REQ-LM-1`: at least one minor unit).
    pub redeem_minimum: Balance,
}

impl Bounds {
    /// Whether these bounds can be set at all.
    pub fn is_valid(&self) -> bool {
        self.window_blocks > 0 && self.redeem_minimum > 0
    }

    /// Whether `self` is no looser than `current` in any field: every cap equal or lower, the
    /// window equal or longer, the redemption minimum equal or higher. Only such a change may take
    /// effect at once (`REQ-GV-2`, `REQ-GV-3`).
    pub fn is_no_looser_than(&self, current: &Bounds) -> bool {
        self.supply_ceiling <= current.supply_ceiling
            && self.window_amount <= current.window_amount
            && self.per_transfer_max <= current.per_transfer_max
            && self.window_blocks >= current.window_blocks
            && self.redeem_minimum >= current.redeem_minimum
    }
}

/// How many buckets a mint window is split into. A bucket spans at most
/// `ceil(window_blocks / WINDOW_BUCKETS)` blocks, so a mint is counted for at most that many blocks
/// longer than the window: the check under-admits by at most one bucket, never over-admits.
pub const WINDOW_BUCKETS: BlockNumber = 10;

/// The most buckets an asset's window keeps. With `WINDOW_BUCKETS + 1` buckets, each opened at
/// least a bucket's length after the one before, the oldest has always left the window by the time
/// a new one would push it out; if bounds changed and it has not, it is merged, not dropped.
pub const WINDOW_SLOTS: usize = WINDOW_BUCKETS as usize + 1;

/// The length of one bucket of a window of `window_blocks` blocks.
pub fn bucket_blocks(window_blocks: BlockNumber) -> BlockNumber {
    window_blocks.div_ceil(WINDOW_BUCKETS).max(1)
}

/// What was minted from block `start` to block `last`, refund mints included.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[ink::scale_derive(Encode, Decode, TypeInfo)]
#[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
pub struct Bucket {
    /// The block the bucket opened at.
    pub start: BlockNumber,
    /// The block of the last mint counted in it. Every mint it counts was made at or before it.
    pub last: BlockNumber,
    /// What it counts.
    pub minted: Balance,
}

/// An asset's sliding mint window (`INV-3`: mints in any window never exceed the window bound).
///
/// A fixed window lets twice the bound through in two blocks: the whole bound in the last block
/// of one window and again in the first block of the next (review of PR #20, M2). This window is
/// a list of buckets, oldest first. A mint at block `now` is let through only if it, plus every
/// bucket whose last mint is inside the `window_blocks` blocks ending at `now`, stays within
/// `window_amount`.
///
/// Why no span of `window_blocks` blocks can exceed the bound: take any such span and its last
/// mint, at block `t`. Every earlier mint in the span was made at a block `m >= t -
/// window_blocks + 1` and sits in a bucket whose `last >= m`, so the check at `t` counted it.
/// Buckets are only dropped once their `last` is out of the window, and a bucket pushed out of the
/// list is merged into the next one, whose `last` is later. So the check at `t` saw the whole span.
/// The price is a bucket's length: a mint counts until the last mint of its bucket leaves the
/// window, at most `bucket_blocks(window_blocks) - 1` blocks late.
///
/// That argument needs every dropped bucket to be out of the window in force, which holds while
/// the window keeps its length or shortens. When `window_blocks` grows from `W` to `W'` at block
/// `n`, mints dropped under `W` may still be inside the new window (review 2 of PR #20, S5), so
/// the window is stalled (see [`Window::lengthen`]): every mint and refund mint of the asset is
/// refused until the newest dropped mint, at block `d`, has left the new window, that is up to
/// block `d + W' - 1`, at most `W' - W` blocks after `n`. After that, nothing the window dropped
/// can be inside a span of `W'` blocks that ends at a new mint, and the argument holds again.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
#[ink::scale_derive(Encode, Decode, TypeInfo)]
#[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
pub struct Window {
    /// The buckets, oldest first; at most [`WINDOW_SLOTS`], plus a [`Window::lengthen`] stall.
    pub buckets: Vec<Bucket>,
    /// The `last` of the newest bucket dropped from the window, if any was: the newest mint the
    /// window no longer counts.
    pub dropped_last: Option<BlockNumber>,
}

impl Window {
    /// The first block of the window of `window_blocks` blocks that ends at `now`.
    fn first_block(now: BlockNumber, window_blocks: BlockNumber) -> BlockNumber {
        now.saturating_sub(window_blocks.saturating_sub(1))
    }

    /// What counts against the window of `window_blocks` blocks ending at `now`.
    pub fn counted(&self, now: BlockNumber, window_blocks: BlockNumber) -> Balance {
        let first = Self::first_block(now, window_blocks);
        self.buckets
            .iter()
            .filter(|bucket| bucket.last >= first)
            .fold(0, |sum, bucket| sum.saturating_add(bucket.minted))
    }

    /// The window after minting `amount` more at `now`, or `None` if that would exceed
    /// `bounds.window_amount` in the window ending at `now`.
    pub fn after(&self, now: BlockNumber, bounds: &Bounds, amount: Balance) -> Option<Window> {
        let first = Self::first_block(now, bounds.window_blocks);
        let mut dropped_last = self.dropped_last;
        let mut buckets: Vec<Bucket> = Vec::new();
        for bucket in &self.buckets {
            if bucket.last >= first {
                buckets.push(*bucket);
            } else {
                dropped_last = dropped_last.max(Some(bucket.last));
            }
        }
        let counted = buckets
            .iter()
            .fold(0, |sum: Balance, bucket| sum.saturating_add(bucket.minted));
        counted
            .checked_add(amount)
            .filter(|total| *total <= bounds.window_amount)?;
        match buckets.last_mut() {
            Some(bucket)
                if now.saturating_sub(bucket.start) < bucket_blocks(bounds.window_blocks) =>
            {
                bucket.minted = bucket.minted.checked_add(amount)?;
                bucket.last = now;
            }
            _ => buckets.push(Bucket {
                start: now,
                last: now,
                minted: amount,
            }),
        }
        if buckets.len() > WINDOW_SLOTS {
            let oldest = buckets.remove(0);
            let next = buckets.first_mut()?;
            next.minted = next.minted.checked_add(oldest.minted)?;
        }
        Some(Window {
            buckets,
            dropped_last,
        })
    }

    /// Stall the window because its length grew to `window_blocks` at `now` (`INV-3`). Returns
    /// whether it changed.
    ///
    /// If the newest dropped mint, at block `d`, is inside the new window ending at `now`, a
    /// bucket that counts `Balance::MAX` at block `d` goes in front: every mint is refused while
    /// it is counted, and it leaves on its own once `d` is out of the window, at block
    /// `d + window_blocks`. A later lengthening keeps it longer, a shortening lets it go sooner,
    /// as each needs. If `d` is already out of the new window, nothing changes.
    pub fn lengthen(&mut self, now: BlockNumber, window_blocks: BlockNumber) -> bool {
        let Some(dropped) = self.dropped_last else {
            return false;
        };
        if dropped < Self::first_block(now, window_blocks) {
            return false;
        }
        let stall = Bucket {
            start: dropped,
            last: dropped,
            minted: Balance::MAX,
        };
        if self.buckets.first() == Some(&stall) {
            return false;
        }
        self.buckets.insert(0, stall);
        true
    }
}

/// The most backends one asset may declare. Since amendment 0007 an asset is paid out on its
/// reserve's network only (`REQ-OT-4`), so one is the usual number.
pub const MAX_BACKENDS: usize = 4;

/// What governance configures an asset with: its bounds and the backends its redemptions may
/// name. Changed only through the delayed path (`schedule_raise`), except that the pause role or
/// governance may lower the bounds at once.
#[derive(Clone, Debug, PartialEq, Eq)]
#[ink::scale_derive(Encode, Decode, TypeInfo)]
#[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
pub struct AssetConfig {
    /// The asset's bounds.
    pub bounds: Bounds,
    /// The backends its redemptions may name (`REQ-OT-4`): one to [`MAX_BACKENDS`], no repeats.
    pub backends: Vec<Backend>,
    /// The supply that already exists of the asset when the contract adds it, issued before its
    /// Issuer and Admin moved to the contract's community. The supply counter starts from it, so
    /// the ceiling bounds all of the asset's supply (`INV-3`). Taken only when the asset is
    /// added; zero for a known asset.
    pub initial_supply: Balance,
}

impl AssetConfig {
    /// Whether the configuration can be set at all.
    pub fn check(&self) -> Result<(), Error> {
        if !self.bounds.is_valid() {
            return Err(Error::InvalidBounds);
        }
        let repeats = self
            .backends
            .iter()
            .enumerate()
            .any(|(i, backend)| self.backends[..i].contains(backend));
        if self.backends.is_empty() || self.backends.len() > MAX_BACKENDS || repeats {
            return Err(Error::InvalidBackends);
        }
        Ok(())
    }
}

/// The three roles (`REQ-HN-9`). They are always three different accounts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[ink::scale_derive(Encode, Decode, TypeInfo)]
#[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
pub struct Roles {
    /// Mints and refund-mints.
    pub delivery: AccountId,
    /// Pauses and lowers bounds.
    pub pause: AccountId,
    /// Unpauses, raises bounds (after the delay) and sets roles; it is replaced only after the
    /// delay, by an account that accepts. On Kreivo, the account of the
    /// community dedicated to the contract, whose decisions need more than one approver
    /// (`REQ-GV-2`, `REQ-HN-10`).
    pub governance: AccountId,
}

impl Roles {
    /// Whether the three roles are held by three different accounts.
    pub fn are_distinct(&self) -> bool {
        self.delivery != self.pause
            && self.delivery != self.governance
            && self.pause != self.governance
    }

    /// Whether `account` can hold a role in the contract at `contract`: not the all-zero account
    /// and not the contract's own, either of which would leave the role with no one to use it.
    pub fn is_usable(account: &AccountId, contract: &AccountId) -> bool {
        *account != AccountId::from([0u8; 32]) && account != contract
    }

    /// Whether every role is held by a usable account.
    pub fn are_usable(&self, contract: &AccountId) -> bool {
        [self.delivery, self.pause, self.governance]
            .iter()
            .all(|account| Self::is_usable(account, contract))
    }
}

/// The backend a redemption is paid out by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[ink::scale_derive(Encode, Decode, TypeInfo)]
#[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
pub enum Backend {
    /// A public EVM origin network, by chain id. Its payout instructions are inline: a 20-byte
    /// address, never the zero address (`REQ-RQ-9`).
    Evm {
        /// The chain id.
        chain_id: u64,
    },
    /// An attested backend, by the id bezent declares for it. Its payout instructions are only a
    /// commitment: a 32-byte hash (`REQ-RQ-9`).
    Attested {
        /// The backend's id.
        id: u32,
    },
}

/// A redemption's payout instructions, or their commitment, once they fit the backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[ink::scale_derive(Encode, Decode, TypeInfo)]
#[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
pub enum Payout {
    /// A destination address on an EVM network.
    Address([u8; 20]),
    /// A hash of the instructions, for an attested backend.
    Commitment([u8; 32]),
}

impl Payout {
    /// The payout `bytes` as `backend` takes them, if they fit it.
    pub fn fit(backend: &Backend, bytes: &[u8]) -> Option<Payout> {
        match backend {
            Backend::Evm { .. } => bytes
                .try_into()
                .ok()
                .filter(|address: &[u8; 20]| *address != [0; 20])
                .map(Payout::Address),
            Backend::Attested { .. } => bytes.try_into().ok().map(Payout::Commitment),
        }
    }
}

/// A redemption, as recorded (`REQ-RQ-9`, `INV-8`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[ink::scale_derive(Encode, Decode, TypeInfo)]
#[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
pub struct Redemption {
    /// Who redeemed; a refund mint goes to them.
    pub redeemer: AccountId,
    /// The asset.
    pub asset: AssetId,
    /// The amount destroyed.
    pub amount: Balance,
    /// The backend that pays it out.
    pub backend: Backend,
    /// The payout instructions or their commitment.
    pub payout: Payout,
    /// Whether it was refunded by a refund mint. Set once, never cleared.
    pub refunded: bool,
}

/// Kreivo's block time in milliseconds: about 2.07 s, measured as 21,997 blocks between 11:09 and
/// 23:49 UTC on 2026-10-15 (bloque-app/bezent#58). Every block count in this contract (the window
/// length, the raise delay and its cap) assumes it; if the issuing network's block time changes,
/// this is the one constant to revisit.
pub const BLOCK_TIME_MS: u32 = 2_070;

/// Blocks in 24 hours at [`BLOCK_TIME_MS`], rounded down: 41,739. A 24-hour window is this many
/// blocks.
pub const BLOCKS_PER_DAY: BlockNumber = 86_400_000 / BLOCK_TIME_MS;

/// The longest raise delay, in blocks: 28 days of Kreivo's ~2.07 s blocks ([`BLOCKS_PER_DAY`] * 28 =
/// 1,168,692). Every delayed path (a raise, a new asset, a change of the delay itself, a governance
/// handover) waits the delay, so an unbounded one could freeze them all; shortening it waits the
/// current delay too (review 2 of PR #20, S6).
pub const MAX_RAISE_DELAY: BlockNumber = 28 * BLOCKS_PER_DAY;

/// Whether `delay` can be the raise delay: not zero (`REQ-GV-3` needs a delay) and not above
/// [`MAX_RAISE_DELAY`].
pub fn check_delay(delay: BlockNumber) -> Result<(), Error> {
    match delay {
        0 => Err(Error::ZeroDelay),
        d if d > MAX_RAISE_DELAY => Err(Error::DelayTooLong),
        _ => Ok(()),
    }
}

/// A raise of an asset's bounds, or a change of its backends, waiting for the delay
/// (`REQ-GV-3`).
#[derive(Clone, Debug, PartialEq, Eq)]
#[ink::scale_derive(Encode, Decode, TypeInfo)]
#[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
pub struct ScheduledRaise {
    /// The configuration it sets.
    pub config: AssetConfig,
    /// The first block it may be applied at.
    pub due: BlockNumber,
}

/// A change of the raise delay, waiting for the current delay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[ink::scale_derive(Encode, Decode, TypeInfo)]
#[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
pub struct ScheduledDelay {
    /// The delay it sets, in blocks.
    pub delay: BlockNumber,
    /// The first block it may be applied at.
    pub due: BlockNumber,
}

/// A change of the governance account, waiting for the raise delay and the new account's
/// acceptance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[ink::scale_derive(Encode, Decode, TypeInfo)]
#[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
pub struct ScheduledGovernance {
    /// The incoming governance account.
    pub governance: AccountId,
    /// The first block it may accept at.
    pub due: BlockNumber,
}

/// Why a message was refused. A refused message changes nothing: the contract reverts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[ink::scale_derive(Encode, Decode, TypeInfo)]
pub enum Error {
    /// The caller does not hold the role the message needs.
    Unauthorised,
    /// The asset has no bounds in the contract.
    UnknownAsset,
    /// The asset or direction is paused.
    Paused,
    /// A mint used the on-chain reference before, or the caller used it for a redemption before
    /// (`INV-2`).
    ReferenceUsed,
    /// A mint of nothing.
    ZeroAmount,
    /// The amount is above the asset's per-transfer maximum.
    AboveTransferMaximum,
    /// The mint would exceed the asset's mint window (`INV-3`).
    AboveWindow,
    /// The mint would take supply above the asset's ceiling (`INV-3`).
    AboveCeiling,
    /// The amount is below the asset's redemption minimum.
    BelowMinimum,
    /// The payout instructions do not fit the backend (`REQ-RQ-9`).
    PayoutDoesNotFitBackend,
    /// The asset does not declare the backend the redemption names (`REQ-OT-4`).
    BackendNotDeclared,
    /// An asset configuration with no backends, more than [`MAX_BACKENDS`], or one repeated.
    InvalidBackends,
    /// An initial supply for an asset the contract already knows: its counter is the contract's.
    SupplyOfAKnownAsset,
    /// No redemption has that redeemer and reference.
    NoSuchRedemption,
    /// That redemption was refunded before (`INV-8`).
    AlreadyRefunded,
    /// A change through `lower_bounds` that loosens a field (`REQ-GV-3`).
    NotALowering,
    /// Bounds with a zero window or a zero redemption minimum.
    InvalidBounds,
    /// No raise is scheduled for the asset.
    NoRaiseScheduled,
    /// The raise's delay has not passed (`REQ-GV-3`).
    RaiseNotDue,
    /// No change of the raise delay is scheduled.
    NoDelayScheduled,
    /// The delay change's own delay has not passed.
    DelayNotDue,
    /// A raise delay of zero blocks: a raise would take effect in the block it is scheduled
    /// (`REQ-GV-3` needs a delay).
    ZeroDelay,
    /// A raise delay above [`MAX_RAISE_DELAY`]: it would hold back raises, delay changes and a
    /// governance handover for longer than the contract allows.
    DelayTooLong,
    /// Delivery, pause and governance are not three different accounts (`REQ-HN-9`).
    RolesNotDistinct,
    /// A role given to the all-zero account or to the contract's own.
    InvalidRoleAccount,
    /// `set_roles` tried to change the governance account, which only `schedule_governance`
    /// and `accept_governance` do, after the delay.
    GovernanceChangeIsDelayed,
    /// No governance change is scheduled.
    NoGovernanceScheduled,
    /// The governance change's delay has not passed.
    GovernanceNotDue,
    /// An amount or a block number would overflow.
    Overflow,
    /// Kreivo's assets API refused.
    Kreivo(KreivoApiError),
    /// The contract's own balance did not rise by the amount pulled from the redeemer
    /// (`REQ-HN-8`).
    DepositMismatch,
    /// Kreivo minted another amount than asked.
    MintMismatch,
    /// Kreivo burnt less than the amount redeemed.
    BurnShort,
}
