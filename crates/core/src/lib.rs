// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! # bezent-core
//!
//! bezent's core types (`F-001`, T-001-02) and the transfer state graph as data (`F-003`,
//! T-003-01). No I/O.
//!
//! - [`Amount`]: an exact integer count of an asset's minor units, with checked arithmetic and no
//!   floating-point conversions (`INV-11`).
//! - [`AssetId`] and [`NetworkId`]: what a bridged asset and a network are called inside bezent.
//! - [`RequestReference`], [`BridgeReference`] and [`OnChainReference`]: the three reference kinds,
//!   distinct types that never convert into each other (`REQ-ID-3`).
//! - [`Coordinate`]: where on a network a movement is (network, block number, block hash, and the
//!   transaction or log index).
//! - [`State`], [`Condition`] and [`edge`]: the states of SPEC §5.4, the named conditions of §10, and
//!   the transition table of amendment 0009 (`REQ-TR-6`), with the review exits ([`review_exit`]).

mod account;
mod amount;
mod coordinate;
mod ids;
mod reference;
mod state;

pub use account::{Listing, canonical_account, listing};
pub use amount::{Amount, AmountError};
pub use coordinate::{BlockHash, Coordinate, Position};
pub use ids::{AssetId, EmptyId, NetworkId};
pub use reference::{BridgeReference, EmptyReference, OnChainReference, RequestReference};
pub use state::{Condition, State, edge, resolution_moves_value, review_exit};
