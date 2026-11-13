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
//! **Reads in flight** (`walk_concurrency`): a step asks for up to that many blocks' headers (by
//! number, ahead of the walk by parent hash; [`crate::chain::Descent`]) and events (the proven
//! read; [`Chain::read_events`]) at once, and takes the answers **in block order**: each block is
//! decoded after its parent, checked and recorded as with one at a time, and the first block that
//! fails stops the step there, whatever was read after it. Nothing is accepted on other terms than
//! a read one at a time: a header is the one its child names, a value is proven under its block's
//! verified header.
//!
//! **Rungs** (issue #49): a first start is about 81,000 blocks behind. Walking them all back from the
//! finalized head before reading any would lose the walk to its first late error or restart, so the
//! backlog is read **a rung at a time**: a rung is the blocks from the watermark up to the next
//! multiple of `rung`. Reaching that block from the head by parent hash records an **anchor** at it
//! and at every multiple of `rung` passed on the way (`observation_walk_anchors`), so the walk
//! resumes from the lowest anchor it reached, and later rungs walk at most `rung` headers. A walk
//! that cannot go on because a header is unavailable keeps its anchors and tries again; one that
//! meets a header which does not verify is a visible gap.
//!
//! **Final heads** (`REQ-OB-2`, `REQ-OB-5`; issue #131): every outcome is written under the
//! finalized head the block was read under, and that head is recorded in `final_heads`, with its
//! block time, in the same transaction, so the core finds the final point an outcome names. Another
//! hash already recorded at its height is an error: nothing is recorded, and the step says so. An
//! outcome written before this rule held may name a head that was never recorded; each cycle,
//! [`Follower::record_missing_heads`] walks to that height from the finalized head by parent hash,
//! and records the head only if the chain has the outcome's hash there.
//!
//! **What an observation is**: one event of the contract, at its coordinate (network, block number,
//! block hash, event index), recorded once (`INV-10`): `Minted`, `Redeemed`, `RefundMinted`, the
//! pause and bounds events, and the contract's other events by name. A redemption's observation
//! carries what the core needs to turn it into a withdrawal: the redeemer, the reference, the
//! backend, and the payout address or commitment (`REQ-RQ-9`). The endpoint decides nothing: it
//! writes no transfer state.

use futures_util::StreamExt;
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};

use crate::chain::{BlockRef, Chain, ChainError, ChainResult, Descent, ancestry_with};
use crate::kreivo::{self, AccountId32, Backend, ContractEvent, Payout};
use crate::supply::{self, Supplies};

/// How many headers a walk reads between two calls of its `progress` callback.
pub const WALK_PROGRESS_EVERY: u64 = 256;

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
    /// The finalized head could not be read, or a walk back from it could not go on: nothing was
    /// read, though a walk keeps the anchors it reached and resumes from them.
    Unavailable(String),
}

/// What [`Follower::record_missing_heads`] did. Each point not recorded is named in `findings`,
/// which the caller logs when they change, so one that stays as it was is not logged every pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Backfill {
    /// Final heads recorded.
    pub recorded: usize,
    /// Final points whose hash is not the finalized chain's, or not the head recorded, at their
    /// height: nothing recorded.
    pub mismatched: usize,
    /// Final points not recorded this time (the chain did not answer, the point is above the
    /// finalized head, or no head with supply readings is recorded above it yet): looked for
    /// again next time.
    pub pending: usize,
    /// Final points more than [`BACKFILL_WALK`] (or `max_walk`) blocks below where a walk to them
    /// would start: not walked to, so a backlog of old points never holds the loop up.
    pub too_far: usize,
    /// Why each point not recorded was not, one line each.
    pub findings: Vec<String>,
}

/// The most blocks the backfill walks down to one final point ([`Follower::record_missing_heads`]):
/// four rungs. The walk runs inside the endpoint's loop, so a point further down than this is not
/// walked to; it waits, and is named in the findings.
pub const BACKFILL_WALK: u64 = 4 * WALK_RUNG;

/// Follows one network's finalized head. See the module documentation./// Follows one network's finalized head. See the module documentation.
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
    /// The blocks between two anchors of a long walk, and the most one step reads (at least 1).
    pub rung: u64,
    /// The most blocks whose header or events are read at once (at least 1; see the module
    /// documentation).
    pub concurrency: usize,
    /// The supply reader: the head outcomes are written under is recorded with its supply
    /// readings first ([`supply::ready_head`]). `None` records the head alone.
    pub supplies: Option<&'a Supplies<'a, C>>,
}

/// The default distance between anchors.
pub const WALK_RUNG: u64 = 4_096;

/// Where a walk of the backfill ended ([`Follower::record_missing_heads`]).
enum Reach {
    /// The finalized chain's hash at the height.
    At([u8; 32]),
    /// Not reached now: looked for again next time.
    Pending(String),
    /// Too far below where the walk would start: not walked to.
    TooFar(String),
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
        if finalized.number - next >= self.max_walk {
            // The same words every look, so the same open gap is recorded once.
            let reason = format!(
                "more than max_walk ({}) blocks behind the finalized head",
                self.max_walk
            );
            return self.gap(next, finalized.number, &finalized, reason).await;
        }
        // This step's rung ends at the next multiple of `rung`, or at the head.
        let rung = self.rung.max(1);
        let top = next.div_ceil(rung).saturating_mul(rung);
        let head = if top >= finalized.number {
            finalized
        } else {
            match self.anchor(&finalized, top).await? {
                Ok(hash) => BlockRef { number: top, hash },
                Err(e @ ChainError::Unavailable(_)) => {
                    // Whatever the walk anchored is kept: the next step resumes from there.
                    tracing::warn!(network = self.network, to = top, reason = %e, "REQ-OB-3: the walk back from the finalized head could not go on; it resumes from its last anchor");
                    return Ok(Step::Unavailable(e.to_string()));
                }
                Err(e) => return self.gap(next, top, &finalized, e.to_string()).await,
            }
        };
        let concurrency = self.concurrency.max(1);
        let blocks = match ancestry_with(self.chain, &head, next, self.max_walk, concurrency).await
        {
            Ok(Some(blocks)) => blocks,
            Ok(None) => {
                let reason = format!(
                    "more than max_walk ({}) blocks behind the finalized head",
                    self.max_walk
                );
                return self.gap(next, head.number, &finalized, reason).await;
            }
            Err(e) => {
                return self.gap(next, head.number, &finalized, e.to_string()).await;
            }
        };
        let mut seq = match last_seq(self.pool, self.network, &self.contract).await? {
            Some(s) => s + 1,
            None => self.first_seq,
        };
        let mut read = 0;
        let mut first_read = None;
        let mut last_read = None;
        // The finalized head's block time, read once, when an outcome is first written under it.
        let mut head_time = None;
        // Up to `concurrency` reads in flight, answered in block order; dropped (and the reads
        // still in flight with it) at the first block that fails.
        let mut reads = futures_util::stream::iter(&blocks)
            .map(|b| self.chain.read_events(b))
            .buffered(concurrency);
        for chunk in blocks.chunks(usize::try_from(self.max_range).unwrap_or(usize::MAX)) {
            let mut observations = Vec::new();
            let mut failed = None;
            let mut done = 0;
            for block in chunk {
                let Some(events) = reads.next().await else {
                    unreachable!("one read per block");
                };
                match self.read_block(block, events, &mut seq).await {
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
                // REQ-OB-2: an outcome rests on the head it is written under, recorded with its
                // block time, read from the chain and never guessed (a rung's head is not among
                // the blocks read).
                if !observations.is_empty() && head_time.is_none() {
                    match supply::ready_head(self.chain, self.supplies, &self.contract, &finalized)
                        .await?
                    {
                        Ok(t) => head_time = Some(t),
                        Err(why) => {
                            tracing::warn!(network = self.network, head = finalized.number, reason = %why, "REQ-OB-2: the finalized head could not be recorded; nothing is recorded under it, and the next step reads again");
                            return Ok(Step::Unavailable(why));
                        }
                    }
                }
                match record_read(
                    self.pool,
                    self.network,
                    &self.contract,
                    first,
                    last,
                    &finalized,
                    head_time,
                    &observations,
                )
                .await?
                {
                    Ok(new) => read += new,
                    Err(conflict) => {
                        tracing::error!(network = self.network, %conflict, "REQ-OB-2: the finalized head read is not the one recorded at its height; nothing is recorded under it");
                        return Ok(Step::Unavailable(conflict.to_string()));
                    }
                }
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

    /// The hash of the final block `number`, reached from `finalized` by parent hash: an anchor
    /// recorded earlier, or else the walk down from the lowest anchor above it (the head itself
    /// if there is none), anchoring every multiple of `rung` and `number` on the way. A walk that
    /// fails keeps its anchors.
    pub async fn anchor(
        &self,
        finalized: &BlockRef,
        number: u64,
    ) -> Result<Result<[u8; 32], ChainError>, sqlx::Error> {
        self.anchor_with(finalized, number, &mut || {}).await
    }

    /// [`Follower::anchor`] that calls `progress` every [`WALK_PROGRESS_EVERY`] headers it walks: a
    /// walk over a long backlog can take minutes, and a caller that watches it (the container's
    /// health check, issue #61) hears that it moves.
    pub async fn anchor_with(
        &self,
        finalized: &BlockRef,
        number: u64,
        progress: &mut (dyn FnMut() + Send),
    ) -> Result<Result<[u8; 32], ChainError>, sqlx::Error> {
        let mut headers = 0u64;
        let rung = self.rung.max(1);
        let concurrency = self.concurrency.max(1);
        let above: Option<(i64, Vec<u8>)> = sqlx::query_as(
            "SELECT block_number, block_hash FROM observation_walk_anchors \
             WHERE network = $1 AND block_number >= $2 ORDER BY block_number LIMIT 1",
        )
        .bind(self.network)
        .bind(i64_of(number)?)
        .fetch_optional(self.pool)
        .await?;
        let mut at =
            match above.and_then(|(n, h)| Some((u64::try_from(n).ok()?, h.try_into().ok()?))) {
                Some((n, hash)) if n == number => return Ok(Ok(hash)),
                Some((n, hash)) => BlockRef { number: n, hash },
                None => *finalized,
            };
        if at.number < number {
            return Ok(Err(ChainError::Invalid(format!(
                "block {number} is above the finalized head {}",
                finalized.number
            ))));
        }
        let mut walk = Descent::new(self.chain, at, number, concurrency);
        loop {
            if at.number == number || at.number.is_multiple_of(rung) {
                match self.keep_anchor(&at, finalized).await? {
                    Ok(()) => {}
                    Err(e) => return Ok(Err(e)),
                }
            }
            if at.number == number {
                return Ok(Ok(at.hash));
            }
            let header = match walk.next().await {
                Ok(h) => h,
                Err(e) => return Ok(Err(e)),
            };
            at = BlockRef {
                number: at.number - 1,
                hash: header.parent,
            };
            headers += 1;
            if headers.is_multiple_of(WALK_PROGRESS_EVERY) {
                progress();
            }
        }
    }

    /// Records `at` as an anchor under `finalized`; another hash at its height is an error.
    async fn keep_anchor(
        &self,
        at: &BlockRef,
        finalized: &BlockRef,
    ) -> Result<Result<(), ChainError>, sqlx::Error> {
        sqlx::query(
            "INSERT INTO observation_walk_anchors (network, block_number, block_hash, \
               final_block_number, final_block_hash) VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT ON CONSTRAINT observation_walk_anchors_one_hash_per_height DO NOTHING",
        )
        .bind(self.network)
        .bind(i64_of(at.number)?)
        .bind(at.hash.as_slice())
        .bind(i64_of(finalized.number)?)
        .bind(finalized.hash.as_slice())
        .execute(self.pool)
        .await?;
        let stored: Vec<u8> = sqlx::query_scalar(
            "SELECT block_hash FROM observation_walk_anchors \
             WHERE network = $1 AND block_number = $2",
        )
        .bind(self.network)
        .bind(i64_of(at.number)?)
        .fetch_one(self.pool)
        .await?;
        Ok(if stored == at.hash {
            Ok(())
        } else {
            Err(ChainError::Invalid(format!(
                "block {} is 0x{} on the chain walked, but 0x{} was anchored there earlier",
                at.number,
                hex::encode(at.hash),
                hex::encode(stored)
            )))
        })
    }

    /// The hash of `start_block`, reached from the finalized head by parent hash (issue #49): what
    /// the start-up check holds each history source's `start_block` to. A first start walks the
    /// whole backlog to get it, which the follower would walk next anyway.
    pub async fn start_anchor(&self) -> Result<Result<[u8; 32], ChainError>, sqlx::Error> {
        self.start_anchor_with(&mut || {}).await
    }

    /// [`Follower::start_anchor`], calling `progress` as the walk moves (see
    /// [`Follower::anchor_with`]).
    pub async fn start_anchor_with(
        &self,
        progress: &mut (dyn FnMut() + Send),
    ) -> Result<Result<[u8; 32], ChainError>, sqlx::Error> {
        let finalized = match self.chain.finalized().await {
            Ok(f) => f,
            Err(e) => return Ok(Err(e)),
        };
        // The walk starts at the lowest anchor at or above start_block (the finalized head if
        // there is none), so `max_walk` bounds the distance still to walk, not how far the chain
        // has moved since start_block: a caught-up endpoint restarts at any height, and a
        // start_block set far too low on a first start is refused here, not after days of walking.
        let above: Option<i64> = sqlx::query_scalar(
            "SELECT block_number FROM observation_walk_anchors \
             WHERE network = $1 AND block_number >= $2 ORDER BY block_number LIMIT 1",
        )
        .bind(self.network)
        .bind(i64_of(self.start_block)?)
        .fetch_optional(self.pool)
        .await?;
        let origin = above
            .and_then(|n| u64::try_from(n).ok())
            .filter(|n| *n <= finalized.number);
        if let Some(from) = origin.filter(|from| *from > self.start_block) {
            if from - self.start_block >= self.max_walk {
                return Ok(Err(ChainError::Invalid(format!(
                    "the lowest stored anchor, block {from}, is more than max_walk ({}) blocks above start_block {}; raise max_walk or set start_block to a later block",
                    self.max_walk, self.start_block
                ))));
            }
        } else if origin.is_none()
            && finalized.number.saturating_sub(self.start_block) >= self.max_walk
        {
            return Ok(Err(ChainError::Invalid(format!(
                "start_block {} is more than max_walk ({}) blocks behind the finalized head {} and nothing is stored to walk from",
                self.start_block, self.max_walk, finalized.number
            ))));
        }
        self.anchor_with(&finalized, self.start_block, progress)
            .await
    }

    /// Records the final heads that outcomes and non-execution proofs of this network name and
    /// `final_heads` lacks (issue #131: written before the follower and the prover recorded their
    /// head). Each is reached from the finalized head by parent hash, as any block the endpoint
    /// reads ([`Follower::anchor`]), so the chain's finalized head is at or above it; it is recorded,
    /// with the block time read from the chain, only if the chain has the hash the outcome names
    /// at that height. A hash that differs records nothing. Highest first, so each walk starts at
    /// the anchors the one before left.
    ///
    /// A recorded head carries no supply readings, and the reserve check takes the latest final
    /// head at or below the watermark. So a point is recorded only below a head that has its
    /// readings and is at or below the watermark: the check then never takes it. Until there is
    /// one, it waits. A point more than [`BACKFILL_WALK`] blocks (or `max_walk`) below where its
    /// walk would start is not walked to at all.
    pub async fn record_missing_heads(&self) -> Result<Backfill, sqlx::Error> {
        self.record_missing_heads_with(&mut || {}).await
    }

    /// [`Follower::record_missing_heads`], calling `progress` as a walk moves (see
    /// [`Follower::anchor_with`]).
    pub async fn record_missing_heads_with(
        &self,
        progress: &mut (dyn FnMut() + Send),
    ) -> Result<Backfill, sqlx::Error> {
        let missing: Vec<(i64, Vec<u8>)> = sqlx::query_as(
            "SELECT DISTINCT p.number, p.hash FROM ( \
                 SELECT x.final_block_number AS number, x.final_block_hash AS hash \
                 FROM observation_outcomes x JOIN observations o ON o.id = x.observation_id \
                 WHERE o.network = $1 AND x.final_block_number IS NOT NULL \
                   AND x.final_block_hash IS NOT NULL \
                 UNION \
                 SELECT final_block_number, final_block_hash FROM non_execution_proofs \
                 WHERE network = $1) p \
             WHERE NOT EXISTS (SELECT 1 FROM final_heads h WHERE h.network = $1 \
                                 AND h.block_number = p.number AND h.block_hash = p.hash) \
             ORDER BY p.number DESC",
        )
        .bind(self.network)
        .fetch_all(self.pool)
        .await?;
        let mut done = Backfill::default();
        if missing.is_empty() {
            return Ok(done);
        }
        let finalized = match self.chain.finalized().await {
            Ok(f) => f,
            Err(e) => {
                done.pending = missing.len();
                done.findings.push(format!(
                    "{} final points wait: the finalized head could not be read: {e}",
                    missing.len()
                ));
                return Ok(done);
            }
        };
        // The highest head with its readings, at or below the watermark: what the reserve check
        // takes, unless a later one comes.
        let supplied: Option<i64> = sqlx::query_scalar(
            "SELECT max(h.block_number) FROM final_heads h \
             JOIN observation_watermarks w ON w.network = h.network \
             WHERE h.network = $1 AND h.block_number <= w.watermark \
               AND (EXISTS (SELECT 1 FROM reserve_readings r WHERE r.network = h.network \
                              AND r.block_number = h.block_number AND r.block_hash = h.block_hash \
                              AND r.kind = 'supply') \
                    OR EXISTS (SELECT 1 FROM fee_position_readings f WHERE f.network = h.network \
                              AND f.block_number = h.block_number AND f.block_hash = h.block_hash))",
        )
        .bind(self.network)
        .fetch_one(self.pool)
        .await?;
        for (number, named) in missing {
            let (Ok(n), Ok(named)) = (
                u64::try_from(number),
                <[u8; 32]>::try_from(named.as_slice()),
            ) else {
                done.mismatched += 1;
                done.findings.push(format!(
                    "block {number}: not a block of this network; nothing is recorded"
                ));
                continue;
            };
            if supplied.is_none_or(|s| s <= number) {
                done.pending += 1;
                done.findings.push(format!(
                    "block {n}: waits for a head with supply readings above it, at or below the watermark (the reserve check takes the latest)"
                ));
                continue;
            }
            match self.reach(&finalized, n, progress).await? {
                Reach::At(hash) if hash == named => {}
                Reach::At(hash) => {
                    done.mismatched += 1;
                    done.findings.push(format!(
                        "block {n}: the outcome names {}, the finalized chain has {}; nothing is recorded",
                        hex0x(&named),
                        hex0x(&hash)
                    ));
                    continue;
                }
                Reach::Pending(why) => {
                    done.pending += 1;
                    done.findings
                        .push(format!("block {n}: not reached now: {why}"));
                    continue;
                }
                Reach::TooFar(why) => {
                    done.too_far += 1;
                    done.findings
                        .push(format!("block {n}: not walked to: {why}"));
                    continue;
                }
            }
            let at = BlockRef {
                number: n,
                hash: named,
            };
            let time_ms = match self.chain.events(&at, &self.contract).await {
                Ok(e) => e.time_ms,
                Err(e) => {
                    done.pending += 1;
                    done.findings
                        .push(format!("block {n}: its block time could not be read: {e}"));
                    continue;
                }
            };
            let mut conn = self.pool.acquire().await?;
            match record_head(&mut conn, self.network, &at, time_ms).await? {
                Ok(()) => {
                    tracing::info!(network = self.network, block = n, hash = %hex0x(&named), "REQ-OB-2: recorded the final head an outcome names, verified on the finalized chain");
                    done.recorded += 1;
                }
                Err(conflict) => {
                    done.mismatched += 1;
                    done.findings
                        .push(format!("block {n}: {conflict}; nothing is recorded"));
                }
            }
        }
        Ok(done)
    }

    /// The hash of the final block `number`, reached from `finalized` by parent hash
    /// ([`Follower::anchor_with`]), if it is fewer than [`BACKFILL_WALK`] and `max_walk` blocks
    /// below where the walk starts (the lowest anchor at or above it, or the finalized head).
    async fn reach(
        &self,
        finalized: &BlockRef,
        number: u64,
        progress: &mut (dyn FnMut() + Send),
    ) -> Result<Reach, sqlx::Error> {
        if number > finalized.number {
            return Ok(Reach::Pending(format!(
                "it is above the finalized head {}",
                finalized.number
            )));
        }
        let above: Option<i64> = sqlx::query_scalar(
            "SELECT block_number FROM observation_walk_anchors \
             WHERE network = $1 AND block_number >= $2 ORDER BY block_number LIMIT 1",
        )
        .bind(self.network)
        .bind(i64_of(number)?)
        .fetch_optional(self.pool)
        .await?;
        let from = above
            .and_then(|n| u64::try_from(n).ok())
            .filter(|n| *n <= finalized.number)
            .unwrap_or(finalized.number);
        let bound = self.max_walk.min(BACKFILL_WALK);
        if from - number >= bound {
            return Ok(Reach::TooFar(format!(
                "{} blocks below block {from}, where the walk would start; the bound is {bound}",
                from - number
            )));
        }
        Ok(match self.anchor_with(finalized, number, progress).await? {
            Ok(hash) => Reach::At(hash),
            Err(e) => Reach::Pending(e.to_string()),
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
        read: ChainResult<C::EventsRead>,
        seq: &mut u64,
    ) -> Result<Vec<NewObservation>, String> {
        let read = read.map_err(|e| e.to_string())?;
        let events = self
            .chain
            .decode_events(block, read, &self.contract)
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

/// The store holds another hash than the chain's at a final head's height (`REQ-OB-2`): a network
/// that finalised two blocks at one height, or a read that lied. Nothing may be decided on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadConflict {
    /// The height.
    pub number: u64,
    /// The hash the chain was read with there.
    pub read: [u8; 32],
    /// The hash the store holds there.
    pub stored: Vec<u8>,
}

impl std::fmt::Display for HeadConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "REQ-OB-2: block {} is final as {} here, and {} is recorded",
            self.number,
            hex0x(&self.read),
            hex0x(&self.stored)
        )
    }
}

/// Records `head`, a final block of `network` with its block time, in `final_heads` (one hash per
/// height), in `conn`. A head already recorded with the same hash is recorded; another hash there
/// is a [`HeadConflict`], never a silent pass.
pub async fn record_head(
    conn: &mut PgConnection,
    network: &str,
    head: &BlockRef,
    time_ms: u64,
) -> Result<Result<(), HeadConflict>, sqlx::Error> {
    sqlx::query(
        "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
         VALUES ($1, $2, $3, to_timestamp(0) + $4 * interval '1 millisecond') \
         ON CONFLICT ON CONSTRAINT final_heads_one_hash_per_height DO NOTHING",
    )
    .bind(network)
    .bind(i64_of(head.number)?)
    .bind(head.hash.as_slice())
    .bind(i64_of(time_ms)?)
    .execute(&mut *conn)
    .await?;
    let stored: Vec<u8> = sqlx::query_scalar(
        "SELECT block_hash FROM final_heads WHERE network = $1 AND block_number = $2",
    )
    .bind(network)
    .bind(i64_of(head.number)?)
    .fetch_one(&mut *conn)
    .await?;
    Ok(if stored == head.hash {
        Ok(())
    } else {
        Err(HeadConflict {
            number: head.number,
            read: head.hash,
            stored,
        })
    })
}

/// Records blocks `first` to `last` as read under `finalized`, with their observations, in one
/// transaction. An observation already recorded at its coordinate is not recorded again
/// (`INV-10`). Returns how many were new.
///
/// When one is new, its outcome is written under `finalized`, and `finalized` is recorded in
/// `final_heads` in the same transaction with `finalized_time_ms`, its block time, which must then
/// be given (`REQ-OB-2`, `REQ-OB-5`). Another hash recorded at its height records nothing: the
/// transaction is rolled back and the conflict returned.
#[allow(clippy::too_many_arguments)]
pub async fn record_read(
    pool: &PgPool,
    network: &str,
    contract: &AccountId32,
    first: u64,
    last: u64,
    finalized: &BlockRef,
    finalized_time_ms: Option<u64>,
    observations: &[NewObservation],
) -> Result<Result<usize, HeadConflict>, sqlx::Error> {
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
    if new > 0 {
        // REQ-OB-2, REQ-OB-5: the final point the outcomes name is a recorded final head.
        let Some(time_ms) = finalized_time_ms else {
            return Err(sqlx::Error::InvalidArgument(format!(
                "REQ-OB-2: outcomes are written under block {} without its block time",
                finalized.number
            )));
        };
        if let Err(conflict) = record_head(&mut tx, network, finalized, time_ms).await? {
            tx.rollback().await?;
            return Ok(Err(conflict));
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
    Ok(Ok(new))
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
