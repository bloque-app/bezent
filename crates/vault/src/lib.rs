// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! bezent's Vault client (issue #64; the Fargate design's PUB-1a).
//!
//! The services read their secrets from Vault themselves, so a distroless image needs no shell and
//! no secret on disk or in the environment (`REQ-KY-1`):
//!
//! - [`Client::login`] logs in with the AWS auth method under the task role's credentials: it signs
//!   `sts:GetCallerIdentity` with AWS Signature Version 4, the server-ID header among the signed
//!   headers, and posts it to `auth/<mount>/login`.
//! - [`Session`] reads KV secrets, version 1 or 2, with field selection, and issues certificates
//!   from a PKI engine, checking what came back ([`pki`]), and asks a Transit engine to sign with an
//!   ed25519 key that stays in Vault ([`transit`]).
//! - [`SecretRef`] is the configuration type for a secret: `{ file = "<path>" }` or
//!   `{ vault = { path, field } }`, with the `[vault]` table ([`VaultConfig`]) that says where Vault
//!   is. [`Resolver`] turns one into its value, and [`StoreSource`] builds the store's URL from a
//!   login and a password in the process.
//!
//! The roles the services log in to have tokens that live for a minute, so none is kept beyond a
//! burst: the reads of a start share one login (held for at most 20 seconds), and the next burst
//! logs in again. Vault namespaces are not supported. Tokens and values are
//! [`zeroize::Zeroizing`], plain HTTP is refused off loopback, no redirect is followed, and no
//! error carries a value ([`Error`]).

mod address;
mod client;
mod config;
mod error;
mod kv;
mod login;
pub mod pki;
mod resolve;
#[cfg(test)]
mod tests;
pub mod transit;

pub use address::loopback;
pub use client::{Client, Session};
pub use config::{SecretRef, StoreSource, VaultConfig, VaultField};
pub use error::Error;
pub use pki::{IssueRequest, Issued, Usage};
pub use resolve::{Resolver, read_file};
pub use transit::TransitKey;
pub use zeroize::Zeroizing;
