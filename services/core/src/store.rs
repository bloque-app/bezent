// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! What the core reads from the store, and the one way it changes a transfer: appending a
//! transition (`REQ-TR-2`, `REQ-TR-6`). Every query runs on one connection of the core's own pool,
//! which logs in as the core's login (a member of `bezent_core` only, F-005 review S5, #46); no role is
//! ever switched on a pooled connection (issue #27).

use bezent_core::{Condition, State};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgConnection;

use crate::refs::Coordinate;

/// A transfer, as the core reads it: the columns of `transfers` of the same names.
#[allow(missing_docs)]
#[derive(sqlx::FromRow, Clone, Debug)]
pub struct Transfer {
    pub id: i64,
    pub bridge_reference: String,
    pub request_reference: String,
    pub kind: String,
    pub asset: Option<String>,
    pub token: Option<String>,
    /// Exact minor units of the asset's book unit, as text (`INV-11`).
    pub amount: String,
    pub destination_network: String,
    pub destination: String,
    pub refund_of: Option<i64>,
    pub expires_at: Option<DateTime<Utc>>,
    pub source_network: Option<String>,
    pub source_block_number: Option<i64>,
    pub source_block_hash: Option<Vec<u8>>,
    pub source_position_kind: Option<String>,
    pub source_position_index: Option<i64>,
    pub on_chain_reference: Option<Vec<u8>>,
    /// A withdrawal's: the reference its payout writes on the origin network (review M1).
    pub destination_reference: Option<Vec<u8>>,
    pub redeemer: Option<String>,
    pub state: String,
    pub condition: Option<String>,
    pub source_final_block_time: Option<DateTime<Utc>>,
    pub destination_final_network: Option<String>,
    pub destination_final_block_number: Option<i64>,
    pub destination_final_block_hash: Option<Vec<u8>>,
    pub destination_final_block_time: Option<DateTime<Utc>>,
}

/// The columns of [`Transfer`], domains cast to text.
pub const TRANSFER: &str = "id, bridge_reference, request_reference, kind, asset, token, \
    amount::text AS amount, destination_network, destination, refund_of, expires_at, \
    source_network, source_block_number, source_block_hash, \
    source_position_kind::text AS source_position_kind, source_position_index, \
    on_chain_reference, destination_reference, redeemer, state::text AS state, \
    condition::text AS condition, \
    source_final_block_time, destination_final_network, destination_final_block_number, \
    destination_final_block_hash, destination_final_block_time";

impl Transfer {
    /// Its state.
    pub fn state(&self) -> State {
        State::parse(&self.state).unwrap_or(State::NeedsReview)
    }

    /// The condition it holds.
    pub fn condition(&self) -> Option<Condition> {
        self.condition.as_deref().and_then(Condition::parse)
    }

    /// Its source binding.
    pub fn source(&self) -> Option<Coordinate> {
        Some(Coordinate {
            network: self.source_network.clone()?,
            block_number: self.source_block_number?,
            block_hash: self.source_block_hash.clone()?,
            position_kind: self.source_position_kind.clone()?,
            position_index: self.source_position_index?,
        })
    }

    /// Its amount (`INV-11`).
    pub fn amount(&self) -> Option<u128> {
        self.amount.parse().ok()
    }

    /// The route it travels: (asset, origin network, direction). A deposit's origin network is its
    /// source's; a withdrawal's, its destination's.
    pub fn route(&self) -> Option<(String, String, &'static str)> {
        let asset = self.asset.clone()?;
        match self.kind.as_str() {
            "deposit" => Some((asset, self.source_network.clone()?, "deposit")),
            "withdrawal" => Some((asset, self.destination_network.clone(), "withdrawal")),
            _ => None,
        }
    }
}

/// A point on a network a transition rests on: block number, hash and time (`REQ-TR-2`), and
/// where in the block, when it is a movement's own coordinate.
#[allow(missing_docs)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Point {
    pub network: String,
    pub block_number: i64,
    pub block_hash: Vec<u8>,
    pub block_time: DateTime<Utc>,
    pub position: Option<(String, i64)>,
}

/// An observation, with its outcome if one is recorded: the columns of the same names.
#[allow(missing_docs)]
#[derive(sqlx::FromRow, Clone, Debug)]
pub struct Observation {
    pub id: i64,
    pub network: String,
    pub block_number: i64,
    pub block_hash: Vec<u8>,
    pub block_time: DateTime<Utc>,
    pub position_kind: String,
    pub position_index: i64,
    pub token: Option<String>,
    pub amount: Option<String>,
    pub sender: Option<String>,
    pub recipient: Option<String>,
    pub on_chain_reference: Option<Vec<u8>>,
    pub executed: bool,
    pub detail: Value,
    pub outcome: Option<String>,
    pub final_block_number: Option<i64>,
    pub final_block_hash: Option<Vec<u8>>,
}

/// The columns of [`Observation`] over `observations o LEFT JOIN observation_outcomes x`.
pub const OBSERVATION: &str = "o.id, o.network, o.block_number, o.block_hash, o.block_time, \
    o.position_kind::text AS position_kind, o.position_index, o.token, o.amount::text AS amount, \
    o.sender, o.recipient, o.on_chain_reference, o.executed, o.detail, x.outcome, \
    x.final_block_number, x.final_block_hash";

impl Observation {
    /// Its own coordinate, as a point.
    pub fn point(&self) -> Point {
        Point {
            network: self.network.clone(),
            block_number: self.block_number,
            block_hash: self.block_hash.clone(),
            block_time: self.block_time,
            position: Some((self.position_kind.clone(), self.position_index)),
        }
    }

    /// Its coordinate.
    pub fn coordinate(&self) -> Coordinate {
        Coordinate {
            network: self.network.clone(),
            block_number: self.block_number,
            block_hash: self.block_hash.clone(),
            position_kind: self.position_kind.clone(),
            position_index: self.position_index,
        }
    }

    /// Its amount (`INV-11`).
    pub fn amount(&self) -> Option<u128> {
        self.amount.as_deref()?.parse().ok()
    }
}

/// Reads one transfer, locked for the rest of the transaction.
pub async fn lock_transfer(conn: &mut PgConnection, id: i64) -> sqlx::Result<Option<Transfer>> {
    sqlx::query_as(&format!(
        "SELECT {TRANSFER} FROM transfers WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(conn)
    .await
}

/// Reads one transfer.
pub async fn transfer(conn: &mut PgConnection, id: i64) -> sqlx::Result<Option<Transfer>> {
    sqlx::query_as(&format!("SELECT {TRANSFER} FROM transfers WHERE id = $1"))
        .bind(id)
        .fetch_optional(conn)
        .await
}

/// The observation at a coordinate.
pub async fn observation_at(
    conn: &mut PgConnection,
    c: &Coordinate,
) -> sqlx::Result<Option<Observation>> {
    sqlx::query_as(&format!(
        "SELECT {OBSERVATION} FROM observations o \
         LEFT JOIN observation_outcomes x ON x.observation_id = o.id \
         WHERE o.network = $1 AND o.block_number = $2 AND o.block_hash = $3 \
           AND o.position_kind = $4::position_kind AND o.position_index = $5"
    ))
    .bind(&c.network)
    .bind(c.block_number)
    .bind(&c.block_hash)
    .bind(&c.position_kind)
    .bind(c.position_index)
    .fetch_optional(conn)
    .await
}

/// The final point (`network`, `number`, `hash`) with its block time, if an endpoint recorded it as
/// a final head (`REQ-OB-5`).
pub async fn final_point(
    conn: &mut PgConnection,
    network: &str,
    number: i64,
    hash: &[u8],
) -> sqlx::Result<Option<Point>> {
    let time: Option<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT block_time FROM final_heads \
         WHERE network = $1 AND block_number = $2 AND block_hash = $3",
    )
    .bind(network)
    .bind(number)
    .bind(hash)
    .fetch_optional(conn)
    .await?;
    Ok(time.map(|block_time| Point {
        network: network.to_owned(),
        block_number: number,
        block_hash: hash.to_vec(),
        block_time,
        position: None,
    }))
}

/// The latest final head recorded for `network`.
pub async fn latest_head(conn: &mut PgConnection, network: &str) -> sqlx::Result<Option<Point>> {
    let row: Option<(i64, Vec<u8>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT block_number, block_hash, block_time FROM final_heads \
         WHERE network = $1 ORDER BY block_number DESC LIMIT 1",
    )
    .bind(network)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|(block_number, block_hash, block_time)| Point {
        network: network.to_owned(),
        block_number,
        block_hash,
        block_time,
        position: None,
    }))
}

/// A transition to append: the columns of `transitions` of the same names.
#[allow(missing_docs)]
#[derive(Clone, Debug)]
pub struct Move<'a> {
    pub from: State,
    pub to: State,
    pub condition: Option<Condition>,
    pub point: Option<&'a Point>,
    pub attempt: Option<i32>,
    pub manual_action: Option<i64>,
}

/// Appends a transition; the store checks it against the §5.4 table and applies it (`REQ-TR-6`).
pub async fn append(conn: &mut PgConnection, transfer: i64, m: &Move<'_>) -> sqlx::Result<()> {
    let p = m.point;
    sqlx::query(
        "INSERT INTO transitions (transfer_id, from_state, to_state, condition, network, \
             block_number, block_hash, block_time, position_kind, position_index, attempt, \
             manual_action_id) \
         VALUES ($1, $2::transfer_state, $3::transfer_state, $4::transfer_condition, $5, $6, $7, \
             $8, $9::position_kind, $10, $11, $12)",
    )
    .bind(transfer)
    .bind(m.from.as_str())
    .bind(m.to.as_str())
    .bind(m.condition.map(Condition::as_str))
    .bind(p.map(|p| p.network.as_str()))
    .bind(p.map(|p| p.block_number))
    .bind(p.map(|p| p.block_hash.as_slice()))
    .bind(p.map(|p| p.block_time))
    .bind(p.and_then(|p| p.position.as_ref().map(|x| x.0.as_str())))
    .bind(p.and_then(|p| p.position.as_ref().map(|x| x.1)))
    .bind(m.attempt)
    .bind(m.manual_action)
    .execute(conn)
    .await
    .map(|_| ())
}

/// The highest delivery attempt a transfer reached (0 for none).
pub async fn current_attempt(conn: &mut PgConnection, transfer: i64) -> sqlx::Result<i32> {
    sqlx::query_scalar("SELECT COALESCE(max(attempt), 0) FROM transitions WHERE transfer_id = $1")
        .bind(transfer)
        .fetch_one(conn)
        .await
}

/// The attempts of a transfer that have an intent (ordered, prepared or signed) and no recorded
/// proof of non-execution (the store's `attempts_unproven`, review M2).
pub async fn attempts_unproven(conn: &mut PgConnection, transfer: i64) -> sqlx::Result<Vec<i32>> {
    sqlx::query_scalar("SELECT attempts_unproven($1)")
        .bind(transfer)
        .fetch_one(conn)
        .await
}

/// Whether any intent (an order or a signing intent) is recorded for a transfer's attempt.
pub async fn attempt_has_intents(
    conn: &mut PgConnection,
    transfer: i64,
    attempt: i32,
) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM intents WHERE transfer_id = $1 AND attempt = $2)",
    )
    .bind(transfer)
    .bind(attempt)
    .fetch_one(conn)
    .await
}

/// The proof of non-execution of a transfer's attempt (F-007's `non_execution_proofs`, read by the
/// store's `attempt_proof`), as its final point with block time, when that point is a recorded
/// final head.
pub async fn proof_of(
    conn: &mut PgConnection,
    transfer: i64,
    attempt: i32,
) -> sqlx::Result<Option<ProofPoint>> {
    let row: (Option<String>, Option<i64>, Option<Vec<u8>>) = sqlx::query_as(
        "SELECT network, final_block_number, final_block_hash FROM attempt_proof($1, $2)",
    )
    .bind(transfer)
    .bind(attempt)
    .fetch_one(&mut *conn)
    .await?;
    let (Some(network), Some(number), Some(hash)) = row else {
        return Ok(None);
    };
    Ok(Some(
        match final_point(conn, &network, number, &hash).await? {
            Some(p) => ProofPoint::Final(p),
            None => ProofPoint::Untimed,
        },
    ))
}

/// A recorded proof of non-execution.
#[derive(Clone, Debug)]
pub enum ProofPoint {
    /// Its final block, with the block time a final head recorded.
    Final(Point),
    /// Recorded, but its final block is not yet a recorded final head: wait.
    Untimed,
}

/// Appends an event to the sequenced log (`REQ-ST-4`).
pub async fn event(
    conn: &mut PgConnection,
    kind: &str,
    transfer: Option<i64>,
    payload: &Value,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO event_log (kind, transfer_id, payload) VALUES ($1, $2, $3::text::jsonb)",
    )
    .bind(kind)
    .bind(transfer)
    .bind(payload.to_string())
    .execute(conn)
    .await
    .map(|_| ())
}

/// Whether the log holds an event of `kind` for `transfer` whose payload field `field` is `value`.
pub async fn has_event(
    conn: &mut PgConnection,
    kind: &str,
    transfer: Option<i64>,
    field: &str,
    value: &str,
) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM event_log WHERE kind = $1 \
           AND transfer_id IS NOT DISTINCT FROM $2 AND payload ->> $3 = $4)",
    )
    .bind(kind)
    .bind(transfer)
    .bind(field)
    .bind(value)
    .fetch_one(conn)
    .await
}
