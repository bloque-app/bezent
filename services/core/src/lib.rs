// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! # bezent-core-service
//!
//! The core of PLAN `DEC-3` (`F-003`): the one process that decides. It runs the state machine of
//! SPEC §5.4 over the store, the named conditions of §10, deadlines and review (§5.9), and records
//! the intents the signer signs. The endpoints only observe and the signer only signs what the
//! core recorded; nothing a requester says is taken as an outcome (`REQ-AR-1`, `REQ-AR-2`,
//! `REQ-RQ-7`).
//!
//! - [`engine`]: [`Core::cycle`], the state machine, deliveries in two steps (order, then signing
//!   intent over the endpoint's prepared bytes), verification of every returned signature
//!   (`AC-I1.2`), conditions and review.
//! - [`store`]: what the core reads, and the transition it appends (`REQ-TR-6`).
//! - [`refs`]: the references the core derives (`REQ-ID-3`, `REQ-IN-4`, `REQ-RF-3`).
//! - [`cutoff`]: route cut-offs (`CND-CutOff`, amendment 0010 A2).
//! - [`reserve`] (`F-004`): the reserve book, the one-minute reserve check and the daily signed
//!   statement.
//! - [`config`]: the configuration file.

pub mod config;
pub mod cutoff;
pub mod engine;
pub mod refs;
pub mod reserve;
pub mod store;

pub use config::Config;
pub use engine::{Core, Report, Settings};
