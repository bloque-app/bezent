// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! What the endpoint reads from and writes to the store besides observations: the network's
//! watermark and gaps (`REQ-OB-3`, `AC-D2.3`), and the final heads it reads under (`REQ-OB-2`,
//! `INV-16`). Every write is append-only (`F-007`'s tables, migration 0018; `final_heads`, 0014).

use sqlx::{PgConnection, PgPool};

use crate::rpc::Header;

/// `0x` hex.
pub fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

/// A block number as the store keeps it.
pub fn i64_of(n: u64) -> Result<i64, sqlx::Error> {
    i64::try_from(n).map_err(|e| sqlx::Error::Encode(Box::new(e)))
}

/// The network's watermark: the highest block up to which every block was read.
pub async fn watermark(pool: &PgPool, network: &str) -> Result<Option<u64>, sqlx::Error> {
    let w: Option<i64> =
        sqlx::query_scalar("SELECT watermark FROM observation_watermarks WHERE network = $1")
            .bind(network)
            .fetch_optional(pool)
            .await?;
    Ok(w.and_then(|w| u64::try_from(w).ok()))
}

/// What recording a final head found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Head {
    /// It is recorded (now, or earlier with the same hash).
    Recorded,
    /// The store holds another hash at that height: a network that finalised two blocks at one
    /// height, or providers that lied together. Nothing may be decided on it.
    Conflict(Vec<u8>),
}

/// Records `head` as a final head of `network` (`final_heads`, one hash per height), in `conn`.
pub async fn final_head(
    conn: &mut PgConnection,
    network: &str,
    head: &Header,
) -> Result<Head, sqlx::Error> {
    sqlx::query(
        "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
         VALUES ($1, $2, $3, to_timestamp(0) + $4::bigint * interval '1 second') \
         ON CONFLICT ON CONSTRAINT final_heads_one_hash_per_height DO NOTHING",
    )
    .bind(network)
    .bind(i64_of(head.number)?)
    .bind(head.hash.as_slice())
    .bind(i64_of(head.timestamp)?)
    .execute(&mut *conn)
    .await?;
    let stored: Vec<u8> = sqlx::query_scalar(
        "SELECT block_hash FROM final_heads WHERE network = $1 AND block_number = $2",
    )
    .bind(network)
    .bind(i64_of(head.number)?)
    .fetch_one(&mut *conn)
    .await?;
    Ok(if stored == head.hash.as_slice() {
        Head::Recorded
    } else {
        Head::Conflict(stored)
    })
}

/// Whether readings are recorded at `head`, a final head of `network` with its hash (issue #42).
/// A head recorded without them (by the payout prover, which records the final head it proves
/// at and reads no balance) has none, and the follower reads them there (PR #50 review M4).
pub async fn head_read(pool: &PgPool, network: &str, head: &Header) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM reserve_readings \
                        WHERE network = $1 AND block_number = $2 AND block_hash = $3)",
    )
    .bind(network)
    .bind(i64_of(head.number)?)
    .bind(head.hash.as_slice())
    .fetch_one(pool)
    .await
}

/// Records `readings`, made at `head`, a recorded final head of `network` (issue #42). A reading
/// already recorded is not recorded again.
pub async fn record_readings(
    conn: &mut PgConnection,
    network: &str,
    head: &Header,
    readings: &[crate::reserve::Reading],
) -> Result<(), sqlx::Error> {
    for r in readings {
        sqlx::query(
            "INSERT INTO reserve_readings (network, block_number, block_hash, kind, subject, \
                 holder, amount, unknown_reason, detail) \
             VALUES ($1, $2, $3, $4, $5, $6, $7::numeric, $8, $9::text::jsonb) \
             ON CONFLICT ON CONSTRAINT reserve_readings_once DO NOTHING",
        )
        .bind(network)
        .bind(i64_of(head.number)?)
        .bind(head.hash.as_slice())
        .bind(r.kind)
        .bind(&r.subject)
        .bind(&r.holder)
        .bind(r.amount.map(|a| a.to_string()))
        .bind(&r.unknown_reason)
        .bind(r.detail.to_string())
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Records blocks `first` to `last` as a gap under `head`, unless the same gap is already open.
pub async fn record_gap(
    pool: &PgPool,
    network: &str,
    first: u64,
    last: u64,
    head: &Header,
    reason: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO observation_ranges (network, first_block, last_block, outcome, \
           final_block_number, final_block_hash, reason) \
         SELECT $1, $2, $3, 'gap', $4, $5, $6 \
         WHERE NOT EXISTS (SELECT 1 FROM observation_gaps \
           WHERE network = $1 AND first_block = $2 AND reason = $6 AND NOT filled)",
    )
    .bind(network)
    .bind(i64_of(first)?)
    .bind(i64_of(last)?)
    .bind(i64_of(head.number)?)
    .bind(head.hash.as_slice())
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(())
}
