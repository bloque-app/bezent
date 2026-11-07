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
//!   hash. Hashes come from the light client's finalized head, by parent hash (`ancestry`, or the
//!   follower's anchors, each reached the same way), so a
//!   verified header is a block of the finalized chain;
//! - a storage value is read with `state_getReadProof` and verified against the state root of the
//!   verified header of its block: a value that is not proven, or a node left out, is `Invalid`,
//!   never "absent";
//! - the hash of `:code` is read from `:code`'s own trie node, in a proof of `:code ‖ 0x00` verified
//!   the same way: the node holds the hash, and the code (about 2 MB) is not sent
//!   ([`CODE_PROBE`]);
//! - a block body is kept only if its extrinsics root is the verified header's.
//!
//! No runtime call is ever made on a source: a call's result carries no proof. A source that does
//! not answer is unknown (`Unavailable`) and the next one is tried; one that answers something
//! that does not verify is logged as an error (it lies, or serves another chain) and the next one
//! is tried; the endpoint keeps using the first source that works.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use parity_scale_codec::Encode;
use smoldot::trie::{self, TrieEntryVersion, proof_decode};

use super::{BlockRef, ChainError, ChainResult, Header, Values};
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

/// The key asked for to prove `:code`'s hash: `:code ‖ 0x00`. No such key exists, and its lookup
/// ends at `:code`'s own trie node, so a node's `state_getReadProof` of it carries the path to that
/// node (under 1 KB on Kreivo) and not the code: asked for `:code` itself, it also sends the value
/// (about 2 MB), since it reads it.
pub const CODE_PROBE: &[u8] = b":code\0";

/// A proof's nodes, decoded and checked to be a trie.
fn decode_proof(nodes: Vec<Vec<u8>>) -> Result<proof_decode::DecodedTrieProof<Vec<u8>>, String> {
    proof_decode::decode_and_verify_proof(proof_decode::Config {
        proof: nodes.encode(),
    })
    .map_err(|e| format!("the storage proof does not verify: {e}"))
}

fn values_of(
    proof: &proof_decode::DecodedTrieProof<Vec<u8>>,
    state_root: &[u8; 32],
    keys: &[Vec<u8>],
) -> Result<Vec<Option<Vec<u8>>>, String> {
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

/// BLAKE2b-256 of `:code` under `state_root`, from `:code`'s trie node in the proof. Under state
/// version 1 the node holds that hash itself (the value is hashed), and the code need not be in the
/// proof; a value in the proof, inline (version 0) or hashed and present, is hashed here. Absent, or
/// not covered by the proof, is an error: never a guess.
fn code_hash_of(
    proof: &proof_decode::DecodedTrieProof<Vec<u8>>,
    state_root: &[u8; 32],
) -> Result<[u8; 32], String> {
    let node = proof
        .trie_node_info(
            state_root,
            trie::bytes_to_nibbles(crate::chain::runtime::CODE.iter().copied()),
        )
        .map_err(|_| "the storage proof does not cover :code under the block's state root")?;
    match node.storage_value {
        proof_decode::StorageValue::HashKnownValueMissing(hash) => Ok(*hash),
        proof_decode::StorageValue::Known { value, .. } => Ok(blake2_256(value)),
        proof_decode::StorageValue::None => Err("the storage proof proves no :code".into()),
    }
}

/// The values of `keys` under `state_root`, from the proof's `nodes`: each proven present or
/// proven absent, or an error.
pub fn verify_storage(
    state_root: &[u8; 32],
    nodes: Vec<Vec<u8>>,
    keys: &[Vec<u8>],
) -> Result<Vec<Option<Vec<u8>>>, String> {
    values_of(&decode_proof(nodes)?, state_root, keys)
}

/// The values of `keys` and the hash of `:code` under `state_root`, from one proof's `nodes` (of
/// `keys` and [`CODE_PROBE`]): see the module documentation.
pub fn verify_storage_and_code_hash(
    state_root: &[u8; 32],
    nodes: Vec<Vec<u8>>,
    keys: &[Vec<u8>],
) -> Result<(Values, [u8; 32]), String> {
    let proof = decode_proof(nodes)?;
    Ok((
        values_of(&proof, state_root, keys)?,
        code_hash_of(&proof, state_root)?,
    ))
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

/// How long a source whose last request failed is tried only after the others.
const RESTING_MS: u64 = 30_000;

/// The configured sources, in order. See the module documentation.
pub struct Archive<S> {
    sources: Vec<S>,
    /// Turns: each request is asked first of the next source in turn that is not resting, so a
    /// walk's reads in flight are spread over every source (all are verified alike).
    turn: AtomicUsize,
    /// Per source, when its last request failed (milliseconds since `epoch`, plus one; zero: it
    /// did not). A source that failed rests for [`RESTING_MS`]: tried after the others, so a
    /// source that is down does not hold one read in every few up for its timeout.
    failed_at: Vec<AtomicU64>,
    epoch: std::time::Instant,
    headers: Mutex<HashMap<[u8; 32], Verified>>,
    /// Per source, how often it answered what does not verify, and how often it failed.
    counts: Vec<Counts>,
}

#[derive(Default)]
struct Counts {
    lied: AtomicU64,
    failed: AtomicU64,
}

/// What one source has done since start-up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceCounts {
    /// The source, for logs.
    pub source: String,
    /// Answers that did not verify.
    pub lied: u64,
    /// Requests that failed otherwise (unreachable, no answer in time, or no such data).
    pub failed: u64,
}

/// Whether the `n`th occurrence of something is logged: the first, then every thousandth.
fn log_this(n: u64) -> bool {
    n == 1 || n.is_multiple_of(1_000)
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
    /// That block's hash, reached from the finalized head by parent hash (the follower's anchor):
    /// not any source's word.
    pub start_hash: [u8; 32],
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
            counts: sources.iter().map(|_| Counts::default()).collect(),
            failed_at: sources.iter().map(|_| AtomicU64::new(0)).collect(),
            sources,
            turn: AtomicUsize::new(0),
            epoch: std::time::Instant::now(),
            headers: Mutex::default(),
        }
    }

    /// How often each source lied or failed since start-up.
    pub fn counts(&self) -> Vec<SourceCounts> {
        self.sources
            .iter()
            .zip(&self.counts)
            .map(|(s, c)| SourceCounts {
                source: s.name().to_owned(),
                lied: c.lied.load(Ordering::Relaxed),
                failed: c.failed.load(Ordering::Relaxed),
            })
            .collect()
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// The source asked first: the next in turn that is not resting, or the next in turn.
    fn first(&self) -> usize {
        let n = self.sources.len().max(1);
        let turn = self.turn.fetch_add(1, Ordering::Relaxed);
        let now = self.now_ms();
        (0..n)
            .map(|k| (turn + k) % n)
            .find(|i| match self.failed_at[*i].load(Ordering::Relaxed) {
                0 => true,
                at => now.saturating_sub(at - 1) >= RESTING_MS,
            })
            .unwrap_or(turn % n)
    }

    fn failed(&self, i: usize) {
        self.failed_at[i].store(self.now_ms().saturating_add(1), Ordering::Relaxed);
    }

    /// Asks each source in turn, from the next in turn that is not resting ([`Archive::first`]),
    /// until one answers what verifies.
    async fn each<'a, T, F, Fut>(&'a self, what: &str, f: F) -> ChainResult<T>
    where
        F: Fn(&'a S) -> Fut,
        Fut: Future<Output = Result<T, Fail>> + 'a,
    {
        let n = self.sources.len();
        if n == 0 {
            return Err(ChainError::Unavailable(
                "no archive_rpc is configured".to_owned(),
            ));
        }
        let first = self.first();
        let (mut failures, mut lied) = (Vec::new(), false);
        for i in (0..n).map(|k| (first + k) % n) {
            let s = &self.sources[i];
            let answer = f(s).await;
            if answer.is_err() {
                self.failed(i);
            } else {
                self.failed_at[i].store(0, Ordering::Relaxed);
            }
            match answer {
                Ok(v) => {
                    return Ok(v);
                }
                Err(Fail::Lied(e)) => {
                    let n = self.counts[i].lied.fetch_add(1, Ordering::Relaxed) + 1;
                    if log_this(n) {
                        tracing::error!(source = s.name(), what, reason = %e, lied = n, "REQ-RQ-7: a history source answered what does not verify; not used, trying the next (logged for the first and every thousandth)");
                    }
                    lied = true;
                    failures.push(format!("{}: {e}", s.name()));
                }
                Err(Fail::Unavailable(e) | Fail::Missing(e)) => {
                    let n = self.counts[i].failed.fetch_add(1, Ordering::Relaxed) + 1;
                    if log_this(n) {
                        tracing::warn!(source = s.name(), what, reason = %e, failed = n, "issue #49: a history source did not answer (logged for the first and every thousandth)");
                    }
                    failures.push(format!("{}: {e}", s.name()))
                }
            }
        }
        let why = format!("{what}: {}", failures.join("; "));
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

    async fn storage_and_code_hash_from(
        &self,
        s: &S,
        at: &[u8; 32],
        keys: &[Vec<u8>],
    ) -> Result<(Values, [u8; 32]), Fail> {
        let root = self.header_from(s, at).await?.state_root;
        let mut asked = keys.to_vec();
        asked.push(CODE_PROBE.to_vec());
        verify_storage_and_code_hash(&root, s.read_proof(at, &asked).await?, keys)
            .map_err(Fail::Lied)
    }

    /// The verified header of block `hash`.
    pub async fn header(&self, hash: &[u8; 32]) -> ChainResult<Verified> {
        self.each("a header", |s| self.header_from(s, hash)).await
    }

    /// The header a source names as block `number`'s (`chain_getBlockHash`), verified to hash
    /// to that hash and to be of that number: **not** known to be of the finalized chain (a
    /// source's word names the hash). See [`crate::chain::Descent`].
    pub async fn header_at(&self, number: u64) -> ChainResult<Verified> {
        self.each("a header by number", |s| async move {
            let hash = s
                .block_hash(number)
                .await?
                .ok_or_else(|| Fail::Missing(format!("no block {number}")))?;
            let v = self.header_from(s, &hash).await?;
            if v.header.block.number != number {
                return Err(Fail::Lied(format!(
                    "the header named block {number} is block {}",
                    v.header.block.number
                )));
            }
            Ok(v)
        })
        .await
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

    /// The values of `keys` and the hash of `:code` at block `at`, from one proof.
    pub async fn storage_and_code_hash(
        &self,
        at: &[u8; 32],
        keys: &[Vec<u8>],
    ) -> ChainResult<(Values, [u8; 32])> {
        self.each("a storage proof", |s| {
            self.storage_and_code_hash_from(s, at, keys)
        })
        .await
    }

    /// The hash of `:code` at block `at`, proven from `:code`'s trie node, without the code.
    pub async fn code_hash(&self, at: &[u8; 32]) -> ChainResult<[u8; 32]> {
        Ok(self.storage_and_code_hash(at, &[]).await?.1)
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
                // A source not yet final at that height may still show a fork's block there:
                // asked again within the same window before it counts as a disagreement.
                Some(_) | None if tries < c.lag_retries => {
                    tries += 1;
                    tokio::time::sleep(c.lag_wait).await;
                }
                Some(h) => {
                    return Err(Fail::Lied(format!(
                        "its block {} is {}, the light client's finalized head is {}",
                        c.finalized.number,
                        hex0x(&h),
                        hex0x(&c.finalized.hash)
                    )));
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
        // Its word is not enough: start_block is the block of the finalized chain reached by
        // parent hash from the light client's head.
        if start != c.start_hash {
            return Err(Fail::Lied(format!(
                "its block {} (start_block) is {}, the finalized chain's is {}",
                c.start_block,
                hex0x(&start),
                hex0x(&c.start_hash)
            )));
        }
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
            start_hash: h(&f["hash"]),
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
            // The one that failed rests: the one that answered is asked first meanwhile.
            assert_eq!((archive.first(), archive.first()), (1, 1));
        }
        // Sources that answer take turns being asked first, so a walk's reads are spread.
        let archive = Archive::new(vec![fake(Does::Honest), fake(Does::Honest)]);
        archive.storage(&at, &keys(&f)).await.unwrap();
        archive.storage(&at, &keys(&f)).await.unwrap();
        assert_eq!((archive.first(), archive.first()), (0, 1));
        // When every source rests, they still take turns.
        let archive = Archive::new(vec![fake(Does::Down), fake(Does::Down)]);
        assert!(archive.storage(&at, &keys(&f)).await.is_err());
        assert_eq!((archive.first(), archive.first()), (1, 0));
        let archive = Archive::new(vec![fake(Does::Down), fake(Does::Down)]);
        assert!(matches!(
            archive.storage(&at, &keys(&f)).await,
            Err(ChainError::Unavailable(_))
        ));
        // No source at all is unknown, not a panic.
        let none: Archive<Fake> = Archive::new(Vec::new());
        assert!(matches!(
            none.storage(&at, &keys(&f)).await,
            Err(ChainError::Unavailable(e)) if e.contains("no archive_rpc")
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

    #[tokio::test]
    async fn issue_49_start_block_is_the_finalized_chains_not_the_sources() {
        let f = fixture();
        // The source's start_block is a block of another chain (the finalized chain's is another
        // hash): refused, however well the source proves its own.
        let other = StartCheck {
            start_hash: [9; 32],
            ..check(&f)
        };
        let err = Archive::new(vec![fake(Does::Honest)])
            .check(&other)
            .await
            .unwrap_err();
        assert!(err.contains("the finalized chain's"), "{err}");
        assert!(err.contains("refusing to start"), "{err}");
    }

    #[tokio::test]
    async fn issue_49_lying_and_failing_sources_are_counted() {
        let f = fixture();
        let at = h(&f["hash"]);
        let archive = Archive::new(vec![
            fake(Does::LieEvents),
            fake(Does::Down),
            fake(Does::Honest),
        ]);
        for _ in 0..3 {
            archive.storage(&at, &keys(&f)).await.unwrap();
        }
        // The one that answered is asked first from then on: the liar and the dead one were
        // tried once each.
        let counts = archive.counts();
        assert_eq!((counts[0].lied, counts[0].failed), (1, 0));
        assert_eq!((counts[1].lied, counts[1].failed), (0, 1));
        assert_eq!((counts[2].lied, counts[2].failed), (0, 0));
        assert!(log_this(1) && !log_this(2) && log_this(1_000) && !log_this(1_001));
    }

    // ------------------------------------------------------------- the proven hash of `:code`

    /// Kreivo block 40,004,556 (`fixtures/kreivo-40004557.json`): its SCALE header and the proof
    /// a node served for [`CODE_PROBE`] there, and the code it proves.
    struct Probe {
        header: Vec<u8>,
        proof: Vec<Vec<u8>>,
    }

    fn probe() -> (Probe, [u8; 32]) {
        let f: Value =
            serde_json::from_str(include_str!("../../fixtures/kreivo-40004557.json")).unwrap();
        (
            Probe {
                header: bytes(&f["parent_header"]),
                proof: list(&f["parent_code_proof"]),
            },
            h(&f["parent_hash"]),
        )
    }

    const CODE_134: &[u8] = include_bytes!("../../fixtures/kreivo-spec-134.wasm");

    impl Source for Probe {
        fn name(&self) -> &str {
            "probe"
        }

        async fn block_hash(&self, _: u64) -> Result<Option<[u8; 32]>, Fail> {
            Ok(None)
        }

        async fn header(&self, _: &[u8; 32]) -> Result<Vec<u8>, Fail> {
            Ok(self.header.clone())
        }

        async fn read_proof(&self, _: &[u8; 32], keys: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, Fail> {
            assert_eq!(keys.last().map(Vec::as_slice), Some(CODE_PROBE));
            Ok(self.proof.clone())
        }

        async fn body(&self, _: &[u8; 32]) -> Result<Vec<Vec<u8>>, Fail> {
            Err(Fail::Missing("no body".into()))
        }
    }

    /// A trie of one leaf at `:code`, and its root. With `inline`, the leaf holds `value`
    /// (state version 0, or a value under 33 bytes); otherwise its hash, and `value` follows as
    /// its own node when `with_value`.
    fn one_leaf(value: &[u8], inline: bool, with_value: bool) -> ([u8; 32], Vec<Vec<u8>>) {
        // `:code` is ten nibbles, packed as its five bytes.
        let mut leaf = vec![if inline { 0x40 } else { 0x20 } | 10];
        leaf.extend_from_slice(crate::chain::runtime::CODE);
        if inline {
            parity_scale_codec::Compact(u32::try_from(value.len()).unwrap()).encode_to(&mut leaf);
            leaf.extend_from_slice(value);
        } else {
            leaf.extend_from_slice(&blake2_256(value));
        }
        let mut nodes = vec![leaf];
        if with_value {
            nodes.push(value.to_vec());
        }
        (blake2_256(&nodes[0]), nodes)
    }

    #[test]
    fn walk_40004557_the_hash_of_code_is_proven_from_a_hashed_value_leaf() {
        // What the node served: under 1 KB, without the code, yet it proves spec 134's code.
        let (p, parent) = probe();
        let root = verify_header(&parent, &p.header).unwrap().state_root;
        assert!(p.proof.iter().map(Vec::len).sum::<usize>() < 1_024);
        assert!(p.proof.iter().all(|n| n.len() < CODE_134.len()));
        let (values, hash) = verify_storage_and_code_hash(&root, p.proof, &[]).unwrap();
        assert!(values.is_empty());
        assert_eq!(hash, blake2_256(CODE_134));
    }

    #[test]
    fn walk_40004557_the_hash_of_code_is_proven_from_a_value_in_the_proof() {
        let code = b"a short runtime, inline in its leaf";
        for (inline, with_value) in [(true, false), (false, true)] {
            let (root, nodes) = one_leaf(code, inline, with_value);
            let (_, hash) = verify_storage_and_code_hash(&root, nodes, &[]).unwrap();
            assert_eq!(hash, blake2_256(code), "inline {inline}");
        }
        // The same leaf, hashed and without its value: the hash, as with Kreivo's.
        let (root, nodes) = one_leaf(code, false, false);
        let (_, hash) = verify_storage_and_code_hash(&root, nodes, &[]).unwrap();
        assert_eq!(hash, blake2_256(code));
    }

    #[test]
    fn walk_40004557_no_code_or_another_root_proves_no_hash() {
        // A trie with no `:code`: proven absent, refused.
        let mut leaf = vec![0x40 | 10];
        leaf.extend_from_slice(b":cod\xff");
        leaf.push(4);
        leaf.push(0);
        let root = blake2_256(&leaf);
        let err = verify_storage_and_code_hash(&root, vec![leaf], &[]).unwrap_err();
        assert!(err.contains("proves no :code"), "{err}");
        // A proof of another state than the root asked for.
        let (_, nodes) = one_leaf(b"code", true, false);
        let err = verify_storage_and_code_hash(&[3; 32], nodes, &[]).unwrap_err();
        assert!(err.contains("does not cover :code"), "{err}");
    }

    #[tokio::test]
    async fn req_rq_7_a_tampered_proof_of_the_code_hash_is_refused() {
        let (p, parent) = probe();
        assert_eq!(
            Archive::new(vec![p]).code_hash(&parent).await,
            Ok(blake2_256(CODE_134))
        );
        let leaf = {
            let (p, _) = probe();
            let hash = blake2_256(CODE_134);
            p.proof
                .iter()
                .position(|n| n.windows(32).any(|w| w == hash))
                .unwrap()
        };
        type Tamper = fn(&mut Probe, usize);
        let tampers: [(&str, Tamper); 4] = [
            ("another code's hash in the leaf", |p, leaf| {
                let last = p.proof[leaf].len() - 1;
                p.proof[leaf][last] ^= 1;
            }),
            ("the leaf left out", |p, leaf| {
                p.proof.remove(leaf);
            }),
            ("the root left out", |p, _| {
                let root = verify_header(&probe().1, &p.header).unwrap().state_root;
                p.proof.retain(|n| blake2_256(n) != root);
            }),
            ("a proof of another trie", |p, _| {
                p.proof = one_leaf(CODE_134, false, false).1;
            }),
        ];
        for (what, tamper) in tampers {
            let (mut p, parent) = probe();
            tamper(&mut p, leaf);
            let got = Archive::new(vec![p]).code_hash(&parent).await;
            assert!(
                matches!(got, Err(ChainError::Invalid(_))),
                "{what}: {got:?}"
            );
        }
    }
}
