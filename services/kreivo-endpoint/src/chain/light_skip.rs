// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! What a skip reads of an old block's state (`notes/KREIVO-WALK-SKIP.md`), for
//! [`super::LightChain`]: `System::BlockHash`, and each end's [`EndState`].
//!
//! Every value comes from one proof against the block's verified header ([`History`]). An end's
//! keys are computed, and its values decoded, with the metadata of the runtime that wrote that
//! state: `System::LastRuntimeUpgrade` at the block names it, and [`Runtimes`] runs it from the
//! code its parent's state proves. The head's metadata is never used for an old end. A read that
//! carries no proven `:code` hash (the light client's side) cannot tell: `None`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};

use scale_value::Value;
use subxt::Metadata;

use super::{LightChain, bytes32, field, invalid, number};
use crate::chain::runtime::{self, History};
use crate::chain::{BlockRef, ChainResult};
use crate::kreivo::AccountId32;
use crate::skip::{EndState, Subject};

/// The prefix of a default child trie's root in the main trie.
const CHILD_STORAGE: &[u8] = b":child_storage:default:";

/// Each contract's `trie_id`, as read last: fixed at instantiation, so a read of a later end can
/// ask for the child root in the same proof. A `trie_id` that differs is read again.
fn trie_ids() -> &'static Mutex<HashMap<AccountId32, Vec<u8>>> {
    static IDS: OnceLock<Mutex<HashMap<AccountId32, Vec<u8>>>> = OnceLock::new();
    IDS.get_or_init(Mutex::default)
}

fn key_with(
    metadata: &Metadata,
    pallet: &str,
    entry: &str,
    keys: Vec<Value<()>>,
) -> ChainResult<Vec<u8>> {
    let address = subxt::dynamic::storage(pallet, entry, keys);
    subxt::ext::subxt_core::storage::get_address_bytes(&address, metadata).map_err(invalid)
}

fn decode_with(
    metadata: &Metadata,
    pallet: &str,
    entry: &str,
    bytes: &[u8],
) -> ChainResult<Value<u32>> {
    let address = subxt::dynamic::storage(pallet, entry, Vec::<Value<()>>::new());
    subxt::ext::subxt_core::storage::decode_value(&mut &bytes[..], &address, metadata)
        .map_err(invalid)?
        .to_value()
        .map_err(invalid)
}

/// The bytes a value holds, each leaf a byte, of any length.
fn byte_leaves(v: &Value<u32>) -> Option<Vec<u8>> {
    fn leaves(v: &Value<u32>, out: &mut Vec<u8>) -> Option<()> {
        match &v.value {
            scale_value::ValueDef::Primitive(scale_value::Primitive::U128(n)) => {
                out.push(u8::try_from(*n).ok()?);
            }
            scale_value::ValueDef::Composite(c) => {
                for x in c.values() {
                    leaves(x, out)?;
                }
            }
            _ => return None,
        }
        Some(())
    }
    let mut out = Vec::new();
    leaves(v, &mut out)?;
    Some(out)
}

impl LightChain {
    /// `System::BlockHash(number)` in the state of `at`, proven. Its key is the head runtime's;
    /// the caller checks that the header of the hash is block `number`, so a key that names
    /// another entry cannot pass.
    pub(super) async fn block_hash_in(
        &self,
        at: &BlockRef,
        number: u64,
    ) -> ChainResult<Option<[u8; 32]>> {
        let n = u32::try_from(number).map_err(invalid)?;
        let key = self.key("System", "BlockHash", vec![Value::u128(u128::from(n))])?;
        let value = self.raw(at, &[key]).await?.pop().flatten();
        match value {
            None => Ok(None),
            Some(b) if b.iter().all(|x| *x == 0) => Ok(None),
            Some(b) => {
                let hash = <[u8; 32]>::try_from(b.as_slice())
                    .map_err(|_| invalid("a System::BlockHash that is not 32 bytes"))?;
                // Its number is known, so its header is asked of the side that holds it (an old
                // block's of a history source, not of the light client's peers). The header is
                // still checked to be that block's.
                let mut numbers = super::lock(&self.numbers);
                if numbers.len() >= super::NUMBERS {
                    numbers.clear();
                }
                numbers.insert(hash, number);
                Ok(Some(hash))
            }
        }
    }

    /// What the state of `at` holds of `subject`'s fingerprint. See the module documentation.
    pub(super) async fn end_state_at(
        &self,
        at: &BlockRef,
        subject: &Subject,
    ) -> ChainResult<Option<EndState>> {
        let upgrade = runtime::LAST_RUNTIME_UPGRADE.to_vec();
        let (values, code) = self
            .storage_and_code_hash(at, std::slice::from_ref(&upgrade))
            .await?;
        let Some(code) = code else {
            return Ok(None);
        };
        let raw_runtime = values.into_iter().next().flatten();
        let id = runtime::runtime_id(raw_runtime.as_deref())?;
        let m = self.runtimes.metadata(self, at, &id).await?;

        let (app, licence) = subject.app;
        let mut keys = vec![
            upgrade,
            key_with(
                &m,
                "Contracts",
                "ContractInfoOf",
                vec![Value::from_bytes(subject.contract)],
            )?,
            key_with(
                &m,
                "ContractsStore",
                "ContractAccount",
                vec![Value::unnamed_composite([
                    Value::u128(u128::from(app)),
                    Value::u128(u128::from(licence)),
                ])],
            )?,
            key_with(
                &m,
                "ContractsStore",
                "Apps",
                vec![Value::u128(u128::from(app))],
            )?,
            key_with(
                &m,
                "System",
                "Account",
                vec![Value::from_bytes(subject.lane)],
            )?,
        ];
        for asset in &subject.assets {
            keys.push(key_with(
                &m,
                "Assets",
                "Asset",
                vec![Value::unnamed_variant(
                    "Here",
                    [Value::u128(u128::from(*asset))],
                )],
            )?);
        }
        let child_key = |trie_id: &[u8]| {
            let mut k = CHILD_STORAGE.to_vec();
            k.extend_from_slice(trie_id);
            k
        };
        let known_trie = trie_ids()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&subject.contract)
            .cloned();
        if let Some(t) = &known_trie {
            keys.push(child_key(t));
        }
        let (mut values, again) = self.storage_and_code_hash(at, &keys).await?;
        match again {
            None => return Ok(None),
            Some(h) if h != code => {
                return Err(invalid(format!(
                    "two proofs of block {} prove two hashes of :code",
                    at.number
                )));
            }
            Some(_) => {}
        }
        if values.len() != keys.len() || values[0] != raw_runtime {
            return Err(invalid(format!(
                "two proofs of block {} do not agree",
                at.number
            )));
        }

        let contract_info = values[1].clone();
        let info = contract_info
            .as_deref()
            .map(|b| decode_with(&m, "Contracts", "ContractInfoOf", b))
            .transpose()?;
        let contract_code_hash = info
            .as_ref()
            .and_then(|v| field(v, "code_hash"))
            .and_then(bytes32);
        let trie_id = info
            .as_ref()
            .and_then(|v| field(v, "trie_id"))
            .and_then(byte_leaves);
        let storage_root = match (&trie_id, &known_trie) {
            (None, _) => None,
            (Some(t), Some(k)) if t == k => values.pop().flatten(),
            (Some(t), _) => {
                trie_ids()
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(subject.contract, t.clone());
                self.storage(at, &[child_key(t)]).await?.pop().flatten()
            }
        };
        let app_account = values[2]
            .as_deref()
            .map(|b| decode_with(&m, "ContractsStore", "ContractAccount", b))
            .transpose()?
            .as_ref()
            .and_then(bytes32);
        let app_info = values[3].clone();
        let app_code_hash = app_info
            .as_deref()
            .map(|b| decode_with(&m, "ContractsStore", "Apps", b))
            .transpose()?
            .as_ref()
            .and_then(|v| field(v, "code_hash"))
            .and_then(bytes32);
        let nonce = match values[4].as_deref() {
            None => Some(0),
            Some(b) => field(&decode_with(&m, "System", "Account", b)?, "nonce")
                .and_then(number)
                .and_then(|n| u64::try_from(n).ok()),
        };
        let mut supplies = Vec::with_capacity(subject.assets.len());
        for v in &values[5..5 + subject.assets.len()] {
            supplies.push(match v.as_deref() {
                None => None,
                Some(b) => Some(
                    field(&decode_with(&m, "Assets", "Asset", b)?, "supply")
                        .and_then(number)
                        .ok_or_else(|| invalid("an asset without a supply"))?,
                ),
            });
        }
        Ok(Some(EndState {
            runtime: raw_runtime,
            spec: Some((id.spec_name, id.spec_version)),
            code,
            contract_info,
            contract_code_hash,
            storage_root,
            app_account,
            app: app_info,
            app_code_hash,
            nonce,
            supplies,
        }))
    }
}
