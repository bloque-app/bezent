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
//! Each runtime's metadata is kept by spec version, seeded with the light client's own (fetched by
//! subxt through smoldot, which ran it on code it verified). A block whose runtime cannot be told,
//! fetched or run is `Invalid` or `Unavailable`: a visible gap, never a guess (`REQ-OB-3`).

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Mutex, PoisonError};

use parity_scale_codec::{Compact, Decode, DecodeAll};
use smoldot::executor::{
    self,
    host::{Config, HostVmPrototype, StorageProofSizeBehavior},
    runtime_call::{self, Nibble, RuntimeCall},
    vm::ExecHint,
};
use smoldot::trie::TrieEntryVersion;
use subxt::Metadata;

use super::{BlockRef, ChainError, ChainResult};

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
/// The most bytes a runtime's code may decompress to (smoldot's own bound for `:code`).
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

    /// The parent of `at`, from its verified header.
    fn parent(&self, at: &BlockRef) -> impl Future<Output = ChainResult<BlockRef>> + Send;
}

/// Each runtime's metadata met so far, by spec version.
pub struct Runtimes {
    by_spec: Mutex<HashMap<u32, Metadata>>,
}

impl Runtimes {
    /// Knowing only `spec_version`'s `metadata`: the light client's, which smoldot ran on code
    /// it verified.
    pub fn seeded(spec_version: u32, metadata: Metadata) -> Runtimes {
        Runtimes {
            by_spec: Mutex::new(HashMap::from([(spec_version, metadata)])),
        }
    }

    fn known(&self, spec_version: u32) -> Option<Metadata> {
        self.by_spec
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&spec_version)
            .cloned()
    }

    /// The metadata of `runtime`, the runtime that executed block `at`: kept, or else run from the
    /// code in the state of `at`'s parent (see the module documentation) and kept.
    pub async fn metadata(
        &self,
        history: &impl History,
        at: &BlockRef,
        runtime: &RuntimeId,
    ) -> ChainResult<Metadata> {
        if let Some(m) = self.known(runtime.spec_version) {
            return Ok(m);
        }
        let parent = history.parent(at).await?;
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
        let expected = runtime.clone();
        let metadata = tokio::task::spawn_blocking(move || {
            metadata_from_code(&code, heap_pages.as_deref(), &expected)
        })
        .await
        .map_err(|e| ChainError::Unavailable(format!("the metadata call did not finish: {e}")))??;
        tracing::info!(
            spec_version = runtime.spec_version,
            block = at.number,
            "the metadata of another runtime, run from its code at the block's parent"
        );
        self.by_spec
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(runtime.spec_version, metadata.clone());
        Ok(metadata)
    }
}
