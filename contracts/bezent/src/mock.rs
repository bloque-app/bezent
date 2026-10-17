// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! A model of the parts of Kreivo the contract uses, for the off-chain unit tests.
//!
//! ink!'s off-chain environment knows nothing of Kreivo's chain extension. This module stands in
//! for it, registered with ink!'s off-chain engine so the contract's real extension calls run:
//!
//! - **`pallet-assets`**, as far as the assets API touches it: balances, total issuance, a minimum
//!   balance per asset, and the Issuer and Admin roles of the contract's community.
//! - **`call_runtime` with `Assets::transfer_approved`** ([`call_runtime`]), decoded as Kreivo's
//!   runtime would (pallet 13, call 25, Kreivo's asset id and `MultiAddress`), moving out of an
//!   approval ([`approve`]) with `keep_alive: false`: the owner may be emptied, and a rest below the
//!   minimum balance goes with the amount (`burn_dust: false`), as pallet-assets does.
//! - **The assets API of `KreivoChainExtensions`** at kreivo#512 (`apis/src/runtime/api_impls/
//!   assets.rs`). Each call's input is decoded as the runtime does, with Kreivo's own asset type
//!   (`virto_common::FungibleAssetLocation`), and refusals return Kreivo's own status codes, built
//!   with `kreivo-apis`' `KreivoApisErrorCode`:
//!   - `balance` reads a balance.
//!   - `deposit` moves from the contract's caller to the contract, keeping the caller at or above
//!     the minimum balance (`Preserve`).
//!   - `mint` needs the community to be the Issuer, and a new account to reach the
//!     minimum balance.
//!   - `burn` needs the community to be the Admin and an account to burn from; it burns from **any** account,
//!     best effort and expendable (a rest below the minimum is burnt too), as Kreivo does. Every burn
//!     is recorded with the account it burnt from, so tests can show the contract only ever names
//!     its own.
//!
//! The extension cannot read ink!'s environment while the engine is calling it, so tests tell the
//! model who the caller and the contract are ([`enter_frame`]).

use crate::kreivo::{
    ASSETS_PALLET, EXTENSION_ID, FN_BALANCE, FN_BURN, FN_DEPOSIT, FN_MINT, TRANSFER_APPROVED,
};
use crate::types::{AccountId, AssetId, Balance};
use kreivo_apis::apis::{AssetsApiError, KreivoApisError, KreivoApisErrorCode};
use scale::{Decode, Encode};
use std::cell::RefCell;
use std::collections::BTreeMap;

/// An asset's parameters in the model.
#[derive(Clone, Copy, Debug)]
struct AssetInfo {
    min_balance: Balance,
    /// Whether the contract's community is the asset's Issuer.
    community_issues: bool,
    /// Whether the contract's community is the asset's Admin.
    community_administers: bool,
}

/// One burn, as Kreivo executed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Burn {
    /// The asset.
    pub asset: AssetId,
    /// The amount asked.
    pub amount: Balance,
    /// The account it burnt from.
    pub who: AccountId,
    /// The amount burnt.
    pub burnt: Balance,
}

#[derive(Default)]
struct Chain {
    assets: BTreeMap<AssetId, AssetInfo>,
    balances: BTreeMap<(AssetId, AccountId), Balance>,
    issuance: BTreeMap<AssetId, Balance>,
    /// The caller of the contract and the contract, for the call running now.
    frame: Option<(AccountId, AccountId)>,
    /// Every burn executed.
    burns: Vec<Burn>,
    /// Every function id the contract called, in order.
    calls: Vec<u16>,
    /// If set, `deposit` moves this much less than asked but reports the full amount.
    short_deposit: Balance,
    /// Approvals: (asset, owner, delegate) to the amount approved.
    approvals: BTreeMap<(AssetId, AccountId, AccountId), Balance>,
}

/// Recorded in [`calls`] for each `call_runtime`, which is no chain-extension function.
pub const CALL_RUNTIME: u16 = 0xFFFF;

thread_local! {
    static CHAIN: RefCell<Chain> = RefCell::new(Chain::default());
}

/// Start from an empty chain and register the chain extension.
pub fn reset() {
    CHAIN.with(|chain| *chain.borrow_mut() = Chain::default());
    ink::env::test::register_chain_extension(AssetsExtension);
}

/// Create a local asset whose Issuer and Admin is the contract's community (`DEC-7`).
pub fn create_asset(asset: AssetId, min_balance: Balance) {
    CHAIN.with(|chain| {
        chain.borrow_mut().assets.insert(
            asset,
            AssetInfo {
                min_balance,
                community_issues: true,
                community_administers: true,
            },
        )
    });
}

/// Create a local asset the contract's community holds no role on.
pub fn create_foreign_asset(asset: AssetId, min_balance: Balance) {
    create_asset(asset, min_balance);
    CHAIN.with(|chain| {
        let mut chain = chain.borrow_mut();
        let info = chain.assets.get_mut(&asset).expect("asset exists");
        info.community_issues = false;
        info.community_administers = false;
    });
}

/// Give `who` value outside the contract (for example, held from before).
pub fn fund(asset: AssetId, who: AccountId, amount: Balance) {
    CHAIN.with(|chain| {
        let mut chain = chain.borrow_mut();
        *chain.balances.entry((asset, who)).or_default() += amount;
        *chain.issuance.entry(asset).or_default() += amount;
    });
}

/// An account's balance of an asset.
pub fn balance(asset: AssetId, who: AccountId) -> Balance {
    CHAIN.with(|chain| chain.borrow().balance(asset, who))
}

/// An asset's total issuance on the chain.
pub fn issuance(asset: AssetId) -> Balance {
    CHAIN.with(|chain| chain.borrow().issuance.get(&asset).copied().unwrap_or(0))
}

/// Every burn executed.
pub fn burns() -> Vec<Burn> {
    CHAIN.with(|chain| chain.borrow().burns.clone())
}

/// Every extension function the contract called, in order.
pub fn calls() -> Vec<u16> {
    CHAIN.with(|chain| chain.borrow().calls.clone())
}

/// Make `deposit` move `short` less than asked while reporting the full amount.
pub fn short_deposit(short: Balance) {
    CHAIN.with(|chain| chain.borrow_mut().short_deposit = short);
}

/// `Assets::approve_transfer`, signed by `owner`: `delegate` may move `amount` of `owner`'s `asset`.
pub fn approve(asset: AssetId, owner: AccountId, delegate: AccountId, amount: Balance) {
    CHAIN.with(|chain| {
        chain
            .borrow_mut()
            .approvals
            .insert((asset, owner, delegate), amount)
    });
}

/// What `delegate` may still move of `owner`'s `asset`.
pub fn approved(asset: AssetId, owner: AccountId, delegate: AccountId) -> Balance {
    CHAIN.with(|chain| {
        chain
            .borrow()
            .approvals
            .get(&(asset, owner, delegate))
            .copied()
            .unwrap_or(0)
    })
}

/// `pallet_contracts`' `call_runtime` from the contract: decode the call as Kreivo's runtime does
/// and dispatch it with the contract as the signed origin. Only `Assets::transfer_approved` is
/// modelled. Returns whether it was dispatched; a refused dispatch changes nothing.
pub fn call_runtime(call: &[u8]) -> bool {
    use scale::Compact;
    type Address = sp_runtime::MultiAddress<sp_runtime::AccountId32, u16>;
    let input = &mut &call[..];
    assert_eq!(
        u8::decode(input).unwrap(),
        ASSETS_PALLET,
        "a call to Assets"
    );
    assert_eq!(
        u8::decode(input).unwrap(),
        TRANSFER_APPROVED,
        "transfer_approved"
    );
    let asset = kreivo_asset(input);
    let id = |address: Address| match address {
        sp_runtime::MultiAddress::Id(account) => AccountId::from(<[u8; 32]>::from(account)),
        other => panic!("the contract names accounts by id, not {other:?}"),
    };
    let owner = id(Address::decode(input).expect("owner"));
    let destination = id(Address::decode(input).expect("destination"));
    let amount = Compact::<u128>::decode(input).expect("amount").0;
    assert!(input.is_empty(), "trailing bytes in the call");
    CHAIN.with(|chain| {
        let mut chain = chain.borrow_mut();
        chain.calls.push(CALL_RUNTIME);
        let (_, contract) = chain.frame.expect("a contract frame");
        chain.transfer_approved(asset, owner, contract, destination, amount)
    })
}

/// The caller of the contract and the contract, for the calls that follow.
pub fn enter_frame(caller: AccountId, contract: AccountId) {
    CHAIN.with(|chain| chain.borrow_mut().frame = Some((caller, contract)));
}

impl Chain {
    fn balance(&self, asset: AssetId, who: AccountId) -> Balance {
        self.balances.get(&(asset, who)).copied().unwrap_or(0)
    }

    fn set_balance(&mut self, asset: AssetId, who: AccountId, amount: Balance) {
        if amount == 0 {
            self.balances.remove(&(asset, who));
        } else {
            self.balances.insert((asset, who), amount);
        }
    }

    /// `fungibles::Mutate::transfer(.., Preserve)`: the source keeps at least the minimum, a new
    /// destination must reach it. Returns whether it moved.
    fn transfer_preserve(
        &mut self,
        asset: AssetId,
        from: AccountId,
        to: AccountId,
        amount: Balance,
    ) -> bool {
        let Some(info) = self.assets.get(&asset).copied() else {
            return false;
        };
        let source = self.balance(asset, from);
        let dest = self.balance(asset, to);
        let ok = source
            .checked_sub(amount)
            .is_some_and(|rest| rest >= info.min_balance)
            && (dest > 0 || amount >= info.min_balance);
        if ok {
            self.set_balance(asset, from, source - amount);
            self.set_balance(asset, to, dest + amount);
        }
        ok
    }
}

impl Chain {
    /// `do_transfer_approved` with `keep_alive: false, burn_dust: false`: the owner may go to
    /// zero, and a rest below the minimum balance is moved with the amount.
    fn transfer_approved(
        &mut self,
        asset: AssetId,
        owner: AccountId,
        delegate: AccountId,
        destination: AccountId,
        amount: Balance,
    ) -> bool {
        let Some(info) = self.assets.get(&asset).copied() else {
            return false;
        };
        let key = (asset, owner, delegate);
        let Some(remaining) = self
            .approvals
            .get(&key)
            .and_then(|approved| approved.checked_sub(amount))
        else {
            return false;
        };
        let source = self.balance(asset, owner);
        let Some(mut rest) = source.checked_sub(amount) else {
            return false;
        };
        let mut credit = amount;
        if rest < info.min_balance {
            credit += rest;
            rest = 0;
        }
        let dest = self.balance(asset, destination);
        if dest == 0 && credit < info.min_balance {
            return false;
        }
        self.set_balance(asset, owner, rest);
        self.set_balance(asset, destination, dest + credit);
        if remaining == 0 {
            self.approvals.remove(&key);
        } else {
            self.approvals.insert(key, remaining);
        }
        true
    }
}

fn status(error: AssetsApiError) -> u32 {
    KreivoApisErrorCode::from(KreivoApisError::Assets(error)).into()
}

/// Kreivo's asset id, decoded from the extension's input with Kreivo's own type, and turned into
/// the contract's (`asset_id_encodes_as_kreivos_fungible_asset_location`).
fn kreivo_asset(input: &mut &[u8]) -> AssetId {
    let asset = virto_common::FungibleAssetLocation::decode(input).expect("Kreivo's asset id");
    AssetId::decode(&mut &asset.encode()[..]).expect("a local asset")
}

fn account(input: &mut &[u8]) -> AccountId {
    AccountId::from(<[u8; 32]>::decode(input).expect("an account"))
}

/// The assets API of Kreivo's chain extension.
struct AssetsExtension;

impl ink::env::test::ChainExtension for AssetsExtension {
    fn ext_id(&self) -> u16 {
        EXTENSION_ID
    }

    fn call(&mut self, func_id: u16, input: &[u8], output: &mut Vec<u8>) -> u32 {
        // The off-chain engine hands the extension the encoded input as a byte vector.
        let bytes = Vec::<u8>::decode(&mut &input[..]).expect("input bytes");
        let input = &mut &bytes[..];
        CHAIN.with(|chain| {
            let mut chain = chain.borrow_mut();
            chain.calls.push(func_id);
            let (caller, contract) = chain
                .frame
                .expect("the test entered the contract's frame before calling it");
            match func_id {
                FN_BALANCE => {
                    let asset = kreivo_asset(input);
                    let who = account(input);
                    assert!(input.is_empty(), "trailing bytes in the input");
                    chain.balance(asset, who).encode_to(output);
                    0
                }
                FN_DEPOSIT => {
                    let asset = kreivo_asset(input);
                    let amount = u128::decode(input).expect("amount");
                    assert!(input.is_empty(), "trailing bytes in the input");
                    let short = chain.short_deposit;
                    let moving = amount.saturating_sub(short);
                    if chain.transfer_preserve(asset, caller, contract, moving) {
                        amount.encode_to(output);
                        0
                    } else {
                        status(AssetsApiError::CannotDeposit)
                    }
                }
                FN_MINT => {
                    let asset = kreivo_asset(input);
                    let amount = u128::decode(input).expect("amount");
                    let beneficiary = account(input);
                    assert!(input.is_empty(), "trailing bytes in the input");
                    let Some(info) = chain.assets.get(&asset).copied() else {
                        return status(AssetsApiError::CannotMint);
                    };
                    if !info.community_issues {
                        return status(AssetsApiError::NotAuthorised);
                    }
                    let dest = chain.balance(asset, beneficiary);
                    if dest == 0 && amount < info.min_balance {
                        return status(AssetsApiError::CannotMint);
                    }
                    chain.set_balance(asset, beneficiary, dest + amount);
                    *chain.issuance.entry(asset).or_default() += amount;
                    amount.encode_to(output);
                    0
                }
                FN_BURN => {
                    let asset = kreivo_asset(input);
                    let amount = u128::decode(input).expect("amount");
                    let who = account(input);
                    assert!(input.is_empty(), "trailing bytes in the input");
                    let Some(info) = chain.assets.get(&asset).copied() else {
                        return status(AssetsApiError::CannotBurn);
                    };
                    if !info.community_administers {
                        return status(AssetsApiError::NotAuthorised);
                    }
                    let held = chain.balance(asset, who);
                    if held == 0 {
                        return status(AssetsApiError::CannotBurn);
                    }
                    // Best effort and expendable: a rest below the minimum goes too.
                    let mut burnt = amount.min(held);
                    if held - burnt < info.min_balance {
                        burnt = held;
                    }
                    chain.set_balance(asset, who, held - burnt);
                    *chain.issuance.entry(asset).or_default() -= burnt;
                    chain.burns.push(Burn {
                        asset,
                        amount,
                        who,
                        burnt,
                    });
                    burnt.encode_to(output);
                    0
                }
                other => panic!("the contract called extension function {other:#06x}"),
            }
        })
    }
}
