// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The query surface (`CTR-QRY`, SPEC §7.B; `REQ-RQ-8`, `REQ-ST-2`, `REQ-ST-4`).
//!
//! A requester reads only what it asked for: a transfer is visible to a requester when one of its
//! requests made or reached it (`intake_answers`), and a refusal when it was the requester's own
//! (`REQ-ID-4`, PLAN `DEC-15`). Anything else reads as not found.

use crate::api::{
    CapacityOut, CoordinateOut, CoordinateQuery, DestinationBody, Health, LogEntry, LogPage,
    LogQuery, Milestone, PositionBody, PositionKind, ReserveEquation, ReserveOut, Reserves,
    RouteOut, RouteTable, TransferList, TransferStatus, WindowOut, refusal_detail, to_hex,
};
use crate::app::{App, Params};
use crate::auth::Requester;
use crate::error::{ApiError, ErrorBody};
use axum::Json;
use axum::extract::{Path, State};
use chrono::{DateTime, Utc};
use sqlx::PgPool;

/// The columns of a transfer's status, from `transfers t`. A transfer's own inclusion milestone is
/// written when it completes, from the attempt that completed it (F-003, migration 0014); while it
/// is `Delivered`, the inclusion shown is its current attempt's (`REQ-TR-4`).
const STATUS_COLUMNS: &str = "\
    t.request_reference, t.bridge_reference, t.kind, t.state::text AS state, \
    t.condition::text AS condition, \
    (SELECT x.error::text FROM transitions x \
       WHERE x.transfer_id = t.id AND x.error IS NOT NULL ORDER BY x.id DESC LIMIT 1) AS error, \
    t.asset, t.token, t.amount::text AS amount, \
    t.source_network, t.source_block_number, t.source_block_hash, \
    t.source_position_kind::text AS source_position_kind, t.source_position_index, \
    t.destination_network, t.destination, t.refund_destination_network, t.refund_destination, \
    t.expires_at, \
    t.source_final_network, t.source_final_block_number, t.source_final_block_hash, \
    t.source_final_block_time, t.source_final_seen_at, \
    COALESCE(t.destination_included_network, (SELECT m.network FROM attempt_milestones m \
       WHERE m.transfer_id = t.id AND m.milestone = 'included' AND t.state = 'Delivered' \
       ORDER BY m.attempt DESC LIMIT 1)) AS destination_included_network, \
    COALESCE(t.destination_included_block_number, (SELECT m.block_number FROM attempt_milestones m \
       WHERE m.transfer_id = t.id AND m.milestone = 'included' AND t.state = 'Delivered' \
       ORDER BY m.attempt DESC LIMIT 1)) AS destination_included_block_number, \
    COALESCE(t.destination_included_block_hash, (SELECT m.block_hash FROM attempt_milestones m \
       WHERE m.transfer_id = t.id AND m.milestone = 'included' AND t.state = 'Delivered' \
       ORDER BY m.attempt DESC LIMIT 1)) AS destination_included_block_hash, \
    COALESCE(t.destination_included_block_time, (SELECT m.block_time FROM attempt_milestones m \
       WHERE m.transfer_id = t.id AND m.milestone = 'included' AND t.state = 'Delivered' \
       ORDER BY m.attempt DESC LIMIT 1)) AS destination_included_block_time, \
    COALESCE(t.destination_included_seen_at, (SELECT m.seen_at FROM attempt_milestones m \
       WHERE m.transfer_id = t.id AND m.milestone = 'included' AND t.state = 'Delivered' \
       ORDER BY m.attempt DESC LIMIT 1)) AS destination_included_seen_at, \
    t.destination_final_network, t.destination_final_block_number, \
    t.destination_final_block_hash, t.destination_final_block_time, t.destination_final_seen_at";

/// Whether requester `$1` may read transfer `t` (`REQ-ID-4`).
const VISIBLE_TO: &str = "EXISTS (SELECT 1 FROM intake_answers a JOIN intake i ON i.id = a.intake_id \
     WHERE a.transfer_id = t.id AND i.requester = $1)";

#[derive(sqlx::FromRow)]
struct StatusRow {
    request_reference: String,
    bridge_reference: String,
    kind: String,
    state: String,
    condition: Option<String>,
    error: Option<String>,
    asset: Option<String>,
    token: Option<String>,
    amount: String,
    source_network: Option<String>,
    source_block_number: Option<i64>,
    source_block_hash: Option<Vec<u8>>,
    source_position_kind: Option<String>,
    source_position_index: Option<i64>,
    destination_network: String,
    destination: String,
    refund_destination_network: Option<String>,
    refund_destination: Option<String>,
    expires_at: Option<DateTime<Utc>>,
    source_final_network: Option<String>,
    source_final_block_number: Option<i64>,
    source_final_block_hash: Option<Vec<u8>>,
    source_final_block_time: Option<DateTime<Utc>>,
    source_final_seen_at: Option<DateTime<Utc>>,
    destination_included_network: Option<String>,
    destination_included_block_number: Option<i64>,
    destination_included_block_hash: Option<Vec<u8>>,
    destination_included_block_time: Option<DateTime<Utc>>,
    destination_included_seen_at: Option<DateTime<Utc>>,
    destination_final_network: Option<String>,
    destination_final_block_number: Option<i64>,
    destination_final_block_hash: Option<Vec<u8>>,
    destination_final_block_time: Option<DateTime<Utc>>,
    destination_final_seen_at: Option<DateTime<Utc>>,
}

fn milestone(
    network: Option<String>,
    number: Option<i64>,
    hash: Option<Vec<u8>>,
    time: Option<DateTime<Utc>>,
    seen: Option<DateTime<Utc>>,
) -> Option<Milestone> {
    Some(Milestone {
        network: network?,
        block_number: number?,
        block_hash: to_hex(&hash?),
        block_time: time?,
        seen_at: seen?,
    })
}

impl From<StatusRow> for TransferStatus {
    fn from(r: StatusRow) -> Self {
        let source = match (
            r.source_network,
            r.source_block_number,
            r.source_block_hash,
            r.source_position_kind,
            r.source_position_index,
        ) {
            (Some(network), Some(block_number), Some(hash), Some(kind), Some(index)) => {
                Some(CoordinateOut {
                    network,
                    block_number,
                    block_hash: to_hex(&hash),
                    position: PositionBody {
                        kind: if kind == "log" {
                            PositionKind::Log
                        } else {
                            PositionKind::Transaction
                        },
                        index: u32::try_from(index).unwrap_or(u32::MAX),
                    },
                })
            }
            _ => None,
        };
        let refund_destination = match (r.refund_destination_network, r.refund_destination) {
            (Some(network), Some(address)) => Some(DestinationBody { network, address }),
            _ => None,
        };
        TransferStatus {
            request_reference: r.request_reference,
            bridge_reference: r.bridge_reference,
            kind: r.kind,
            state: r.state,
            condition: r.condition,
            detail: refusal_detail(r.error.as_deref()),
            error: r.error,
            asset: r.asset,
            token: r.token,
            amount: Some(r.amount),
            source,
            destination: Some(DestinationBody {
                network: r.destination_network,
                address: r.destination,
            }),
            refund_destination,
            expires_at: r.expires_at,
            source_final: milestone(
                r.source_final_network,
                r.source_final_block_number,
                r.source_final_block_hash,
                r.source_final_block_time,
                r.source_final_seen_at,
            ),
            destination_included: milestone(
                r.destination_included_network,
                r.destination_included_block_number,
                r.destination_included_block_hash,
                r.destination_included_block_time,
                r.destination_included_seen_at,
            ),
            destination_final: milestone(
                r.destination_final_network,
                r.destination_final_block_number,
                r.destination_final_block_hash,
                r.destination_final_block_time,
                r.destination_final_seen_at,
            ),
        }
    }
}

/// A transfer's status.
async fn transfer_status(pool: &PgPool, transfer_id: i64) -> Result<TransferStatus, ApiError> {
    let row: StatusRow = sqlx::query_as(&format!(
        "SELECT {STATUS_COLUMNS} FROM transfers t WHERE t.id = $1"
    ))
    .bind(transfer_id)
    .fetch_one(pool)
    .await?;
    Ok(row.into())
}

/// The status a request was answered with, as it stands now (`REQ-RQ-1`, `REQ-ID-1`): the
/// transfer it made or reached, or its refusal.
pub(crate) async fn status_of_intake(
    pool: &PgPool,
    intake_id: i64,
) -> Result<TransferStatus, ApiError> {
    let (transfer_id, bridge_reference, error, kind, request_reference): (
        Option<i64>,
        String,
        Option<String>,
        String,
        String,
    ) = sqlx::query_as(
        "SELECT a.transfer_id, a.bridge_reference, a.error::text, i.kind, i.request_reference \
         FROM intake_answers a JOIN intake i ON i.id = a.intake_id WHERE a.intake_id = $1",
    )
    .bind(intake_id)
    .fetch_one(pool)
    .await?;
    match transfer_id {
        Some(id) => transfer_status(pool, id).await,
        None => Ok(TransferStatus {
            request_reference,
            bridge_reference,
            kind,
            state: "Refused".into(),
            condition: None,
            detail: refusal_detail(error.as_deref()),
            error,
            asset: None,
            token: None,
            amount: None,
            source: None,
            destination: None,
            refund_destination: None,
            expires_at: None,
            source_final: None,
            destination_included: None,
            destination_final: None,
        }),
    }
}

/// Status by request reference (`REQ-ST-2`).
#[utoipa::path(
    get, path = "/v0/transfers/by-request/{reference}", tag = "queries",
    params(("reference" = String, Path, description = "The request reference")),
    security(("bearer" = [])),
    responses(
        (status = 200, body = TransferStatus),
        (status = 401, body = ErrorBody), (status = 404, body = ErrorBody),
    )
)]
pub(crate) async fn by_request(
    State(app): State<App>,
    who: Requester,
    Path(reference): Path<String>,
) -> Result<Json<TransferStatus>, ApiError> {
    let id: Option<i64> =
        sqlx::query_scalar("SELECT id FROM intake WHERE requester = $1 AND request_reference = $2")
            .bind(&who.name)
            .bind(&reference)
            .fetch_optional(app.pool())
            .await?;
    let id = id.ok_or(ApiError::NotFound)?;
    Ok(Json(status_of_intake(app.pool(), id).await?))
}

/// Status by bridge reference (`REQ-ST-2`).
#[utoipa::path(
    get, path = "/v0/transfers/by-bridge/{reference}", tag = "queries",
    params(("reference" = String, Path, description = "The bridge reference")),
    security(("bearer" = [])),
    responses(
        (status = 200, body = TransferStatus),
        (status = 401, body = ErrorBody), (status = 404, body = ErrorBody),
    )
)]
pub(crate) async fn by_bridge(
    State(app): State<App>,
    who: Requester,
    Path(reference): Path<String>,
) -> Result<Json<TransferStatus>, ApiError> {
    let row: Option<StatusRow> = sqlx::query_as(&format!(
        "SELECT {STATUS_COLUMNS} FROM transfers t WHERE t.bridge_reference = $2 AND {VISIBLE_TO}"
    ))
    .bind(&who.name)
    .bind(&reference)
    .fetch_optional(app.pool())
    .await?;
    if let Some(row) = row {
        return Ok(Json(row.into()));
    }
    let refused: Option<i64> = sqlx::query_scalar(
        "SELECT a.intake_id FROM intake_answers a JOIN intake i ON i.id = a.intake_id \
         WHERE a.transfer_id IS NULL AND a.bridge_reference = $2 AND i.requester = $1",
    )
    .bind(&who.name)
    .bind(&reference)
    .fetch_optional(app.pool())
    .await?;
    let id = refused.ok_or(ApiError::NotFound)?;
    Ok(Json(status_of_intake(app.pool(), id).await?))
}

/// Status by coordinate (`REQ-ST-2`): the transfers this requester may read that the coordinate
/// funds.
#[utoipa::path(
    get, path = "/v0/transfers/by-coordinate", tag = "queries",
    params(CoordinateQuery),
    security(("bearer" = [])),
    responses(
        (status = 200, body = TransferList),
        (status = 400, body = ErrorBody), (status = 401, body = ErrorBody),
    )
)]
pub(crate) async fn by_coordinate(
    State(app): State<App>,
    who: Requester,
    Params(q): Params<CoordinateQuery>,
) -> Result<Json<TransferList>, ApiError> {
    let c = q.body().parse()?;
    let rows: Vec<StatusRow> = sqlx::query_as(&format!(
        "SELECT {STATUS_COLUMNS} FROM transfers t \
         WHERE t.source_network = $2 AND t.source_block_number = $3 AND t.source_block_hash = $4 \
           AND t.source_position_kind = $5::position_kind AND t.source_position_index = $6 \
           AND {VISIBLE_TO} \
         ORDER BY t.id"
    ))
    .bind(&who.name)
    .bind(c.network.as_str())
    .bind(i64::try_from(c.block_number).map_err(|_| ApiError::NotFound)?)
    .bind(c.block_hash.as_bytes())
    .bind(q.position_kind.as_str())
    .bind(i64::from(q.position_index))
    .fetch_all(app.pool())
    .await?;
    Ok(Json(TransferList {
        transfers: rows.into_iter().map(Into::into).collect(),
    }))
}

/// A route as the route table shows it.
#[derive(sqlx::FromRow)]
struct RouteRow {
    pub asset: String,
    pub network: String,
    pub direction: String,
    pub minimum: String,
    pub maximum: String,
    pub venue_minimum: Option<String>,
    pub window_limit: String,
    pub window_seconds: i64,
    pub capacity: i64,
    pub cutoffs: serde_json::Value,
    pub venue_addresses: serde_json::Value,
    pub completion_budget_seconds: i64,
    pub paused: bool,
    pub used: String,
    pub in_flight: i64,
}

/// The routes, with what each one's trailing window has used and what it carries now. A route's
/// transfers are those of its asset and kind on its origin network: the source network of a
/// deposit, the destination network of a withdrawal.
const ROUTES: &str = "\
    SELECT r.asset, r.network, r.direction, r.minimum::text AS minimum, r.maximum::text AS maximum, \
           r.venue_minimum::text AS venue_minimum, r.window_limit::text AS window_limit, \
           r.window_seconds, r.capacity, r.cutoffs, r.venue_addresses, r.completion_budget_seconds, \
           EXISTS (SELECT 1 FROM pauses p \
                   WHERE (p.asset IS NULL OR p.asset = r.asset) \
                     AND (p.network IS NULL OR p.network = r.network) \
                     AND (p.direction IS NULL OR p.direction = r.direction)) AS paused, \
           u.used::text AS used, u.in_flight \
    FROM routes r \
    CROSS JOIN LATERAL ( \
        SELECT COALESCE(sum(t.amount) FILTER ( \
                   WHERE t.state NOT IN ('Refused', 'Expired', 'Withdrawn') \
                     AND t.created_at > now() - make_interval(secs => r.window_seconds)), 0) AS used, \
               count(*) FILTER (WHERE NOT is_terminal_state(t.state)) AS in_flight \
        FROM transfers t \
        WHERE t.asset = r.asset AND t.kind = r.direction \
          AND CASE r.direction WHEN 'deposit' THEN t.source_network \
                               ELSE t.destination_network END = r.network \
    ) u";

fn route_out(r: RouteRow) -> RouteOut {
    let limit: u128 = r.window_limit.parse().unwrap_or(0);
    let remaining = r
        .used
        .parse::<u128>()
        .map(|used| limit.saturating_sub(used))
        .unwrap_or(0);
    RouteOut {
        asset: r.asset,
        network: r.network,
        direction: r.direction,
        minimum: r.minimum,
        maximum: r.maximum,
        venue_minimum: r.venue_minimum,
        window: WindowOut {
            limit: r.window_limit,
            seconds: r.window_seconds,
            used: r.used,
            remaining: remaining.to_string(),
        },
        capacity: CapacityOut {
            declared: r.capacity,
            in_flight: r.in_flight,
            available: (r.capacity - r.in_flight).max(0),
        },
        cutoffs: r.cutoffs,
        venue_addresses: r.venue_addresses,
        completion_budget_seconds: r.completion_budget_seconds,
        paused: r.paused,
    }
}

/// The route table with current capacity (`REQ-LM-1`, `REQ-AS-5`).
#[utoipa::path(
    get, path = "/v0/routes", tag = "queries",
    security(("bearer" = [])),
    responses((status = 200, body = RouteTable), (status = 401, body = ErrorBody))
)]
pub(crate) async fn routes(
    State(app): State<App>,
    _who: Requester,
) -> Result<Json<RouteTable>, ApiError> {
    let rows: Vec<RouteRow> = sqlx::query_as(&format!(
        "{ROUTES} ORDER BY r.asset, r.network, r.direction"
    ))
    .fetch_all(app.pool())
    .await?;
    Ok(Json(RouteTable {
        routes: rows.into_iter().map(route_out).collect(),
    }))
}

/// Reserve per (asset, network), as the reserve book holds it (`REQ-RQ-8`).
#[utoipa::path(
    get, path = "/v0/reserves", tag = "queries",
    security(("bearer" = [])),
    responses((status = 200, body = Reserves), (status = 401, body = ErrorBody))
)]
pub(crate) async fn reserves(
    State(app): State<App>,
    _who: Requester,
) -> Result<Json<Reserves>, ApiError> {
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT r.asset, r.network, r.address, COALESCE(b.balance, 0)::text \
         FROM reserves r \
         LEFT JOIN book_balances b \
           ON b.asset = r.asset AND b.account = 'reserve' AND b.network = r.network \
         ORDER BY r.asset, r.network",
    )
    .fetch_all(app.pool())
    .await?;
    Ok(Json(Reserves {
        reserves: rows
            .into_iter()
            .map(|(asset, network, address, balance)| ReserveOut {
                asset,
                network,
                address,
                balance,
            })
            .collect(),
    }))
}

/// The reserve equation's latest evaluation, as stored (`REQ-RQ-8`). The reserve check that
/// stores evaluations is `F-004`; until then there is none.
#[utoipa::path(
    get, path = "/v0/reserve-equation", tag = "queries",
    security(("bearer" = [])),
    responses((status = 200, body = ReserveEquation), (status = 401, body = ErrorBody))
)]
pub(crate) async fn reserve_equation(_who: Requester) -> Json<ReserveEquation> {
    Json(ReserveEquation { latest: None })
}

/// The sequenced log from a position (`REQ-ST-4`): this requester's entries in the positions after
/// `after`. Positions are gapless across the log, and an entry's position is its stable key
/// (`REQ-ST-5`). Read again from `next_after` until it reaches `head`.
#[utoipa::path(
    get, path = "/v0/log", tag = "queries",
    params(LogQuery),
    security(("bearer" = [])),
    responses(
        (status = 200, body = LogPage),
        (status = 400, body = ErrorBody), (status = 401, body = ErrorBody),
    )
)]
pub(crate) async fn log(
    State(app): State<App>,
    who: Requester,
    Params(q): Params<LogQuery>,
) -> Result<Json<LogPage>, ApiError> {
    if q.after < 0 {
        return Err(ApiError::Malformed("after is a position, 0 or more".into()));
    }
    let page = q.limit.unwrap_or(app.log_page()).clamp(1, app.log_page());
    // Appends serialise on the log's head and commit in order, so every position up to the
    // committed head is visible: reading up to it leaves no position behind.
    let head: i64 = sqlx::query_scalar("SELECT seq FROM event_log_head")
        .fetch_one(app.pool())
        .await?;
    let until = q.after.saturating_add(page).min(head).max(q.after);
    let rows: Vec<(i64, String, serde_json::Value, DateTime<Utc>)> = sqlx::query_as(
        "SELECT e.seq, e.kind, e.payload, e.appended_at FROM event_log e \
         WHERE e.seq > $2 AND e.seq <= $3 \
           AND ((e.transfer_id IS NOT NULL AND EXISTS ( \
                   SELECT 1 FROM intake_answers a JOIN intake i ON i.id = a.intake_id \
                   WHERE a.transfer_id = e.transfer_id AND i.requester = $1)) \
                OR (e.kind = 'refusal' AND e.payload ->> 'requester' = $1)) \
         ORDER BY e.seq",
    )
    .bind(&who.name)
    .bind(q.after)
    .bind(until)
    .fetch_all(app.pool())
    .await?;
    Ok(Json(LogPage {
        entries: rows
            .into_iter()
            .map(|(seq, kind, payload, appended_at)| LogEntry {
                seq,
                kind,
                payload,
                appended_at,
            })
            .collect(),
        next_after: until,
        head,
    }))
}

/// Liveness and the store's readiness.
#[utoipa::path(
    get, path = "/health", tag = "service",
    responses((status = 200, body = Health), (status = 503, body = ErrorBody))
)]
pub(crate) async fn health(State(app): State<App>) -> Result<Json<Health>, ApiError> {
    match sqlx::query("SELECT 1").execute(app.pool()).await {
        Ok(_) => {
            app.store_answered();
            Ok(Json(Health {
                status: "ok".into(),
            }))
        }
        // Every connection is busy with requests: the instance is alive and serving, and a
        // container health check must not replace a task for being loaded (issue #61), but only
        // while the store has answered something in the last minute. A store that cannot be
        // reached at all, or that has not answered for longer, is a 503.
        Err(sqlx::Error::PoolTimedOut) if app.store_answered_recently() => Ok(Json(Health {
            status: "busy".into(),
        })),
        Err(e) => Err(e.into()),
    }
}
