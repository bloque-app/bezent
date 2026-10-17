// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Following the finalized head and recording the contract's events as observations (`T-007-02`;
//! `REQ-OB-1` to `REQ-OB-5`, `AC-D2.3`, `INV-10`; PLAN `DEC-5`).
//!
//! **Finality rule** (`REQ-OB-1`): Kreivo's own finality, the relay chain's GRANDPA as the light
//! client verifies it; never a count of confirmations. The endpoint reads only blocks at or below
//! the finalized head, reached from it by parent hash, so every block it reads is final and still
//! has the hash it was read under (`REQ-OB-2`), and every read that decides anything is made at a
//! final block (`REQ-OB-5`).
//!
//! **Contiguous ranges** (`REQ-OB-3`, `AC-D2.3`): each step reads the blocks from the network's
//! watermark (the highest block up to which every block was read) to the finalized head, in ranges
//! of at most `max_range` blocks, and records each range it read with the observations in it, in
//! one transaction. A block it fails to read (the network's error, an event it cannot decode, or a
//! jump in the contract's event sequence) stops the step there: the range from that block on is
//! recorded as a **gap**, visible in `observation_gaps`, and the next step starts at that block
//! again, which is how the gap is filled from history. The store refuses a read range that starts
//! past the watermark, so nothing moves past a gap; and nothing past the watermark may be decided.
//!
//! **What an observation is**: one event of the contract, at its coordinate (network, block number,
//! block hash, event index), recorded once (`INV-10`): `Minted`, `Redeemed`, `RefundMinted`, the
//! pause and bounds events, and the contract's other events by name. A redemption's observation
//! carries what the core needs to turn it into a withdrawal: the redeemer, the reference, the
//! backend, and the payout address or commitment (`REQ-RQ-9`). The endpoint decides nothing: it
//! writes no transfer state.

use serde_json::{Value, json};
use sqlx::PgPool;

use crate::chain::{BlockRef, Chain, ancestry};
use crate::kreivo::{self, AccountId32, Backend, ContractEvent, Payout};

/// One observation to record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewObservation {
    /// The block.
    pub block: BlockRef,
    /// The block's time, in milliseconds since the Unix epoch.
    pub time_ms: u64,
    /// The event's index in the block.
    pub index: u32,
    /// The event.
    pub event: ContractEvent,
}

/// What one step did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Every final block is read; the watermark is the finalized head.
    CaughtUp {
        /// The watermark.
        watermark: u64,
    },
    /// Blocks `first` to `last` were read and recorded.
    Read {
        /// First block.
        first: u64,
        /// Last block.
        last: u64,
        /// Observations recorded.
        observations: usize,
    },
    /// Blocks from `first` could not be read: a visible gap, after whatever was read before it.
    Gap {
        /// First block of the gap.
        first: u64,
        /// Last block of the gap.
        last: u64,
        /// Why.
        reason: String,
    },
    /// The finalized head could not be read: nothing was recorded.
    Unavailable(String),
}

/// Follows one network's finalized head. See the module documentation.
pub struct Follower<'a, C> {
    /// The chain.
    pub chain: &'a C,
    /// The store, as the service role.
    pub pool: &'a PgPool,
    /// The network id.
    pub network: &'a str,
    /// The contract.
    pub contract: AccountId32,
    /// The first block to read.
    pub start_block: u64,
    /// The contract's first event sequence number.
    pub first_seq: u64,
    /// The most blocks one recorded range covers.
    pub max_range: u64,
    /// The most blocks walked back from the finalized head.
    pub max_walk: u64,
}

impl<C: Chain> Follower<'_, C> {
    /// Reads from the watermark to the finalized head, or to the first block it cannot read.
    pub async fn step(&self) -> Result<Step, sqlx::Error> {
        let finalized = match self.chain.finalized().await {
            Ok(f) => f,
            Err(e) => return Ok(Step::Unavailable(e.to_string())),
        };
        let watermark = watermark(self.pool, self.network).await?;
        let next = watermark.map_or(self.start_block, |w| w + 1);
        if next > finalized.number {
            return Ok(Step::CaughtUp {
                watermark: watermark.unwrap_or(finalized.number),
            });
        }
        let blocks = match ancestry(self.chain, &finalized, next, self.max_walk).await {
            Ok(Some(blocks)) => blocks,
            Ok(None) => {
                // The same words every look, so the same open gap is recorded once.
                let reason = format!(
                    "more than max_walk ({}) blocks behind the finalized head",
                    self.max_walk
                );
                return self.gap(next, finalized.number, &finalized, reason).await;
            }
            Err(e) => {
                return self
                    .gap(next, finalized.number, &finalized, e.to_string())
                    .await;
            }
        };
        let mut seq = match last_seq(self.pool, self.network, &self.contract).await? {
            Some(s) => s + 1,
            None => self.first_seq,
        };
        let mut read = 0;
        let mut first_read = None;
        let mut last_read = None;
        for chunk in blocks.chunks(usize::try_from(self.max_range).unwrap_or(usize::MAX)) {
            let mut observations = Vec::new();
            let mut failed = None;
            let mut done = 0;
            for block in chunk {
                match self.read_block(block, &mut seq).await {
                    Ok(mut found) => {
                        observations.append(&mut found);
                        done += 1;
                    }
                    Err(reason) => {
                        failed = Some((block.number, reason));
                        break;
                    }
                }
            }
            if done > 0 {
                let (first, last) = (chunk[0].number, chunk[done - 1].number);
                read += record_read(
                    self.pool,
                    self.network,
                    &self.contract,
                    first,
                    last,
                    &finalized,
                    &observations,
                )
                .await?;
                first_read.get_or_insert(first);
                last_read = Some(last);
            }
            if let Some((at, reason)) = failed {
                let last = chunk.last().map_or(at, |b| b.number);
                return self.gap(at, last, &finalized, reason).await;
            }
        }
        Ok(match (first_read, last_read) {
            (Some(first), Some(last)) => Step::Read {
                first,
                last,
                observations: read,
            },
            _ => Step::CaughtUp {
                watermark: finalized.number,
            },
        })
    }

    async fn gap(
        &self,
        first: u64,
        last: u64,
        finalized: &BlockRef,
        reason: String,
    ) -> Result<Step, sqlx::Error> {
        tracing::warn!(
            network = self.network,
            first,
            last,
            reason = %reason,
            "REQ-OB-3: a range of blocks could not be read; recorded as a gap, to be filled before anything after it"
        );
        record_gap(self.pool, self.network, first, last, finalized, &reason).await?;
        Ok(Step::Gap {
            first,
            last,
            reason,
        })
    }

    /// The contract's events in `block`, each checked to be the contract's next by sequence.
    async fn read_block(
        &self,
        block: &BlockRef,
        seq: &mut u64,
    ) -> Result<Vec<NewObservation>, String> {
        let events = self
            .chain
            .events(block, &self.contract)
            .await
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        let mut next = *seq;
        for emitted in &events.emitted {
            let event = kreivo::decode_event(&emitted.topics, &emitted.data)
                .map_err(|e| format!("event {} of block {}: {e}", emitted.index, block.number))?;
            if event.seq() != next {
                return Err(format!(
                    "AC-D2.3: the contract's event sequence jumps from {next} to {} at event {} of block {}",
                    event.seq(),
                    emitted.index,
                    block.number
                ));
            }
            next += 1;
            out.push(NewObservation {
                block: *block,
                time_ms: events.time_ms,
                index: emitted.index,
                event,
            });
        }
        *seq = next;
        Ok(out)
    }
}

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn backend_json(b: &Backend) -> Value {
    match b {
        Backend::Evm { chain_id } => json!({"kind": "evm", "chain_id": chain_id}),
        Backend::Attested { id } => json!({"kind": "attested", "id": id}),
    }
}

fn payout_json(p: &Payout) -> Value {
    match p {
        Payout::Address(a) => json!({"address": hex0x(a)}),
        Payout::Commitment(c) => json!({"commitment": hex0x(c)}),
    }
}

/// The columns of an observation: token, amount, sender, recipient, on-chain reference, detail.
type Columns = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<Vec<u8>>,
    Value,
);

fn columns(contract: &AccountId32, e: &ContractEvent) -> Columns {
    let mut detail = json!({"contract": hex0x(contract), "event": e.name(), "seq": e.seq()});
    let cols = match e {
        ContractEvent::Minted {
            reference,
            asset,
            to,
            amount,
            ..
        } => (
            Some(asset.text()),
            Some(amount.to_string()),
            None,
            Some(hex0x(to)),
            Some(reference.to_vec()),
        ),
        ContractEvent::Redeemed {
            reference,
            redeemer,
            asset,
            amount,
            backend,
            payout,
            ..
        } => {
            detail["backend"] = backend_json(backend);
            detail["payout"] = payout_json(payout);
            (
                Some(asset.text()),
                Some(amount.to_string()),
                Some(hex0x(redeemer)),
                None,
                Some(reference.to_vec()),
            )
        }
        ContractEvent::RefundMinted {
            reference,
            to,
            asset,
            amount,
            ..
        } => (
            Some(asset.text()),
            Some(amount.to_string()),
            None,
            Some(hex0x(to)),
            Some(reference.to_vec()),
        ),
        ContractEvent::Paused { scope, by, .. } => {
            detail["scope"] = json!(format!("{scope:?}"));
            (None, None, Some(hex0x(by)), None, None)
        }
        ContractEvent::Unpaused { scope, .. } => {
            detail["scope"] = json!(format!("{scope:?}"));
            (None, None, None, None, None)
        }
        ContractEvent::BoundsSet { asset, bounds, .. } => {
            detail["bounds"] = json!({
                "supply_ceiling": bounds.supply_ceiling.to_string(),
                "window_amount": bounds.window_amount.to_string(),
                "window_blocks": bounds.window_blocks,
                "per_transfer_max": bounds.per_transfer_max.to_string(),
                "redeem_minimum": bounds.redeem_minimum.to_string(),
            });
            (Some(asset.text()), None, None, None, None)
        }
        ContractEvent::Other { data, .. } => {
            detail["data"] = json!(hex0x(data));
            (None, None, None, None, None)
        }
    };
    (cols.0, cols.1, cols.2, cols.3, cols.4, detail)
}

fn i64_of(n: u64) -> Result<i64, sqlx::Error> {
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

/// The contract's last recorded event sequence number on `network`.
pub async fn last_seq(
    pool: &PgPool,
    network: &str,
    contract: &AccountId32,
) -> Result<Option<u64>, sqlx::Error> {
    let s: Option<i64> = sqlx::query_scalar(
        "SELECT max((detail ->> 'seq')::bigint) FROM observations \
         WHERE network = $1 AND position_kind = 'log' AND detail ->> 'contract' = $2",
    )
    .bind(network)
    .bind(hex0x(contract))
    .fetch_one(pool)
    .await?;
    Ok(s.and_then(|s| u64::try_from(s).ok()))
}

/// Records blocks `first` to `last` as read under `finalized`, with their observations, in one
/// transaction. An observation already recorded at its coordinate is not recorded again
/// (`INV-10`). Returns how many were new.
pub async fn record_read(
    pool: &PgPool,
    network: &str,
    contract: &AccountId32,
    first: u64,
    last: u64,
    finalized: &BlockRef,
    observations: &[NewObservation],
) -> Result<usize, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let mut new = 0;
    for o in observations {
        let (token, amount, sender, recipient, reference, detail) = columns(contract, &o.event);
        let id: Option<i64> = sqlx::query_scalar(
            "INSERT INTO observations (network, block_number, block_hash, block_time, \
               position_kind, position_index, token, amount, sender, recipient, \
               on_chain_reference, executed, detail, kind) \
             VALUES ($1, $2, $3, to_timestamp(0) + $4 * interval '1 millisecond', 'log', $5, $6, \
               $7::numeric, $8, $9, $10, true, $11::text::jsonb, $12) \
             ON CONFLICT ON CONSTRAINT observations_coordinate_once DO NOTHING RETURNING id",
        )
        .bind(network)
        .bind(i64_of(o.block.number)?)
        .bind(o.block.hash.as_slice())
        .bind(i64_of(o.time_ms)?)
        .bind(i64::from(o.index))
        .bind(token)
        .bind(amount)
        .bind(sender)
        .bind(recipient)
        .bind(reference)
        .bind(detail.to_string())
        .bind(o.event.kind())
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(id) = id {
            new += 1;
            // Read at a final block, under the finalized head: final as observed (REQ-OB-2).
            sqlx::query(
                "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
                   final_block_hash) VALUES ($1, 'final', $2, $3)",
            )
            .bind(id)
            .bind(i64_of(finalized.number)?)
            .bind(finalized.hash.as_slice())
            .execute(&mut *tx)
            .await?;
        }
    }
    sqlx::query(
        "INSERT INTO observation_ranges (network, first_block, last_block, outcome, \
           final_block_number, final_block_hash) VALUES ($1, $2, $3, 'read', $4, $5)",
    )
    .bind(network)
    .bind(i64_of(first)?)
    .bind(i64_of(last)?)
    .bind(i64_of(finalized.number)?)
    .bind(finalized.hash.as_slice())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(new)
}

/// Records blocks `first` to `last` as a gap, unless the same gap is already open.
pub async fn record_gap(
    pool: &PgPool,
    network: &str,
    first: u64,
    last: u64,
    finalized: &BlockRef,
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
    .bind(i64_of(finalized.number)?)
    .bind(finalized.hash.as_slice())
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(())
}
