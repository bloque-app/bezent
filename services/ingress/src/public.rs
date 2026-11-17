// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The public status route (F-018, T-018-10; `REQ-ST-8`, `REQ-ID-4`, `REQ-ST-1`, `REQ-ST-3`;
//! amendment 0029, Open 8, ruled yes, `d-0029-status`).
//!
//! Anyone may read, with no credential, the state, the named conditions and the coordinates of a
//! **directed deposit**, by its arrival's coordinate, and of a **withdrawal**, by its redemption's
//! on-chain reference (the redeemer and the reference it chose). Nothing else:
//!
//! - It runs on a listener of its own (`[public] listen`), apart from the authenticated surfaces, so
//!   the deployment exposes it alone, behind its rate-limiting front (Cloudflare, amendment 0029), and
//!   never the request surface by mistake.
//! - It reads the store as a login of its own, a member of `bezent_public` and of nothing else
//!   (`[public] database_url`), checked at start-up like every service's (#46). That role reads one
//!   view, `public_status` (migration 0146), and no table: the view has no column a requester
//!   supplied (no request reference, intake body, requester, callback or extra, `REQ-ST-8`), and no
//!   row a request made or reached but a withdrawal. A requester's deposit therefore answers "not
//!   found" by its coordinate, with exactly the body an unknown coordinate gets (`REQ-ID-4`): the
//!   route says nothing about what a requester asked for.
//! - Each answer is a handful of index reads: the coordinate or the redemption derives the bridge
//!   references (`bz-d-`, `bz-u-`; `bz-w-`, as the ingress and the core derive them), which the view
//!   finds by the unique index on `transfers.bridge_reference`. No query scans; a bounded number of
//!   requests are answered at once (`ERR-Overloaded` beyond), on a small pool with a short statement
//!   bound, so a flood costs bezent little, and the front limits it besides.
//! - The answer is a report, not a verdict (`REQ-ST-3`): the coordinates bezent holds on each
//!   network, and whether each is final as bezent observed it. A reader checks them on the networks
//!   (`REQ-ST-1`); the web app does (`apps/bridge/src/status/route.ts`, which reads exactly this
//!   shape).
//! - It is public data, so any origin may read it (`Access-Control-Allow-Origin: *`, no credentials),
//!   and it may be cached for a few seconds.
//! - What an unauthenticated caller can make it do is bounded before any work (review M1, L2 and L3
//!   of PR #139): a redeemer longer than an issuing-network account can be (49 base58 characters,
//!   or 66 as `0x` hex) and a reference longer than 66 are refused before they are decoded, so no
//!   query costs more than a few microseconds of parsing; a request URI is at most
//!   [`MAX_URI_BYTES`] and a request head at most [`MAX_HEAD_BYTES`] in [`MAX_HEADERS`] headers; a
//!   request is answered within `request_timeout_ms` (`ERR-Overloaded` beyond), a head must arrive
//!   within `header_read_timeout_ms` of its first byte, an idle connection is kept for
//!   `idle_timeout_ms`, one exchange (a request's first byte to its answer's last byte written)
//!   holds a connection for at most `connection_timeout_ms`, so a client that never reads its
//!   answers is dropped, and at most `max_http_connections` are held open, one more answered
//!   `ERR-Overloaded` and closed ([`serve`]; second fix round of PR #139); and `/health` takes a
//!   permit like a query. The origin should accept connections from the front
//!   (Cloudflare) only, by the network's rules: these bounds are the listener's own, not a rate
//!   limit.

use crate::api::{CoordinateQuery, parse_hex, to_hex};
use crate::config::PublicConfig;
use crate::refs;
use axum::Router;
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};

/// How long a cache in front may keep an answer, in seconds.
pub const CACHE_SECONDS: u32 = 5;

/// The longest redeemer the route reads in SS58: an issuing-network account (32 bytes) with a
/// prefix of one or two bytes and a checksum of two is at most 36 bytes, at most 49 base58
/// characters (review M1 of PR #139).
pub const MAX_SS58_CHARS: usize = 49;

/// `0x` and 64 hex digits: a 32-byte account or reference in hex.
pub const HEX32_CHARS: usize = 66;

/// The longest request URI the route reads, in bytes: its longest query is some 300.
pub const MAX_URI_BYTES: usize = 2048;

/// The longest request head the listener reads, in bytes: hyper's least buffer, and some 25 times
/// what a browser sends for these queries.
pub const MAX_HEAD_BYTES: usize = 8192;

/// The most headers the listener reads in one request.
pub const MAX_HEADERS: usize = 32;

/// The public route's state: its own pool, as `bezent_public`, and the bound on requests at once.
#[derive(Clone)]
pub struct PublicApp {
    pool: PgPool,
    permits: Arc<Semaphore>,
}

impl PublicApp {
    /// The route on `pool`, whose login must be a member of `bezent_public` only, answering at most
    /// `max_in_flight` requests at once.
    pub fn new(pool: PgPool, max_in_flight: usize) -> Self {
        Self {
            pool,
            permits: Arc::new(Semaphore::new(max_in_flight.max(1))),
        }
    }

    /// The store pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// For tests: takes one of the permits, so the route answers one fewer at once.
    #[doc(hidden)]
    pub fn hold_for_tests(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        Arc::clone(&self.permits).try_acquire_owned().ok()
    }
}

/// What the route answers when it does not answer a status.
#[derive(Debug)]
pub enum PublicError {
    /// The query does not parse (`ERR-Malformed`).
    Malformed(String),
    /// The request URI is longer than [`MAX_URI_BYTES`] (`ERR-Malformed`, 414).
    UriTooLong,
    /// No directed deposit or withdrawal has that key; or one a request made or reached
    /// (`REQ-ID-4`): the same answer.
    NotFound,
    /// More requests at once than the route answers, or the store cannot take more (`ERR-Overloaded`,
    /// `REQ-LM-4`); retryable.
    Overloaded,
    /// A failure inside bezent.
    Internal,
}

/// The body of every answer that is not a status.
#[derive(Serialize)]
struct PublicErrorBody {
    error: &'static str,
    detail: String,
}

impl IntoResponse for PublicError {
    fn into_response(self) -> Response {
        let (status, error, detail) = match self {
            Self::Malformed(why) => (StatusCode::BAD_REQUEST, "ERR-Malformed", why),
            Self::UriTooLong => (
                StatusCode::URI_TOO_LONG,
                "ERR-Malformed",
                format!("the request URI is longer than {MAX_URI_BYTES} bytes"),
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                "not-found",
                "bezent reports no directed deposit or withdrawal with that key".to_owned(),
            ),
            Self::Overloaded => (
                StatusCode::SERVICE_UNAVAILABLE,
                "ERR-Overloaded",
                "bezent cannot answer more now; retry (REQ-LM-4)".to_owned(),
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "bezent failed to answer".to_owned(),
            ),
        };
        let retry = matches!(status, StatusCode::SERVICE_UNAVAILABLE);
        let mut response = (status, axum::Json(PublicErrorBody { error, detail })).into_response();
        if retry {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        }
        response
    }
}

impl From<sqlx::Error> for PublicError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::Io(_) => {
                tracing::warn!(error = %e, "REQ-ST-8: the store is unavailable to the public route; answering ERR-Overloaded");
                Self::Overloaded
            }
            other => {
                tracing::error!(error = %other, "REQ-ST-8: the public route's query failed");
                Self::Internal
            }
        }
    }
}

impl From<crate::error::ApiError> for PublicError {
    fn from(e: crate::error::ApiError) -> Self {
        match e {
            crate::error::ApiError::Malformed(why) => Self::Malformed(why),
            _ => Self::Malformed("the query does not parse".to_owned()),
        }
    }
}

/// Query parameters whose refusal is a [`PublicError::Malformed`].
struct Params<T>(T);

impl<S: Send + Sync, T: serde::de::DeserializeOwned> FromRequestParts<S> for Params<T> {
    type Rejection = PublicError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Query::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Query(value)) => Ok(Self(value)),
            Err(rejection) => Err(PublicError::Malformed(rejection.body_text())),
        }
    }
}

/// A coordinate bezent holds for the transfer, on one network, and whether bezent observed it final.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicCoordinate {
    /// The network.
    pub network: String,
    /// What it is to the transfer: `arrival`, `mint`, `return`, `refund`, `redemption`, `payout` or
    /// `refund mint`.
    pub role: String,
    /// The block's number.
    pub block_number: i64,
    /// The block's hash.
    pub block_hash: String,
    /// The position in the block, where bezent holds one (the source's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_kind: Option<String>,
    /// The index of that position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_index: Option<i64>,
    /// Whether bezent observed it final (`REQ-TR-4`).
    pub r#final: bool,
}

/// One transfer of the answer: the directed deposit, its return or its refund; the withdrawal or
/// its refund mint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicTransfer {
    /// `deposit`, `return`, `refund` or `withdrawal`.
    pub role: String,
    /// bezent's bridge reference, derived from the coordinate or the redemption (`REQ-ID-3`).
    pub bridge_reference: String,
    /// The state (SPEC §5.4).
    pub state: String,
    /// The named condition it holds, if any (SPEC §10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

/// The answer: what `REQ-ST-8` lets the route say, and nothing else.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicStatus {
    /// `directed_deposit` or `withdrawal`.
    pub kind: String,
    /// The bridge reference of the deposit, the return or the withdrawal.
    pub bridge_reference: String,
    /// Its state.
    pub state: String,
    /// The named conditions its transfers hold now.
    pub conditions: Vec<String>,
    /// The coordinates bezent holds on each network, each once.
    pub coordinates: Vec<PublicCoordinate>,
    /// Its transfers: the first is the deposit, the return or the withdrawal; a refund follows.
    pub transfers: Vec<PublicTransfer>,
    /// When bezent last changed any of them.
    pub updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct Row {
    role: String,
    bridge_reference: String,
    state: String,
    condition: Option<String>,
    source_network: Option<String>,
    source_block_number: Option<i64>,
    source_block_hash: Option<Vec<u8>>,
    source_position_kind: Option<String>,
    source_position_index: Option<i64>,
    source_final: bool,
    included_network: Option<String>,
    included_block_number: Option<i64>,
    included_block_hash: Option<Vec<u8>>,
    final_network: Option<String>,
    final_block_number: Option<i64>,
    final_block_hash: Option<Vec<u8>>,
    updated_at: DateTime<Utc>,
}

/// The rows of `subject` whose lookup reference is one of `references`, the deposit, the return or
/// the withdrawal first.
#[doc(hidden)]
pub const LOOKUP: &str = "\
    SELECT role, bridge_reference, state, condition, source_network, source_block_number, \
           source_block_hash, source_position_kind, source_position_index, source_final, \
           included_network, included_block_number, included_block_hash, \
           final_network, final_block_number, final_block_hash, updated_at \
    FROM public_status \
    WHERE lookup_reference = ANY($1) AND subject = $2 \
    ORDER BY CASE role WHEN 'refund' THEN 1 ELSE 0 END, bridge_reference";

/// The names each role's coordinates take: its source's and its delivery's.
fn names(subject: &str, role: &str) -> (&'static str, &'static str) {
    match (subject, role) {
        ("directed_deposit", "deposit") => ("arrival", "mint"),
        ("directed_deposit", "return") => ("arrival", "return"),
        ("withdrawal", "withdrawal") => ("redemption", "payout"),
        ("withdrawal", _) => ("redemption", "refund mint"),
        _ => ("arrival", "refund"),
    }
}

fn status(subject: &str, rows: Vec<Row>) -> Option<PublicStatus> {
    let first = rows.first()?;
    let mut answer = PublicStatus {
        kind: subject.to_owned(),
        bridge_reference: first.bridge_reference.clone(),
        state: first.state.clone(),
        conditions: Vec::new(),
        coordinates: Vec::new(),
        transfers: Vec::new(),
        updated_at: first.updated_at,
    };
    for r in rows {
        let (source_role, delivery_role) = names(subject, &r.role);
        if let Some(c) = &r.condition
            && !answer.conditions.contains(c)
        {
            answer.conditions.push(c.clone());
        }
        answer.updated_at = answer.updated_at.max(r.updated_at);
        let mut coordinates = Vec::new();
        if let (Some(network), Some(number), Some(hash)) =
            (r.source_network, r.source_block_number, r.source_block_hash)
        {
            coordinates.push(PublicCoordinate {
                network,
                role: source_role.to_owned(),
                block_number: number,
                block_hash: to_hex(&hash),
                position_kind: r.source_position_kind,
                position_index: r.source_position_index,
                r#final: r.source_final,
            });
        }
        // The delivery: final when bezent observed it final; otherwise included, not final.
        let delivery = match (
            (r.final_network, r.final_block_number, r.final_block_hash),
            (
                r.included_network,
                r.included_block_number,
                r.included_block_hash,
            ),
        ) {
            ((Some(network), Some(number), Some(hash)), _) => Some((network, number, hash, true)),
            (_, (Some(network), Some(number), Some(hash))) => Some((network, number, hash, false)),
            _ => None,
        };
        if let Some((network, number, hash, r#final)) = delivery {
            coordinates.push(PublicCoordinate {
                network,
                role: delivery_role.to_owned(),
                block_number: number,
                block_hash: to_hex(&hash),
                position_kind: None,
                position_index: None,
                r#final,
            });
        }
        for c in coordinates {
            if !answer.coordinates.iter().any(|a| {
                a.network == c.network
                    && a.block_hash == c.block_hash
                    && a.position_kind == c.position_kind
                    && a.position_index == c.position_index
                    && a.role == c.role
            }) {
                answer.coordinates.push(c);
            }
        }
        answer.transfers.push(PublicTransfer {
            role: r.role,
            bridge_reference: r.bridge_reference,
            state: r.state,
            condition: r.condition,
        });
    }
    Some(answer)
}

async fn lookup(
    app: &PublicApp,
    subject: &str,
    references: Vec<String>,
) -> Result<axum::Json<PublicStatus>, PublicError> {
    let _permit = Arc::clone(&app.permits)
        .try_acquire_owned()
        .map_err(|_| PublicError::Overloaded)?;
    let rows: Vec<Row> = sqlx::query_as(LOOKUP)
        .bind(&references)
        .bind(subject)
        .fetch_all(app.pool())
        .await?;
    status(subject, rows)
        .map(axum::Json)
        .ok_or(PublicError::NotFound)
}

/// `GET /v0/public/deposits/by-coordinate`: a directed deposit by its arrival's coordinate
/// (`REQ-ST-8`, `REQ-ST-1`).
async fn deposit_by_coordinate(
    State(app): State<PublicApp>,
    Params(q): Params<CoordinateQuery>,
) -> Result<axum::Json<PublicStatus>, PublicError> {
    let c = q.body().parse()?;
    lookup(
        &app,
        "directed_deposit",
        vec![refs::deposit(&c), refs::unassociated_refund(&c)],
    )
    .await
}

/// A redemption's on-chain reference: its redeemer (SS58, any prefix, or `0x` and 64 hex digits)
/// and the reference it chose (`0x` and 64 hex digits).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedemptionQuery {
    /// The redeemer.
    pub redeemer: String,
    /// The reference.
    pub reference: String,
}

/// `GET /v0/public/withdrawals/by-redemption`: a withdrawal by its redemption's on-chain reference
/// (`REQ-ST-8`).
async fn withdrawal_by_redemption(
    State(app): State<PublicApp>,
    Params(q): Params<RedemptionQuery>,
) -> Result<axum::Json<PublicStatus>, PublicError> {
    // Review M1 of PR #139: base58 decoding is quadratic in its input, so nothing longer than an
    // account can be is decoded, and the reference neither: refused before any work.
    let redeemer = q.redeemer.trim();
    let longest = if redeemer.starts_with("0x") {
        HEX32_CHARS
    } else {
        MAX_SS58_CHARS
    };
    if redeemer.len() > longest {
        return Err(PublicError::Malformed(format!(
            "redeemer is an issuing-network account: SS58 (at most {MAX_SS58_CHARS} characters), or 0x and 64 hex digits"
        )));
    }
    let reference = q.reference.trim();
    if reference.len() > HEX32_CHARS {
        return Err(PublicError::Malformed(
            "reference is 0x and 64 hex digits, as the redemption chose it".to_owned(),
        ));
    }
    let redeemer = refs::account32(redeemer).ok_or_else(|| {
        PublicError::Malformed(
            "redeemer is an issuing-network account: SS58, or 0x and 64 hex digits".to_owned(),
        )
    })?;
    let reference = parse_hex("reference", reference)?;
    if reference.len() != 32 {
        return Err(PublicError::Malformed(
            "reference is 0x and 64 hex digits, as the redemption chose it".to_owned(),
        ));
    }
    lookup(
        &app,
        "withdrawal",
        vec![refs::withdrawal(&redeemer, &reference)],
    )
    .await
}

/// `GET /health` on the public listener: the store answers this login. It takes a permit as a
/// query does (review L2 of PR #139), so a flood of it is bounded the same way.
async fn health(
    State(app): State<PublicApp>,
) -> Result<axum::Json<serde_json::Value>, PublicError> {
    let _permit = Arc::clone(&app.permits)
        .try_acquire_owned()
        .map_err(|_| PublicError::Overloaded)?;
    sqlx::query("SELECT 1").execute(app.pool()).await?;
    Ok(axum::Json(serde_json::json!({"status": "ok"})))
}

/// Public data: any origin may read it, with no credentials, and a cache in front may keep it for a
/// few seconds. Every answer says so, an error too (a refusal is no secret), and nothing else is
/// served.
async fn headers(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let method = request.method().clone();
    let too_long = request
        .uri()
        .path_and_query()
        .is_some_and(|p| p.as_str().len() > MAX_URI_BYTES);
    let mut response = if too_long {
        PublicError::UriTooLong.into_response()
    } else if method == Method::OPTIONS {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(request).await
    };
    let unavailable = response.status() == StatusCode::SERVICE_UNAVAILABLE;
    let h = response.headers_mut();
    // Every 503 is `ERR-Overloaded` (the request timeout's too, review M1 of PR #139); each says
    // when to retry, whichever made it.
    if unavailable && !h.contains_key(header::RETRY_AFTER) {
        h.insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    }
    h.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    h.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, OPTIONS"),
    );
    h.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("accept"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if !h.contains_key(header::CACHE_CONTROL) {
        let value = if response.status().is_success() || response.status() == StatusCode::NOT_FOUND
        {
            format!("public, max-age={CACHE_SECONDS}")
        } else {
            "no-store".to_owned()
        };
        if let Ok(v) = HeaderValue::from_str(&value) {
            response.headers_mut().insert(header::CACHE_CONTROL, v);
        }
    }
    response
}

/// The public route, and nothing else: no request, query, webhook or operator path is served here.
/// A request is answered within [`DEFAULT_REQUEST_TIMEOUT`].
pub fn public_router(app: PublicApp) -> Router {
    public_router_with(app, DEFAULT_REQUEST_TIMEOUT)
}

/// The request timeout of [`public_router`]: `[public] request_timeout_ms`'s default.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// [`public_router`], answering each request within `timeout` (review M1 of PR #139): beyond it, the
/// handler is dropped and the answer is `ERR-Overloaded` (503, `Retry-After`), as when the route
/// answers as many as it can at once (second fix round of PR #139: the body says so too).
pub fn public_router_with(app: PublicApp, timeout: Duration) -> Router {
    let timeout = timeout.max(Duration::from_millis(1));
    Router::new()
        .route("/health", get(health))
        .route(
            "/v0/public/deposits/by-coordinate",
            get(deposit_by_coordinate),
        )
        .route(
            "/v0/public/withdrawals/by-redemption",
            get(withdrawal_by_redemption),
        )
        .layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| async move {
                match tokio::time::timeout(timeout, next.run(request)).await {
                    Ok(response) => response,
                    Err(_) => PublicError::Overloaded.into_response(),
                }
            },
        ))
        .layer(axum::middleware::from_fn(headers))
        .with_state(app)
}

/// What the listener answers on a connection beyond its bound (second fix round of PR #139):
/// `ERR-Overloaded` with `Retry-After`, with the route's own headers, to any request.
fn refusal_router() -> Router {
    Router::new()
        .fallback(|| async { PublicError::Overloaded })
        .layer(axum::middleware::from_fn(headers))
}

/// The public listener's own bounds (review L3 of PR #139 and its second fix round).
#[derive(Clone, Copy, Debug)]
pub struct ListenerBounds {
    /// The longest a request's head may take to arrive, from its first byte, and a new connection's
    /// first byte from its accept; then the connection is closed.
    pub header_read_timeout: Duration,
    /// The longest a connection may stay idle between requests, its answers all written, before it
    /// is closed: longer than the header-read timeout, so a front's pooled connections are kept.
    pub idle_timeout: Duration,
    /// The longest one exchange may hold a connection, from the first byte of a request to the last
    /// byte of its answer written, pipelined requests behind it included: a client that never reads
    /// its answers holds its connection no longer (second fix round of PR #139). Then the connection
    /// is shut down gracefully and dropped after [`CONNECTION_GRACE`].
    pub connection_timeout: Duration,
    /// Connections held open at once; one more is answered `ERR-Overloaded` and closed.
    pub max_connections: usize,
}

impl ListenerBounds {
    /// The bounds `[public]` names.
    pub fn of(public: &PublicConfig) -> Self {
        Self {
            header_read_timeout: Duration::from_millis(public.header_read_timeout_ms.max(1)),
            idle_timeout: Duration::from_millis(public.idle_timeout_ms.max(1)),
            connection_timeout: Duration::from_millis(public.connection_timeout_ms.max(1)),
            max_connections: public.max_http_connections.max(1),
        }
    }
}

/// How long a connection past its [`ListenerBounds::connection_timeout`] is given to finish once it
/// is told to shut down; then it is dropped.
pub const CONNECTION_GRACE: Duration = Duration::from_secs(1);

/// How long the open connections are given to finish when the listener stops.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// How long a connection beyond the bound is given to read its request and take its refusal.
const REFUSAL_GRACE: Duration = Duration::from_secs(1);

/// Where a connection is, as its IO and its service see it ([`Pace`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Nothing read of a next request, and every answer written.
    Idle,
    /// Some of a request's head read, and no request taken yet.
    Head,
    /// A request taken, and its answer, or one before it, not yet written.
    Busy,
}

/// A connection's pace (second fix round of PR #139): hyper bounds neither how long an answer takes
/// to be written (a client that never reads holds it forever) nor an idle keep-alive apart from the
/// head's timeout, so the listener keeps its own clock. Its IO ([`Paced`]) reports bytes read and
/// written and each flush; its service ([`PacedService`]) each request taken and each answer done.
#[derive(Debug)]
struct Pace {
    phase: Phase,
    /// When the phase began.
    since: tokio::time::Instant,
    /// When the exchange began: the first byte read after the connection was idle.
    exchange: tokio::time::Instant,
    /// Requests taken whose answers are not yet done.
    in_flight: usize,
    /// Bytes written to the socket since its last flush.
    unflushed: bool,
    /// Whether a request was ever taken: a new connection waits the head's timeout, not the idle one.
    served: bool,
}

#[derive(Debug)]
struct Watch {
    pace: std::sync::Mutex<Pace>,
    moved: tokio::sync::Notify,
}

impl Watch {
    fn new() -> Arc<Self> {
        let now = tokio::time::Instant::now();
        Arc::new(Self {
            pace: std::sync::Mutex::new(Pace {
                phase: Phase::Idle,
                since: now,
                exchange: now,
                in_flight: 0,
                unflushed: false,
                served: false,
            }),
            moved: tokio::sync::Notify::new(),
        })
    }

    fn with(&self, f: impl FnOnce(&mut Pace, tokio::time::Instant) -> bool) {
        let mut pace = self.pace.lock().unwrap_or_else(|e| e.into_inner());
        if f(&mut pace, tokio::time::Instant::now()) {
            self.moved.notify_one();
        }
    }

    fn read(&self) {
        self.with(|p, now| {
            if p.phase != Phase::Idle {
                return false;
            }
            p.phase = Phase::Head;
            p.since = now;
            p.exchange = now;
            true
        });
    }

    fn wrote(&self) {
        self.with(|p, _| {
            p.unflushed = true;
            // The answer's bytes after it was done (see `answered`): still its exchange.
            if p.phase == Phase::Idle {
                p.phase = Phase::Busy;
                return true;
            }
            false
        });
    }

    fn flushed(&self) {
        self.with(|p, now| {
            p.unflushed = false;
            p.idle_if_done(now)
        });
    }

    fn request(&self) {
        self.with(|p, now| {
            p.in_flight += 1;
            p.served = true;
            match p.phase {
                Phase::Busy => false,
                Phase::Head => {
                    p.phase = Phase::Busy;
                    true
                }
                Phase::Idle => {
                    // A request hyper had read already, behind the one answered. (Idle means
                    // every answer was written and flushed, so this is the exchange's start.)
                    p.phase = Phase::Busy;
                    p.exchange = now;
                    true
                }
            }
        });
    }

    fn answered(&self) {
        self.with(|p, now| {
            p.in_flight = p.in_flight.saturating_sub(1);
            p.idle_if_done(now)
        });
    }

    /// When the connection's current phase runs out, and what it then is.
    fn deadline(&self, bounds: &ListenerBounds) -> (tokio::time::Instant, Phase) {
        let p = self.pace.lock().unwrap_or_else(|e| e.into_inner());
        let at = match p.phase {
            Phase::Idle if p.served => p.since + bounds.idle_timeout,
            Phase::Idle => p.since + bounds.header_read_timeout,
            Phase::Head => {
                (p.since + bounds.header_read_timeout).min(p.exchange + bounds.connection_timeout)
            }
            Phase::Busy => p.exchange + bounds.connection_timeout,
        };
        (at, p.phase)
    }
}

impl Pace {
    fn idle_if_done(&mut self, now: tokio::time::Instant) -> bool {
        if self.phase == Phase::Busy && self.in_flight == 0 && !self.unflushed {
            self.phase = Phase::Idle;
            self.since = now;
            return true;
        }
        false
    }
}

/// A connection's socket, reporting what moves on it to its [`Watch`].
struct Paced {
    io: tokio::net::TcpStream,
    watch: Arc<Watch>,
}

impl tokio::io::AsyncRead for Paced {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let polled = std::pin::Pin::new(&mut self.io).poll_read(cx, buf);
        if matches!(polled, std::task::Poll::Ready(Ok(()))) && buf.filled().len() > before {
            self.watch.read();
        }
        polled
    }
}

impl tokio::io::AsyncWrite for Paced {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.watch.wrote();
        std::pin::Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.watch.wrote();
        std::pin::Pin::new(&mut self.io).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let polled = std::pin::Pin::new(&mut self.io).poll_flush(cx);
        if matches!(polled, std::task::Poll::Ready(Ok(()))) {
            self.watch.flushed();
        }
        polled
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

/// Held from a request taken until its answer's body is done (dropped by hyper once written to its
/// buffer, or with the connection).
struct Answering(Arc<Watch>);

impl Drop for Answering {
    fn drop(&mut self) {
        self.0.answered();
    }
}

/// An answer's body, holding its [`Answering`].
struct PacedBody {
    body: axum::body::Body,
    _answering: Answering,
}

impl hyper::body::Body for PacedBody {
    type Data = axum::body::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        std::pin::Pin::new(&mut self.body).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.body.size_hint()
    }
}

/// The route as hyper serves it on one connection, reporting each request and answer.
struct PacedService {
    inner: hyper_util::service::TowerToHyperService<Router>,
    watch: Arc<Watch>,
}

impl hyper::service::Service<hyper::Request<hyper::body::Incoming>> for PacedService {
    type Response = hyper::Response<PacedBody>;
    type Error = std::convert::Infallible;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn call(&self, request: hyper::Request<hyper::body::Incoming>) -> Self::Future {
        self.watch.request();
        let answering = Answering(Arc::clone(&self.watch));
        let answer = self.inner.call(request);
        Box::pin(async move {
            let response = answer.await?;
            Ok(response.map(|body| PacedBody {
                body,
                _answering: answering,
            }))
        })
    }
}

/// Waits for `stop` to turn true, or for its sender to go.
async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|s| *s).await.map(|_| ());
}

/// Serves `router` on `listener` until `stop` turns true (review L3 of PR #139 and its second fix
/// round): HTTP/1 only, a request head of at most [`MAX_HEAD_BYTES`] in at most [`MAX_HEADERS`]
/// headers (431 beyond), within the header-read timeout of its first byte (and a new connection's
/// first byte within it of its accept); an idle connection kept for the idle timeout; one exchange, from a request's first
/// byte to its answer's last byte written, within the connection timeout, so a client that never
/// reads its answers is shut down and dropped ([`ListenerBounds`]); and at most `max_connections`
/// connections open at once, one more answered `ERR-Overloaded` and closed. On `stop` it accepts no
/// more and lets the open ones finish for up to five seconds.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    bounds: ListenerBounds,
    mut stop: watch::Receiver<bool>,
) -> std::io::Result<()> {
    use hyper_util::rt::{TokioIo, TokioTimer};
    let most = bounds.max_connections.max(1);
    let open = Arc::new(Semaphore::new(most));
    let refusing = Arc::new(Semaphore::new(most));
    let mut http = hyper::server::conn::http1::Builder::new();
    // The head's timeout is the listener's own (`Watch`): hyper's starts when a connection goes
    // idle, so an idle keep-alive would be closed at it.
    http.timer(TokioTimer::new())
        .header_read_timeout(None)
        .max_buf_size(MAX_HEAD_BYTES)
        .max_headers(MAX_HEADERS);
    let mut refusal_http = http.clone();
    refusal_http.keep_alive(false);
    let refusal = refusal_router();
    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(e) => {
                    // Out of descriptors, say: wait a little rather than spin.
                    tracing::warn!(error = %e, "the public listener could not accept a connection");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            () = stopped(&mut stop) => break,
        };
        let Ok(permit) = Arc::clone(&open).try_acquire_owned() else {
            // Beyond the bound: refused at once, not queued (second fix round of PR #139). The
            // refusal reads the request so the answer is not lost to a reset, for a second at most;
            // beyond as many refusals at once, the connection is dropped.
            if let Ok(refusing) = Arc::clone(&refusing).try_acquire_owned() {
                let service = hyper_util::service::TowerToHyperService::new(refusal.clone());
                let connection = refusal_http.serve_connection(TokioIo::new(stream), service);
                tokio::spawn(async move {
                    let _refusing = refusing;
                    let _ = tokio::time::timeout(REFUSAL_GRACE, connection).await;
                });
            }
            continue;
        };
        let watch = Watch::new();
        // A second handle on the socket, kept to reset it when the connection is dropped at a
        // deadline (review N3 of PR #139): see `reset_on_close`.
        let reset = std::os::fd::AsFd::as_fd(&stream).try_clone_to_owned().ok();
        let io = TokioIo::new(Paced {
            io: stream,
            watch: Arc::clone(&watch),
        });
        let service = PacedService {
            inner: hyper_util::service::TowerToHyperService::new(router.clone()),
            watch: Arc::clone(&watch),
        };
        let connection = http.serve_connection(io, service);
        let mut stop = stop.clone();
        tokio::spawn(async move {
            let _open = permit;
            let mut connection = std::pin::pin!(connection);
            loop {
                let (at, phase) = watch.deadline(&bounds);
                tokio::select! {
                    ended = connection.as_mut() => {
                        if let Err(e) = ended {
                            tracing::debug!(error = %e, "a public connection ended with an error");
                        }
                        return;
                    }
                    () = watch.moved.notified() => {}
                    () = tokio::time::sleep_until(at) => {
                        if watch.deadline(&bounds).0 > tokio::time::Instant::now() {
                            continue;
                        }
                        if phase == Phase::Busy {
                            tracing::debug!("a public connection held an exchange past its timeout; shutting it down");
                            connection.as_mut().graceful_shutdown();
                            let _ = tokio::time::timeout(CONNECTION_GRACE, connection.as_mut()).await;
                        }
                        if phase != Phase::Idle {
                            reset_on_close(reset);
                        }
                        return;
                    }
                    () = stopped(&mut stop) => {
                        connection.as_mut().graceful_shutdown();
                        let _ = tokio::time::timeout(SHUTDOWN_GRACE, connection.as_mut()).await;
                        return;
                    }
                }
            }
        });
    }
    drop(listener);
    // Every connection holds a permit until it ends: all of them back means all ended.
    let all = u32::try_from(most).unwrap_or(u32::MAX);
    if tokio::time::timeout(
        SHUTDOWN_GRACE + Duration::from_secs(1),
        open.acquire_many(all),
    )
    .await
    .is_err()
    {
        tracing::warn!("the public listener stopped with connections still open");
    }
    Ok(())
}

/// Makes the socket `reset` names close with a reset (`SO_LINGER` of zero) when the connection that
/// owns it is dropped next, review N3 of PR #139: a peer that stopped reading (a zero window) would
/// otherwise keep a closing socket, and its unsent bytes, lingering on the server. `reset` is a
/// second handle on the same socket; it is closed here, and the socket by the connection's drop.
fn reset_on_close(reset: Option<std::os::fd::OwnedFd>) {
    let Some(fd) = reset else {
        return;
    };
    match tokio::net::TcpStream::from_std(std::net::TcpStream::from(fd)) {
        Ok(handle) => {
            if let Err(e) = handle.set_zero_linger() {
                tracing::debug!(error = %e, "a public connection's socket could not be set to reset on close");
            }
        }
        Err(e) => {
            tracing::debug!(error = %e, "a public connection's socket could not be set to reset on close");
        }
    }
}

/// Connects the route's own pool and checks its login is a member of `bezent_public` and of nothing
/// else, holding exactly that role's grants (#46). Fails closed.
pub async fn connect(
    public: &PublicConfig,
    resolver: &bezent_vault::Resolver,
) -> Result<PgPool, String> {
    let url = public.store_url(resolver).await?;
    let pool = bezent_store::tls::connect(public.pool_options(), &url)
        .await
        .map_err(|e| format!("[public]: cannot connect to the store: {e}"))?;
    bezent_store::login::check_login(&pool, bezent_store::PUBLIC_ROLE)
        .await
        .map_err(|e| format!("[public] database_url: {e}"))?;
    sqlx::query("SELECT 1 FROM public_status LIMIT 0")
        .execute(&pool)
        .await
        .map_err(|e| format!("[public]: the store's public_status view is not readable: {e}"))?;
    Ok(pool)
}
