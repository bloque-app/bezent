// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Answers that are not a recorded request: refusals made before a request is taken in, and
//! failures. A refusal recorded with a request (a route limit, a destination, a source) is a
//! status, not one of these (`api::TransferStatus`).
//!
//! Nothing secret reaches a body or a log line: no credential, no store URL, no query text.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

/// A refusal before intake, or a failure.
#[derive(Debug)]
pub enum ApiError {
    /// No credential, or one that does not authenticate (`REQ-PN-1`). Answered with
    /// `ERR-NotAuthorised`: a caller that cannot be authenticated holds no grant.
    Unauthenticated,
    /// The requester holds no grant for this kind of request (`REQ-PN-2`, `ERR-NotAuthorised`).
    NotAuthorised,
    /// The body or the query does not parse, or carries a field the surface does not define, such
    /// as a claimed outcome (`REQ-RQ-7`), or lacks a field its kind requires (`ERR-Malformed`).
    Malformed(String),
    /// The request reference was used before with a different body (`REQ-ID-1`).
    ReferenceConflict,
    /// bezent cannot take more work within budget; retryable, and nothing was recorded
    /// (`REQ-LM-4`).
    Overloaded,
    /// The request names an arrival bezent observed that is not final yet. Nothing was
    /// recorded, and the request reference is not consumed: the same request, sent again once the
    /// arrival is final, is taken in (`REQ-RQ-3`, `REQ-RQ-6`, `REQ-IN-2`).
    NotFinal,
    /// Nothing this requester may read has that key (`REQ-ID-4`).
    NotFound,
    /// This ingress runs no webhooks (no `[webhooks]` in its configuration).
    WebhooksDisabled,
    /// A failure inside bezent, before anything was recorded. Nothing was taken in.
    Internal,
    /// The store did not confirm the commit, so whether the request was taken in is unknown.
    /// Repeating the same request is safe: a repeat answers with the original (`REQ-ID-1`).
    OutcomeUnknown,
    /// The request was taken in, but its status could not be read back to answer it
    /// (`REQ-RQ-1`). It reads by its request reference (`REQ-RQ-8`).
    TakenIn,
}

/// The body of every answer that is not a status.
#[derive(Serialize, ToSchema)]
pub struct ErrorBody {
    /// The named error of SPEC §10 (`ERR-*`), or `not-final`, `not-found`, `internal`,
    /// `outcome-unknown`, `taken-in` or `webhooks-disabled`.
    pub error: &'static str,
    /// A human-readable explanation. Never carries a secret.
    pub detail: String,
}

impl ApiError {
    fn parts(&self) -> (StatusCode, &'static str, String) {
        match self {
            Self::Unauthenticated => (
                StatusCode::UNAUTHORIZED,
                "ERR-NotAuthorised",
                "the requester could not be authenticated (REQ-PN-1)".into(),
            ),
            Self::NotAuthorised => (
                StatusCode::FORBIDDEN,
                "ERR-NotAuthorised",
                "the requester holds no grant for this kind of request (REQ-PN-2)".into(),
            ),
            Self::Malformed(why) => (StatusCode::BAD_REQUEST, "ERR-Malformed", why.clone()),
            Self::ReferenceConflict => (
                StatusCode::CONFLICT,
                "ERR-ReferenceConflict",
                "the request reference was used before with a different body (REQ-ID-1)".into(),
            ),
            Self::Overloaded => (
                StatusCode::SERVICE_UNAVAILABLE,
                "ERR-Overloaded",
                "bezent cannot take more work within budget; retry (REQ-LM-4)".into(),
            ),
            Self::NotFinal => (
                StatusCode::SERVICE_UNAVAILABLE,
                "not-final",
                "the coordinate holds an arrival bezent observed that is not final yet; nothing \
                 was recorded: send the same request again once it is final (REQ-RQ-3, REQ-IN-2)"
                    .into(),
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                "not-found",
                "nothing of this requester has that key".into(),
            ),
            Self::WebhooksDisabled => (
                StatusCode::SERVICE_UNAVAILABLE,
                "webhooks-disabled",
                "this ingress sends no webhooks; read the sequenced log (REQ-ST-4)".into(),
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "bezent failed to answer; nothing was taken in".into(),
            ),
            Self::OutcomeUnknown => (
                StatusCode::SERVICE_UNAVAILABLE,
                "outcome-unknown",
                "the store did not confirm the commit; repeat the same request, and a repeat \
                 answers with the original (REQ-ID-1)"
                    .into(),
            ),
            Self::TakenIn => (
                StatusCode::ACCEPTED,
                "taken-in",
                "the request is taken in; read its status by its request reference (REQ-RQ-8)"
                    .into(),
            ),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error, detail) = self.parts();
        let mut response = (status, Json(ErrorBody { error, detail })).into_response();
        if matches!(self, Self::Overloaded | Self::OutcomeUnknown) {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        }
        if matches!(self, Self::NotFinal) {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("10"));
        }
        response
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            // The store cannot take more now: refuse rather than accept what it cannot hold
            // (REQ-LM-4, DEC-4).
            sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::Io(_) => {
                tracing::warn!(error = %e, "store unavailable; answering ERR-Overloaded");
                Self::Overloaded
            }
            other => {
                tracing::error!(error = %other, "store error");
                Self::Internal
            }
        }
    }
}
