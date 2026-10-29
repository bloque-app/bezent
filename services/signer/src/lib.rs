// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! # bezent-signer
//!
//! The signer of PLAN `DEC-3` (`F-005`, the `M-0` stub): a process apart from the one that holds
//! bezent's state, that holds no state of its own and signs only intents the core recorded
//! (`REQ-AR-3`, `INV-9`).
//!
//! - [`link`]: the core sends an intent id over a Unix socket; the caller is authenticated by its
//!   peer user id and a token, the signer by its own user id (`T-005-02`).
//! - [`engine`]: the one interface every signing engine implements ([`engine::SigningEngine`]):
//!   a key's scheme, public key and address, a signature over a prepared message or digest, and a
//!   health check. The engines: [`keys`] (the file of test keys, `T-005-03`), [`vault`] (the other
//!   keys, read from Vault KV), [`kms`] (AWS KMS), [`transit`] (Vault Transit: ed25519 Kreivo
//!   accounts) and [`vault_plugin`] (a Vault signing plugin: the Polygon key); [`custody`] joins
//!   the ones the configuration names. sr25519, ECDSA and ed25519 for Kreivo accounts, secp256k1
//!   for EVM networks.
//! - [`signer`]: reads the recorded intent through the `bezent_signer` store role, applies the
//!   guards (`DEC-3`, a second line), signs, and records the signature append-only (`T-005-04`,
//!   `T-005-05`). A restart returns the recorded signature and signs nothing new (`AC-D2.1`).
//! - [`delivery`]: the formats of what the signer signs, decoded strictly, so it compares the exact
//!   bytes with the core's order (the audit of PR #56, must-fix 2; issue #23), and encoded, for
//!   the tests and the sidecar's stand-in.
//! - [`compromise`]: run by the core, not the signer: a signature seen on chain, or returned by the
//!   signer, that does not verify over a recorded intent's bytes starts the compromise path
//!   (`AC-I1.2`, `INV-9`). [`verify`] holds the checks.
//! - [`prepare`]: the `prepare` subcommand of a signer that runs beside its one caller (a sidecar):
//!   the caller's token, the signer's configuration and the socket directory, each given to the
//!   user who reads it.
//! - [`process`]: the signer forbids its own core dumps at start (`REQ-CU-1`).
//!
//! No key material appears in any log line, error or configuration dump (`REQ-KY-1`): keys are
//! read from a file the configuration names, never from the environment, and every `Debug` of a
//! key holder prints key ids and public keys only.

pub mod cancel;
pub mod compromise;
pub mod config;
pub mod custody;
pub mod delivery;
pub mod engine;
pub mod health;
pub mod intent;
pub mod keys;
pub mod kms;
pub mod link;
pub mod log;
pub mod prepare;
pub mod process;
pub mod signer;
pub mod transit;
pub mod vault;
pub mod vault_plugin;
pub mod verify;

pub use signer::{Outcome, Refusal, Signed, Signer};
