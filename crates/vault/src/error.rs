// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! The errors of the Vault client.
//!
//! No error carries a secret, a token, a signature, a response body or a URL (`REQ-KY-1`): a
//! variant holds a fixed sentence, a status code, or the name of a path or field (names are
//! configuration, not secrets).

use std::fmt;

/// Why a Vault operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The configuration is refused (an address that is not HTTPS, a path that is not a path, a
    /// `vault` secret with no `[vault]` table).
    Config(String),
    /// No AWS credentials could be obtained, or the login request could not be signed.
    Credentials(&'static str),
    /// Vault could not be reached, or did not answer in time.
    Unreachable(&'static str),
    /// Vault answered with a status that is not a success.
    Status {
        /// What was being asked: `login`, `read` or `issue`.
        operation: &'static str,
        /// The HTTP status.
        status: u16,
    },
    /// Vault's answer is not what the client asked for.
    Malformed(&'static str),
    /// The secret at `path` has no string field `field`.
    NoField {
        /// The secret's path.
        path: String,
        /// The field that was asked for.
        field: String,
    },
    /// A secret is the empty string (after trimming, for a file): a token, key, URL, login or
    /// password that is empty is a mistake to stop at, never a value to use. Names the secret, never
    /// a value.
    Empty {
        /// The Vault `path` and field, the file, or the store part.
        what: String,
    },
    /// A secret file could not be read.
    File {
        /// The file.
        path: String,
        /// The kind of the I/O error.
        kind: std::io::ErrorKind,
    },
    /// What Vault issued does not pass the checks of [`crate::pki`].
    Pki(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Config(why) => write!(f, "Vault configuration refused: {why}"),
            Error::Credentials(why) => write!(f, "Vault login: {why}"),
            Error::Unreachable(why) => write!(f, "Vault is unreachable ({why})"),
            Error::Status { operation, status } => {
                write!(f, "Vault answered {status} for the {operation}")
            }
            Error::Malformed(why) => write!(f, "Vault's answer is not usable: {why}"),
            Error::NoField { path, field } => {
                write!(f, "the Vault secret {path} has no string field {field}")
            }
            Error::Empty { what } => write!(f, "the secret {what} is empty"),
            Error::File { path, kind } => write!(f, "cannot read the secret file {path}: {kind}"),
            Error::Pki(why) => write!(f, "Vault issued an unusable certificate: {why}"),
        }
    }
}

impl std::error::Error for Error {}
