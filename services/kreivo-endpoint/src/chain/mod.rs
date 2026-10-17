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
//!
//! Every read is made at a named block, and the endpoint reads only finalized blocks for what it
//! records (`REQ-OB-5`). Blocks are reached from the finalized head by parent hash, never by
//! number: the light client answers no lookup by number.

pub mod fake;
#[cfg(feature = "light-client")]
pub mod light;

use std::future::Future;

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

/// Kreivo. See the module documentation.
pub trait Chain: Send + Sync {
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

    /// The events of block `at` that the endpoint reads: `contract`'s, and every extrinsic's
    /// result.
    fn events(
        &self,
        at: &BlockRef,
        contract: &AccountId32,
    ) -> impl Future<Output = ChainResult<BlockEvents>> + Send;

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

    /// The runtime API `ContractsApi_call` with SCALE-encoded `args`, at `at`: a dry run.
    fn contracts_api_call(
        &self,
        at: &BlockRef,
        args: &[u8],
    ) -> impl Future<Output = ChainResult<Vec<u8>>> + Send;

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
    if first > head.number {
        return Ok(Some(Vec::new()));
    }
    if head.number - first >= limit {
        return Ok(None);
    }
    let mut out = Vec::new();
    let mut hash = head.hash;
    let mut expected = head.number;
    loop {
        let header = chain.header(&hash).await?;
        if header.block.number != expected || header.block.hash != hash {
            return Err(ChainError::Invalid(format!(
                "the header asked for as block {expected} is block {}",
                header.block.number
            )));
        }
        out.push(header.block);
        if expected == first {
            break;
        }
        expected -= 1;
        hash = header.parent;
    }
    out.reverse();
    Ok(Some(out))
}
