// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Skipping stretches of history proven unchanged (`notes/KREIVO-WALK-SKIP.md`; `REQ-OB-3` as
//! amendment 0027 drafts it, unruled: off unless `[walk_skip]` is configured).
//!
//! The walk derives one thing from old blocks: the configured contract's events (and the times of
//! the blocks that hold them). Every event the contract emits adds one to its `event_seq`, a field
//! of its storage, so between two blocks whose state holds the same contract storage root there
//! is no event of the contract, provided its code did not change and back in between. What is
//! compared at each end, read with proofs against that end's verified header, keyed and decoded
//! with that end's own runtime ([`EndState`]):
//!
//! - **F1, the runtime**: `System::LastRuntimeUpgrade` and the hash of `:code`, equal, and a
//!   runtime whose paths to the contract's code were read ([`AUDITED_RUNTIMES`]);
//! - **F2, the contract**: `Contracts::ContractInfoOf(contract)`, with the pinned code hash, and
//!   its child trie's root;
//! - **F3, the contracts store**: `ContractsStore::ContractAccount((app, licence))` is the
//!   contract and `ContractsStore::Apps(app)` (whose `version` rises with every publish) holds the
//!   pinned code hash, so no swap of the instance's code went through the store;
//! - **F4, not load-bearing**: the delivery account's nonce and each bridged asset's supply.
//!
//! Anything that cannot be told means the stretch is read block by block.

use std::sync::{Mutex, PoisonError};

use crate::chain::BlockRef;
use crate::kreivo::AccountId32;

/// The runtimes whose paths to a contract's code were read (`spec_name`, `spec_version`): Kreivo
/// 0.17.1 (spec 134, `87f8728`) and 0.18.1 (spec 136, whose diff from 0.17.1 does not touch
/// `config/contracts` or `pallets/contracts-store`). Through both, only Root and the contracts
/// store's `upgrade` change an instance's code. A stretch under any other runtime is read block by
/// block.
pub const AUDITED_RUNTIMES: &[(&str, u32)] =
    &[("kreivo-parachain", 134), ("kreivo-parachain", 136)];

/// The most blocks whose ends differ that are split again; a stretch this short is read block by
/// block.
pub const SKIP_LEAF: u64 = 16;

/// A follower's skip: what it is about, how short a stretch is read block by block, and the
/// fingerprint of the last rung's top, which is the next rung's lower end.
#[derive(Debug)]
pub struct Skip {
    /// What it is about.
    pub subject: Subject,
    /// The most blocks whose ends differ that are read block by block ([`SKIP_LEAF`]).
    pub leaf: u64,
    last: Mutex<Option<(BlockRef, Fingerprint)>>,
}

impl Skip {
    /// A skip about `subject`, with leaves of [`SKIP_LEAF`] blocks.
    pub fn new(subject: Subject) -> Skip {
        Skip {
            subject,
            leaf: SKIP_LEAF,
            last: Mutex::new(None),
        }
    }

    /// The last fingerprint remembered, and its block.
    pub fn last(&self) -> Option<(BlockRef, Fingerprint)> {
        self.last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Remembers `fp` as `at`'s.
    pub fn remember(&self, at: BlockRef, fp: Fingerprint) {
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = Some((at, fp));
    }
}

/// What a skip is about: the walk's contract and what pins it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subject {
    /// The contract.
    pub contract: AccountId32,
    /// Its pinned code hash (`contract_code_hash`).
    pub code_hash: [u8; 32],
    /// Its app and licence in the contracts store.
    pub app: (u32, u64),
    /// The delivery account (F4).
    pub lane: AccountId32,
    /// The bridged assets' local indices, `Here(index)` (F4).
    pub assets: Vec<u32>,
}

/// What one end's state holds of the fingerprint, each value proven under its block's verified
/// header and decoded with the metadata of the runtime that wrote that state. `None` is absent
/// (or, for a decoded field, not there).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EndState {
    /// `System::LastRuntimeUpgrade`, raw.
    pub runtime: Option<Vec<u8>>,
    /// Its `spec_name` and `spec_version`.
    pub spec: Option<(String, u32)>,
    /// The hash of `:code`.
    pub code: [u8; 32],
    /// `Contracts::ContractInfoOf(contract)`, raw.
    pub contract_info: Option<Vec<u8>>,
    /// Its `code_hash`.
    pub contract_code_hash: Option<[u8; 32]>,
    /// The contract's child-trie root (`:child_storage:default:` ‖ its `trie_id`).
    pub storage_root: Option<Vec<u8>>,
    /// `ContractsStore::ContractAccount((app, licence))`.
    pub app_account: Option<AccountId32>,
    /// `ContractsStore::Apps(app)`, raw.
    pub app: Option<Vec<u8>>,
    /// Its `code_hash`.
    pub app_code_hash: Option<[u8; 32]>,
    /// The delivery account's nonce (0 for no account).
    pub nonce: Option<u64>,
    /// Each asset's supply, in [`Subject::assets`]' order.
    pub supplies: Vec<Option<u128>>,
}

/// What is compared between two ends. Equal at both ends of a stretch: the stretch holds no
/// event of the contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    runtime: Vec<u8>,
    code: [u8; 32],
    contract_info: Vec<u8>,
    storage_root: Vec<u8>,
    app: Vec<u8>,
    nonce: u64,
    supplies: Vec<Option<u128>>,
}

/// `end`'s fingerprint, or why it cannot be one: an unaudited runtime, the contract not there with
/// the pinned code, the store's app not this contract's or holding another code. Every one of
/// those is "cannot tell", never "equal".
pub fn fingerprint(end: &EndState, subject: &Subject) -> Result<Fingerprint, String> {
    let (name, version) = end.spec.as_ref().ok_or("no System::LastRuntimeUpgrade")?;
    if !AUDITED_RUNTIMES
        .iter()
        .any(|(n, v)| n == name && v == version)
    {
        return Err(format!(
            "runtime {name} {version} is not one whose paths to a contract's code were read"
        ));
    }
    let runtime = end.runtime.clone().ok_or("no System::LastRuntimeUpgrade")?;
    let contract_info = end.contract_info.clone().ok_or("no contract")?;
    if end.contract_code_hash != Some(subject.code_hash) {
        return Err("the contract's code hash is not the pinned one".into());
    }
    let storage_root = end
        .storage_root
        .clone()
        .ok_or("no child-trie root for the contract")?;
    if end.app_account != Some(subject.contract) {
        return Err("the contracts store's app instance is not the contract".into());
    }
    let app = end.app.clone().ok_or("no app in the contracts store")?;
    if end.app_code_hash != Some(subject.code_hash) {
        return Err("the contracts store's app holds another code than the pinned one".into());
    }
    let nonce = end.nonce.ok_or("no nonce for the delivery account")?;
    if end.supplies.len() != subject.assets.len() {
        return Err("a supply was not read".into());
    }
    Ok(Fingerprint {
        runtime,
        code: end.code,
        contract_info,
        storage_root,
        app,
        nonce,
        supplies: end.supplies.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subject() -> Subject {
        Subject {
            contract: [0xc0; 32],
            code_hash: [0xcc; 32],
            app: (5, 0),
            lane: [0x1a; 32],
            assets: vec![101, 102],
        }
    }

    fn end() -> EndState {
        EndState {
            runtime: Some(vec![0x19, 0x02]),
            spec: Some(("kreivo-parachain".into(), 134)),
            code: [0x0c; 32],
            contract_info: Some(vec![1, 2, 3]),
            contract_code_hash: Some([0xcc; 32]),
            storage_root: Some(vec![0x55; 32]),
            app_account: Some([0xc0; 32]),
            app: Some(vec![9, 9]),
            app_code_hash: Some([0xcc; 32]),
            nonce: Some(7),
            supplies: vec![Some(10), None],
        }
    }

    #[test]
    fn skip_every_value_counts_and_equal_ends_are_equal() {
        let s = subject();
        let a = fingerprint(&end(), &s).unwrap();
        assert_eq!(a, fingerprint(&end(), &s).unwrap());
        let changes: [fn(&mut EndState); 7] = [
            |e| e.runtime = Some(vec![0x21, 0x02]),
            |e| e.code = [0x0d; 32],
            |e| e.contract_info = Some(vec![1, 2, 4]),
            |e| e.storage_root = Some(vec![0x56; 32]),
            |e| e.app = Some(vec![9, 10]),
            |e| e.nonce = Some(8),
            |e| e.supplies[0] = Some(11),
        ];
        for (i, change) in changes.iter().enumerate() {
            let mut e = end();
            change(&mut e);
            assert_ne!(fingerprint(&e, &s).unwrap(), a, "change {i}");
        }
    }

    #[test]
    fn skip_what_cannot_be_told_is_never_a_fingerprint() {
        let s = subject();
        let cases: [fn(&mut EndState); 9] = [
            |e| e.spec = Some(("kreivo-parachain".into(), 135)),
            |e| e.spec = None,
            |e| e.contract_info = None,
            |e| e.contract_code_hash = Some([0xcd; 32]),
            |e| e.storage_root = None,
            |e| e.app_account = Some([0xc1; 32]),
            |e| e.app_code_hash = Some([0xcd; 32]),
            |e| e.nonce = None,
            |e| e.supplies.pop().map(drop).unwrap_or(()),
        ];
        for (i, case) in cases.iter().enumerate() {
            let mut e = end();
            case(&mut e);
            assert!(fingerprint(&e, &s).is_err(), "case {i}");
        }
    }
}
