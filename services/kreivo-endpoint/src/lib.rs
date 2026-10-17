// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! # bezent-kreivo-endpoint
//!
//! The endpoint of the issuing network (`F-007`; PLAN `DEC-3`, `DEC-5`): Kreivo through subxt and
//! its embedded smoldot light client. It reports what it observes at coordinates and writes the
//! exact bytes it signs; it **decides nothing** and writes no transfer state (`REQ-AR-2`). Kreivo's
//! RPC is never trusted for an outcome.
//!
//! | Module | Task | What |
//! |---|---|---|
//! | [`network`] | `T-007-01` | The declared network, the pinned chain spec and the chain agree on the genesis, or the start fails closed (`AC-I4.1`) |
//! | [`observe`] | `T-007-02` | The finalized head followed in contiguous ranges; the contract's events as observations by coordinate; gaps visible and filled (`REQ-OB-*`, `AC-D2.3`, `INV-10`) |
//! | [`submit`] | `T-007-03` | Mints and refund mints: era born at the finalized head, dry run, recorded bytes, the signer link and its check, persisted before submission, one declared lane (`REQ-EO-1`, `REQ-EO-4`, `REQ-HN-3`, `AC-I1.2`) |
//! | [`proof`] | `T-007-04` | Inclusion, refusal and non-execution, from chain data anyone can check (`REQ-NX-1`, `REQ-NX-2`, `AC-C2.1`) |
//! | [`supply`] | issue #42 | Each bridged asset's supply at each recorded final head, the component's count and the issuance, for the reserve check (`REQ-BK-8`, `NFR-5`) |
//! | [`signal`] | | Compromise and anomaly signals, written to the store once and never dropped (`AC-I1.2`) |
//! | [`kreivo`] | | Kreivo's and the contract's encoding |
//! | [`chain`] | | The chain interface; an in-memory chain for tests; the light client (feature `light-client`) |

pub mod chain;
pub mod config;
pub mod kreivo;
pub mod network;
pub mod observe;
pub mod proof;
pub mod signal;
pub mod submit;
pub mod supply;
