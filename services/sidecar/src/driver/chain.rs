// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The driver's chains: blocks it makes up, final heads it records, observations at its blocks.
//!
//! There is no chain behind the sidecar, so the driver stands in for one per network. A block is
//! a height and a hash derived from it ([`block_hash`]), so the same height always has the same
//! hash, across restarts, and the store's "one final head per height" holds. The tip is the
//! highest block the store holds for the network (an observation's or a final head's); a new
//! block is one past it. Nothing is kept in memory: the store is the chain's whole state.
//!
//! Every write goes through the network's endpoint login, as the endpoints' own writes do, and
//! the store's rules apply to it unchanged (`REQ-OB-6`, `INV-8`, `REQ-OB-2`, `REQ-OB-3`).

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;

use super::Error;

/// Every row the driver writes says so in its `detail`.
pub const DRIVER: &str = "bezent-sidecar chain-event driver (TEST only)";

/// `0x` hex.
pub fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

/// The hash of block `number` of `network`: the same for a height every time.
pub fn block_hash(network: &str, number: i64) -> Vec<u8> {
    Sha256::digest(format!("bezent-sidecar-driver/v0/{network}/{number}").as_bytes()).to_vec()
}

/// A block of the driver's chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// Its network.
    pub network: String,
    /// Its height.
    pub number: i64,
    /// Its hash.
    pub hash: Vec<u8>,
    /// Its time.
    pub time: DateTime<Utc>,
}

impl Block {
    /// The block as JSON.
    pub fn json(&self) -> Value {
        json!({
            "network": self.network,
            "block_number": self.number,
            "block_hash": hex0x(&self.hash),
            "block_time": self.time.to_rfc3339(),
        })
    }

    /// The coordinate of position `index` of kind log in this block, as the ingress takes it.
    pub fn coordinate(&self, index: i64) -> Value {
        json!({
            "network": self.network,
            "block_number": self.number,
            "block_hash": hex0x(&self.hash),
            "position": {"kind": "log", "index": index},
        })
    }
}

/// The highest block the store holds for `network`, if any.
pub async fn tip(conn: &mut PgConnection, network: &str) -> Result<Option<Block>, Error> {
    let row: Option<(i64, Vec<u8>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT block_number, block_hash, block_time FROM ( \
           SELECT block_number, block_hash, block_time FROM observations WHERE network = $1 \
           UNION ALL \
           SELECT block_number, block_hash, block_time FROM final_heads WHERE network = $1) b \
         ORDER BY block_number DESC, block_time DESC LIMIT 1",
    )
    .bind(network)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|(number, hash, time)| Block {
        network: network.to_owned(),
        number,
        hash,
        time,
    }))
}

/// A new block of `network`, at height `at_least` or one past the tip, whichever is higher. Its
/// time is now, never before the tip's.
pub async fn new_block(
    conn: &mut PgConnection,
    network: &str,
    at_least: i64,
) -> Result<Block, Error> {
    let tip = tip(conn, network).await?;
    let number = tip.as_ref().map_or(1, |t| t.number + 1).max(at_least);
    let now = Utc::now();
    let time = tip.map_or(now, |t| t.time.max(now));
    Ok(Block {
        network: network.to_owned(),
        number,
        hash: block_hash(network, number),
        time,
    })
}

/// The tip of `network`, or a first block if it has none.
pub async fn tip_or_new(conn: &mut PgConnection, network: &str) -> Result<Block, Error> {
    match tip(conn, network).await? {
        Some(t) => Ok(t),
        None => new_block(conn, network, 1).await,
    }
}

/// Records `block` as a final head of its network, and every block up to it as read (an
/// `observation_ranges` row from the watermark on), as an endpoint's follower does (`REQ-OB-2`,
/// `REQ-OB-3`). A head already recorded with the same hash is left as it is.
pub async fn finalise(conn: &mut PgConnection, block: &Block) -> Result<(), Error> {
    sqlx::query(
        "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT ON CONSTRAINT final_heads_one_hash_per_height DO NOTHING",
    )
    .bind(&block.network)
    .bind(block.number)
    .bind(&block.hash)
    .bind(block.time)
    .execute(&mut *conn)
    .await?;
    let stored: Vec<u8> = sqlx::query_scalar(
        "SELECT block_hash FROM final_heads WHERE network = $1 AND block_number = $2",
    )
    .bind(&block.network)
    .bind(block.number)
    .fetch_one(&mut *conn)
    .await?;
    if stored != block.hash {
        return Err(Error::Conflict(format!(
            "{} already holds another final head at block {} (REQ-OB-2)",
            block.network, block.number
        )));
    }
    let watermark: Option<i64> =
        sqlx::query_scalar("SELECT watermark FROM observation_watermarks WHERE network = $1")
            .bind(&block.network)
            .fetch_optional(&mut *conn)
            .await?
            .flatten();
    if watermark.is_none_or(|w| w < block.number) {
        sqlx::query(
            "INSERT INTO observation_ranges (network, first_block, last_block, outcome, \
               final_block_number, final_block_hash) VALUES ($1, $2, $3, 'read', $3, $4)",
        )
        .bind(&block.network)
        .bind(watermark.map_or(0, |w| w + 1))
        .bind(block.number)
        .bind(&block.hash)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// The latest final head of `network`, finalising the tip (or a first block) if it has none.
pub async fn final_head(conn: &mut PgConnection, network: &str) -> Result<Block, Error> {
    let row: Option<(i64, Vec<u8>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT block_number, block_hash, block_time FROM final_heads WHERE network = $1 \
         ORDER BY block_number DESC LIMIT 1",
    )
    .bind(network)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some((number, hash, time)) = row {
        return Ok(Block {
            network: network.to_owned(),
            number,
            hash,
            time,
        });
    }
    let block = tip_or_new(conn, network).await?;
    finalise(conn, &block).await?;
    Ok(block)
}

/// What an observation records, as the network's endpoint would classify it.
#[derive(Clone, Debug, Default)]
pub struct Observed {
    /// `observations.kind`.
    pub kind: &'static str,
    /// The token, as the store spells it.
    pub token: Option<String>,
    /// The amount, in the token's minor units.
    pub amount: Option<u128>,
    /// From.
    pub sender: Option<String>,
    /// To.
    pub recipient: Option<String>,
    /// The on-chain reference the movement carries.
    pub reference: Option<Vec<u8>>,
    /// The network's own data; `event` is required.
    pub detail: Value,
}

/// Records `o` at log 0 of `block`, executed and not yet final. Returns its id.
pub async fn observe(conn: &mut PgConnection, block: &Block, o: &Observed) -> Result<i64, Error> {
    let mut detail = o.detail.clone();
    detail["driver"] = json!(DRIVER);
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO observations (network, block_number, block_hash, block_time, \
           position_kind, position_index, token, amount, sender, recipient, \
           on_chain_reference, executed, detail, kind) \
         VALUES ($1, $2, $3, $4, 'log', 0, $5, $6::numeric, $7, $8, $9, true, $10::text::jsonb, $11) \
         RETURNING id",
    )
    .bind(&block.network)
    .bind(block.number)
    .bind(&block.hash)
    .bind(block.time)
    .bind(&o.token)
    .bind(o.amount.map(|a| a.to_string()))
    .bind(&o.sender)
    .bind(&o.recipient)
    .bind(&o.reference)
    .bind(detail.to_string())
    .bind(o.kind)
    .fetch_one(&mut *conn)
    .await?;
    Ok(id)
}

/// Decides observation `id` as `outcome` (`final` or `withdrawn`) at final head `head`.
pub async fn decide(
    conn: &mut PgConnection,
    id: i64,
    outcome: &str,
    head: &Block,
) -> Result<(), Error> {
    sqlx::query(
        "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
           final_block_hash) VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(outcome)
    .bind(head.number)
    .bind(&head.hash)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// A decimal amount in minor units (`INV-11`): digits only, at most `u128::MAX`.
pub fn amount(text: &str) -> Result<u128, Error> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::Malformed(format!(
            "amount {text:?} is not a decimal integer of minor units"
        )));
    }
    text.parse()
        .map_err(|_| Error::Malformed(format!("amount {text} is past u128")))
}

/// `0x` and hexadecimal digits, as bytes.
pub fn hex_bytes(text: &str, what: &str) -> Result<Vec<u8>, Error> {
    text.strip_prefix("0x")
        .and_then(|h| hex::decode(h).ok())
        .filter(|b| !b.is_empty())
        .ok_or_else(|| Error::Malformed(format!("{what} {text:?} is not 0x-prefixed hex")))
}
