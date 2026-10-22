// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! History from archive nodes, verified (issue #49; `REQ-RN-6`, `REQ-OB-3`, `REQ-RQ-7`, `AC-I4.1`).
//!
//! The light client follows the live chain, but cannot read state older than its peers keep (most
//! prune it after 256 blocks), and the endpoint's first start replays from the contract's
//! instantiation. History is therefore read from the configured `archive_rpc` sources, in order,
//! and **nothing they say is taken on their word**:
//!
//! - a header is asked for by hash and kept only if its SCALE bytes hash (BLAKE2b-256) to that
//!   hash. Hashes come from the light client's finalized head, by parent hash (`ancestry`), so a
//!   verified header is a block of the finalized chain;
//! - a storage value is read with `state_getReadProof` and verified against the state root of the
//!   verified header of its block: a value that is not proven, or a node left out, is `Invalid`,
//!   never "absent";
//! - a block body is kept only if its extrinsics root is the verified header's.
//!
//! No runtime call is ever made on a source: a call's result carries no proof. A source that does
//! not answer is unknown (`Unavailable`) and the next one is tried; one that answers something
//! that does not verify is logged as an error (it lies, or serves another chain) and the next one
//! is tried; the endpoint keeps using the first source that works.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use parity_scale_codec::Encode;
use smoldot::trie::{self, TrieEntryVersion, proof_decode};

use super::{BlockRef, ChainError, ChainResult, Header};
use crate::kreivo::blake2_256;

/// Kreivo's block numbers are `u32`.
const BLOCK_NUMBER_BYTES: usize = 4;
/// Verified headers kept, by hash, so a block's header is fetched once per replay.
const HEADER_CACHE: usize = 131_072;

/// Why one source gave nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fail {
    /// It could not be reached, or did not answer in time.
    Unavailable(String),
    /// It answered that it does not have it (an unknown block, or state already discarded).
    Missing(String),
    /// It answered something that does not verify.
    Lied(String),
}

/// One archive node's JSON-RPC, as raw as it serves it. Nothing here is verified.
pub trait Source: Send + Sync {
    /// The source, for logs (its URL).
    fn name(&self) -> &str;
    /// `chain_getBlockHash(number)`.
    fn block_hash(
        &self,
        number: u64,
    ) -> impl Future<Output = Result<Option<[u8; 32]>, Fail>> + Send;
    /// The SCALE encoding of `chain_getHeader(hash)`.
    fn header(&self, hash: &[u8; 32]) -> impl Future<Output = Result<Vec<u8>, Fail>> + Send;
    /// The trie nodes of `state_getReadProof(keys, at)`.
    fn read_proof(
        &self,
        at: &[u8; 32],
        keys: &[Vec<u8>],
    ) -> impl Future<Output = Result<Vec<Vec<u8>>, Fail>> + Send;
    /// The extrinsics of `chain_getBlock(hash)`.
    fn body(&self, hash: &[u8; 32]) -> impl Future<Output = Result<Vec<Vec<u8>>, Fail>> + Send;
}

/// A header whose bytes hash to its hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verified {
    /// The block and its parent.
    pub header: Header,
    /// Its state root.
    pub state_root: [u8; 32],
    /// Its extrinsics root.
    pub extrinsics_root: [u8; 32],
}

/// `bytes`, as the header of block `hash`.
pub fn verify_header(hash: &[u8; 32], bytes: &[u8]) -> Result<Verified, String> {
    if blake2_256(bytes) != *hash {
        return Err(format!(
            "the header served for 0x{} does not hash to it",
            hex::encode(hash)
        ));
    }
    let h = smoldot::header::decode(bytes, BLOCK_NUMBER_BYTES)
        .map_err(|e| format!("the header of 0x{} does not decode: {e}", hex::encode(hash)))?;
    Ok(Verified {
        header: Header {
            block: BlockRef {
                number: h.number,
                hash: *hash,
            },
            parent: *h.parent_hash,
        },
        state_root: *h.state_root,
        extrinsics_root: *h.extrinsics_root,
    })
}

/// The values of `keys` under `state_root`, from the proof's `nodes`: each proven present or
/// proven absent, or an error.
pub fn verify_storage(
    state_root: &[u8; 32],
    nodes: Vec<Vec<u8>>,
    keys: &[Vec<u8>],
) -> Result<Vec<Option<Vec<u8>>>, String> {
    let proof = proof_decode::decode_and_verify_proof(proof_decode::Config {
        proof: nodes.encode(),
    })
    .map_err(|e| format!("the storage proof does not verify: {e}"))?;
    keys.iter()
        .map(|key| match proof.storage_value(state_root, key) {
            Ok(v) => Ok(v.map(|(value, _)| value.to_vec())),
            Err(_) => Err(format!(
                "the storage proof does not cover 0x{} under the block's state root",
                hex::encode(key)
            )),
        })
        .collect()
}

/// Whether `extrinsics` are the block body whose extrinsics root is `root`.
pub fn verify_body(root: &[u8; 32], extrinsics: &[Vec<u8>]) -> Result<(), String> {
    for version in [TrieEntryVersion::V0, TrieEntryVersion::V1] {
        if trie::ordered_root(version, trie::HashFunction::Blake2, extrinsics) == *root {
            return Ok(());
        }
    }
    Err("the block body served does not match its header's extrinsics root".into())
}

/// The configured sources, in order. See the module documentation.
pub struct Archive<S> {
    sources: Vec<S>,
    /// The source that answered last: tried first.
    current: AtomicUsize,
    headers: Mutex<HashMap<[u8; 32], Verified>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What the start-up check asks of each source.
pub struct StartCheck<'a> {
    /// The declared genesis.
    pub genesis: [u8; 32],
    /// The light client's finalized head.
    pub finalized: BlockRef,
    /// The block the contract was instantiated in.
    pub start_block: u64,
    /// The storage key of the contract's `ContractInfoOf`.
    pub contract_key: Vec<u8>,
    /// The declared code hash.
    pub code_hash: [u8; 32],
    /// Reads the code hash from a `ContractInfoOf` value.
    pub decode_code_hash: &'a (dyn Fn(&[u8]) -> Option<[u8; 32]> + Sync),
    /// How many times to ask again for the finalized head's height while a source lags.
    pub lag_retries: u32,
    /// How long to wait between those.
    pub lag_wait: std::time::Duration,
}

impl<S: Source> Archive<S> {
    /// The sources, in the order they are tried.
    pub fn new(sources: Vec<S>) -> Archive<S> {
        Archive {
            sources,
            current: AtomicUsize::new(0),
            headers: Mutex::default(),
        }
    }

    /// Asks each source in turn, from the one that answered last, until one answers what
    /// verifies.
    async fn each<'a, T, F, Fut>(&'a self, what: &str, f: F) -> ChainResult<T>
    where
        F: Fn(&'a S) -> Fut,
        Fut: Future<Output = Result<T, Fail>> + 'a,
    {
        let n = self.sources.len();
        let first = self.current.load(Ordering::Relaxed);
        let (mut failures, mut lied) = (Vec::new(), false);
        for i in (0..n).map(|k| (first + k) % n) {
            let s = &self.sources[i];
            match f(s).await {
                Ok(v) => {
                    self.current.store(i, Ordering::Relaxed);
                    return Ok(v);
                }
                Err(Fail::Lied(e)) => {
                    tracing::error!(source = s.name(), what, reason = %e, "REQ-RQ-7: a history source answered what does not verify; not used, trying the next");
                    lied = true;
                    failures.push(format!("{}: {e}", s.name()));
                }
                Err(Fail::Unavailable(e) | Fail::Missing(e)) => {
                    failures.push(format!("{}: {e}", s.name()))
                }
            }
        }
        let why = if n == 0 {
            "no archive_rpc is configured".to_owned()
        } else {
            format!("{what}: {}", failures.join("; "))
        };
        Err(if lied {
            ChainError::Invalid(why)
        } else {
            ChainError::Unavailable(why)
        })
    }

    async fn header_from(&self, s: &S, hash: &[u8; 32]) -> Result<Verified, Fail> {
        if let Some(v) = lock(&self.headers).get(hash) {
            return Ok(*v);
        }
        let v = verify_header(hash, &s.header(hash).await?).map_err(Fail::Lied)?;
        let mut cache = lock(&self.headers);
        if cache.len() >= HEADER_CACHE {
            cache.clear();
        }
        cache.insert(*hash, v);
        Ok(v)
    }

    async fn storage_from(
        &self,
        s: &S,
        at: &[u8; 32],
        keys: &[Vec<u8>],
    ) -> Result<Vec<Option<Vec<u8>>>, Fail> {
        let root = self.header_from(s, at).await?.state_root;
        verify_storage(&root, s.read_proof(at, keys).await?, keys).map_err(Fail::Lied)
    }

    /// The verified header of block `hash`.
    pub async fn header(&self, hash: &[u8; 32]) -> ChainResult<Verified> {
        self.each("a header", |s| self.header_from(s, hash)).await
    }

    /// The values of `keys` at block `at`, each proven.
    pub async fn storage(
        &self,
        at: &[u8; 32],
        keys: &[Vec<u8>],
    ) -> ChainResult<Vec<Option<Vec<u8>>>> {
        self.each("a storage proof", |s| self.storage_from(s, at, keys))
            .await
    }

    /// The extrinsics of block `at`, proven by its extrinsics root.
    pub async fn extrinsics(&self, at: &[u8; 32]) -> ChainResult<Vec<Vec<u8>>> {
        self.each("a block body", |s| async move {
            let root = self.header_from(s, at).await?.extrinsics_root;
            let body = s.body(at).await?;
            verify_body(&root, &body).map_err(Fail::Lied)?;
            Ok(body)
        })
        .await
    }

    /// The start-up check (issue #49): every source must agree with the light client on the
    /// genesis and on the finalized head's hash at its height, and must serve proven state at
    /// the contract's instantiation (the contract there, with the declared code hash, and not at
    /// its parent). A source that disagrees, does not verify or lacks that state refuses the
    /// start; one that cannot be reached is left out, and the start is refused if none is left.
    pub async fn check(&self, c: &StartCheck<'_>) -> Result<(), String> {
        let mut passed = 0;
        for s in &self.sources {
            match self.check_source(s, c).await {
                Ok(()) => passed += 1,
                Err(Fail::Unavailable(e)) => {
                    tracing::error!(source = s.name(), reason = %e, "issue #49: a history source is unreachable at start-up; left out until it answers")
                }
                Err(Fail::Missing(e) | Fail::Lied(e)) => {
                    return Err(format!(
                        "issue #49: history source {}: {e}; refusing to start",
                        s.name()
                    ));
                }
            }
        }
        if passed == 0 {
            return Err("issue #49: no history source (archive_rpc) passed the start-up check; refusing to start".into());
        }
        Ok(())
    }

    async fn check_source(&self, s: &S, c: &StartCheck<'_>) -> Result<(), Fail> {
        let hex0x = |h: &[u8; 32]| format!("0x{}", hex::encode(h));
        match s.block_hash(0).await? {
            Some(g) if g == c.genesis => {}
            other => {
                return Err(Fail::Lied(format!(
                    "its genesis is {:?}, not the declared {}",
                    other.as_ref().map(hex0x),
                    hex0x(&c.genesis)
                )));
            }
        }
        let mut tries = 0;
        loop {
            match s.block_hash(c.finalized.number).await? {
                Some(h) if h == c.finalized.hash => break,
                Some(h) => {
                    return Err(Fail::Lied(format!(
                        "its block {} is {}, the light client's finalized head is {}",
                        c.finalized.number,
                        hex0x(&h),
                        hex0x(&c.finalized.hash)
                    )));
                }
                None if tries < c.lag_retries => {
                    tries += 1;
                    tokio::time::sleep(c.lag_wait).await;
                }
                None => {
                    return Err(Fail::Unavailable(format!(
                        "it has no block {} yet (the light client's finalized head)",
                        c.finalized.number
                    )));
                }
            }
        }
        let missing = |e: Fail| match e {
            Fail::Unavailable(_) | Fail::Lied(_) => e,
            Fail::Missing(m) => Fail::Missing(format!(
                "it cannot serve the contract's instantiation block {}: {m}",
                c.start_block
            )),
        };
        let start = s.block_hash(c.start_block).await?.ok_or_else(|| {
            Fail::Missing(format!("it has no block {} (start_block)", c.start_block))
        })?;
        let keys = std::slice::from_ref(&c.contract_key);
        let info = self.storage_from(s, &start, keys).await.map_err(missing)?;
        let code = info[0].as_deref().and_then(c.decode_code_hash);
        if code != Some(c.code_hash) {
            return Err(Fail::Missing(format!(
                "at start_block {} the contract's code hash is {:?}, not the declared {}",
                c.start_block,
                code.as_ref().map(hex0x),
                hex0x(&c.code_hash)
            )));
        }
        let parent = self.header_from(s, &start).await?.header.parent;
        if self.storage_from(s, &parent, keys).await.map_err(missing)?[0].is_some() {
            return Err(Fail::Missing(format!(
                "the contract already exists before start_block {}: start_block is not its instantiation",
                c.start_block
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Against what the archive node served at the contract's instantiation (block 39,867,043,
    //! recorded read-only in `fixtures/archive-39867043.json`), and against sources that lie.

    use parity_scale_codec::Decode;
    use serde_json::Value;

    use super::*;

    fn fixture() -> Value {
        serde_json::from_str(include_str!("../../fixtures/archive-39867043.json")).unwrap()
    }

    fn bytes(v: &Value) -> Vec<u8> {
        hex::decode(v.as_str().unwrap().trim_start_matches("0x")).unwrap()
    }

    fn h(v: &Value) -> [u8; 32] {
        bytes(v).try_into().unwrap()
    }

    fn list(v: &Value) -> Vec<Vec<u8>> {
        v.as_array().unwrap().iter().map(bytes).collect()
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Does {
        Honest,
        Down,
        /// Kippu's answer for that block: the header, but "State already discarded".
        Discarded,
        FlipHeader,
        /// Another value of `System::Events` in the proof: an event that never happened.
        LieEvents,
        DropNode,
        FlipBody,
        OtherGenesis,
        OtherFinalized,
    }

    struct Fake {
        does: Does,
        f: Value,
    }

    fn fake(does: Does) -> Fake {
        Fake { does, f: fixture() }
    }

    impl Fake {
        fn down(&self) -> Result<(), Fail> {
            match self.does {
                Does::Down => Err(Fail::Unavailable("connection refused".into())),
                _ => Ok(()),
            }
        }
    }

    impl Source for Fake {
        fn name(&self) -> &str {
            "fake"
        }

        async fn block_hash(&self, number: u64) -> Result<Option<[u8; 32]>, Fail> {
            self.down()?;
            let f = &self.f;
            Ok(match number {
                0 if self.does == Does::OtherGenesis => Some([7; 32]),
                0 => Some(h(&f["genesis"])),
                39_867_043 if self.does == Does::OtherFinalized => Some([8; 32]),
                39_867_043 => Some(h(&f["hash"])),
                39_867_042 => Some(h(&f["parent"]["hash"])),
                _ => None,
            })
        }

        async fn header(&self, hash: &[u8; 32]) -> Result<Vec<u8>, Fail> {
            self.down()?;
            let f = &self.f;
            let mut b = if *hash == h(&f["hash"]) {
                bytes(&f["header"])
            } else if *hash == h(&f["parent"]["hash"]) {
                bytes(&f["parent"]["header"])
            } else {
                return Err(Fail::Missing("no such block".into()));
            };
            if self.does == Does::FlipHeader {
                b[40] ^= 1;
            }
            Ok(b)
        }

        async fn read_proof(&self, at: &[u8; 32], _: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, Fail> {
            self.down()?;
            let f = &self.f;
            if self.does == Does::Discarded {
                return Err(Fail::Missing(format!(
                    "state_getReadProof: Client error: UnknownBlock: State already discarded for 0x{}",
                    hex::encode(at)
                )));
            }
            let mut nodes = if *at == h(&f["hash"]) {
                list(&f["proof"])
            } else if *at == h(&f["parent"]["hash"]) {
                list(&f["parent"]["proof"])
            } else {
                return Err(Fail::Missing("no such block".into()));
            };
            match self.does {
                Does::LieEvents => {
                    // The node holding System::Events (the largest), one byte of its value changed.
                    let i = (0..nodes.len()).max_by_key(|&i| nodes[i].len()).unwrap();
                    let last = nodes[i].len() - 1;
                    nodes[i][last] ^= 1;
                }
                Does::DropNode => {
                    nodes.remove(nodes.len() / 2);
                }
                _ => {}
            }
            Ok(nodes)
        }

        async fn body(&self, hash: &[u8; 32]) -> Result<Vec<Vec<u8>>, Fail> {
            self.down()?;
            if *hash != h(&self.f["hash"]) {
                return Err(Fail::Missing("no such block".into()));
            }
            let mut body = list(&self.f["body"]);
            if self.does == Does::FlipBody {
                body[0][10] ^= 1;
            }
            Ok(body)
        }
    }

    fn keys(f: &Value) -> Vec<Vec<u8>> {
        ["now", "events", "contract_info"]
            .iter()
            .map(|k| bytes(&f["keys"][k]))
            .collect()
    }

    /// `ContractInfo { trie_id: Vec<u8>, code_hash, .. }`.
    fn code_hash(b: &[u8]) -> Option<[u8; 32]> {
        <(Vec<u8>, [u8; 32])>::decode(&mut &b[..])
            .ok()
            .map(|(_, c)| c)
    }

    fn check(f: &Value) -> StartCheck<'static> {
        StartCheck {
            genesis: h(&f["genesis"]),
            finalized: BlockRef {
                number: 39_867_043,
                hash: h(&f["hash"]),
            },
            start_block: 39_867_043,
            contract_key: bytes(&f["keys"]["contract_info"]),
            code_hash: code_hash(&bytes(&f["values"]["contract_info"])).unwrap(),
            decode_code_hash: &code_hash,
            lag_retries: 0,
            lag_wait: std::time::Duration::ZERO,
        }
    }

    #[tokio::test]
    async fn issue_49_what_the_archive_served_verifies() {
        let f = fixture();
        let archive = Archive::new(vec![fake(Does::Honest)]);
        let at = h(&f["hash"]);
        let header = archive.header(&at).await.unwrap();
        assert_eq!(header.header.block.number, 39_867_043);
        assert_eq!(header.header.parent, h(&f["parent"]["hash"]));
        let values = archive.storage(&at, &keys(&f)).await.unwrap();
        assert_eq!(values[0], Some(bytes(&f["values"]["now"])));
        assert!(values[1].as_ref().is_some_and(|e| e.len() > 1_000));
        assert_eq!(values[2], Some(bytes(&f["values"]["contract_info"])));
        // The live contract's code hash (config.rs).
        assert_eq!(
            hex::encode(code_hash(values[2].as_ref().unwrap()).unwrap()),
            "e89943fbbed677f2e33fde16e4877c84bf339354d05c796accf97614e2fb24ad"
        );
        // Before its instantiation, the contract is proven absent, not merely missing.
        let parent = h(&f["parent"]["hash"]);
        let before = archive.storage(&parent, &keys(&f)[2..]).await.unwrap();
        assert_eq!(before, vec![None]);
        assert_eq!(archive.extrinsics(&at).await.unwrap(), list(&f["body"]));
    }

    #[tokio::test]
    async fn req_rq_7_a_source_that_lies_is_never_believed() {
        let f = fixture();
        let at = h(&f["hash"]);
        for does in [Does::FlipHeader, Does::LieEvents, Does::DropNode] {
            let archive = Archive::new(vec![fake(does)]);
            let got = archive.storage(&at, &keys(&f)).await;
            assert!(matches!(got, Err(ChainError::Invalid(_))), "{got:?}");
        }
        let archive = Archive::new(vec![fake(Does::FlipBody)]);
        assert!(matches!(
            archive.extrinsics(&at).await,
            Err(ChainError::Invalid(_))
        ));
        // A hash the source never had is unknown, not a lie.
        let archive = Archive::new(vec![fake(Does::Honest)]);
        assert!(matches!(
            archive.header(&[1; 32]).await,
            Err(ChainError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn issue_49_the_next_source_answers_when_one_is_down_or_lies() {
        let f = fixture();
        let at = h(&f["hash"]);
        for first in [Does::Down, Does::LieEvents, Does::Discarded] {
            let archive = Archive::new(vec![fake(first), fake(Does::Honest)]);
            let values = archive.storage(&at, &keys(&f)).await.unwrap();
            assert_eq!(values[0], Some(bytes(&f["values"]["now"])));
            // The one that answered is asked first from then on.
            assert_eq!(archive.current.load(Ordering::Relaxed), 1);
        }
        let archive = Archive::new(vec![fake(Does::Down), fake(Does::Down)]);
        assert!(matches!(
            archive.storage(&at, &keys(&f)).await,
            Err(ChainError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn issue_49_the_start_up_check() {
        let f = fixture();
        let c = check(&f);
        assert_eq!(
            Archive::new(vec![fake(Does::Honest)]).check(&c).await,
            Ok(())
        );
        // One source unreachable is left out while another passes.
        assert_eq!(
            Archive::new(vec![fake(Does::Down), fake(Does::Honest)])
                .check(&c)
                .await,
            Ok(())
        );
        // Unreachable alone, missing the instantiation block's state, another genesis, another
        // finalized hash, a lie: each refuses the start.
        for sources in [
            vec![fake(Does::Down)],
            vec![fake(Does::Discarded)],
            vec![fake(Does::Honest), fake(Does::Discarded)],
            vec![fake(Does::OtherGenesis)],
            vec![fake(Does::OtherFinalized)],
            vec![fake(Does::LieEvents)],
            vec![fake(Does::FlipHeader)],
        ] {
            let err = Archive::new(sources).check(&c).await.unwrap_err();
            assert!(err.contains("refusing to start"), "{err}");
        }
        let err = Archive::new(vec![fake(Does::Discarded)])
            .check(&c)
            .await
            .unwrap_err();
        assert!(err.contains("instantiation block 39867043"), "{err}");
        // A start_block after the instantiation is refused: the contract exists at its parent.
        let late = StartCheck {
            start_block: 39_867_044,
            ..check(&f)
        };
        assert!(
            Archive::new(vec![fake(Does::Honest)])
                .check(&late)
                .await
                .is_err()
        );
        // No source at all.
        assert!(Archive::<Fake>::new(vec![]).check(&c).await.is_err());
    }
}
