// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! # bezent-sidecar
//!
//! bezent for other repositories' end-to-end harnesses, **TEST only**: the ingress, the core's
//! cycle and a test signer in one process (`main.rs`), and the chain-event driver through which a
//! harness makes the test networks do things ([`driver`], PLAN `DEC-19`).
//!
//! - [`setup`]: the test-network rule, the declarations, the logins, the test signer's
//!   configuration.
//! - [`driver`]: the chain-event driver; bound only in test mode, behind its own token.

pub mod driver;
pub mod setup;
