// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `bezent-evm-endpoint` (`F-009`): one instance per EVM origin network. It observes the reserve
//! vault, prepares and submits payouts, and proves non-execution; it decides nothing: it writes
//! observations, prepared bytes, signed bytes and proofs, never a transfer state (PLAN `DEC-3`,
//! `DEC-8`, `DEC-10`, `DEC-12`).
//!
//! | Module | Task | What |
//! |---|---|---|
//! | [`config`], [`network`] | `T-009-01` | configuration; chain id, pinned block and finalized head checked on both providers at start-up |
//! | [`rpc`], [`pair`] | `T-009-01` | one provider; two, cross-checked (`DEC-12`) |
//! | [`observe`] | `T-009-02`, `T-009-03`, `T-009-05` | the finalized-range follower, gaps; arrivals, conversions and payout legs at the vault |
//! | [`submit`] | `T-009-04` | payout preparation, decode-and-compare, signing, persisted bytes, the nonce lane |
//! | [`proof`] | `T-009-06` | non-execution proofs, cross-checked; unknown for unidentified consumers |
//! | [`reserve`] | issue #42 | the vault's balance of each admitted token and convertible at each recorded final head, from both providers (`REQ-BK-8`, `DEC-12`) |
//! | [`evm`] | | the delivery call, EIP-1559 payloads, the vault's events |
//! | [`signal`], [`store`] | | compromise and anomaly signals; store helpers |

pub mod config;
pub mod evm;
pub mod network;
pub mod observe;
pub mod pair;
pub mod proof;
pub mod reserve;
pub mod rpc;
pub mod signal;
pub mod store;
pub mod submit;
