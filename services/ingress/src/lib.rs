// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! # bezent-ingress
//!
//! The request surface (`CTR-REQ`, SPEC §7.A) and the query surface (`CTR-QRY`, §7.B) over HTTP
//! and JSON, on the store (`F-001`), as PLAN `DEC-3` and `DEC-15` shape them (`F-002`, M-0 part).
//!
//! - Every request is authenticated by a scoped service credential and checked against the
//!   requester's grant for its kind (`REQ-PN-1`, `REQ-PN-2`).
//! - Every request is taken in durably, with its answer, before it is answered (`REQ-RQ-1`). A
//!   repeat of a request reference with the same body returns the original's status, and with a
//!   different body is refused with `ERR-ReferenceConflict` (`REQ-ID-1`). References are scoped to
//!   their requester (`REQ-ID-4`, `DEC-15`).
//! - No request carries an outcome: a coordinate is a pointer, and the amount, the token and the
//!   finality are read from what the endpoints observed, never from the request (`REQ-RQ-7`).
//! - Route limits refuse with their named error before any value moves (`REQ-LM-2`); when bezent
//!   cannot keep up, `ERR-Overloaded`, retryable, and nothing is recorded (`REQ-LM-4`).
//! - Queries: status by request reference, bridge reference and coordinate; the route table; reserve
//!   per (asset, network); the reserve equation's latest evaluation; the sequenced log from a
//!   position (`REQ-RQ-8`, `REQ-ST-4`).
//!
//! - Webhooks (`REQ-ST-5`, `REQ-IC-5`): signed, at-least-once notifications that point at the
//!   sequenced log, which stays the source of truth ([`webhooks`]).
//!
//! The ingress moves no transfer past `Received`, `Refused` or `Accepted`, and verifies nothing
//! against a chain: that is the core's (`F-003`) and the endpoints'.

mod address;
mod api;
mod app;
mod auth;
pub mod config;
mod error;
mod intake;
pub mod openapi;
mod query;
mod refs;
pub mod webhooks;

pub use app::{App, Limits, connect, router, run, serve};
pub use config::Config;
