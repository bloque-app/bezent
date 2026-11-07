// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Kreivo, as the endpoint reads and writes it: one trait, [`Chain`], with two implementations.
//!
//! - [`fake::FakeChain`]: in memory, for every test here. It models blocks, finality, contract
//!   events, mortal eras and nonces, signature checks, dry runs, a read that fails and heals, and
//!   another transaction consuming a nonce. No test reaches a real network.
//! - `light::LightChain` (feature `light-client`, the default): subxt over the embedded smoldot
//!   light client (`DEC-5`). Kreivo's RPC is never trusted for an outcome: smoldot verifies the
//!   relay-chain-finalised head itself, and every storage read and runtime call it serves is proven.
//!   History older than its peers keep comes from archive nodes, each answer proven against the
//!   light client's finalized chain (`archive`, issue #49).
//!
//! Every read is made at a named block, and the endpoint reads only finalized blocks for what it
//! records (`REQ-OB-5`). Blocks are reached from the finalized head by parent hash, never by
//! number: the light client answers no lookup by number.

#[cfg(feature = "light-client")]
pub mod archive;
pub mod fake;
#[cfg(feature = "light-client")]
pub mod light;
#[cfg(feature = "light-client")]
pub mod runtime;
#[cfg(feature = "light-client")]
pub mod watch;

use std::future::Future;

use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use futures_util::stream::FuturesOrdered;

use crate::kreivo::AccountId32;

/// A block: its number and hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockRef {
    /// Its number.
    pub number: u64,
    /// Its hash.
    pub hash: [u8; 32],
}

/// A block header, as far as the endpoint needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// The block.
    pub block: BlockRef,
    /// Its parent's hash.
    pub parent: [u8; 32],
}

/// The runtime's version, at a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeVersion {
    /// `spec_version`.
    pub spec_version: u32,
    /// `transaction_version`.
    pub transaction_version: u32,
}

/// One `Contracts::ContractEmitted` of the configured contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Emitted {
    /// Its index among the block's events: its coordinate (with the block's number and hash).
    pub index: u32,
    /// The event record's topics.
    pub topics: Vec<[u8; 32]>,
    /// The event's data.
    pub data: Vec<u8>,
}

/// One extrinsic's execution result in a block (`System::ExtrinsicSuccess` or `ExtrinsicFailed`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtrinsicResult {
    /// The extrinsic's index in the block.
    pub index: u32,
    /// Whether its dispatch succeeded.
    pub success: bool,
    /// The network's error, as it renders it, if it did not.
    pub error: Option<String>,
}

/// What a block's events say, for the endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockEvents {
    /// The block's time (`Timestamp::Now`), in milliseconds since the Unix epoch.
    pub time_ms: u64,
    /// The configured contract's events, in order.
    pub emitted: Vec<Emitted>,
    /// Every extrinsic's result.
    pub results: Vec<ExtrinsicResult>,
}

/// Why the chain did not answer. Either is unknown, never an outcome (`REQ-NX-3`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChainError {
    /// It could not be reached, or did not answer: try again.
    Unavailable(String),
    /// It answered with something the endpoint cannot use. Never guessed around.
    Invalid(String),
}

impl std::fmt::Display for ChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChainError::Unavailable(e) => write!(f, "Kreivo is unavailable: {e}"),
            ChainError::Invalid(e) => {
                write!(f, "Kreivo answered what the endpoint cannot use: {e}")
            }
        }
    }
}

impl std::error::Error for ChainError {}

/// A result from the chain.
pub type ChainResult<T> = Result<T, ChainError>;

/// Values of storage keys, in the order asked for: `None` is proven absent.
pub type Values = Vec<Option<Vec<u8>>>;

/// Kreivo. See the module documentation.
pub trait Chain: Send + Sync {
    /// What [`Chain::read_events`] reads of a block: its proven values, not yet decoded.
    type EventsRead: Send;

    /// The genesis hash of the chain the client follows (`AC-I4.1`).
    fn genesis(&self) -> impl Future<Output = ChainResult<[u8; 32]>> + Send;

    /// The runtime's version at `at`.
    fn runtime_version(
        &self,
        at: &BlockRef,
    ) -> impl Future<Output = ChainResult<RuntimeVersion>> + Send;

    /// The finalized head.
    fn finalized(&self) -> impl Future<Output = ChainResult<BlockRef>> + Send;

    /// The header of the block with `hash`.
    fn header(&self, hash: &[u8; 32]) -> impl Future<Output = ChainResult<Header>> + Send;

    /// A header a history source names as block `number`'s, whose bytes hash to its hash, but
    /// **not known to be of the finalized chain**: a walk by parent hash ([`Descent`]) takes it
    /// only as the header of the hash it expects there, so it can be asked for ahead of the walk.
    /// `None` when there is none to ask (the default) or the source does not answer.
    fn header_hint(&self, number: u64) -> impl Future<Output = Option<Header>> + Send {
        let _ = number;
        async { None }
    }

    /// The network half of [`Chain::events`]: what block `at`'s events are read from, proven,
    /// and nothing else. Reads of several blocks may run at once; nothing they share changes.
    fn read_events(
        &self,
        at: &BlockRef,
    ) -> impl Future<Output = ChainResult<Self::EventsRead>> + Send;

    /// The other half of [`Chain::events`]: `read`, of block `at`, decoded. Called in block
    /// order, each block after its parent: a block's runtime is told from its parent's state.
    fn decode_events(
        &self,
        at: &BlockRef,
        read: Self::EventsRead,
        contract: &AccountId32,
    ) -> impl Future<Output = ChainResult<BlockEvents>> + Send;

    /// The events of block `at` that the endpoint reads: `contract`'s, and every extrinsic's
    /// result.
    fn events(
        &self,
        at: &BlockRef,
        contract: &AccountId32,
    ) -> impl Future<Output = ChainResult<BlockEvents>> + Send {
        async move {
            let read = self.read_events(at).await?;
            self.decode_events(at, read, contract).await
        }
    }

    /// The hash of block `number` as `System::BlockHash` holds it in the state of `at` (a block of
    /// the finalized chain), proven under `at`'s verified header: the hash of `at`'s ancestor at that
    /// height (`notes/KREIVO-WALK-SKIP.md`). `None` when that state does not hold it, or when the
    /// chain cannot say (the default): the caller then walks headers.
    fn proven_block_hash(
        &self,
        at: &BlockRef,
        number: u64,
    ) -> impl Future<Output = ChainResult<Option<[u8; 32]>>> + Send {
        let _ = (at, number);
        async { Ok(None) }
    }

    /// What the state of `at` holds of a skip's fingerprint ([`crate::skip::EndState`]), each value
    /// proven and decoded with the runtime that wrote that state. `None` when the chain cannot say
    /// (the default; the light client's side, which proves no `:code` hash): the stretch is then
    /// read block by block.
    fn end_state(
        &self,
        at: &BlockRef,
        subject: &crate::skip::Subject,
    ) -> impl Future<Output = ChainResult<Option<crate::skip::EndState>>> + Send {
        let _ = (at, subject);
        async { Ok(None) }
    }

    /// The extrinsics of block `at`, as their bytes.
    fn extrinsics(&self, at: &BlockRef) -> impl Future<Output = ChainResult<Vec<Vec<u8>>>> + Send;

    /// `account`'s nonce at `at`: how many of its extrinsics are included up to and with `at`.
    fn account_nonce(
        &self,
        at: &BlockRef,
        account: &AccountId32,
    ) -> impl Future<Output = ChainResult<u64>> + Send;

    /// The issuance (`pallet-assets`' `Asset::supply`) of local asset `Here(asset)` at `at`;
    /// `None` when no such asset exists there (issue #42, `REQ-BK-8`).
    fn asset_issuance(
        &self,
        at: &BlockRef,
        asset: u32,
    ) -> impl Future<Output = ChainResult<Option<u128>>> + Send;

    /// The code hash of the contract instantiated at `contract` (`Contracts::ContractInfoOf`), at
    /// `at`; `None` when no contract is instantiated there (issue #24).
    fn contract_code_hash(
        &self,
        at: &BlockRef,
        contract: &AccountId32,
    ) -> impl Future<Output = ChainResult<Option<[u8; 32]>>> + Send;

    /// The runtime API `ContractsApi_call` with SCALE-encoded `args`, at `at`: a dry run.
    fn contracts_api_call(
        &self,
        at: &BlockRef,
        args: &[u8],
    ) -> impl Future<Output = ChainResult<Vec<u8>>> + Send;

    /// The SCALE bytes of the runtime's metadata at `at` (`RuntimeMetadataPrefixed`, version
    /// [`crate::format::METADATA_VERSION`]), from the runtime itself: what its transaction format is
    /// checked against before anything is built (issue #104).
    fn runtime_metadata(&self, at: &BlockRef) -> impl Future<Output = ChainResult<Vec<u8>>> + Send;

    /// Broadcast `extrinsic`. Broadcasting the same bytes again is harmless.
    fn submit(&self, extrinsic: &[u8]) -> impl Future<Output = ChainResult<()>> + Send;
}

/// The blocks from `first` up to and with `head`, in ascending order, reached from `head` by
/// parent hash. At most `limit` blocks are walked; `None` if `first` is further back than that.
pub async fn ancestry<C: Chain>(
    chain: &C,
    head: &BlockRef,
    first: u64,
    limit: u64,
) -> ChainResult<Option<Vec<BlockRef>>> {
    ancestry_with(chain, head, first, limit, 1).await
}

/// [`ancestry`], asking for up to `concurrency` headers ahead of the walk ([`Descent`]).
pub async fn ancestry_with<C: Chain>(
    chain: &C,
    head: &BlockRef,
    first: u64,
    limit: u64,
    concurrency: usize,
) -> ChainResult<Option<Vec<BlockRef>>> {
    if first > head.number {
        return Ok(Some(Vec::new()));
    }
    if head.number - first >= limit {
        return Ok(None);
    }
    let mut out = Vec::new();
    let mut walk = Descent::new(chain, *head, first, concurrency);
    loop {
        let header = walk.next().await?;
        out.push(header.block);
        if header.block.number == first {
            break;
        }
    }
    out.reverse();
    Ok(Some(out))
}

/// A walk down from a block of the finalized chain by parent hash, one header at a time, which
/// asks for the headers of up to `concurrency` blocks below it ahead of the walk
/// ([`Chain::header_hint`]: by number, from a history source). What is trusted is what a walk by
/// parent hash trusts: block `n`'s header is the one whose hash its child names, verified as
/// [`Chain::header`] verifies it. A hint is taken only when it is exactly that header (its hash and
/// number); any other hint, or none, and the walk asks for that hash, as with no hints at all.
pub struct Descent<'a, C> {
    chain: &'a C,
    /// The next block to read: the head, or the parent its verified child names.
    at: Option<BlockRef>,
    /// The next number to hint, if any: hints are asked for down to `floor`.
    hinted: Option<u64>,
    floor: u64,
    concurrency: usize,
    /// The hints asked for, in descending number from `at`'s.
    hints: FuturesOrdered<BoxFuture<'a, Option<Header>>>,
}

impl<'a, C: Chain> Descent<'a, C> {
    /// A walk from `head` (trusted), with hints down to `floor`. `concurrency` 1 asks for none.
    pub fn new(chain: &'a C, head: BlockRef, floor: u64, concurrency: usize) -> Descent<'a, C> {
        Descent {
            chain,
            at: Some(head),
            hinted: (concurrency > 1).then_some(head.number),
            floor,
            concurrency,
            hints: FuturesOrdered::new(),
        }
    }

    /// The header of the next block down (the head first), verified to be the block the walk
    /// expects.
    pub async fn next(&mut self) -> ChainResult<Header> {
        let at = self
            .at
            .ok_or_else(|| ChainError::Invalid("the walk went below the genesis".into()))?;
        let mut hint = None;
        if self.hinted.is_some() || !self.hints.is_empty() {
            while self.hints.len() < self.concurrency
                && let Some(n) = self.hinted.filter(|n| *n >= self.floor)
            {
                self.hints.push_back(Box::pin(self.chain.header_hint(n)));
                self.hinted = n.checked_sub(1);
            }
            // The front of the queue is the hint for `at`'s number.
            hint = self.hints.next().await.flatten();
        }
        let header = match hint {
            Some(h) if h.block == at => h,
            _ => self.chain.header(&at.hash).await?,
        };
        if header.block != at {
            return Err(ChainError::Invalid(format!(
                "the header asked for as block {} is block {}",
                at.number, header.block.number
            )));
        }
        self.at = at.number.checked_sub(1).map(|number| BlockRef {
            number,
            hash: header.parent,
        });
        Ok(header)
    }
}
