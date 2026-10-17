// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The request surface (`CTR-REQ`, SPEC §7.A).
//!
//! Every request runs in one store transaction that records it (`intake`), decides it against the
//! declarations and what the endpoints observed, makes or reaches its transfer, records the answer
//! (`intake_answers`), and commits before anything is answered (`REQ-RQ-1`). The one exception is
//! `ERR-Overloaded`: the transaction is rolled back, nothing is recorded, and the same request may
//! be sent again (`REQ-LM-4`).
//!
//! A request refused once its asset and amount are known is a transfer in `Refused`, with its named
//! error on the transition (§5.4). A request refused before it names anything bezent can bind
//! (`ERR-SourceNotFound`, `ERR-UnknownRoute` for a token not admitted, and a withdrawal's extras
//! that reach nothing) is recorded as a refusal with a bridge reference of its own and appended to
//! the sequenced log (`REQ-ST-4`).
//!
//! A transfer leaves the ingress in `Received`, `Refused` or `Accepted`; the core moves it on
//! (`F-003`). Nothing a request says about a chain is taken as observed (`REQ-RQ-7`): the amount
//! and the token of an arrival come from its observation.

use crate::address::well_formed;
use crate::api::{
    DepositRequest, DestinationBody, RefundRequest, TransferStatus, WithdrawalExtrasRequest,
    parse_hex,
};
use crate::app::{App, Body};
use crate::auth::{Grant, Requester};
use crate::error::{ApiError, ErrorBody};
use crate::query::status_of_intake;
use crate::refs;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use bezent_core::{Coordinate, Position};
use bezent_core::{Listing, listing};
use serde::Serialize;
use sqlx::{PgConnection, Postgres, Transaction};

const SOURCE_NOT_FOUND: &str = "ERR-SourceNotFound";
const SOURCE_CONSUMED: &str = "ERR-SourceConsumed";
const COMMITMENT_MISMATCH: &str = "ERR-CommitmentMismatch";
const REFERENCE_CONFLICT: &str = "ERR-ReferenceConflict";
const UNKNOWN_ROUTE: &str = "ERR-UnknownRoute";
const ROUTE_PAUSED: &str = "ERR-RoutePaused";
const BELOW_MINIMUM: &str = "ERR-BelowMinimum";
const ABOVE_MAXIMUM: &str = "ERR-AboveMaximum";
const WINDOW_EXCEEDED: &str = "ERR-WindowExceeded";
const INVALID_DESTINATION: &str = "ERR-InvalidDestination";

/// What a request came to.
enum Outcome {
    /// It made or reached this transfer; `error` is its refusal, if it was refused.
    Transfer {
        id: i64,
        error: Option<&'static str>,
    },
    /// It made this transfer, in `Received`, within the route's rules: the answer and the
    /// transition to `Accepted` are recorded together.
    Accepted { id: i64, bridge_reference: String },
    /// It was refused before it named anything bezent can bind.
    Refused(&'static str),
    /// It names an arrival bezent observed that is not final yet: nothing is recorded, and the
    /// same request may be sent again once it is (`REQ-RQ-3`, `REQ-RQ-6`, `REQ-IN-2`).
    NotFinal,
    /// bezent cannot take it now; nothing is recorded (`REQ-LM-4`).
    Overloaded,
}

/// The answer to a request: 201 when it is taken in now, 200 for a repeat of one taken in before.
type Answer = Result<(StatusCode, Json<TransferStatus>), ApiError>;

/// Records the request, or finds the one it repeats (`REQ-ID-1`).
enum Recorded {
    New(i64),
    Repeat(i64),
}

async fn record(
    tx: &mut PgConnection,
    requester: &str,
    kind: &str,
    reference: &str,
    body: &impl Serialize,
) -> Result<Recorded, ApiError> {
    let canonical = serde_json::to_vec(body).map_err(|_| ApiError::Internal)?;
    let digest = refs::body_digest(kind, &canonical);
    let json: serde_json::Value =
        serde_json::from_slice(&canonical).map_err(|_| ApiError::Internal)?;

    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO intake (request_reference, requester, kind, body, body_digest) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT ON CONSTRAINT intake_reference_once_per_requester DO NOTHING \
         RETURNING id",
    )
    .bind(reference)
    .bind(requester)
    .bind(kind)
    .bind(sqlx::types::Json(&json))
    .bind(&digest[..])
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(id) = id {
        return Ok(Recorded::New(id));
    }

    let (id, original_kind, original_digest): (i64, String, Vec<u8>) = sqlx::query_as(
        "SELECT id, kind, body_digest FROM intake WHERE requester = $1 AND request_reference = $2",
    )
    .bind(requester)
    .bind(reference)
    .fetch_one(&mut *tx)
    .await?;
    if original_kind == kind && refs::same_digest(&original_digest, &digest) {
        Ok(Recorded::Repeat(id))
    } else {
        Err(ApiError::ReferenceConflict)
    }
}

/// Records the answer and commits, then answers with the status as it stands.
async fn finish(
    app: &App,
    mut tx: Transaction<'static, Postgres>,
    requester: &str,
    kind: &str,
    reference: &str,
    intake_id: i64,
    outcome: Outcome,
) -> Answer {
    match outcome {
        Outcome::Overloaded => {
            tx.rollback().await?;
            return Err(ApiError::Overloaded);
        }
        Outcome::NotFinal => {
            tx.rollback().await?;
            return Err(ApiError::NotFinal);
        }
        Outcome::Transfer { id, error } => {
            sqlx::query(
                "INSERT INTO intake_answers (intake_id, transfer_id, bridge_reference, error) \
                 SELECT $1, id, bridge_reference, $3::refusal_error FROM transfers WHERE id = $2",
            )
            .bind(intake_id)
            .bind(id)
            .bind(error)
            .execute(&mut *tx)
            .await?;
        }
        Outcome::Accepted {
            id,
            bridge_reference,
        } => {
            sqlx::query(
                "WITH answer AS ( \
                   INSERT INTO intake_answers (intake_id, transfer_id, bridge_reference) \
                   VALUES ($1, $2, $3)) \
                 INSERT INTO transitions (transfer_id, from_state, to_state) \
                 VALUES ($2, 'Received', 'Accepted')",
            )
            .bind(intake_id)
            .bind(id)
            .bind(&bridge_reference)
            .execute(&mut *tx)
            .await?;
        }
        Outcome::Refused(error) => {
            let bridge_reference = refs::refused(requester, reference);
            sqlx::query(
                "INSERT INTO intake_answers (intake_id, bridge_reference, error) \
                 VALUES ($1, $2, $3::refusal_error)",
            )
            .bind(intake_id)
            .bind(&bridge_reference)
            .bind(error)
            .execute(&mut *tx)
            .await?;
            // REQ-ST-4: a refusal is a state, so it is in the log too.
            sqlx::query(
                "INSERT INTO event_log (kind, payload) VALUES ('refusal', jsonb_build_object( \
                   'bridge_reference', $1::text, 'request_reference', $2::text, \
                   'requester', $3::text, 'kind', $4::text, 'state', 'Refused', \
                   'error', $5::text))",
            )
            .bind(&bridge_reference)
            .bind(reference)
            .bind(requester)
            .bind(kind)
            .bind(error)
            .execute(&mut *tx)
            .await?;
        }
    }
    // A store error on COMMIT means it did not commit; a lost connection leaves it unknown.
    match tx.commit().await {
        Ok(()) => {}
        Err(e @ sqlx::Error::Database(_)) => return Err(e.into()),
        Err(e) => {
            tracing::error!(error = %e, requester, kind, "the commit's outcome is unknown");
            return Err(ApiError::OutcomeUnknown);
        }
    }
    // Taken in from here on: a failed read never answers "nothing recorded" (REQ-RQ-1).
    let status = status_of_intake(app.pool(), intake_id)
        .await
        .map_err(|_| ApiError::TakenIn)?;
    tracing::info!(
        requester,
        kind,
        bridge_reference = %status.bridge_reference,
        state = %status.state,
        error = status.error.as_deref().unwrap_or(""),
        "request taken in"
    );
    Ok((StatusCode::CREATED, Json(status)))
}

/// Answers a repeat with the original's status (`REQ-ID-1`).
async fn replay(app: &App, tx: Transaction<'static, Postgres>, intake_id: i64) -> Answer {
    tx.rollback().await?;
    Ok((
        StatusCode::OK,
        Json(status_of_intake(app.pool(), intake_id).await?),
    ))
}

/// What the endpoints observed at a coordinate.
struct Arrival {
    token: String,
    amount: String,
    recipient: String,
}

/// What a request may name, by its kind (`source_bindable()`, migration 0034).
#[derive(Clone, Copy)]
enum Purpose {
    /// A deposit: an `arrival` or a `conversion_output` at a reserve (`REQ-RQ-3`, `REQ-IN-3`,
    /// `REQ-CV-2`); never a `convertible_arrival` (`REQ-CV-1`).
    Deposit,
    /// The refund of an unassociated arrival: any arrival at a reserve (`REQ-RQ-6`, `REQ-IN-6`).
    Refund,
}

impl Purpose {
    fn as_str(self) -> &'static str {
        match self {
            Purpose::Deposit => "deposit",
            Purpose::Refund => "refund",
        }
    }
}

/// What is at a coordinate, for a request of a purpose.
enum Source {
    /// A final observation of a kind the request may name.
    Final(Arrival),
    /// An observation of a kind the request may name, not final yet (and not withdrawn).
    NotFinal,
    /// Nothing the request may name: no observation, one of another kind, or one withdrawn
    /// before finality (`REQ-IN-7`).
    None,
}

/// An observation at a coordinate: token, amount, recipient, its outcome (`final` or `withdrawn`;
/// none while it is open) and whether its kind is one the request may name.
type ObservedRow = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
);

/// The source at `c` for a request of `purpose`: an observation whose kind the endpoint wrote and
/// the store's `source_bindable()` admits for it, executed, at a reserve, carrying a token, an
/// amount and a recipient (`REQ-RQ-3`, `REQ-RQ-6`, `REQ-IN-3`, `REQ-IN-6`, `REQ-CV-1`, `REQ-CV-2`,
/// `INV-2`). An observation without an explicit bindable kind is none (fail closed). That it is
/// still unconsumed is decided by the callers, and backed by the store.
async fn source(
    conn: &mut PgConnection,
    c: &Coordinate,
    purpose: Purpose,
) -> Result<Source, ApiError> {
    let (kind, index) = position(c);
    let row: Option<ObservedRow> = sqlx::query_as(
        "SELECT o.token, o.amount::text, o.recipient, oo.outcome, \
                    source_bindable($6, o.network, o.block_number, o.block_hash, \
                                    o.position_kind, o.position_index) \
             FROM observations o \
             LEFT JOIN observation_outcomes oo ON oo.observation_id = o.id \
             WHERE o.network = $1 AND o.block_number = $2 AND o.block_hash = $3 \
               AND o.position_kind = $4::position_kind AND o.position_index = $5",
    )
    .bind(c.network.as_str())
    .bind(block_number(c)?)
    .bind(c.block_hash.as_bytes())
    .bind(kind)
    .bind(index)
    .bind(purpose.as_str())
    .fetch_optional(conn)
    .await?;
    Ok(match row {
        Some((Some(token), Some(amount), Some(recipient), Some(outcome), true))
            if outcome == "final" =>
        {
            Source::Final(Arrival {
                token,
                amount,
                recipient,
            })
        }
        Some((_, _, _, None, true)) => Source::NotFinal,
        _ => Source::None,
    })
}

/// Whether the arrival at `c` is from an address the operator declared it funds `asset`'s
/// reserve from, at or after the declaration's block: a surplus contribution, which no deposit
/// binds (`REQ-BK-5`, `REQ-IN-3`, `INV-1`; review MF1). The store's `funding_arrival()` rule,
/// which the store also enforces on the binding itself.
async fn funding_arrival(
    conn: &mut PgConnection,
    asset: &str,
    c: &Coordinate,
) -> Result<bool, ApiError> {
    let (kind, index) = position(c);
    let funding: Option<bool> = sqlx::query_scalar(
        "SELECT funding_arrival($6, o.network, o.sender, o.block_number) FROM observations o \
         WHERE o.network = $1 AND o.block_number = $2 AND o.block_hash = $3 \
           AND o.position_kind = $4::position_kind AND o.position_index = $5",
    )
    .bind(c.network.as_str())
    .bind(block_number(c)?)
    .bind(c.block_hash.as_bytes())
    .bind(kind)
    .bind(index)
    .bind(asset)
    .fetch_optional(conn)
    .await?;
    Ok(funding.unwrap_or(false))
}

fn position(c: &Coordinate) -> (&'static str, i64) {
    match c.position {
        Position::Transaction(i) => ("transaction", i64::from(i)),
        Position::Log(i) => ("log", i64::from(i)),
    }
}

fn block_number(c: &Coordinate) -> Result<i64, ApiError> {
    i64::try_from(c.block_number)
        .map_err(|_| ApiError::Malformed("coordinate.block_number is out of range".into()))
}

/// The bridged asset an admitted token backs on a network (`REQ-AS-2`).
async fn admitted_asset(
    conn: &mut PgConnection,
    network: &str,
    token: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT asset FROM admitted_tokens WHERE network = $1 AND token = $2")
        .bind(network)
        .bind(token)
        .fetch_optional(conn)
        .await
}

/// Whether `d` is well formed, on `network`, and not a declared ledger contract account
/// (`ERR-InvalidDestination`, `REQ-IN-5`).
async fn destination_ok(
    conn: &mut PgConnection,
    d: &DestinationBody,
    network: &str,
) -> Result<bool, sqlx::Error> {
    if d.network != network {
        return Ok(false);
    }
    let format: Option<String> =
        sqlx::query_scalar("SELECT address_format FROM networks WHERE network = $1")
            .bind(network)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(format) = format.filter(|f| well_formed(f, &d.address)) else {
        return Ok(false);
    };
    // REQ-IN-5, DEC-15: delivering into a ledger is the requester's, from its own account. A
    // destination on the declared list of ledger contract accounts credits no one: refused. The
    // account is compared by its canonical bytes, not its spelling; a list that cannot be read
    // back refuses too (never fails open).
    let declared: Vec<(String, Vec<u8>)> =
        sqlx::query_as("SELECT address, account FROM ledger_accounts_listed WHERE network = $1")
            .bind(network)
            .fetch_all(conn)
            .await?;
    Ok(listing(&format, &d.address, &declared) == Listing::Clear)
}

/// The issuing network (§0).
async fn issuing_network(conn: &mut PgConnection) -> Result<String, ApiError> {
    let network: Option<String> =
        sqlx::query_scalar("SELECT network FROM networks WHERE role = 'issuing'")
            .fetch_optional(conn)
            .await?;
    network.ok_or_else(|| {
        tracing::error!("no issuing network is declared");
        ApiError::Internal
    })
}

/// Whether a transfer that has not ended with nothing moved binds `c` (`INV-2`).
async fn consumed(conn: &mut PgConnection, c: &Coordinate) -> Result<bool, ApiError> {
    let (kind, index) = position(c);
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM transfers \
           WHERE source_network = $1 AND source_block_number = $2 AND source_block_hash = $3 \
             AND source_position_kind = $4::position_kind AND source_position_index = $5 \
             AND refund_of IS NULL AND state NOT IN ('Refused', 'Expired', 'Withdrawn'))",
    )
    .bind(c.network.as_str())
    .bind(block_number(c)?)
    .bind(c.block_hash.as_bytes())
    .bind(kind)
    .bind(index)
    .fetch_one(conn)
    .await?)
}

/// A route's declared limits, and whether it is paused (`REQ-LM-1`, `REQ-GV-1`).
#[derive(sqlx::FromRow)]
struct RouteLimits {
    minimum: String,
    maximum: String,
    venue_minimum: Option<String>,
    window_limit: String,
    window_seconds: i64,
    capacity: i64,
    paused: bool,
}

async fn route_limits(
    conn: &mut PgConnection,
    asset: &str,
    network: &str,
    direction: &str,
) -> Result<Option<RouteLimits>, sqlx::Error> {
    sqlx::query_as(
        "SELECT r.minimum::text AS minimum, r.maximum::text AS maximum, \
                r.venue_minimum::text AS venue_minimum, r.window_limit::text AS window_limit, \
                r.window_seconds, r.capacity, \
                EXISTS (SELECT 1 FROM pauses p \
                        WHERE (p.asset IS NULL OR p.asset = r.asset) \
                          AND (p.network IS NULL OR p.network = r.network) \
                          AND (p.direction IS NULL OR p.direction = r.direction)) AS paused \
         FROM routes r WHERE r.asset = $1 AND r.network = $2 AND r.direction = $3",
    )
    .bind(asset)
    .bind(network)
    .bind(direction)
    .fetch_optional(conn)
    .await
}

/// Takes the route's lock until the transaction ends, and reads what its window has used and how
/// many of its transfers are in flight, as of after the lock (`route_usage_locked`, 0011). The
/// lock serialises every decision that reads them, so the window and the capacity hold exactly
/// (`REQ-LM-1`, `REQ-LM-4`); everything that does not read them is decided before it.
async fn lock_route_usage(
    conn: &mut PgConnection,
    asset: &str,
    network: &str,
    direction: &str,
    window_seconds: i64,
) -> Result<(String, i64), sqlx::Error> {
    sqlx::query_as("SELECT used, in_flight FROM route_usage_locked($1, $2, $3, $4)")
        .bind(asset)
        .bind(network)
        .bind(direction)
        .bind(window_seconds)
        .fetch_one(conn)
        .await
}

/// A transfer as the request asks it.
struct NewTransfer<'a> {
    bridge_reference: String,
    request_reference: &'a str,
    kind: &'static str,
    /// The bridged asset; `None` only on the refund of an arrival whose token is not admitted,
    /// which names `token` instead (`REQ-IN-6`).
    asset: Option<&'a str>,
    token: Option<&'a str>,
    amount: &'a str,
    destination: &'a DestinationBody,
    refund_destination: Option<&'a DestinationBody>,
    source: Option<&'a Coordinate>,
}

/// Inserts the transfer in `Received`. `None` when the store already holds a transfer that binds
/// the same source, or carries the same bridge reference (`INV-2`): the insert does nothing.
async fn insert_transfer(
    conn: &mut PgConnection,
    t: &NewTransfer<'_>,
) -> Result<Option<i64>, sqlx::Error> {
    // The coordinate was parsed with its block number in the store's range.
    let source = t.source.map(|c| {
        let (kind, index) = position(c);
        (
            c.network.as_str(),
            i64::try_from(c.block_number).unwrap_or(i64::MAX),
            c.block_hash.as_bytes(),
            kind,
            index,
        )
    });
    sqlx::query_scalar(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, refund_destination_network, refund_destination, \
             source_network, source_block_number, source_block_hash, source_position_kind, \
             source_position_index, state, token) \
         VALUES ($1, $2, $3, $4, $5::numeric, $6, $7, $8, $9, $10, $11, $12, \
             $13::position_kind, $14, 'Received', $15) \
         ON CONFLICT DO NOTHING \
         RETURNING id",
    )
    .bind(&t.bridge_reference)
    .bind(t.request_reference)
    .bind(t.kind)
    .bind(t.asset)
    .bind(t.amount)
    .bind(&t.destination.network)
    .bind(&t.destination.address)
    .bind(t.refund_destination.map(|d| d.network.as_str()))
    .bind(t.refund_destination.map(|d| d.address.as_str()))
    .bind(source.map(|s| s.0))
    .bind(source.map(|s| s.1))
    .bind(source.map(|s| s.2))
    .bind(source.map(|s| s.3))
    .bind(source.map(|s| s.4))
    .bind(t.token)
    .fetch_optional(conn)
    .await
}

/// Appends the transition out of `Received` (§5.4): to `Accepted`, or to `Refused` with its error.
async fn decide(
    conn: &mut PgConnection,
    id: i64,
    error: Option<&'static str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO transitions (transfer_id, from_state, to_state, error) \
         VALUES ($1, 'Received', $2::transfer_state, $3::refusal_error)",
    )
    .bind(id)
    .bind(if error.is_some() {
        "Refused"
    } else {
        "Accepted"
    })
    .bind(error)
    .execute(conn)
    .await
    .map(|_| ())
}

/// Makes the transfer in `Refused` with `error`. It binds nothing (`INV-2`: a refused transfer
/// consumed nothing), so its bridge reference is the refusal's, not the coordinate's. A request
/// that names no bridged asset makes no transfer: its refusal is the answer alone.
async fn refuse(
    conn: &mut PgConnection,
    requester: &str,
    t: NewTransfer<'_>,
    error: &'static str,
) -> Result<Outcome, ApiError> {
    if t.asset.is_none() {
        return Ok(Outcome::Refused(error));
    }
    let t = NewTransfer {
        bridge_reference: refs::refused(requester, t.request_reference),
        source: None,
        ..t
    };
    let Some(id) = insert_transfer(conn, &t).await? else {
        tracing::error!("a refused transfer's own bridge reference is taken");
        return Err(ApiError::Internal);
    };
    decide(conn, id, Some(error)).await?;
    Ok(Outcome::Transfer {
        id,
        error: Some(error),
    })
}

/// Makes the transfer bound to its source. If another transfer bound the same source in the
/// meantime, the store's unique index on the binding (or on the coordinate's bridge reference)
/// makes the insert do nothing (`INV-2`), and the request is refused with `ERR-SourceConsumed`.
async fn insert_bound(
    conn: &mut PgConnection,
    requester: &str,
    t: NewTransfer<'_>,
) -> Result<Result<(i64, String), Outcome>, ApiError> {
    match insert_transfer(conn, &t).await? {
        Some(id) => Ok(Ok((id, t.bridge_reference))),
        None => Ok(Err(refuse(conn, requester, t, SOURCE_CONSUMED).await?)),
    }
}

fn amount_of(text: &str) -> Option<u128> {
    text.parse().ok()
}

/// Request a deposit (`REQ-RQ-3`): bind the arrival at a coordinate.
#[utoipa::path(
    post, path = "/v0/deposits", tag = "requests",
    request_body = DepositRequest,
    security(("bearer" = [])),
    responses(
        (status = 201, description = "Taken in now: `Accepted`, or `Refused` with its named error", body = TransferStatus),
        (status = 200, description = "A repeat: the original's status (REQ-ID-1)", body = TransferStatus),
        (status = 202, description = "taken-in: taken in, but the status could not be read back; read it by request reference", body = ErrorBody),
        (status = 400, body = ErrorBody), (status = 401, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 503, description = "ERR-Overloaded: nothing recorded; retry. not-final: the arrival is observed but not final yet; nothing recorded, the request reference is not consumed: send the same request again once it is final. outcome-unknown: repeat the same request", body = ErrorBody),
    )
)]
pub(crate) async fn deposit(
    State(app): State<App>,
    who: Requester,
    Body(request): Body<DepositRequest>,
) -> Answer {
    who.require(Grant::Deposit)?;
    let request = request.canonical()?;
    let c = request.coordinate.parse()?;
    let _permit = app.permit()?;
    let mut tx = app.pool().begin().await?;
    let reference = request.request_reference.as_str();
    let intake_id = match record(&mut tx, &who.name, "deposit", reference, &request).await? {
        Recorded::New(id) => id,
        Recorded::Repeat(id) => return replay(&app, tx, id).await,
    };
    let outcome = decide_deposit(&mut tx, &who.name, &request, &c).await?;
    finish(
        &app, tx, &who.name, "deposit", reference, intake_id, outcome,
    )
    .await
}

async fn decide_deposit(
    conn: &mut PgConnection,
    requester: &str,
    request: &DepositRequest,
    c: &Coordinate,
) -> Result<Outcome, ApiError> {
    let network = c.network.as_str();
    let arrival = match source(conn, c, Purpose::Deposit).await? {
        Source::Final(arrival) => arrival,
        Source::NotFinal => return Ok(Outcome::NotFinal),
        Source::None => return Ok(Outcome::Refused(SOURCE_NOT_FOUND)),
    };
    let Some(asset) = admitted_asset(conn, network, &arrival.token).await? else {
        return Ok(Outcome::Refused(UNKNOWN_ROUTE));
    };
    // An arrival a deposit binds is one at the asset's reserve address (§5.5).
    let reserve: Option<String> =
        sqlx::query_scalar("SELECT address FROM reserves WHERE asset = $1 AND network = $2")
            .bind(&asset)
            .bind(network)
            .fetch_optional(&mut *conn)
            .await?;
    if reserve.as_deref() != Some(arrival.recipient.as_str()) {
        return Ok(Outcome::Refused(SOURCE_NOT_FOUND));
    }
    // An operator top-up is surplus, not a movement a deposit can bind (review MF1; INV-1, INV-2,
    // REQ-IN-3): ERR-SourceNotFound, "the coordinate does not hold a movement bezent can bind".
    if funding_arrival(conn, &asset, c).await? {
        return Ok(Outcome::Refused(SOURCE_NOT_FOUND));
    }

    let t = NewTransfer {
        bridge_reference: refs::deposit(c),
        request_reference: &request.request_reference,
        kind: "deposit",
        asset: Some(&asset),
        token: None,
        amount: &arrival.amount,
        destination: &request.destination,
        refund_destination: Some(&request.refund_destination),
        source: Some(c),
    };

    let issuing = issuing_network(conn).await?;
    if !destination_ok(conn, &request.destination, &issuing).await?
        || !destination_ok(conn, &request.refund_destination, network).await?
    {
        return refuse(conn, requester, t, INVALID_DESTINATION).await;
    }

    // Nothing up to the route lock reads the window or the capacity. A coordinate bound by a
    // request that commits after this read is caught by the store's index on the binding.
    let Some(route) = route_limits(conn, &asset, network, "deposit").await? else {
        return refuse(conn, requester, t, UNKNOWN_ROUTE).await;
    };
    if route.paused {
        return refuse(conn, requester, t, ROUTE_PAUSED).await;
    }
    if consumed(conn, c).await? {
        return refuse(conn, requester, t, SOURCE_CONSUMED).await;
    }
    let Some(amount) = amount_of(&arrival.amount) else {
        return refuse(conn, requester, t, ABOVE_MAXIMUM).await;
    };
    if let Some(error) = per_transfer_limits(&route, amount) {
        return refuse(conn, requester, t, error).await;
    }

    // From here to the commit the route is locked: the usage read, the insert, the answer with
    // the transition, and the commit.
    let (used, in_flight) =
        lock_route_usage(conn, &asset, network, "deposit", route.window_seconds).await?;
    if !within_window(&route, &used, amount) {
        return refuse(conn, requester, t, WINDOW_EXCEEDED).await;
    }
    if in_flight >= route.capacity {
        return Ok(Outcome::Overloaded);
    }
    match insert_bound(conn, requester, t).await? {
        Ok((id, bridge_reference)) => Ok(Outcome::Accepted {
            id,
            bridge_reference,
        }),
        Err(refused) => Ok(refused),
    }
}

/// A route's per-transfer limits (`REQ-LM-1`, `REQ-LM-2`).
fn per_transfer_limits(route: &RouteLimits, amount: u128) -> Option<&'static str> {
    let (Some(minimum), Some(maximum)) = (amount_of(&route.minimum), amount_of(&route.maximum))
    else {
        return Some(ABOVE_MAXIMUM);
    };
    let venue_minimum = route.venue_minimum.as_deref().and_then(amount_of);
    if amount < minimum || venue_minimum.is_some_and(|v| amount < v) {
        return Some(BELOW_MINIMUM);
    }
    if amount > maximum {
        return Some(ABOVE_MAXIMUM);
    }
    None
}

/// Whether `amount` fits in what the route's trailing window has left (`REQ-LM-1`).
fn within_window(route: &RouteLimits, used: &str, amount: u128) -> bool {
    let limit = amount_of(&route.window_limit);
    amount_of(used)
        .and_then(|used| used.checked_add(amount))
        .zip(limit)
        .is_some_and(|(total, limit)| total <= limit)
}

/// Request the refund of an unassociated arrival (`REQ-RQ-6`). The refund waits in `Received` for
/// the operator's second approver (`REQ-IN-6`, `F-014`).
#[utoipa::path(
    post, path = "/v0/refunds", tag = "requests",
    request_body = RefundRequest,
    security(("bearer" = [])),
    responses(
        (status = 201, description = "Taken in now: `Received` (awaiting the operator's second approver), or `Refused`", body = TransferStatus),
        (status = 200, description = "A repeat: the original's status (REQ-ID-1)", body = TransferStatus),
        (status = 202, description = "taken-in: taken in, but the status could not be read back; read it by request reference", body = ErrorBody),
        (status = 400, body = ErrorBody), (status = 401, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 503, description = "ERR-Overloaded: nothing recorded; retry. not-final: the arrival is observed but not final yet; nothing recorded, the request reference is not consumed: send the same request again once it is final. outcome-unknown: repeat the same request", body = ErrorBody),
    )
)]
pub(crate) async fn refund(
    State(app): State<App>,
    who: Requester,
    Body(request): Body<RefundRequest>,
) -> Answer {
    who.require(Grant::Refund)?;
    let request = request.canonical()?;
    let c = request.coordinate.parse()?;
    let _permit = app.permit()?;
    let mut tx = app.pool().begin().await?;
    let reference = request.request_reference.as_str();
    let intake_id = match record(&mut tx, &who.name, "refund", reference, &request).await? {
        Recorded::New(id) => id,
        Recorded::Repeat(id) => return replay(&app, tx, id).await,
    };
    let outcome = decide_refund(&mut tx, &who.name, &request, &c).await?;
    finish(&app, tx, &who.name, "refund", reference, intake_id, outcome).await
}

async fn decide_refund(
    conn: &mut PgConnection,
    requester: &str,
    request: &RefundRequest,
    c: &Coordinate,
) -> Result<Outcome, ApiError> {
    let network = c.network.as_str();
    let arrival = match source(conn, c, Purpose::Refund).await? {
        Source::Final(arrival) => arrival,
        Source::NotFinal => return Ok(Outcome::NotFinal),
        Source::None => return Ok(Outcome::Refused(SOURCE_NOT_FOUND)),
    };
    let at_reserve: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM reserves WHERE network = $1 AND address = $2)",
    )
    .bind(network)
    .bind(&arrival.recipient)
    .fetch_one(&mut *conn)
    .await?;
    if !at_reserve {
        return Ok(Outcome::Refused(SOURCE_NOT_FOUND));
    }
    // A token that is not admitted needs no asset to be refunded: the refund names the token,
    // and its amount is in the token's minor units (REQ-IN-6; amendment 0010, "Not a gap").
    let asset = admitted_asset(conn, network, &arrival.token).await?;
    // An operator top-up is surplus, not an unassociated arrival a requester can have refunded
    // (INV-1, INV-2, REQ-IN-3, REQ-RQ-6; review SF-A): the same rule as a deposit's, the same
    // answer. The store enforces it on the binding as well.
    if let Some(asset) = asset.as_deref()
        && funding_arrival(conn, asset, c).await?
    {
        return Ok(Outcome::Refused(SOURCE_NOT_FOUND));
    }

    let t = NewTransfer {
        bridge_reference: refs::unassociated_refund(c),
        request_reference: &request.request_reference,
        kind: "refund",
        asset: asset.as_deref(),
        token: if asset.is_none() {
            Some(&arrival.token)
        } else {
            None
        },
        amount: &arrival.amount,
        destination: &request.destination,
        refund_destination: None,
        source: Some(c),
    };
    if !destination_ok(conn, &request.destination, network).await? {
        return refuse(conn, requester, t, INVALID_DESTINATION).await;
    }
    if consumed(conn, c).await? {
        return refuse(conn, requester, t, SOURCE_CONSUMED).await;
    }
    match insert_bound(conn, requester, t).await? {
        Ok((id, _)) => Ok(Outcome::Transfer { id, error: None }),
        Err(refused) => Ok(refused),
    }
}

/// Add a withdrawal's extras by its redemption's reference (`REQ-RQ-4`, `REQ-RQ-9`).
#[utoipa::path(
    post, path = "/v0/withdrawals/extras", tag = "requests",
    request_body = WithdrawalExtrasRequest,
    security(("bearer" = [])),
    responses(
        (status = 201, description = "Taken in now: the withdrawal's status, or `Refused` with its named error", body = TransferStatus),
        (status = 200, description = "A repeat: the original's status (REQ-ID-1)", body = TransferStatus),
        (status = 202, description = "taken-in: taken in, but the status could not be read back; read it by request reference", body = ErrorBody),
        (status = 400, body = ErrorBody), (status = 401, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 503, body = ErrorBody),
    )
)]
pub(crate) async fn withdrawal_extras(
    State(app): State<App>,
    who: Requester,
    Body(request): Body<WithdrawalExtrasRequest>,
) -> Answer {
    who.require(Grant::Withdrawal)?;
    let request = request.canonical()?;
    let _permit = app.permit()?;
    let mut tx = app.pool().begin().await?;
    let reference = request.request_reference.as_str();
    let intake_id = match record(&mut tx, &who.name, "withdrawal", reference, &request).await? {
        Recorded::New(id) => id,
        Recorded::Repeat(id) => return replay(&app, tx, id).await,
    };
    // An expiry already past hands the core a withdrawal it can only expire (REQ-LM-2). Checked
    // for a new request only, so a repeat still answers with the original (REQ-ID-1); nothing is
    // recorded.
    if request.expires_at.is_some_and(|e| e <= chrono::Utc::now()) {
        tx.rollback().await?;
        return Err(ApiError::Malformed(
            "expires_at is not in the future (REQ-LM-2)".into(),
        ));
    }
    let outcome = decide_extras(&mut tx, &who, intake_id, &request).await?;
    finish(
        &app,
        tx,
        &who.name,
        "withdrawal",
        reference,
        intake_id,
        outcome,
    )
    .await
}

#[derive(sqlx::FromRow)]
struct Withdrawal {
    id: i64,
    terminal: bool,
    has_expiry: bool,
    assurance_class: String,
    commitment: Option<String>,
    has_extras: bool,
}

async fn decide_extras(
    conn: &mut PgConnection,
    who: &Requester,
    intake_id: i64,
    request: &WithdrawalExtrasRequest,
) -> Result<Outcome, ApiError> {
    let redemption = parse_hex("redemption_reference", &request.redemption_reference)?;
    // The withdrawal the redemption funds (REQ-OT-2), locked first. Under READ COMMITTED a
    // statement that waits for a row lock keeps the snapshot it took before waiting, so what the
    // extras are checked against is read by a new statement once the lock is held: a request
    // that waited sees the extras the holder wrote (REQ-RQ-4, REQ-ID-4). The store backs it with
    // one answer per transfer (intake_answers_transfer_once, 0011).
    let id: Option<i64> = sqlx::query_scalar(
        "SELECT t.id FROM transfers t \
         WHERE t.kind = 'withdrawal' AND t.refund_of IS NULL AND t.on_chain_reference = $1 \
         ORDER BY is_terminal_state(t.state), t.id DESC \
         LIMIT 1 \
         FOR UPDATE",
    )
    .bind(&redemption)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(id) = id else {
        return Ok(Outcome::Refused(SOURCE_NOT_FOUND));
    };
    // With the commitment its redemption carried: the redemption's observation records it in the
    // network's own data (REQ-ST-3).
    let w: Withdrawal = sqlx::query_as(
        "SELECT t.id, is_terminal_state(t.state) AS terminal, \
                t.expires_at IS NOT NULL AS has_expiry, a.assurance_class, \
                (SELECT o.detail ->> 'commitment' FROM observations o \
                   WHERE o.network = t.source_network AND o.block_number = t.source_block_number \
                     AND o.block_hash = t.source_block_hash \
                     AND o.position_kind = t.source_position_kind \
                     AND o.position_index = t.source_position_index) AS commitment, \
                EXISTS (SELECT 1 FROM intake_answers x WHERE x.transfer_id = t.id) AS has_extras \
         FROM transfers t JOIN assets a ON a.asset = t.asset \
         WHERE t.id = $1",
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    // For an attested backend only the requester holding the attesting grant may add extras
    // (REQ-RQ-4, REQ-PN-2). Nothing is recorded: the request is not this requester's to make.
    if w.assurance_class == "attested" {
        who.require(Grant::Attest)?;
    }
    // Never to a finished withdrawal.
    if w.terminal {
        return Ok(Outcome::Refused(SOURCE_CONSUMED));
    }
    // Extras are written once (REQ-RQ-4): the first accepted wins, and a later request is a
    // different body for the same reference.
    if w.has_extras || (request.expires_at.is_some() && w.has_expiry) {
        return Ok(Outcome::Refused(REFERENCE_CONFLICT));
    }
    let relayed = match &request.instructions {
        None => None,
        Some(hex) => {
            // REQ-RQ-9: only an attested backend's redemption carries a commitment, and relayed
            // instructions are accepted only when they hash to it.
            let instructions = parse_hex("instructions", hex)?;
            let commitment = w
                .commitment
                .as_deref()
                .and_then(|c| hex::decode(c.strip_prefix("0x").unwrap_or(c)).ok());
            match commitment {
                Some(commitment)
                    if w.assurance_class == "attested"
                        && refs::same_digest(&refs::commitment(&instructions), &commitment) =>
                {
                    Some((instructions, commitment))
                }
                _ => return Ok(Outcome::Refused(COMMITMENT_MISMATCH)),
            }
        }
    };

    if let Some(expires_at) = request.expires_at {
        sqlx::query("UPDATE transfers SET expires_at = $2 WHERE id = $1")
            .bind(w.id)
            .bind(expires_at)
            .execute(&mut *conn)
            .await?;
    }
    if let Some((instructions, commitment)) = relayed {
        sqlx::query(
            "INSERT INTO relayed_instructions (transfer_id, intake_id, instructions, commitment) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(w.id)
        .bind(intake_id)
        .bind(&instructions)
        .bind(&commitment)
        .execute(&mut *conn)
        .await?;
    }
    Ok(Outcome::Transfer {
        id: w.id,
        error: None,
    })
}
