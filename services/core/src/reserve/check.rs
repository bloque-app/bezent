// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The reserve check (`T-004-03`, `T-004-04`; `REQ-BK-1`, `REQ-BK-2`, `REQ-BK-3`, `REQ-BK-8`,
//! `REQ-BK-10`, `REQ-BE-6`, `REQ-IC-2`, `NFR-5`, `INV-1`).
//!
//! [`ReserveCheck::tick`] evaluates every asset whose last evaluation is a minute old or more, by
//! the clock it is given, so a test drives it minute by minute.
//!
//! An evaluation of asset *A*:
//!
//! 1. Posts every final movement not yet in the book ([`book::post_pending`]).
//! 2. Picks one final point per network: the latest recorded final head at or below the block up
//!    to which the network is read without a hole (`observation_watermarks`, `REQ-OB-3`) and below
//!    every observation still awaiting its outcome. Every movement at or before that point is
//!    then observed, final and booked.
//! 3. Re-derives every term at those points (`REQ-BK-8`): reserve and supply from [`Readings`];
//!    owed mints and owed payouts, item by item, from the final observations the endpoints
//!    recorded (an arrival not minted, refunded or contributed; a redemption not paid out or
//!    refund-minted), matched by the references bezent writes, never by amount or time
//!    (`REQ-OT-2`); value in transit is empty, since bezent sends no value through a venue in V0
//!    (SPEC §4.3, `DEF-8`); an attested position is its party's report (`REQ-BE-4`).
//! 4. Reads the book as of the same points, compares every term and every item at zero tolerance,
//!    and checks that the surplus the networks imply is the surplus the book records and is not
//!    negative (`REQ-BK-3`).
//!
//! Any difference is a divergence: the asset's routes pause (a `pauses` row the store lets the core
//! write for this cause only) and a human is paged (`REQ-IC-2`, `AC-E2.1`). Transfers in flight
//! keep being observed and booked; only new work stops (`INV-15`). Lifting the pause is the
//! operator's. Surplus below its alert level, below its floor, or a reversal not restored within a
//! business day alerts (`REQ-BK-10`, `REQ-BE-6`); so does a floor or alert level not declared.
//!
//! A supply reading the endpoint flagged (the on-chain component's count and the issuance differ)
//! is a breach of `INV-4` (PLAN `DEC-16`, 2026-10-15): it raises `key-compromise` once per
//! mismatch event and pauses the asset's routes under `INV-4`, and while it stands at the point read the
//! evaluation diverges whatever the book says ([`supply_breach`]; PR #50 review M1).
//!
//! # The check never stops, and never holds on an old point (review M1, M2)
//!
//! - Each final point records how far, in block time, it lags its network's latest recorded final
//!   head. Beyond the network's bound (`networks.reserve_point_lag_seconds`: one minute plus its
//!   finality time) the evaluation diverges, whatever the terms say: an observation that never
//!   gets its outcome, or a read watermark that stalls, cannot freeze the point the check reads
//!   at (`NFR-5`, `REQ-IC-2`: a finality stall, an observation gap).
//! - The latest recorded final head is measured against the check's clock as well: a network with
//!   no final head recorded within its bound diverges too (review MF2), so heads that stop being
//!   recorded (both providers behind, the observer down) never leave the check holding on an old
//!   point while mints go unseen.
//! - An evaluation that cannot be made (no final point, a reading or the store failed, or the
//!   evaluation itself errored) is recorded as incomplete and paged once, never taken as holding.
//!   One that stays incomplete for longer than the smallest bound of the asset's networks pauses
//!   the asset's routes (`REQ-IC-2`; review SF1).
//! - Each asset is evaluated on its own: a failure on one is recorded and paged, and the others
//!   are still evaluated; an observation the book cannot post is quarantined
//!   ([`book::post_pending_with`]) and the asset it moves is evaluated at once.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};

use super::readings::{self, Readings, SupplyMismatch};
use super::{Asset, FinalPoint, Item, Terms, book, coordinate_key, map_json, text};
use crate::refs;
use crate::store::{self, Observation};

/// How often each asset is evaluated: once a minute (`NFR-5`).
pub fn period() -> Duration {
    Duration::minutes(1)
}

/// The time the check runs at. Tests drive a clock of their own.
pub trait Clock: Send + Sync {
    /// Now.
    fn now(&self) -> DateTime<Utc>;
}

/// The wall clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A page to a human (`REQ-IC-2`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    /// What happened: `reserve-divergence`, `reserve-alert` or `reserve-check-incomplete`.
    pub kind: String,
    /// The asset.
    pub asset: String,
    /// The detail, as recorded in the event log.
    pub detail: Value,
}

/// Who is paged. Every page is also in the event log, where no requester reads it.
pub trait Pager: Send + Sync {
    /// Pages a human.
    fn page(&self, page: &Page);
}

/// Pages through the service's log, at error level, for the deployment's alerting to carry.
#[derive(Clone, Copy, Debug, Default)]
pub struct LogPager;

impl Pager for LogPager {
    fn page(&self, page: &Page) {
        tracing::error!(kind = %page.kind, asset = %page.asset, detail = %page.detail, "page");
    }
}

/// An evaluation's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every term matches the book and surplus is not negative (`INV-1`).
    Holds,
    /// Some term differs, or surplus is negative (`REQ-BK-1`, `REQ-BK-3`).
    Diverges,
    /// It could not be made; see the reason.
    Incomplete,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Holds => "holds",
            Self::Diverges => "diverges",
            Self::Incomplete => "incomplete",
        }
    }
}

/// A term or item the networks and the book disagree on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Difference {
    /// The term: `reserve`, `in_transit`, `supply`, `owed_mint`, `owed_payout`, `surplus`.
    pub term: String,
    /// The item or network, for an itemised term.
    pub item: Option<String>,
    /// What the networks show, in book units.
    pub chain: i128,
    /// What the book holds.
    pub book: i128,
}

impl Difference {
    fn json(&self) -> Value {
        json!({"term": self.term, "item": self.item,
               "chain": text(self.chain), "book": text(self.book)})
    }
}

/// How far a final point lags its network's latest recorded final head, and how far that head
/// lags the check's clock (review M2, MF2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Lag {
    /// The latest recorded final head of the network.
    pub head_block_number: i64,
    /// The point's distance from it in blocks.
    pub blocks: i64,
    /// Its distance in block time, in seconds.
    pub seconds: i64,
    /// How long before the check's clock the latest recorded final head was produced, in
    /// seconds: how long no newer final head has been recorded (review MF2).
    pub head_age_seconds: i64,
    /// The most either may be (`networks.reserve_point_lag_seconds`).
    pub bound_seconds: i64,
}

impl Lag {
    /// Whether the point lags its network's head beyond the bound: the evaluation cannot hold on
    /// it.
    pub fn behind_head(&self) -> bool {
        self.seconds > self.bound_seconds
    }

    /// Whether no final head has been recorded for the network within the bound, by the clock: a
    /// finality stall, or an observer that stopped (review MF2; `NFR-6`, `REQ-IC-2`).
    pub fn head_stale(&self) -> bool {
        self.head_age_seconds > self.bound_seconds
    }

    /// Whether the evaluation cannot hold on this point, for either reason.
    pub fn stale(&self) -> bool {
        self.behind_head() || self.head_stale()
    }
}

/// One evaluation, as recorded.
#[derive(Clone, Debug)]
pub struct Evaluation {
    /// Its row in `reserve_evaluations`.
    pub id: i64,
    /// The asset.
    pub asset: String,
    /// When, by the check's clock.
    pub evaluated_at: DateTime<Utc>,
    /// The outcome.
    pub outcome: Outcome,
    /// Why it is incomplete.
    pub reason: Option<String>,
    /// The final point per network.
    pub points: BTreeMap<String, FinalPoint>,
    /// How far each point lags its network's latest recorded final head.
    pub lags: BTreeMap<String, Lag>,
    /// The terms re-derived from the networks.
    pub chain: Terms,
    /// The book's terms as of the same points.
    pub book: Terms,
    /// Every difference.
    pub differences: Vec<Difference>,
    /// The alerts that hold.
    pub alerts: Vec<String>,
}

/// The reserve check over the core's pool, with the readings it makes and whom it pages.
pub struct ReserveCheck<R, P> {
    pool: PgPool,
    readings: R,
    pager: P,
    /// Assets whose evaluation failed and could not even be recorded, since when, and whether the
    /// stall was paged: the store cannot hold the episode, so the check does (review SF1).
    unrecorded: Mutex<HashMap<String, (DateTime<Utc>, bool)>>,
}

/// What a re-derivation found, for the evaluation's record and the statement.
#[derive(Default)]
struct Derived {
    terms: Terms,
    reserve: Vec<Value>,
    supply: Value,
    owed_mint: Vec<Item>,
    owed_payout: Vec<Item>,
    convertible: Vec<Value>,
    /// The arrivals from the operator's declared funding addresses, contributions to surplus, so a
    /// verifier can tell them from owed mints (`REQ-BK-5`, `AC-E1.1`).
    contributions: Vec<Value>,
    /// The supply reading at the issuing network's point, when the endpoint flagged it: the
    /// on-chain component's count and the issuance differ (`INV-4`; PR #50 review M1).
    supply_mismatch: Option<SupplyMismatch>,
}

type Incomplete = String;

/// The final points an evaluation rests on, how far each lags, and what was derived at them.
type Points = (BTreeMap<String, FinalPoint>, BTreeMap<String, Lag>, Derived);

impl<R: Readings, P: Pager> ReserveCheck<R, P> {
    /// The check over `pool` (the core's login), `readings` and `pager`.
    pub fn new(pool: PgPool, readings: R, pager: P) -> Self {
        Self {
            pool,
            readings,
            pager,
            unrecorded: Mutex::default(),
        }
    }

    /// The readings it makes.
    pub fn readings(&self) -> &R {
        &self.readings
    }

    /// Evaluates every asset whose last evaluation is at least [`period`] old by `clock`, and
    /// returns the evaluations made (`NFR-5`). Each asset is evaluated on its own: one whose
    /// evaluation fails is paged and recorded as incomplete when the store allows it, and the
    /// others are still evaluated; an asset with an observation just quarantined is evaluated at
    /// once (review M1). Only a store that cannot list the assets is an error.
    pub async fn tick(&self, clock: &impl Clock) -> sqlx::Result<Vec<Evaluation>> {
        let now = clock.now();
        let quarantined = self.post().await;
        let assets: Vec<(String, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT a.asset, (SELECT max(v.evaluated_at) FROM reserve_evaluations v \
                              WHERE v.asset = a.asset) \
             FROM assets a ORDER BY a.asset",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut done = Vec::new();
        for (asset, last) in assets {
            if (last.is_none_or(|l| now - l >= period()) || quarantined.contains(&asset))
                && let Some(e) = self.evaluate_or_page(&asset, now).await
            {
                done.push(e);
            }
        }
        Ok(done)
    }

    /// Posts what is pending; a failure to read it is logged, never a reason not to evaluate.
    async fn post(&self) -> Vec<String> {
        match book::post_pending_with(&self.pool, &self.pager).await {
            Ok(p) => p.quarantined,
            Err(e) => {
                tracing::warn!(error = %e, "REQ-BK-7: pending movements not read; evaluating anyway");
                Vec::new()
            }
        }
    }

    /// Evaluates `asset`. On a failure it records an incomplete evaluation, pages when the asset's
    /// evaluations turn incomplete (not on every failure), and pauses the asset's routes once they
    /// have been incomplete for longer than the bound, as an incomplete evaluation does (review
    /// SF1; `REQ-IC-2`). If even that cannot be recorded, the episode is kept here: paged once,
    /// and paged as a stall once past the bound (the pause needs the store).
    async fn evaluate_or_page(&self, asset: &str, now: DateTime<Utc>) -> Option<Evaluation> {
        let error = match self.evaluated(asset, now).await {
            Ok(e) => {
                self.unrecorded_episode().remove(asset);
                return Some(e);
            }
            Err(e) => e.to_string(),
        };
        match self.record_failure(asset, now, &error).await {
            Ok(pages) => {
                self.unrecorded_episode().remove(asset);
                for p in &pages {
                    self.pager.page(p);
                }
            }
            Err(r) => {
                tracing::error!(asset, error = %r, "NFR-5: a failed evaluation could not be recorded");
                let (first, stall) = {
                    let mut episodes = self.unrecorded_episode();
                    let first = !episodes.contains_key(asset);
                    let (since, stall_paged) =
                        episodes.entry(asset.to_owned()).or_insert((now, false));
                    let stall = !*stall_paged && now - *since > period();
                    *stall_paged |= stall;
                    (first, stall.then_some(*since))
                };
                if first {
                    self.pager.page(&Page {
                        kind: "reserve-check-failed".into(),
                        asset: asset.to_owned(),
                        detail: json!({"spec": "NFR-5, REQ-IC-2", "asset": asset,
                                       "error": error, "recorded": false}),
                    });
                }
                if let Some(since) = stall {
                    self.pager.page(&Page {
                        kind: "reserve-check-stalled".into(),
                        asset: asset.to_owned(),
                        detail: json!({
                            "spec": "REQ-IC-2, NFR-5, INV-1",
                            "asset": asset,
                            "incomplete_since": since.to_rfc3339(),
                            "action": "the reserve equation could not be evaluated, nor the failure recorded, for longer than the bound; the store refused the pause too: pause the asset's routes by hand",
                        }),
                    });
                }
            }
        }
        None
    }

    fn unrecorded_episode(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<String, (DateTime<Utc>, bool)>> {
        self.unrecorded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records a failed evaluation of `asset` as incomplete and returns the pages it calls for:
    /// `reserve-check-failed` when the previous evaluation was not incomplete, and the stall's.
    async fn record_failure(
        &self,
        asset: &str,
        now: DateTime<Utc>,
        error: &str,
    ) -> sqlx::Result<Vec<Page>> {
        let mut tx = self.pool.begin().await?;
        let previous: Option<String> = sqlx::query_scalar(
            "SELECT outcome FROM reserve_evaluations WHERE asset = $1 \
             ORDER BY evaluated_at DESC, id DESC LIMIT 1",
        )
        .bind(asset)
        .fetch_optional(&mut *tx)
        .await?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO reserve_evaluations (asset, evaluated_at, outcome, reason) \
             VALUES ($1, $2, 'incomplete', $3) RETURNING id",
        )
        .bind(asset)
        .bind(now)
        .bind(format!("the evaluation failed: {error}"))
        .fetch_one(&mut *tx)
        .await?;
        let mut pages = Vec::new();
        if previous.as_deref() != Some("incomplete") {
            pages.push(Page {
                kind: "reserve-check-failed".into(),
                asset: asset.to_owned(),
                detail: json!({"spec": "NFR-5, REQ-IC-2", "asset": asset, "evaluation": id,
                               "error": error}),
            });
        }
        if let Some(page) = stalled(&mut tx, asset, id, now).await? {
            pages.push(page);
        }
        for p in &pages {
            store::event(&mut tx, &p.kind, None, &p.detail).await?;
        }
        tx.commit().await?;
        Ok(pages)
    }

    /// Evaluates the reserve equation for `asset` at `now` and acts on the outcome. Posting what is
    /// pending first never stops it (review M1).
    pub async fn evaluate(&self, asset: &str, now: DateTime<Utc>) -> sqlx::Result<Evaluation> {
        self.post().await;
        self.evaluated(asset, now).await
    }

    async fn evaluated(&self, asset: &str, now: DateTime<Utc>) -> sqlx::Result<Evaluation> {
        let mut conn = self.pool.acquire().await?;
        let derived = self
            .derive(&mut conn, asset, now)
            .await
            .unwrap_or_else(|e| Err(format!("store: {e}")));
        let mut entry_cut = None;
        let (points, lags, derived, book_terms, reason) = match derived {
            Ok((points, lags, derived)) => {
                let cuts = points
                    .iter()
                    .map(|(n, p)| (n.clone(), p.block_number))
                    .collect();
                // The book as the evaluation reads it: no entry committed later, whatever its id
                // (review SF5; issue #42).
                let read = match book::entry_cut(&mut conn).await {
                    Ok(cut) => {
                        let terms = book::as_of(&mut conn, asset, &cuts, Some(&cut)).await;
                        entry_cut = Some(cut);
                        terms
                    }
                    Err(e) => Err(e),
                };
                match read {
                    Ok(b) => (points, lags, derived, b, None),
                    Err(e) => (
                        BTreeMap::new(),
                        BTreeMap::new(),
                        Derived::default(),
                        Terms::default(),
                        Some(format!("store: {e}")),
                    ),
                }
            }
            Err(why) => (
                BTreeMap::new(),
                BTreeMap::new(),
                Derived::default(),
                Terms::default(),
                Some(why),
            ),
        };
        let mut differences = if reason.is_some() {
            Vec::new()
        } else {
            compare(&derived.terms, &book_terms)
        };
        // INV-4 (PLAN DEC-16, 2026-10-15; PR #50 review M1): while the component's count and the
        // issuance differ at the point, the evaluation never holds, whatever the book says: a
        // book correction that matches the issuance does not make the breach go away.
        if let Some(m) = &derived.supply_mismatch {
            differences.push(Difference {
                term: "supply_mismatch".into(),
                item: Some(format!(
                    "INV-4: the on-chain component counts {} and the issuance is {} on {} at \
                     block {}: value was minted or destroyed outside the component",
                    m.contract_supply.as_deref().unwrap_or("?"),
                    m.issuance.as_deref().unwrap_or("?"),
                    m.network,
                    m.block_number
                )),
                chain: m
                    .issuance
                    .as_deref()
                    .and_then(|i| i.parse().ok())
                    .unwrap_or_default(),
                book: m
                    .contract_supply
                    .as_deref()
                    .and_then(|c| c.parse().ok())
                    .unwrap_or_default(),
            });
        }
        // Every mismatch the endpoint ever recorded for the asset, from the durable flag: one
        // gone by the point read now is still a breach to raise (PR #50 review M1).
        let mismatches = readings::supply_mismatches(&mut conn, asset).await?;
        // Review M2: a point too far behind its network's final head is no check at all; nor,
        // review MF2, is one on a network with no final head recorded within the bound by the
        // clock (a finality stall, an observer that stopped): mints there would go unseen.
        for (network, lag) in &lags {
            if lag.behind_head() {
                differences.push(Difference {
                    term: "final_point".into(),
                    item: Some(format!(
                        "{network}: the point lags the latest final head by {} s, beyond {} s \
                         (NFR-5, REQ-IC-2)",
                        lag.seconds, lag.bound_seconds
                    )),
                    chain: lag.head_block_number.into(),
                    book: points[network].block_number.into(),
                });
            }
            if lag.head_stale() {
                differences.push(Difference {
                    term: "final_head".into(),
                    item: Some(format!(
                        "{network}: no final head recorded for {} s, beyond {} s: a finality \
                         stall or an observer that stopped (NFR-5, NFR-6, REQ-IC-2)",
                        lag.head_age_seconds, lag.bound_seconds
                    )),
                    chain: lag.head_age_seconds.into(),
                    book: lag.bound_seconds.into(),
                });
            }
        }
        let outcome = match (&reason, differences.is_empty()) {
            (Some(_), _) => Outcome::Incomplete,
            (None, true) => Outcome::Holds,
            (None, false) => Outcome::Diverges,
        };
        let alerts = if reason.is_some() {
            Vec::new()
        } else {
            self.alerts(&mut conn, asset, &book_terms, now).await?
        };
        drop(conn);

        let mut tx = self.pool.begin().await?;
        let previous: Option<(String, Value)> = sqlx::query_as(
            "SELECT outcome, alerts FROM reserve_evaluations WHERE asset = $1 \
             ORDER BY evaluated_at DESC, id DESC LIMIT 1",
        )
        .bind(asset)
        .fetch_optional(&mut *tx)
        .await?;
        let points_json: serde_json::Map<String, Value> = points
            .iter()
            .map(|(n, p)| {
                let mut v = json!(p);
                v["lag"] = json!(lags.get(n));
                (n.clone(), v)
            })
            .collect();
        let chain_json = json!({
            "reserve": derived.reserve,
            "in_transit": {"amount": "0", "items": []},
            "supply": derived.supply,
            "owed_mint": {"amount": text(derived.terms.owed_mint.values().sum()),
                          "items": derived.owed_mint},
            "owed_payout": {"amount": text(derived.terms.owed_payout.values().sum()),
                            "items": derived.owed_payout},
            "surplus": {"amount": text(derived.terms.implied_surplus()),
                        "contributions": derived.contributions},
            "convertible": derived.convertible,
        });
        let book_json = json!({
            "reserve": map_json(&book_terms.reserve),
            "in_transit": map_json(&book_terms.in_transit),
            "supply": text(book_terms.supply),
            "owed_mint": map_json(&book_terms.owed_mint),
            "owed_payout": map_json(&book_terms.owed_payout),
            "surplus": text(book_terms.surplus),
        });
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO reserve_evaluations (asset, evaluated_at, outcome, reason, points, chain, \
                 book, differences, alerts, book_entry_cut, book_snapshot) \
             VALUES ($1, $2, $3, $4, $5::text::jsonb, $6::text::jsonb, $7::text::jsonb, \
                 $8::text::jsonb, $9::text::jsonb, $10, $11::text::pg_snapshot) RETURNING id",
        )
        .bind(asset)
        .bind(now)
        .bind(outcome.as_str())
        .bind(&reason)
        .bind(Value::Object(points_json).to_string())
        .bind(chain_json.to_string())
        .bind(book_json.to_string())
        .bind(Value::Array(differences.iter().map(Difference::json).collect()).to_string())
        .bind(json!(alerts).to_string())
        .bind(
            entry_cut
                .as_ref()
                .filter(|_| reason.is_none())
                .map(|c| c.id),
        )
        .bind(
            entry_cut
                .as_ref()
                .filter(|_| reason.is_none())
                .and_then(|c| c.snapshot.clone()),
        )
        .fetch_one(&mut *tx)
        .await?;

        let mut pages = supply_breach(
            &mut tx,
            asset,
            id,
            &mismatches,
            derived.supply_mismatch.as_ref(),
        )
        .await?;
        match outcome {
            Outcome::Diverges => {
                let paused: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM pauses WHERE asset = $1 AND cause = 'REQ-BK-1')",
                )
                .bind(asset)
                .fetch_one(&mut *tx)
                .await?;
                if !paused {
                    sqlx::query(
                        "INSERT INTO pauses (asset, cause, evaluation_id) VALUES ($1, 'REQ-BK-1', $2)",
                    )
                    .bind(asset)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                    pages.push(Page {
                        kind: "reserve-divergence".into(),
                        asset: asset.to_owned(),
                        detail: json!({
                            "spec": "REQ-BK-1, REQ-IC-2, AC-E2.1",
                            "asset": asset,
                            "evaluation": id,
                            "differences": differences.iter().map(Difference::json).collect::<Vec<_>>(),
                            "action": "the asset's routes are paused; transfers in flight are still observed",
                        }),
                    });
                }
            }
            Outcome::Incomplete => {
                if previous.as_ref().is_none_or(|(o, _)| o != "incomplete") {
                    pages.push(Page {
                        kind: "reserve-check-incomplete".into(),
                        asset: asset.to_owned(),
                        detail: json!({"spec": "NFR-5, REQ-BK-1", "asset": asset,
                                       "evaluation": id, "reason": reason}),
                    });
                }
                if let Some(page) = stalled(&mut tx, asset, id, now).await? {
                    pages.push(page);
                }
            }
            Outcome::Holds => {}
        }
        let before: BTreeSet<String> = previous
            .as_ref()
            .and_then(|(_, a)| serde_json::from_value(a.clone()).ok())
            .unwrap_or_default();
        for alert in alerts.iter().filter(|a| !before.contains(*a)) {
            pages.push(Page {
                kind: "reserve-alert".into(),
                asset: asset.to_owned(),
                detail: json!({"spec": "REQ-BK-10, REQ-BE-6", "asset": asset, "evaluation": id,
                               "alert": alert, "surplus": text(book_terms.surplus)}),
            });
        }
        for p in &pages {
            store::event(&mut tx, &p.kind, None, &p.detail).await?;
        }
        tx.commit().await?;
        for p in &pages {
            self.pager.page(p);
        }
        Ok(Evaluation {
            id,
            asset: asset.to_owned(),
            evaluated_at: now,
            outcome,
            reason,
            points,
            lags,
            chain: derived.terms,
            book: book_terms,
            differences,
            alerts,
        })
    }

    /// The alerts that hold for `asset` (`REQ-BK-10`, `REQ-BE-6`).
    async fn alerts(
        &self,
        conn: &mut PgConnection,
        asset: &str,
        book: &Terms,
        now: DateTime<Utc>,
    ) -> sqlx::Result<Vec<String>> {
        let Some(a) = Asset::load(conn, asset).await? else {
            return Ok(Vec::new());
        };
        let floor = i128::try_from(a.surplus_floor).unwrap_or(i128::MAX);
        let level = i128::try_from(a.surplus_alert).unwrap_or(i128::MAX);
        let mut alerts = Vec::new();
        // REQ-BK-10: "a floor it declares", and an alert before it (review S6).
        if !a.levels_declared {
            alerts.push("surplus-levels-undeclared".to_owned());
        }
        if book.surplus < level {
            alerts.push("surplus-below-alert-level".to_owned());
        }
        if book.surplus < floor {
            alerts.push("surplus-below-floor".to_owned());
        }
        if book::mints_paused(conn, asset).await? {
            alerts.push("mints-paused-reversal-below-floor".to_owned());
        }
        for (reference, at) in book::unrestored_reversals(conn, asset).await? {
            let due: DateTime<Utc> = sqlx::query_scalar("SELECT one_business_day_after($1)")
                .bind(at)
                .fetch_one(&mut *conn)
                .await?;
            if now > due {
                alerts.push(format!("reversal-not-restored:{reference}"));
            }
        }
        Ok(alerts)
    }

    /// Re-derives every term of `asset` at one final point per network (`REQ-BK-8`).
    async fn derive(
        &self,
        conn: &mut PgConnection,
        asset: &str,
        now: DateTime<Utc>,
    ) -> sqlx::Result<Result<Points, Incomplete>> {
        let Some(a) = Asset::load(conn, asset).await? else {
            return Ok(Err(format!("{asset} is not declared")));
        };
        let (Some(issuing), Some(issuing_asset), Some(issuing_ratio)) = (
            a.issuing_network.clone(),
            a.issuing_asset.clone(),
            a.issuing_ratio(),
        ) else {
            return Ok(Err(format!(
                "{asset} has no issuing network, issuing identity or precision ratio"
            )));
        };
        let mut points = BTreeMap::new();
        let mut lags = BTreeMap::new();
        let networks = std::iter::once(issuing.clone()).chain(
            a.positions
                .iter()
                .filter(|p| !p.attested())
                .map(|p| p.network.clone()),
        );
        for network in networks {
            match final_point(conn, &network, now).await? {
                Ok((f, lag)) => {
                    points.insert(network.clone(), f);
                    lags.insert(network, lag);
                }
                Err(why) => return Ok(Err(why)),
            }
        }
        let k = points[&issuing].clone();

        let mut d = Derived::default();
        // Supply (issuing network).
        let supply = match self.readings.supply(&k, &issuing_asset).await {
            Ok(s) => s,
            Err(e) => return Ok(Err(format!("supply of {issuing_asset}: {e}"))),
        };
        let Some(supply_book) = supply
            .checked_mul(issuing_ratio)
            .and_then(|s| i128::try_from(s).ok())
        else {
            return Ok(Err("supply out of range (INV-11)".into()));
        };
        d.terms.supply = supply_book;
        d.supply = json!({"amount": text(supply_book), "issuing_asset": issuing_asset,
                          "network_amount": supply.to_string(), "final_block": k});
        // INV-4 (PLAN DEC-16, 2026-10-15): the flag the endpoint recorded with this reading.
        d.supply_mismatch = readings::supply_mismatch_at(conn, &k, &issuing_asset).await?;

        // Reserve, per position, and convertible holdings outside the equation (REQ-CV-4).
        for p in &a.positions {
            let Some((token, precision)) = &p.token else {
                return Ok(Err(format!("no origin token admitted on {}", p.network)));
            };
            let Some(ratio) = a.token_ratio(*precision) else {
                return Ok(Err(format!("precision of {token} out of range")));
            };
            if p.attested() {
                let positions = match self.readings.attested(asset, &p.network).await {
                    Ok(v) => v,
                    Err(e) => return Ok(Err(format!("attested reserve on {}: {e}", p.network))),
                };
                let mut total: i128 = 0;
                for q in positions {
                    let Some(amount) = q
                        .amount
                        .parse::<u128>()
                        .ok()
                        .and_then(|m| m.checked_mul(ratio))
                        .and_then(|m| i128::try_from(m).ok())
                    else {
                        return Ok(Err("attested amount out of range (INV-11)".into()));
                    };
                    total += amount;
                    d.reserve.push(json!({
                        "network": p.network, "position": q.position, "token": token,
                        "amount": text(amount), "network_amount": q.amount,
                        "attested_by": q.party, "report": q.report,
                    }));
                }
                d.terms.reserve.insert(p.network.clone(), total);
                continue;
            }
            let at = &points[&p.network];
            let balance = match self.readings.reserve(at, token, &p.address).await {
                Ok(b) => b,
                Err(e) => return Ok(Err(format!("reserve on {}: {e}", p.network))),
            };
            let Some(amount) = balance
                .checked_mul(ratio)
                .and_then(|m| i128::try_from(m).ok())
            else {
                return Ok(Err("reserve out of range (INV-11)".into()));
            };
            d.terms.reserve.insert(p.network.clone(), amount);
            d.reserve.push(json!({
                "network": p.network, "address": p.address, "token": token,
                "amount": text(amount), "network_amount": balance.to_string(), "final_block": at,
            }));
            match self.readings.convertible(at, asset, &p.address).await {
                Ok(holdings) => {
                    for h in holdings {
                        d.convertible.push(json!({
                            "network": p.network, "address": p.address, "token": h.token,
                            "amount": h.amount, "final_block": at,
                        }));
                    }
                }
                Err(e) => return Ok(Err(format!("convertible holdings on {}: {e}", p.network))),
            }
        }

        // Owed mints and owed payouts, item by item, from the final observations (REQ-BK-2).
        if let Err(why) = owed(conn, &a, &issuing_asset, &points, &mut d).await? {
            return Ok(Err(why));
        }
        // Surplus is the one term no network shows: the balance the others imply (§5.3).
        d.terms.surplus = d.terms.implied_surplus();
        Ok(Ok((points, lags, d)))
    }
}

/// The key-compromise path of a supply mismatch (`INV-4`; PLAN `DEC-16`, 2026-10-15; PR #50 review
/// M1), within evaluation `id`'s transaction; returns the pages, which the caller also logs.
///
/// - Each mismatch event the endpoint recorded for `asset` raises `key-compromise` once, scoped to
///   the asset, with the actions to pause, rotate and report: value was minted or destroyed
///   outside the on-chain component, so a key with asset-admin power, or the component, is not
///   what bezent believes. An event is named by the flagged reading it starts at
///   ([`readings::supply_mismatches`]), not by the size of its difference: the same difference
///   again after the counts agreed is a new event, raised again (PR #50 re-check S-2).
/// - The asset's routes pause under `INV-4` when a mismatch is first raised, and again whenever
///   one still stands at the point read and no `INV-4` pause is in place: lifting it while the
///   breach stands does not hold. The core never lifts it, and a book correction does not either:
///   that is the operator's, after the key is dealt with.
async fn supply_breach(
    tx: &mut PgConnection,
    asset: &str,
    id: i64,
    mismatches: &[SupplyMismatch],
    standing: Option<&SupplyMismatch>,
) -> sqlx::Result<Vec<Page>> {
    let mut pages = Vec::new();
    let mut pause_on = None;
    for m in mismatches {
        let dedupe = format!("INV-4:supply-mismatch:{asset}:reading:{}", m.reading);
        // The core's own alert, by its detection key (0049): an endpoint's row under the same
        // dedupe never stands in for it.
        let raised = bezent_signer::compromise::raised(
            &mut *tx,
            bezent_signer::compromise::DETECTOR_CORE,
            &format!("dedupe:{dedupe}"),
        )
        .await?;
        if raised {
            continue;
        }
        pause_on.get_or_insert(m.reading);
        pages.push(Page {
            kind: "key-compromise".into(),
            asset: asset.to_owned(),
            detail: json!({
                "dedupe": dedupe,
                "spec": "INV-4, REQ-IC-4, REQ-IC-2",
                "scope": "asset",
                "asset": asset,
                "network": m.network,
                "reading": m.reading,
                "block_number": m.block_number,
                "block_hash": format!("0x{}", hex::encode(&m.block_hash)),
                "contract_supply": m.contract_supply,
                "issuance": m.issuance,
                "difference": m.difference,
                "evaluation": id,
                "reason": "the on-chain component's supply count and the asset's issuance differ: value was minted or destroyed outside the component (INV-4). The issuance is the supply.",
                "actions": ["pause", "rotate", "report"],
            }),
        });
    }
    if pause_on.is_none() {
        pause_on = standing.map(|m| m.reading);
    }
    let Some(reading) = pause_on else {
        return Ok(pages);
    };
    let paused: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pauses WHERE asset = $1 AND cause = 'INV-4')",
    )
    .bind(asset)
    .fetch_one(&mut *tx)
    .await?;
    if !paused {
        sqlx::query(
            "INSERT INTO pauses (asset, cause, evaluation_id, reading_id) \
             VALUES ($1, 'INV-4', $2, $3)",
        )
        .bind(asset)
        .bind(id)
        .bind(reading)
        .execute(&mut *tx)
        .await?;
        pages.push(Page {
            kind: "supply-mismatch-pause".into(),
            asset: asset.to_owned(),
            detail: json!({
                "spec": "INV-4, REQ-IC-2",
                "asset": asset,
                "evaluation": id,
                "reading": reading,
                "standing": standing.is_some(),
                "action": "the asset's routes are paused until the operator lifts the pause; a book correction does not lift it",
            }),
        });
    }
    Ok(pages)
}

/// When `asset`'s evaluations have been incomplete for longer than the smallest lag bound of its
/// networks, pauses its routes under `REQ-IC-2`, naming evaluation `id`, and returns the page; once
/// per pause (review M2: an evaluation that cannot be made is a stall, never a quiet hold).
async fn stalled(
    tx: &mut PgConnection,
    asset: &str,
    id: i64,
    now: DateTime<Utc>,
) -> sqlx::Result<Option<Page>> {
    let since: Option<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT min(v.evaluated_at) FROM reserve_evaluations v \
         WHERE v.asset = $1 AND v.outcome = 'incomplete' \
           AND v.evaluated_at > COALESCE((SELECT max(c.evaluated_at) FROM reserve_evaluations c \
                                          WHERE c.asset = $1 AND c.outcome <> 'incomplete'), \
                                         '-infinity'::timestamptz)",
    )
    .bind(asset)
    .fetch_one(&mut *tx)
    .await?;
    let bound: i64 = sqlx::query_scalar(
        "SELECT COALESCE(min(n.reserve_point_lag_seconds), 60)::bigint FROM networks n \
         WHERE n.role = 'issuing' OR n.network IN (SELECT r.network FROM reserves r WHERE r.asset = $1)",
    )
    .bind(asset)
    .fetch_one(&mut *tx)
    .await?;
    let Some(since) = since.filter(|s| (now - *s).num_seconds() > bound) else {
        return Ok(None);
    };
    let paused: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pauses WHERE asset = $1 AND cause = 'REQ-IC-2' \
                          AND evaluation_id IS NOT NULL)",
    )
    .bind(asset)
    .fetch_one(&mut *tx)
    .await?;
    if paused {
        return Ok(None);
    }
    sqlx::query("INSERT INTO pauses (asset, cause, evaluation_id) VALUES ($1, 'REQ-IC-2', $2)")
        .bind(asset)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(Some(Page {
        kind: "reserve-check-stalled".into(),
        asset: asset.to_owned(),
        detail: json!({
            "spec": "REQ-IC-2, NFR-5, INV-1",
            "asset": asset,
            "evaluation": id,
            "incomplete_since": since.to_rfc3339(),
            "bound_seconds": bound,
            "action": "the asset's routes are paused: the reserve equation could not be evaluated for longer than the bound",
        }),
    }))
}

/// The final point the check reads `network` at, how far it lags the network's latest recorded
/// final head, and how far that head lags `now`: see the module documentation.
async fn final_point(
    conn: &mut PgConnection,
    network: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<Result<(FinalPoint, Lag), Incomplete>> {
    let watermark: Option<i64> =
        sqlx::query_scalar("SELECT watermark FROM observation_watermarks WHERE network = $1")
            .bind(network)
            .fetch_optional(&mut *conn)
            .await?
            .flatten();
    let Some(watermark) = watermark else {
        return Ok(Err(format!(
            "{network} has no range read without a hole (REQ-OB-3)"
        )));
    };
    let pending: Option<i64> = sqlx::query_scalar(
        "SELECT min(o.block_number) FROM observations o WHERE o.network = $1 \
           AND NOT EXISTS (SELECT 1 FROM observation_outcomes x WHERE x.observation_id = o.id)",
    )
    .bind(network)
    .fetch_one(&mut *conn)
    .await?;
    let bound = pending.map_or(watermark, |p| watermark.min(p - 1));
    let head: Option<(i64, Vec<u8>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT block_number, block_hash, block_time FROM final_heads \
         WHERE network = $1 AND block_number <= $2 ORDER BY block_number DESC LIMIT 1",
    )
    .bind(network)
    .bind(bound)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((block_number, block_hash, at)) = head else {
        return Ok(Err(format!(
            "{network} has no recorded final head at or below block {bound}"
        )));
    };
    let (latest, latest_at): (i64, DateTime<Utc>) = sqlx::query_as(
        "SELECT block_number, block_time FROM final_heads WHERE network = $1 \
         ORDER BY block_number DESC LIMIT 1",
    )
    .bind(network)
    .fetch_one(&mut *conn)
    .await?;
    let bound_seconds: i64 = sqlx::query_scalar(
        "SELECT reserve_point_lag_seconds::bigint FROM networks WHERE network = $1",
    )
    .bind(network)
    .fetch_optional(&mut *conn)
    .await?
    .unwrap_or(60);
    Ok(Ok((
        FinalPoint {
            network: network.to_owned(),
            block_number,
            block_hash,
        },
        Lag {
            head_block_number: latest,
            blocks: latest - block_number,
            seconds: (latest_at - at).num_seconds().max(0),
            head_age_seconds: (now - latest_at).num_seconds().max(0),
            bound_seconds,
        },
    )))
}

/// Final, executed observations on `network` at or below `cut`, filtered by `filter`.
async fn finals(
    conn: &mut PgConnection,
    network: &str,
    cut: i64,
    filter: &str,
) -> sqlx::Result<Vec<Observation>> {
    sqlx::query_as(&format!(
        "SELECT {} FROM observations o \
         JOIN observation_outcomes x ON x.observation_id = o.id AND x.outcome = 'final' \
         WHERE o.executed AND o.network = $1 AND o.block_number <= $2 AND {filter} \
         ORDER BY o.id",
        store::OBSERVATION
    ))
    .bind(network)
    .bind(cut)
    .fetch_all(conn)
    .await
}

/// Owed mints and owed payouts at `points`, from the final observations, each item matched by
/// the reference bezent writes for it (`REQ-OT-2`, `REQ-IN-4`; review M1).
async fn owed(
    conn: &mut PgConnection,
    a: &Asset,
    issuing_asset: &str,
    points: &BTreeMap<String, FinalPoint>,
    d: &mut Derived,
) -> sqlx::Result<Result<(), Incomplete>> {
    let Some(issuing) = a.issuing_network.as_deref() else {
        return Ok(Err("no issuing network".into()));
    };
    let Some(ratio) = a.issuing_ratio() else {
        return Ok(Err("precision ratio out of range".into()));
    };
    let k = points[issuing].block_number;
    let issued = |o: &Observation, event: &str| {
        o.detail.get("event").and_then(Value::as_str) == Some(event)
            && o.token.as_deref() == Some(issuing_asset)
    };
    let on_issuing = finals(
        conn,
        issuing,
        k,
        "o.detail ->> 'event' IN ('Minted', 'Redeemed', 'RefundMinted')",
    )
    .await?;
    let minted: HashSet<Vec<u8>> = on_issuing
        .iter()
        .filter(|o| issued(o, "Minted"))
        .filter_map(|o| o.on_chain_reference.clone())
        .collect();
    let refund_minted: Vec<(Vec<u8>, String)> = on_issuing
        .iter()
        .filter(|o| issued(o, "RefundMinted"))
        .filter_map(|o| o.on_chain_reference.clone().zip(o.recipient.clone()))
        .collect();
    // Payout legs out of each public reserve address, and arrivals into it.
    let mut paid: HashSet<Vec<u8>> = HashSet::new();
    let mut arrivals = Vec::new();
    for p in a.positions.iter().filter(|p| !p.attested()) {
        let cut = points[&p.network].block_number;
        let Some((token, precision)) = &p.token else {
            continue;
        };
        let legs = finals(conn, &p.network, cut, "o.detail ->> 'event' = 'Payout'").await?;
        paid.extend(
            legs.into_iter()
                .filter(|o| {
                    o.sender
                        .as_deref()
                        .is_some_and(|s| s.eq_ignore_ascii_case(&p.address))
                })
                .filter_map(|o| o.on_chain_reference),
        );
        let found = sqlx::query_as::<_, Observation>(&format!(
            "SELECT {} FROM observations o \
             JOIN observation_outcomes x ON x.observation_id = o.id AND x.outcome = 'final' \
             WHERE o.executed AND o.network = $1 AND o.block_number <= $2 \
               AND o.recipient = $3 AND o.token = $4 \
               AND o.detail ->> 'event' IS DISTINCT FROM 'Payout' \
               AND (o.amount IS NULL OR o.amount > 0) \
             ORDER BY o.id",
            store::OBSERVATION
        ))
        .bind(&p.network)
        .bind(cut)
        .bind(&p.address)
        .bind(token)
        .fetch_all(&mut *conn)
        .await?;
        let Some(r) = a.token_ratio(*precision) else {
            return Ok(Err("precision ratio out of range".into()));
        };
        arrivals.extend(found.into_iter().map(|o| {
            let funded = p.funded_by(o.sender.as_deref(), o.block_number);
            (o, r, funded)
        }));
    }
    for (o, r, funded) in arrivals {
        let c = o.coordinate();
        let Some(amount) = o
            .amount()
            .and_then(|m| m.checked_mul(r))
            .and_then(|m| i128::try_from(m).ok())
        else {
            return Ok(Err("arrival amount out of range (INV-11)".into()));
        };
        // A movement of zero is owed nothing (review M1).
        if amount == 0 {
            continue;
        }
        // REQ-BK-5, REQ-BK-8 (review S1): an arrival from an address the operator declared it
        // funds the reserve from is a contribution to surplus; the sender is on chain, the
        // declaration is published, so anyone can tell it from an owed mint.
        if funded {
            d.contributions.push(json!({
                "coordinate": coordinate_key(&c),
                "amount": text(amount),
                "from": o.sender,
            }));
            continue;
        }
        if minted.contains(refs::mint_reference(&c).as_slice())
            || paid.contains(refs::unassociated_refund_reference(&c).as_slice())
        {
            continue;
        }
        let key = coordinate_key(&c);
        *d.terms.owed_mint.entry(key.clone()).or_default() += amount;
        d.owed_mint.push(Item {
            item: key.clone(),
            amount: text(amount),
            coordinate: Some(key),
        });
    }
    for o in on_issuing.iter().filter(|o| issued(o, "Redeemed")) {
        let (Some(redeemer), Some(reference)) =
            (o.sender.as_deref(), o.on_chain_reference.as_deref())
        else {
            continue;
        };
        let item = refs::withdrawal(redeemer, reference);
        let refunded = refund_minted
            .iter()
            .any(|(r, to)| r.as_slice() == reference && refs::same_account(to, redeemer));
        if refunded || paid.contains(refs::payout_reference(&item).as_slice()) {
            continue;
        }
        let Some(amount) = o
            .amount()
            .and_then(|m| m.checked_mul(ratio))
            .and_then(|m| i128::try_from(m).ok())
        else {
            return Ok(Err("redemption amount out of range (INV-11)".into()));
        };
        if amount == 0 {
            continue;
        }
        *d.terms.owed_payout.entry(item.clone()).or_default() += amount;
        d.owed_payout.push(Item {
            item,
            amount: text(amount),
            coordinate: Some(coordinate_key(&o.coordinate())),
        });
    }
    Ok(Ok(()))
}

/// Every difference between the networks' terms and the book's, at zero tolerance (`REQ-BK-1`),
/// and a negative surplus (`REQ-BK-3`).
pub fn compare(chain: &Terms, book: &Terms) -> Vec<Difference> {
    let mut out = Vec::new();
    let mut maps = |term: &str, c: &BTreeMap<String, i128>, b: &BTreeMap<String, i128>| {
        let keys: BTreeSet<&String> = c.keys().chain(b.keys()).collect();
        for key in keys {
            let (x, y) = (
                c.get(key).copied().unwrap_or(0),
                b.get(key).copied().unwrap_or(0),
            );
            if x != y {
                out.push(Difference {
                    term: term.to_owned(),
                    item: Some(key.clone()),
                    chain: x,
                    book: y,
                });
            }
        }
    };
    maps("reserve", &chain.reserve, &book.reserve);
    maps("in_transit", &chain.in_transit, &book.in_transit);
    maps("owed_mint", &chain.owed_mint, &book.owed_mint);
    maps("owed_payout", &chain.owed_payout, &book.owed_payout);
    if chain.supply != book.supply {
        out.push(Difference {
            term: "supply".into(),
            item: None,
            chain: chain.supply,
            book: book.supply,
        });
    }
    let implied = chain.implied_surplus();
    if implied != book.surplus {
        out.push(Difference {
            term: "surplus".into(),
            item: None,
            chain: implied,
            book: book.surplus,
        });
    }
    if book.surplus < 0 {
        out.push(Difference {
            term: "surplus".into(),
            item: Some("REQ-BK-3: surplus below zero".into()),
            chain: implied,
            book: book.surplus,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms() -> Terms {
        let mut t = Terms {
            supply: 900,
            surplus: 50,
            ..Terms::default()
        };
        t.reserve.insert("origin".into(), 1_000);
        t.owed_mint.insert("arrival-1".into(), 30);
        t.owed_payout.insert("bz-w-1".into(), 20);
        t
    }

    #[test]
    fn inv_1_equal_terms_hold() {
        assert_eq!(terms().implied_surplus(), 50);
        assert!(compare(&terms(), &terms()).is_empty());
    }

    #[test]
    fn req_bk_1_a_difference_of_one_minor_unit_diverges() {
        let mut chain = terms();
        *chain.reserve.get_mut("origin").unwrap() -= 1;
        let d = compare(&chain, &terms());
        assert!(
            d.iter()
                .any(|d| d.term == "reserve" && d.chain == 999 && d.book == 1_000)
        );
        assert!(d.iter().any(|d| d.term == "surplus"));
    }

    #[test]
    fn req_bk_2_an_item_moved_between_entries_diverges_even_when_totals_agree() {
        let mut chain = terms();
        chain.owed_mint.clear();
        chain.owed_mint.insert("arrival-2".into(), 30);
        let d = compare(&chain, &terms());
        assert_eq!(d.len(), 2, "{d:?}");
        assert!(d.iter().all(|d| d.term == "owed_mint"));
    }

    #[test]
    fn req_bk_3_a_negative_surplus_diverges() {
        let mut book = terms();
        let mut chain = terms();
        book.surplus = -1;
        *book.reserve.get_mut("origin").unwrap() -= 51;
        *chain.reserve.get_mut("origin").unwrap() -= 51;
        let d = compare(&chain, &book);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].item.as_deref(), Some("REQ-BK-3: surplus below zero"));
    }
}
