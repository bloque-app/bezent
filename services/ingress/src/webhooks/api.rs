// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The webhook surface (`REQ-ST-5`, `REQ-IC-5`, `REQ-ID-4`): registering callback URLs, obtaining
//! the signing secret, and reading one's deliveries.

use super::{random, secret_aad, url_aad};
use crate::app::{App, Body, Params};
use crate::auth::Requester;
use crate::error::{ApiError, ErrorBody};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// A callback URL to register.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CallbackRegistration {
    /// An absolute `https` URL, without user information or fragment, at most 2048 bytes, whose
    /// host is or resolves only to global addresses: a loopback, private, link-local or other
    /// internal address is refused here when written as one, and when the host resolves to one
    /// at send time. It is
    /// sealed at rest and never logged, so it may carry the requester's own access token.
    pub url: String,
}

/// A transfer's callback, as registered.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct TransferCallback {
    /// The transfer's bridge reference.
    pub bridge_reference: String,
    /// The requester's own reference for it.
    pub request_reference: String,
    /// When the URL now in use was registered.
    pub registered_at: DateTime<Utc>,
}

/// The condition URL, as registered.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct ConditionCallback {
    /// When this URL was registered; it replaces any earlier one.
    pub registered_at: DateTime<Utc>,
}

/// A new signing secret. It is shown once; bezent keeps it sealed.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct WebhookSecret {
    /// 32 random bytes, hexadecimal. Verify a delivery by first rejecting a `bezent-timestamp`
    /// outside a bound of your clock (for example 5 minutes either way: replay protection), then
    /// computing HMAC-SHA256 under these bytes of `<bezent-timestamp header>.<raw body>` and
    /// comparing `v1=<hex>` with the `bezent-signature` header in constant time, then
    /// de-duplicating by `bezent-dedupe-key` (kept for longer than the bound).
    pub secret: String,
    /// From when deliveries are signed with it, retries included; the secret before it is no
    /// longer used. Accept either until your receiver holds this one: a delivery rejected
    /// meanwhile is retried, and counts toward the dead-letter bound.
    pub created_at: DateTime<Utc>,
}

/// One delivery of a notification.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct DeliveryOut {
    /// The delivery's id; also the `after` of the next page.
    pub id: i64,
    /// The log position it points at.
    pub seq: i64,
    /// Its stable de-duplication key (`REQ-ST-5`).
    pub dedupe_key: String,
    /// `transfer` (a transfer's callback URL) or `condition` (the condition URL).
    pub target: String,
    /// `pending`, `delivered` or `dead` (dead-lettered after the configured attempts).
    pub state: String,
    /// Attempts made.
    pub attempts: i32,
    /// The HTTP status of the last attempt, if one answered.
    pub last_status: Option<i32>,
    /// Why the last attempt failed, if it did, as a coarse class only: `no-secret`,
    /// `url-unopenable`, `url-not-allowed`, `address-not-allowed` (also a host that does not resolve), `timeout`,
    /// `connect-failed`, `http-status` (with `last_status`), `request-failed` or
    /// `notice-refused`. Never an address, an error text or a response body.
    pub last_error: Option<String>,
    /// When it was appended.
    pub created_at: DateTime<Utc>,
    /// When the next attempt is due, while pending.
    pub next_attempt_at: DateTime<Utc>,
    /// When it ended delivered or dead.
    pub finished_at: Option<DateTime<Utc>>,
}

/// A page of deliveries.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct DeliveryList {
    /// The deliveries, by id.
    pub deliveries: Vec<DeliveryOut>,
    /// The `after` of the next page, when this one was full.
    pub next_after: Option<i64>,
}

/// A read of one's deliveries.
#[derive(Clone, Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct DeliveryQuery {
    /// Only deliveries in this state: `pending`, `delivered` or `dead`.
    pub state: Option<String>,
    /// Deliveries with an id after this one.
    #[serde(default)]
    pub after: i64,
    /// The most deliveries returned, at most 500.
    pub limit: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct DeliveryRow {
    id: i64,
    seq: i64,
    dedupe_key: String,
    target: String,
    state: String,
    attempts: i32,
    last_status: Option<i32>,
    last_error: Option<String>,
    created_at: DateTime<Utc>,
    next_attempt_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
}

/// Sets the callback URL of a transfer, by request reference (`REQ-ST-5`, `REQ-ID-4`).
#[utoipa::path(
    put, path = "/v0/webhooks/transfers/by-request/{reference}", tag = "webhooks",
    params(("reference" = String, Path, description = "The requester's request reference")),
    request_body = CallbackRegistration,
    security(("bearer" = [])),
    responses(
        (status = 201, description = "Registered", body = TransferCallback),
        (status = 200, description = "The same URL again, or a new URL that replaced the earlier one (each change is recorded)", body = TransferCallback),
        (status = 400, body = ErrorBody), (status = 401, body = ErrorBody),
        (status = 404, description = "No transfer of this requester has that reference (a request refused before any transfer was made has nothing to notify)", body = ErrorBody),
        (status = 503, description = "webhooks-disabled", body = ErrorBody),
    )
)]
pub(crate) async fn register_by_request(
    State(app): State<App>,
    who: Requester,
    Path(reference): Path<String>,
    Body(body): Body<CallbackRegistration>,
) -> Result<(StatusCode, Json<TransferCallback>), ApiError> {
    let hooks = app.webhooks()?;
    let url = hooks.check_url(&body.url)?;
    let found: Option<(i64, String, String)> = sqlx::query_as(
        "SELECT t.id, t.bridge_reference, i.request_reference \
         FROM intake i JOIN intake_answers a ON a.intake_id = i.id \
         JOIN transfers t ON t.id = a.transfer_id \
         WHERE i.requester = $1 AND i.request_reference = $2",
    )
    .bind(&who.name)
    .bind(&reference)
    .fetch_optional(app.pool())
    .await?;
    let (id, bridge, request) = found.ok_or(ApiError::NotFound)?;
    register_transfer(&app, &who, id, bridge, request, &url).await
}

/// Sets the callback URL of a transfer, by bridge reference (`REQ-ST-5`, `REQ-ID-4`).
#[utoipa::path(
    put, path = "/v0/webhooks/transfers/by-bridge/{reference}", tag = "webhooks",
    params(("reference" = String, Path, description = "The bridge reference")),
    request_body = CallbackRegistration,
    security(("bearer" = [])),
    responses(
        (status = 201, description = "Registered", body = TransferCallback),
        (status = 200, description = "The same URL again, or a new URL that replaced the earlier one (each change is recorded)", body = TransferCallback),
        (status = 400, body = ErrorBody), (status = 401, body = ErrorBody),
        (status = 404, description = "No transfer this requester's request made or reached has that reference: knowing a reference is not enough (REQ-ID-4)", body = ErrorBody),
        (status = 503, description = "webhooks-disabled", body = ErrorBody),
    )
)]
pub(crate) async fn register_by_bridge(
    State(app): State<App>,
    who: Requester,
    Path(reference): Path<String>,
    Body(body): Body<CallbackRegistration>,
) -> Result<(StatusCode, Json<TransferCallback>), ApiError> {
    let hooks = app.webhooks()?;
    let url = hooks.check_url(&body.url)?;
    let found: Option<(i64, String, String)> = sqlx::query_as(
        "SELECT t.id, t.bridge_reference, i.request_reference \
         FROM transfers t JOIN intake_answers a ON a.transfer_id = t.id \
         JOIN intake i ON i.id = a.intake_id \
         WHERE t.bridge_reference = $2 AND i.requester = $1",
    )
    .bind(&who.name)
    .bind(&reference)
    .fetch_optional(app.pool())
    .await?;
    let (id, bridge, request) = found.ok_or(ApiError::NotFound)?;
    register_transfer(&app, &who, id, bridge, request, &url).await
}

async fn register_transfer(
    app: &App,
    who: &Requester,
    transfer: i64,
    bridge_reference: String,
    request_reference: String,
    url: &str,
) -> Result<(StatusCode, Json<TransferCallback>), ApiError> {
    let hooks = app.webhooks()?;
    let aad = url_aad(&who.name);
    let digest: Vec<u8> = hooks.key.url_digest(&who.name, url).to_vec();
    let mut tx = app.pool().begin().await?;
    // One change at a time per (requester, transfer), so two equal PUTs record one change.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("bezent/webhook-url/{}/{transfer}", who.name))
        .execute(&mut *tx)
        .await?;
    let latest: Option<(Vec<u8>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT url_digest, registered_at FROM webhook_transfer_urls \
         WHERE requester = $1 AND transfer_id = $2 ORDER BY id DESC LIMIT 1",
    )
    .bind(&who.name)
    .bind(transfer)
    .fetch_optional(&mut *tx)
    .await?;
    let (status, registered_at) = match latest {
        Some((current, at)) if current == digest => (StatusCode::OK, at),
        previous => {
            let (nonce, sealed) = hooks.key.seal(&aad, url.as_bytes());
            let at: DateTime<Utc> = sqlx::query_scalar(
                "INSERT INTO webhook_transfer_urls \
                   (requester, transfer_id, url_nonce, url_sealed, url_digest, previous_digest, \
                    after_seq) \
                 SELECT $1, $2, $3, $4, $5, $6, seq FROM event_log_head \
                 RETURNING registered_at",
            )
            .bind(&who.name)
            .bind(transfer)
            .bind(nonce.as_slice())
            .bind(&sealed)
            .bind(&digest)
            .bind(previous.as_ref().map(|(d, _)| d.as_slice()))
            .fetch_one(&mut *tx)
            .await?;
            let status = if previous.is_some() {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            (status, at)
        }
    };
    tx.commit().await?;
    Ok((
        status,
        Json(TransferCallback {
            bridge_reference,
            request_reference,
            registered_at,
        }),
    ))
}

/// Registers the URL conditions are notified to (`REQ-IC-5`). It replaces any earlier one.
#[utoipa::path(
    put, path = "/v0/webhooks/conditions", tag = "webhooks",
    request_body = CallbackRegistration,
    security(("bearer" = [])),
    responses(
        (status = 200, description = "Registered", body = ConditionCallback),
        (status = 400, body = ErrorBody), (status = 401, body = ErrorBody),
        (status = 503, description = "webhooks-disabled", body = ErrorBody),
    )
)]
pub(crate) async fn register_conditions(
    State(app): State<App>,
    who: Requester,
    Body(body): Body<CallbackRegistration>,
) -> Result<Json<ConditionCallback>, ApiError> {
    let hooks = app.webhooks()?;
    let url = hooks.check_url(&body.url)?;
    let (nonce, sealed) = hooks.key.seal(&url_aad(&who.name), url.as_bytes());
    let registered_at: DateTime<Utc> = sqlx::query_scalar(
        "INSERT INTO webhook_condition_urls (requester, url_nonce, url_sealed) \
         VALUES ($1, $2, $3) RETURNING registered_at",
    )
    .bind(&who.name)
    .bind(nonce.as_slice())
    .bind(&sealed)
    .fetch_one(app.pool())
    .await?;
    Ok(Json(ConditionCallback { registered_at }))
}

/// Makes a new signing secret and returns it, once (`REQ-ST-5`). Every later delivery, retries
/// included, is signed with it.
#[utoipa::path(
    post, path = "/v0/webhooks/secret", tag = "webhooks",
    security(("bearer" = [])),
    responses(
        (status = 201, description = "The new secret; shown once", body = WebhookSecret),
        (status = 401, body = ErrorBody),
        (status = 503, description = "webhooks-disabled", body = ErrorBody),
    )
)]
pub(crate) async fn rotate_secret(
    State(app): State<App>,
    who: Requester,
) -> Result<(StatusCode, Json<WebhookSecret>), ApiError> {
    let hooks = app.webhooks()?;
    let secret: [u8; 32] = random();
    let (nonce, sealed) = hooks.key.seal(&secret_aad(&who.name), &secret);
    let created_at: DateTime<Utc> = sqlx::query_scalar(
        "INSERT INTO webhook_secrets (requester, nonce, sealed) VALUES ($1, $2, $3) \
         RETURNING created_at",
    )
    .bind(&who.name)
    .bind(nonce.as_slice())
    .bind(&sealed)
    .fetch_one(app.pool())
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(WebhookSecret {
            secret: hex::encode(secret),
            created_at,
        }),
    ))
}

/// This requester's deliveries, by id: what is pending, delivered or dead-lettered.
#[utoipa::path(
    get, path = "/v0/webhooks/deliveries", tag = "webhooks",
    params(DeliveryQuery),
    security(("bearer" = [])),
    responses(
        (status = 200, body = DeliveryList),
        (status = 400, body = ErrorBody), (status = 401, body = ErrorBody),
        (status = 503, description = "webhooks-disabled", body = ErrorBody),
    )
)]
pub(crate) async fn deliveries(
    State(app): State<App>,
    who: Requester,
    Params(q): Params<DeliveryQuery>,
) -> Result<Json<DeliveryList>, ApiError> {
    app.webhooks()?;
    if q.state
        .as_deref()
        .is_some_and(|s| !["pending", "delivered", "dead"].contains(&s))
    {
        return Err(ApiError::Malformed(
            "state is pending, delivered or dead".into(),
        ));
    }
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let rows: Vec<DeliveryRow> = sqlx::query_as(
        "SELECT id, seq, dedupe_key, target, state, attempts, last_status, last_error, \
                created_at, next_attempt_at, finished_at \
         FROM webhook_deliveries \
         WHERE requester = $1 AND ($2::text IS NULL OR state = $2) AND id > $3 \
         ORDER BY id LIMIT $4",
    )
    .bind(&who.name)
    .bind(q.state.as_deref())
    .bind(q.after)
    .bind(limit)
    .fetch_all(app.pool())
    .await?;
    let full = i64::try_from(rows.len()).unwrap_or(i64::MAX) == limit;
    let deliveries: Vec<DeliveryOut> = rows
        .into_iter()
        .map(|r| DeliveryOut {
            id: r.id,
            seq: r.seq,
            dedupe_key: r.dedupe_key,
            target: r.target,
            state: r.state,
            attempts: r.attempts,
            last_status: r.last_status,
            last_error: r.last_error,
            created_at: r.created_at,
            next_attempt_at: r.next_attempt_at,
            finished_at: r.finished_at,
        })
        .collect();
    let next_after = if full {
        deliveries.last().map(|d| d.id)
    } else {
        None
    };
    Ok(Json(DeliveryList {
        deliveries,
        next_after,
    }))
}
