// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! Coordinates: where on a network a movement is.

use crate::NetworkId;

/// A block's hash, as the network reports it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockHash(Vec<u8>);

impl BlockHash {
    /// A block hash from its bytes.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// The hash's bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Where in a block a movement is: a transaction (extrinsic) index or a log (event) index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Position {
    /// The transaction's index in its block.
    Transaction(u32),
    /// The log's (event's) index in its block.
    Log(u32),
}

/// A coordinate: network, block number, block hash, and the transaction or log index.
///
/// The block hash is part of the coordinate: a coordinate whose block is replaced at the same
/// height no longer matches the chain (`REQ-OB-2`, `REQ-IN-7`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Coordinate {
    /// The network.
    pub network: NetworkId,
    /// The block's number (height).
    pub block_number: u64,
    /// The block's hash.
    pub block_hash: BlockHash,
    /// The movement's position in the block.
    pub position: Position,
}
