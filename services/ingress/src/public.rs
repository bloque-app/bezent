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
use tokio::sync::Semaphore;

/// How long a cache in front may keep an answer, in seconds.
pub const CACHE_SECONDS: u32 = 5;

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
    let redeemer = refs::account32(q.redeemer.trim()).ok_or_else(|| {
        PublicError::Malformed(
            "redeemer is an issuing-network account: SS58, or 0x and 64 hex digits".to_owned(),
        )
    })?;
    let reference = parse_hex("reference", q.reference.trim())?;
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

/// `GET /health` on the public listener: the store answers this login.
async fn health(
    State(app): State<PublicApp>,
) -> Result<axum::Json<serde_json::Value>, PublicError> {
    sqlx::query("SELECT 1").execute(app.pool()).await?;
    Ok(axum::Json(serde_json::json!({"status": "ok"})))
}

/// Public data: any origin may read it, with no credentials, and a cache in front may keep it for a
/// few seconds. Every answer says so, an error too (a refusal is no secret), and nothing else is
/// served.
async fn headers(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let method = request.method().clone();
    let mut response = if method == Method::OPTIONS {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(request).await
    };
    let h = response.headers_mut();
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
pub fn public_router(app: PublicApp) -> Router {
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
        .layer(axum::middleware::from_fn(headers))
        .with_state(app)
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
