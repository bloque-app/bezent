// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The metadata of the runtime that produced a block (feature `light-client`).
//!
//! A block's `System::Events` are SCALE bytes whose meaning is the metadata of the runtime that
//! wrote them. Decoded with any other runtime's metadata they misalign or fail: Kreivo's spec 136
//! widened `CommunityTracks`' track id from `u16` to `u32`, so a `CommunityReferenda::Submitted` of
//! spec 134 read with 136's metadata swallows two bytes of its proposal and finds no variant 227
//! (block 40,004,557). So events are decoded with the metadata of the block's own runtime, and that
//! metadata is never taken from a source's word:
//!
//! - **which runtime**: `System::LastRuntimeUpgrade` in the block's own state, read with the events
//!   (and, from a history source, proven with them). The runtime executing a block writes it in that
//!   block when it is the first block it executes, so it names the runtime that wrote the events.
//! - **its code**: `:code` (and `:heappages`) in the state of the block's **parent**. The block's own
//!   state is not the one: in the block that applies a runtime upgrade, `:code` is already the new
//!   runtime while its events were written by the old one. Read like any other value, so a history
//!   source's answer is proven against the verified header ([`super::archive`]).
//! - **its metadata**: `Metadata_metadata_at_version(15)` executed here, on that code over an empty
//!   state, in smoldot's executor, never asked of a node (a call's result carries no proof). Its
//!   types are the code's; its constants are not the chain's and are never read
//!   ([`metadata_from_code`]). The code's own runtime version must be the one
//!   `System::LastRuntimeUpgrade` names, or nothing is decoded.
//!
//! Each runtime's metadata is kept by the hash of its code, never by spec version: a code swap
//! that keeps the version would otherwise be served the old metadata. That hash is proven at every
//! block, in the parent's state, without reading the code (about 2 MB): the code is read only for a
//! hash not met ([`Runtimes`]). A block whose runtime cannot be told,
//! fetched or run is `Invalid` or `Unavailable`: a visible gap, never a guess (`REQ-OB-3`).

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Mutex, MutexGuard, PoisonError};

use parity_scale_codec::{Compact, Decode, DecodeAll};
use smoldot::executor::{
    self,
    host::{Config, HostVmPrototype, StorageProofSizeBehavior},
    runtime_call::{self, Nibble, RuntimeCall},
    vm::ExecHint,
};
use smoldot::trie::TrieEntryVersion;
use subxt::Metadata;

use super::{BlockRef, ChainError, ChainResult, Header, Values};
use crate::kreivo::blake2_256;

/// `Timestamp::Now`: `twox128("Timestamp") ‖ twox128("Now")`.
pub const TIMESTAMP_NOW: [u8; 32] =
    hex32("f0c365c3cf59d671eb72da0e7a4113c49f1f0515f462cdcf84e0f1d6045dfcbb");
/// `System::Events`: `twox128("System") ‖ twox128("Events")`.
pub const SYSTEM_EVENTS: [u8; 32] =
    hex32("26aa394eea5630e07c48ae0c9558cef780d41e5e16056765bc8461851072c9d7");
/// `System::LastRuntimeUpgrade`: `twox128("System") ‖ twox128("LastRuntimeUpgrade")`.
pub const LAST_RUNTIME_UPGRADE: [u8; 32] =
    hex32("26aa394eea5630e07c48ae0c9558cef7f9cce9c888469bb1a0dceaa129672ef8");
/// The runtime's code.
pub const CODE: &[u8] = b":code";
/// The runtime's heap pages, if set.
pub const HEAP_PAGES: &[u8] = b":heappages";

/// The metadata version asked of a runtime; subxt also reads 14 and 16.
const METADATA_VERSION: u32 = 15;
/// The most bytes of `:code` loaded, as stored (before any decompression).
const MAX_CODE: usize = 50 * 1024 * 1024;

const fn hex32(s: &str) -> [u8; 32] {
    const fn nibble(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => panic!("not lowercase hex"),
        }
    }
    let b = s.as_bytes();
    assert!(b.len() == 64, "not 32 bytes of hex");
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = (nibble(b[2 * i]) << 4) | nibble(b[2 * i + 1]);
        i += 1;
    }
    out
}

fn invalid(e: impl std::fmt::Display) -> ChainError {
    ChainError::Invalid(e.to_string())
}

/// The runtime that executed a block, as its `System::LastRuntimeUpgrade` names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeId {
    /// `spec_version`.
    pub spec_version: u32,
    /// `spec_name`.
    pub spec_name: String,
}

/// `LastRuntimeUpgradeInfo { spec_version: Compact<u32>, spec_name: String }`, decoded by hand:
/// it is what chooses the metadata, so it cannot itself depend on one. None or trailing bytes are
/// refused.
pub fn runtime_id(value: Option<&[u8]>) -> ChainResult<RuntimeId> {
    let bytes = value.ok_or_else(|| invalid("no System::LastRuntimeUpgrade at the block"))?;
    let (Compact(spec_version), spec_name) = <(Compact<u32>, String)>::decode_all(&mut &bytes[..])
        .map_err(|e| invalid(format!("System::LastRuntimeUpgrade does not decode: {e}")))?;
    Ok(RuntimeId {
        spec_version,
        spec_name,
    })
}

/// The metadata of the runtime `code`, run here: `Metadata_metadata_at_version(15)` in smoldot's
/// executor, over an **empty state**. The code's own runtime version must be `expected`.
///
/// Why empty: what the endpoint takes from it is types (the events, `Timestamp::Now`), and a
/// runtime's types are its code's, whatever the state; state only feeds the values of constants,
/// and Kreivo's call does read state for them (`System::Digest`, and it iterates
/// `CommunityTracks::Tracks` for a referenda constant). Read from a history source that state
/// would need proofs of next keys, which nothing here verifies. So every read is answered "absent"
/// and the result is a pure function of verified code: **its constants are not the chain's, and
/// the endpoint never reads a constant from it**. A call that fails on that state, or asks the
/// host for anything else (a child trie, a signature check, a trie root, an offchain service), is
/// refused: the block is a gap.
pub fn metadata_from_code(
    code: &[u8],
    heap_pages: Option<&[u8]>,
    expected: &RuntimeId,
) -> ChainResult<Metadata> {
    let mut call = start(code, heap_pages, expected)?;
    let output = loop {
        call = match call {
            RuntimeCall::Finished(Ok(done)) => {
                break done.virtual_machine.value().as_ref().to_vec();
            }
            RuntimeCall::Finished(Err(e)) => {
                return Err(invalid(format!("the metadata call failed: {}", e.detail)));
            }
            RuntimeCall::LogEmit(log) => log.resume(),
            RuntimeCall::StorageGet(get) if get.child_trie().is_none() => {
                get.inject_value(None::<(std::iter::Empty<&[u8]>, TrieEntryVersion)>)
            }
            RuntimeCall::NextKey(next) if next.child_trie().is_none() && !next.branch_nodes() => {
                next.inject_key(None::<std::iter::Empty<Nibble>>)
            }
            RuntimeCall::NextKey(next) if next.child_trie().is_none() => {
                return Err(invalid("the metadata call asked for a trie's branch nodes"));
            }
            RuntimeCall::StorageGet(_) | RuntimeCall::NextKey(_) => {
                return Err(invalid("the metadata call read a child trie"));
            }
            RuntimeCall::ClosestDescendantMerkleValue(_) => {
                return Err(invalid("the metadata call asked for a trie root"));
            }
            RuntimeCall::SignatureVerification(_) => {
                return Err(invalid("the metadata call asked for a signature check"));
            }
            RuntimeCall::OffchainStorageSet(_) | RuntimeCall::Offchain(_) => {
                return Err(invalid("the metadata call asked for an offchain service"));
            }
        };
    };
    // `Option<OpaqueMetadata>`: `None` when the runtime has no metadata of that version.
    let opaque = Option::<Vec<u8>>::decode_all(&mut &output[..])
        .map_err(|e| invalid(format!("the metadata call's answer does not decode: {e}")))?
        .ok_or_else(|| invalid(format!("the runtime has no metadata V{METADATA_VERSION}")))?;
    Metadata::decode(&mut &opaque[..]).map_err(|e| invalid(format!("the metadata: {e}")))
}

/// Loads `code`, checks its runtime version, and starts the metadata call.
fn start(code: &[u8], heap_pages: Option<&[u8]>, expected: &RuntimeId) -> ChainResult<RuntimeCall> {
    let heap_pages = executor::storage_heap_pages_to_value(heap_pages)
        .map_err(|e| invalid(format!(":heappages: {e}")))?;
    if code.len() > MAX_CODE {
        return Err(invalid("the runtime's code is too large"));
    }
    let vm = HostVmPrototype::new(Config {
        module: code,
        heap_pages,
        exec_hint: ExecHint::ValidateAndExecuteOnce,
        allow_unresolved_imports: false,
    })
    .map_err(|e| invalid(format!("the runtime's code does not load: {e}")))?;
    {
        let version = vm.runtime_version().decode();
        if version.spec_version != expected.spec_version || version.spec_name != expected.spec_name
        {
            return Err(invalid(format!(
                "the parent's code is {} {}, but the block's System::LastRuntimeUpgrade names {} {}",
                version.spec_name, version.spec_version, expected.spec_name, expected.spec_version
            )));
        }
    }
    runtime_call::run(runtime_call::Config {
        virtual_machine: vm,
        function_to_call: "Metadata_metadata_at_version",
        parameter: std::iter::once(METADATA_VERSION.to_le_bytes()),
        storage_main_trie_changes: Default::default(),
        storage_proof_size_behavior: StorageProofSizeBehavior::proof_recording_disabled(),
        max_log_level: 0,
        calculate_trie_changes: false,
        calculate_child_tries_roots_on_finish: false,
    })
    .map_err(|(e, _)| invalid(format!("the metadata call does not start: {e}")))
}

/// What [`Runtimes`] reads of the chain, at blocks of the finalized chain.
pub trait History {
    /// `keys` in the state of `at`, each proven; `None` is proven absent.
    fn storage(
        &self,
        at: &BlockRef,
        keys: &[Vec<u8>],
    ) -> impl Future<Output = ChainResult<Vec<Option<Vec<u8>>>>> + Send;

    /// [`History::storage`], and the proven hash of `:code` in the same state when the same read
    /// can carry it (a history source's proof); `None` when it cannot (the light client).
    fn storage_and_code_hash(
        &self,
        at: &BlockRef,
        keys: &[Vec<u8>],
    ) -> impl Future<Output = ChainResult<(Values, Option<CodeHash>)>> + Send;

    /// BLAKE2b-256 of `:code` in the state of `at`, proven, without the code.
    fn code_hash(&self, at: &BlockRef) -> impl Future<Output = ChainResult<CodeHash>> + Send;

    /// The verified header of `at`; it is refused unless it is `at`'s.
    fn block_header(&self, at: &BlockRef) -> impl Future<Output = ChainResult<Header>> + Send;
}

/// A hash of a runtime's code: BLAKE2b-256 of `:code`, as a block's state proves it.
pub type CodeHash = [u8; 32];

/// How many blocks' code hash is remembered (about a day of Kreivo's blocks).
const REMEMBERED_BLOCKS: usize = 16_384;

/// A runtime met: what its code is, as run here.
struct Met {
    /// The code's own runtime version, checked when it was run.
    id: RuntimeId,
    metadata: Metadata,
}

#[derive(Default)]
struct Kept {
    /// Each runtime's metadata, by the hash of its code.
    by_code: HashMap<CodeHash, Met>,
    /// The proven hash of `:code` in the state of each recent block, by block hash: the code that
    /// executes that block's children.
    state_code: HashMap<[u8; 32], CodeHash>,
    order: VecDeque<[u8; 32]>,
}

/// Each runtime's metadata met so far, by the hash of its code.
///
/// The spec version is not the key: a code swap that keeps it (`set_code_without_checks`,
/// `authorize_upgrade` without the version check) does not touch `System::LastRuntimeUpgrade`. Nor
/// is a block's code carried from its parent's on the word of a digest: a raw `set_storage` or
/// `kill_storage` of `:code`, or a migration that writes it, deposits none. The key of every block
/// is the hash of `:code` in its parent's state, **proven** against the parent's verified state
/// root: under state version 1 the trie node of `:code` holds that hash, so the proof is under
/// 1 KB, not the 2 MB of the code ([`History::code_hash`]). The hash proven in a block's own state
/// is the key of its children, so a walk that reads each block's events from a history source
/// proves the next block's key in the same proof ([`History::storage_and_code_hash`]). The code
/// itself is read only for a hash not met, and must hash to it. The spec name and version of the
/// code are checked against the block's `System::LastRuntimeUpgrade` on every use.
#[derive(Default)]
pub struct Runtimes {
    kept: Mutex<Kept>,
}

impl Runtimes {
    /// Nothing known: the first block met reads its code.
    pub fn new() -> Runtimes {
        Runtimes::default()
    }

    fn kept(&self) -> MutexGuard<'_, Kept> {
        self.kept.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Remembers `code`, proven in the state of `at`, as the code that executes `at`'s children.
    pub fn proven(&self, at: &BlockRef, code: CodeHash) {
        let mut kept = self.kept();
        if kept.state_code.insert(at.hash, code).is_none() {
            kept.order.push_back(at.hash);
            if kept.order.len() > REMEMBERED_BLOCKS
                && let Some(old) = kept.order.pop_front()
            {
                kept.state_code.remove(&old);
            }
        }
    }

    /// The metadata of `code` if it was met, checked against `runtime`. The hash is proven, so
    /// reading the code again would give the same code and the same version: a mismatch is the
    /// chain's (its `System::LastRuntimeUpgrade` names another runtime than the code in force),
    /// and nothing is decoded.
    fn met(&self, code: &CodeHash, runtime: &RuntimeId) -> ChainResult<Option<Metadata>> {
        let kept = self.kept();
        let Some(met) = kept.by_code.get(code) else {
            return Ok(None);
        };
        if met.id != *runtime {
            return Err(invalid(format!(
                "the parent's code is {} {}, but the block's System::LastRuntimeUpgrade names {} {}",
                met.id.spec_name, met.id.spec_version, runtime.spec_name, runtime.spec_version
            )));
        }
        Ok(Some(met.metadata.clone()))
    }

    /// The metadata of `runtime`, the runtime that executed block `at`: that of the code whose
    /// hash `at`'s parent's state proves, run here from that code the first time it is met (see
    /// the module documentation).
    pub async fn metadata(
        &self,
        history: &impl History,
        at: &BlockRef,
        runtime: &RuntimeId,
    ) -> ChainResult<Metadata> {
        // The parent's hash, and so the parent's state root: from `at`'s verified header.
        let header = history.block_header(at).await?;
        let parent = BlockRef {
            number: at
                .number
                .checked_sub(1)
                .ok_or_else(|| invalid("the genesis has no parent"))?,
            hash: header.parent,
        };
        let known = self.kept().state_code.get(&parent.hash).copied();
        let hash = match known {
            Some(h) => h,
            None => {
                let h = history.code_hash(&parent).await?;
                self.proven(&parent, h);
                h
            }
        };
        if let Some(m) = self.met(&hash, runtime)? {
            return Ok(m);
        }
        // A code not met: read it, and only the code whose hash is the proven one is run.
        let mut values = history
            .storage(&parent, &[CODE.to_vec(), HEAP_PAGES.to_vec()])
            .await?;
        if values.len() != 2 {
            return Err(invalid("a storage read answered another number of keys"));
        }
        let heap_pages = values.pop().flatten();
        let code = values
            .pop()
            .flatten()
            .ok_or_else(|| invalid(format!("no :code at block {}", parent.number)))?;
        if blake2_256(&code) != hash {
            return Err(invalid(format!(
                "the :code read at block {} is not the code its state proves",
                parent.number
            )));
        }
        let expected = runtime.clone();
        let metadata = tokio::task::spawn_blocking(move || {
            metadata_from_code(&code, heap_pages.as_deref(), &expected)
        })
        .await
        .map_err(|e| ChainError::Unavailable(format!("the metadata call did not finish: {e}")))??;
        tracing::info!(
            spec_version = runtime.spec_version,
            block = at.number,
            code_hash = hex::encode(hash),
            "the metadata of another runtime, run from its code at the block's parent"
        );
        self.kept().by_code.insert(
            hash,
            Met {
                id: runtime.clone(),
                metadata: metadata.clone(),
            },
        );
        Ok(metadata)
    }
}
