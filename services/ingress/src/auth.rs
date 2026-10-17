// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Requester authentication and grants (`REQ-PN-1`, `REQ-PN-2`; PLAN `DEC-15`).
//!
//! A credential is `<credential id>.<secret>`, sent as `Authorization: Bearer …`. The store keeps
//! only the secret's SHA-256 (`requester_credentials`), compared in constant time. A secret is
//! long and random, so a fast hash is enough. Nothing about a credential is logged.

use crate::app::App;
use crate::error::ApiError;
use crate::refs::same_digest;
use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use sha2::{Digest, Sha256};

/// A kind of request a requester may be granted (`REQ-PN-2`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grant {
    /// Request deposits (`REQ-RQ-3`).
    Deposit,
    /// Add a withdrawal's extras (`REQ-RQ-4`).
    Withdrawal,
    /// Request the refund of an unassociated arrival (`REQ-RQ-6`).
    Refund,
    /// Attest for an attested backend, which includes adding its redemptions' extras
    /// (`REQ-RQ-4`).
    Attest,
}

impl Grant {
    fn as_str(self) -> &'static str {
        match self {
            Self::Deposit => "deposit",
            Self::Withdrawal => "withdrawal",
            Self::Refund => "refund",
            Self::Attest => "attest",
        }
    }
}

/// An authenticated requester and its grants.
#[derive(Clone, Debug)]
pub struct Requester {
    /// Its name; every request records it (`REQ-PN-1`).
    pub name: String,
    grants: Vec<String>,
}

impl Requester {
    /// Refuses with `ERR-NotAuthorised` unless the requester holds `grant`.
    pub fn require(&self, grant: Grant) -> Result<(), ApiError> {
        if self.grants.iter().any(|g| g == grant.as_str()) {
            Ok(())
        } else {
            Err(ApiError::NotAuthorised)
        }
    }
}

impl FromRequestParts<App> for Requester {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &App) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(ApiError::Unauthenticated)?;
        let (id, secret) = token.split_once('.').ok_or(ApiError::Unauthenticated)?;
        if id.is_empty() || secret.is_empty() {
            return Err(ApiError::Unauthenticated);
        }

        let row: Option<(String, Vec<u8>, Vec<String>)> = sqlx::query_as(
            "SELECT c.requester, c.secret_sha256, \
                    COALESCE(array_agg(g.grant_kind) FILTER (WHERE g.grant_kind IS NOT NULL), '{}') \
             FROM requester_credentials c \
             LEFT JOIN requester_grants g ON g.requester = c.requester \
             WHERE c.credential_id = $1 \
             GROUP BY c.requester, c.secret_sha256",
        )
        .bind(id)
        .fetch_optional(app.pool())
        .await?;

        let presented = Sha256::digest(secret.as_bytes());
        match row {
            Some((name, stored, grants)) if same_digest(&stored, &presented) => {
                Ok(Requester { name, grants })
            }
            _ => Err(ApiError::Unauthenticated),
        }
    }
}
