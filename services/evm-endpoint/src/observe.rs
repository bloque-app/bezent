// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Following the finalized head and recording what reaches and leaves the vault (`T-009-02`,
//! `T-009-03`, `T-009-05`; `REQ-OB-1` to `REQ-OB-5`, `REQ-IN-1`, `REQ-IN-3`, `REQ-IN-6`,
//! `REQ-CV-2`, `REQ-CV-4`, `AC-D2.3`, `AC-C1.3`, `INV-10`, `INV-14`; PLAN `DEC-8`, `DEC-12`).
//!
//! **Finality rule** (`REQ-OB-1`): the network's own `finalized` tag (on Polygon PoS, Heimdall's
//! milestones), never a count of confirmations. Both providers are read; the final point is the
//! lower of their finalized heads, with the hash both report at that height (`DEC-12`). The
//! endpoint reads only blocks at or below it, so every observation is final when it is recorded,
//! and its outcome `final` is written with it, under that point, which is also recorded in
//! `final_heads` (`REQ-OB-2`, `REQ-OB-5`). An arrival seen before finality is not recorded at all
//! (`AC-A2.1`); one in a block that is replaced before finality never reaches a final range
//! (`AC-A2.3`). `REQ-OB-4`'s re-checks before finality therefore do not arise here.
//!
//! **The hash is re-checked** (`REQ-OB-2`): every log's block hash must be the hash both providers
//! give for its height at the final point; a log with another hash (a provider serving a replaced
//! block) fails the range.
//!
//! **Contiguous ranges** (`REQ-OB-3`, `AC-D2.3`): each step reads from the watermark to the final
//! point in ranges of at most `max_range` blocks, and records each range with its observations in
//! one transaction. A range that cannot be read (a provider's error, the providers disagreeing on
//! its logs or its hashes, a removed log) is recorded as a **gap** and the step stops there; the
//! next step reads it again, which is how the gap is filled from history. The store refuses a read
//! range that starts past the watermark, so nothing moves past a gap.
//!
//! **What is observed** (one observation per log, at its coordinate, recorded once, `INV-10`):
//!
//! | Event | Log | Meaning |
//! |---|---|---|
//! | `Arrival` | an admitted origin token's `Transfer` into the vault | waits unassociated until a deposit names it (`REQ-IN-3`) |
//! | `Conversion` | the origin token's `Transfer` into the vault from a declared pool, in a transaction where the vault paid that pool's convertible | the converted output, with both amounts (`REQ-CV-2`) |
//! | `ConversionInput` | the vault's `Transfer` of a convertible to its declared pool, in such a transaction | the conversion's input |
//! | `ConvertibleArrival` | an admitted convertible's `Transfer` into the vault | itemised, never reserve (`REQ-CV-4`) |
//! | `OtherArrival` | any other token's `Transfer` into the vault | unassociated, with an owner (`REQ-IN-6`) |
//! | `Payout` | the vault's `Transfer` of an origin token, amount above zero, to an address that is not the vault, **immediately** followed in the same transaction by the emitter's `Reference` | a payout leg: reference, token, amount, sender and recipient from the transfer (`INV-14`, `AC-C1.3`; F-008 NOTES) |
//! | `Outflow`, `ZeroTransfer`, `SelfTransfer` | any other `Transfer` out of the vault | never a payout; a self transfer has no `recipient` column (it is in the detail), so no deposit can bind it |
//! | `UnpairedReference` | an emitter's `Reference` not paired as above (a bare emit) | never a payout |
//! | `UndecodedTransfer` | a `Transfer` topic at the vault that is not an ERC-20 transfer | recorded, never a movement |
//!
//! An undecodable `Transfer` is recorded, not a gap: anyone can send the vault a token whose events
//! bezent does not read, and a gap would stop observation for good. An amount above the store's
//! domain (`u128`) is recorded with no amount and its text in the detail, for the same reason. The
//! endpoint decides nothing: it writes no transfer state.

use std::collections::BTreeMap;

use alloy::primitives::{Address, B256, U256};
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::evm::{self, REFERENCE_TOPIC, TRANSFER_TOPIC};
use crate::network::Declarations;
use crate::pair::{Pair, Read};
use crate::reserve;
use crate::rpc::{Header, Log, LogQuery, Rpc};
use crate::store::{self, Head, hex0x, i64_of};

/// One observation to record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observed {
    /// The log it is at.
    pub log: Log,
    /// Its block's time, in seconds since the Unix epoch.
    pub time: u64,
    /// What it is (the module documentation's table).
    pub event: &'static str,
    /// The token, as the store spells it.
    pub token: Option<String>,
    /// The amount in the token's minor units, a decimal string; `None` above `u128`.
    pub amount: Option<String>,
    /// Who sent it.
    pub sender: Option<String>,
    /// Who received it.
    pub recipient: Option<String>,
    /// The on-chain reference it carries.
    pub reference: Option<B256>,
    /// The network's own data.
    pub detail: Value,
}

/// The store's `observations.kind` for an event of the table above (migration 0034, issue #30;
/// F-002 NOTES): the endpoint says what each observation is, once, and the store's rules read it.
/// `ZeroTransfer`, `UnpairedReference`, `UndecodedReference`, `UndecodedTransfer` and anything not
/// listed are `other`, which nothing binds.
pub fn kind(event: &str) -> &'static str {
    match event {
        "Arrival" => "arrival",
        "OtherArrival" => "other_arrival",
        "ConvertibleArrival" => "convertible_arrival",
        "Conversion" => "conversion_output",
        "ConversionInput" => "conversion_input",
        "Payout" => "payout_leg",
        "Outflow" => "outflow",
        "SelfTransfer" => "self_transfer",
        _ => "other",
    }
}

/// What the vault's observer knows of the network: the vault, the emitter, what is admitted.
#[derive(Clone, Debug)]
pub struct Watch<'a> {
    /// The vault.
    pub vault: Address,
    /// The reference emitter.
    pub emitter: Address,
    /// The store's declarations: the vault's spelling and the admitted origin tokens.
    pub declarations: &'a Declarations,
    /// The admitted convertibles and their declared pools (F-008, "Pinned pools").
    pub convertibles: &'a [(Address, Address)],
}

/// An address as bezent writes it: the store's spelling for the vault and the admitted tokens,
/// EIP-55 for any other.
fn spell(w: &Watch<'_>, a: Address) -> String {
    if a == w.vault {
        w.declarations.vault_text.clone()
    } else if let Some(t) = w.declarations.tokens.get(&a) {
        t.text.clone()
    } else {
        a.to_checksum(None)
    }
}

fn amount_text(v: U256) -> Option<String> {
    u128::try_from(v).ok().map(|a| a.to_string())
}

impl Watch<'_> {
    fn pool_of(&self, convertible: Address) -> Option<Address> {
        self.convertibles
            .iter()
            .find(|(t, _)| *t == convertible)
            .map(|(_, p)| *p)
    }

    /// The observations of one transaction's logs at the vault, ordered by log index.
    pub fn classify(&self, logs: &[Log], time: u64) -> Vec<Observed> {
        let transfers: Vec<Option<(Address, Address, U256)>> = logs
            .iter()
            .map(|l| {
                (l.topics.first() == Some(&TRANSFER_TOPIC))
                    .then(|| evm::decode_transfer(&l.topics, &l.data))
                    .flatten()
            })
            .collect();
        // The pools this transaction paid a convertible to, and the convertibles they took.
        let paid: Vec<(Address, Address)> = logs
            .iter()
            .zip(&transfers)
            .filter_map(|(l, t)| {
                let (from, to, _) = (*t)?;
                (from == self.vault && self.pool_of(l.address) == Some(to))
                    .then_some((to, l.address))
            })
            .collect();
        let converted_from: Vec<Address> = logs
            .iter()
            .zip(&transfers)
            .filter_map(|(l, t)| {
                let (from, to, _) = (*t)?;
                (to == self.vault
                    && from != self.vault
                    && self.declarations.tokens.contains_key(&l.address)
                    && paid.iter().any(|(pool, _)| *pool == from))
                .then_some(from)
            })
            .collect();
        let payout_at = |i: usize| -> Option<B256> {
            let (l, (from, to, amount)) = (&logs[i], transfers[i]?);
            let next = logs.get(i + 1)?;
            (from == self.vault
                && to != self.vault
                && !amount.is_zero()
                && self.declarations.tokens.contains_key(&l.address)
                && next.address == self.emitter
                && next.log_index == l.log_index + 1
                && next.tx_hash == l.tx_hash)
                .then(|| evm::decode_reference(&next.topics, &next.data))
                .flatten()
        };

        let mut out = Vec::new();
        let mut paired = Vec::new();
        for (i, l) in logs.iter().enumerate() {
            let base = json!({
                "tx_hash": l.tx_hash,
                "tx_index": l.tx_index,
                "emitter": l.address,
            });
            let mut o = Observed {
                log: l.clone(),
                time,
                event: "",
                token: None,
                amount: None,
                sender: None,
                recipient: None,
                reference: None,
                detail: base,
            };
            if l.address == self.emitter && l.topics.first() == Some(&REFERENCE_TOPIC) {
                if paired.contains(&l.log_index) {
                    continue;
                }
                o.event = "UnpairedReference";
                o.reference = evm::decode_reference(&l.topics, &l.data);
                if o.reference.is_none() {
                    o.event = "UndecodedReference";
                    o.detail["topics"] = json!(l.topics);
                    o.detail["data"] = json!(hex0x(&l.data));
                }
                out.push(o);
                continue;
            }
            let Some((from, to, value)) = transfers[i] else {
                o.event = "UndecodedTransfer";
                o.detail["topics"] = json!(l.topics);
                o.detail["data"] = json!(hex0x(&l.data));
                out.push(o);
                continue;
            };
            o.token = Some(spell(self, l.address));
            o.amount = amount_text(value);
            if o.amount.is_none() {
                o.detail["amount_text"] = json!(value.to_string());
            }
            o.sender = Some(spell(self, from));
            o.recipient = Some(spell(self, to));
            let admitted = self.declarations.tokens.contains_key(&l.address);
            o.event = if from == self.vault && to == self.vault {
                // Nothing arrived. The recipient is in the detail, not the column: a deposit
                // request binds an observation by its token and its recipient (the reserve), so
                // a self transfer must never look like an arrival (INV-6, INV-14).
                o.recipient = None;
                o.detail["to"] = json!(spell(self, to));
                "SelfTransfer"
            } else if to == self.vault {
                if admitted && converted_from.contains(&from) {
                    let input = logs.iter().zip(&transfers).find(|(il, it)| {
                        matches!(it, Some((f, t, _)) if *f == self.vault && *t == from
                            && self.pool_of(il.address) == Some(from))
                    });
                    if let Some((il, Some((_, _, amount_in)))) = input {
                        o.detail["conversion"] = json!({
                            "pool": from,
                            "token_in": spell(self, il.address),
                            "amount_in": amount_text(*amount_in),
                            "log_index_in": il.log_index,
                        });
                    }
                    "Conversion"
                } else if admitted {
                    "Arrival"
                } else if self.pool_of(l.address).is_some() {
                    "ConvertibleArrival"
                } else {
                    "OtherArrival"
                }
            } else if from == self.vault {
                if let Some(reference) = payout_at(i) {
                    paired.push(l.log_index + 1);
                    o.reference = Some(reference);
                    o.detail["reference_log_index"] = json!(l.log_index + 1);
                    "Payout"
                } else if value.is_zero() {
                    "ZeroTransfer"
                } else if self.pool_of(l.address) == Some(to)
                    && logs.iter().zip(&transfers).any(|(ol, ot)| {
                        matches!(ot, Some((f, t, _)) if *f == to && *t == self.vault)
                            && self.declarations.tokens.contains_key(&ol.address)
                    })
                {
                    "ConversionInput"
                } else {
                    "Outflow"
                }
            } else {
                // Neither from nor to the vault: not what the queries ask for.
                continue;
            };
            out.push(o);
        }
        for o in &mut out {
            o.detail["event"] = json!(o.event);
        }
        out
    }
}

/// What one step did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Every final block is read; the watermark is the final point.
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
    /// Blocks `first` to `last` could not be read: a visible gap, after whatever was read before.
    Gap {
        /// First block of the gap.
        first: u64,
        /// Last block of the gap.
        last: u64,
        /// Why.
        reason: String,
    },
    /// No final point both providers agree on: nothing was recorded (`DEC-12`).
    Unavailable(String),
}

/// Follows one network's final point. See the module documentation.
pub struct Follower<'a, R> {
    /// The two providers.
    pub pair: &'a Pair<R>,
    /// The store, as the EVM endpoint's role.
    pub pool: &'a PgPool,
    /// The network id.
    pub network: &'a str,
    /// What is watched.
    pub watch: Watch<'a>,
    /// The first block to read.
    pub start_block: u64,
    /// The most blocks one range covers.
    pub max_range: u64,
}

impl<R: Rpc> Follower<'_, R> {
    /// Reads from the watermark to the final point, or to the first range it cannot read.
    pub async fn step(&self) -> Result<Step, sqlx::Error> {
        let head = match self.pair.final_point().await {
            Ok(h) => h,
            Err(e) => return Ok(Step::Unavailable(e.to_string())),
        };
        {
            // Issue #42: the vault's balances at the head, read before the head is recorded and
            // recorded with it, in one transaction, so a final point the reserve check picks has
            // its readings (REQ-BK-8, NFR-5). A head already read is not read again; a head the
            // payout prover recorded first, with no readings, is read now, or the check would
            // find no reading at the latest final point while a payout is in flight and pause
            // the asset (PR #50 review M4).
            let readings = if store::head_read(self.pool, self.network, &head).await? {
                Vec::new()
            } else {
                reserve::readings(self.pair, &self.watch, head.number).await
            };
            let mut tx = self.pool.begin().await?;
            if let Head::Conflict(stored) = store::final_head(&mut tx, self.network, &head).await? {
                return Ok(Step::Unavailable(format!(
                    "REQ-OB-2: block {} is final at {} here, and {} is recorded",
                    head.number,
                    head.hash,
                    hex0x(&stored)
                )));
            }
            store::record_readings(&mut tx, self.network, &head, &readings).await?;
            tx.commit().await?;
            for r in readings.iter().filter(|r| r.amount.is_none()) {
                tracing::warn!(
                    network = self.network,
                    block = head.number,
                    token = %r.subject,
                    reason = r.unknown_reason.as_deref().unwrap_or_default(),
                    "REQ-BK-8: a balance at the vault is unknown at this final head"
                );
            }
        }
        let watermark = store::watermark(self.pool, self.network).await?;
        let mut next = watermark.map_or(self.start_block, |w| w + 1);
        if next > head.number {
            return Ok(Step::CaughtUp {
                watermark: watermark.unwrap_or(head.number),
            });
        }
        let (first, mut recorded) = (next, 0);
        while next <= head.number {
            let last = head.number.min(next.saturating_add(self.max_range - 1));
            match self.read_range(next, last).await {
                Ok(found) => {
                    recorded += self.record_read(next, last, &head, &found).await?;
                    next = last + 1;
                }
                Err(reason) => {
                    tracing::warn!(
                        network = self.network,
                        first = next,
                        last,
                        reason = %reason,
                        "REQ-OB-3: a range could not be read; recorded as a gap, to be filled before anything after it"
                    );
                    store::record_gap(self.pool, self.network, next, last, &head, &reason).await?;
                    return Ok(Step::Gap {
                        first: next,
                        last,
                        reason,
                    });
                }
            }
        }
        Ok(Step::Read {
            first,
            last: head.number,
            observations: recorded,
        })
    }

    /// The vault's logs in blocks `first` to `last`, the same from both providers, each at the
    /// hash both give for its block, classified. Any failure is the range's gap reason.
    pub async fn read_range(&self, first: u64, last: u64) -> Result<Vec<Observed>, String> {
        let w = &self.watch;
        let vault = w.vault.into_word();
        let queries = [
            LogQuery {
                from: first,
                to: last,
                address: vec![],
                topics: vec![Some(TRANSFER_TOPIC), None, Some(vault)],
            },
            LogQuery {
                from: first,
                to: last,
                address: vec![],
                topics: vec![Some(TRANSFER_TOPIC), Some(vault)],
            },
            LogQuery {
                from: first,
                to: last,
                address: vec![w.emitter],
                topics: vec![Some(REFERENCE_TOPIC)],
            },
        ];
        let mut logs: BTreeMap<(u64, u64), Log> = BTreeMap::new();
        for q in &queries {
            for l in self.pair.logs(q).await.map_err(|e| e.to_string())? {
                if l.removed {
                    return Err(format!(
                        "REQ-OB-2: log {} of block {} is marked removed below the final point",
                        l.log_index, l.block_number
                    ));
                }
                if !(first..=last).contains(&l.block_number) {
                    return Err(format!(
                        "a log of block {} answers a query for blocks {first} to {last}",
                        l.block_number
                    ));
                }
                logs.insert((l.block_number, l.log_index), l);
            }
        }
        // REQ-OB-2: each log's block still has the hash it reports, as both providers give it.
        let mut headers: BTreeMap<u64, Header> = BTreeMap::new();
        for &(n, _) in logs.keys() {
            if headers.contains_key(&n) {
                continue;
            }
            let h = match self.pair.header(n).await {
                Ok(Some(h)) => h,
                Ok(None) => return Err(format!("block {n} is missing below the final point")),
                Err(Read::Disagree(d)) => return Err(format!("DEC-12: {d}")),
                Err(e) => return Err(e.to_string()),
            };
            headers.insert(n, h);
        }
        let mut by_tx: BTreeMap<(u64, u64), Vec<Log>> = BTreeMap::new();
        for l in logs.into_values() {
            let h = &headers[&l.block_number];
            if l.block_hash != h.hash {
                return Err(format!(
                    "REQ-OB-2: log {} of block {} reports block hash {}, the final chain has {}",
                    l.log_index, l.block_number, l.block_hash, h.hash
                ));
            }
            by_tx
                .entry((l.block_number, l.tx_index))
                .or_default()
                .push(l);
        }
        let mut out = Vec::new();
        for ((n, _), tx_logs) in by_tx {
            if tx_logs.iter().any(|l| l.tx_hash != tx_logs[0].tx_hash) {
                return Err(format!(
                    "two transaction hashes at one transaction index of block {n}"
                ));
            }
            out.extend(w.classify(&tx_logs, headers[&n].timestamp));
        }
        Ok(out)
    }

    /// Records blocks `first` to `last` as read under `head`, with their observations, each
    /// final, in one transaction. An observation already recorded at its coordinate is not
    /// recorded again (`INV-10`). Returns how many were new.
    async fn record_read(
        &self,
        first: u64,
        last: u64,
        head: &Header,
        observations: &[Observed],
    ) -> Result<usize, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let mut new = 0;
        for o in observations {
            let id: Option<i64> = sqlx::query_scalar(
                "INSERT INTO observations (network, block_number, block_hash, block_time, \
                   position_kind, position_index, token, amount, sender, recipient, \
                   on_chain_reference, executed, detail, kind) \
                 VALUES ($1, $2, $3, to_timestamp(0) + $4::bigint * interval '1 second', 'log', \
                   $5, $6, $7::numeric, $8, $9, $10, true, $11::text::jsonb, $12) \
                 ON CONFLICT ON CONSTRAINT observations_coordinate_once DO NOTHING RETURNING id",
            )
            .bind(self.network)
            .bind(i64_of(o.log.block_number)?)
            .bind(o.log.block_hash.as_slice())
            .bind(i64_of(o.time)?)
            .bind(i64_of(o.log.log_index)?)
            .bind(&o.token)
            .bind(&o.amount)
            .bind(&o.sender)
            .bind(&o.recipient)
            .bind(o.reference.map(|r| r.to_vec()))
            .bind(o.detail.to_string())
            .bind(kind(o.event))
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(id) = id {
                new += 1;
                sqlx::query(
                    "INSERT INTO observation_outcomes (observation_id, outcome, \
                       final_block_number, final_block_hash) VALUES ($1, 'final', $2, $3)",
                )
                .bind(id)
                .bind(i64_of(head.number)?)
                .bind(head.hash.as_slice())
                .execute(&mut *tx)
                .await?;
            }
        }
        sqlx::query(
            "INSERT INTO observation_ranges (network, first_block, last_block, outcome, \
               final_block_number, final_block_hash) VALUES ($1, $2, $3, 'read', $4, $5)",
        )
        .bind(self.network)
        .bind(i64_of(first)?)
        .bind(i64_of(last)?)
        .bind(i64_of(head.number)?)
        .bind(head.hash.as_slice())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(new)
    }
}
