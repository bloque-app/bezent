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
//! Any difference is a divergence: the money differs, so the asset's routes close at once (the
//! gate and the signer read the latest evaluation), and, once a second reading at a newer final
//! point on every network differs as well (amendment 0028), the routes pause (a `pauses` row the
//! store lets the core write for this cause only) and a human is paged (`REQ-BK-1`, `REQ-IC-2`,
//! `AC-E2.1`). Transfers in flight keep being observed and booked; only new work stops
//! (`INV-15`). Lifting the pause is the operator's, by two logins (`REQ-GV-2`). Surplus below its
//! alert level, below its floor, or a reversal not restored within a business day alerts
//! (`REQ-BK-10`, `REQ-BE-6`); so does a floor or alert level not declared.
//!
//! # Blind is a hold, money is a pause (amendment 0028)
//!
//! An evaluation that cannot see is not a divergence. While the check is blind for an asset (its
//! evaluation could not be made, or a point it rests on lags its network's final head beyond the
//! bound, or a network's final head is stale), the asset's routes are **held**: a row of `pauses`
//! of cause `blind` (kind `hold`), which refuses new work exactly as a pause does, because every
//! reader of `pauses` reads it. The hold names the evaluation it began at, and the core lifts it,
//! in the transaction of the next evaluation that holds (`AC-E2.3`). A person is paged
//! (`reserve-blind`) once the asset has been blind longer than its bound
//! (`networks.reserve_blind_page_seconds`, default ten minutes, the smallest of the asset's
//! networks'), or at once on a finality stall. A hold never masks a money difference: an
//! evaluation that differs while it also lags diverges, and pauses, and is held as well.
//!
//! **Catching up or a finality stall.** The two are told apart per network from the final heads
//! the endpoints record, which are always the network's live finalized head, never one walked from
//! history:
//!
//! - *Catching up*: the network's latest recorded final head is fresh by the clock (within the
//!   bound), but the point the check can read at lags it beyond the bound, because the walk (the
//!   watermark) or an observation awaiting its outcome is behind. The chain is finalizing; our
//!   reading of it is behind. Held, paged only past the blind bound.
//! - *Finality stall*: no final head recorded within the bound by the clock ([`Lag::head_stale`]),
//!   or, once the asset has been held for longer than the network's bound, the latest final head is
//!   still the one recorded when the hold began. The chain is not finalizing, or nothing is
//!   recording its heads (an endpoint stopped, both providers behind): the core cannot tell these
//!   two apart, and pages at once for both, since neither ends by itself.
//!
//! The classification decides only when a person is paged, never whether work is held.
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
//!   finality time) the evaluation never holds, whatever the terms say: an observation that never
//!   gets its outcome, or a read watermark that stalls, cannot freeze the point the check reads
//!   at (`NFR-5`, `REQ-IC-2`). It is blind (recorded as incomplete, naming the lag), and holds the
//!   asset's routes; it diverges only on a difference of value.
//! - The latest recorded final head is measured against the check's clock as well: a network with
//!   no final head recorded within its bound is blind too (review MF2), so heads that stop being
//!   recorded (both providers behind, the observer down) never leave the check holding on an old
//!   point while mints go unseen.
//! - An evaluation that cannot be made (no final point, a reading or the store failed, or the
//!   evaluation itself errored) is recorded as incomplete and holds the asset's routes at once,
//!   never taken as holding (`REQ-IC-2`; review SF1). If even that cannot be recorded, nothing can
//!   be held, so a person is paged at once (`reserve-check-failed`), and again past the bound.
//! - Each asset is evaluated on its own: a failure on one is recorded and held, and the others
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

/// The blind bound when the store cannot say (`networks.reserve_blind_page_seconds`' default).
pub const DEFAULT_BLIND_PAGE_SECONDS: i64 = 600;

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
    /// What happened: `reserve-divergence`, `reserve-blind`, `reserve-alert`, `supply-mismatch-pause`,
    /// `key-compromise`, `reserve-check-failed` or `reserve-check-stalled`. Its detail names its
    /// `class` (`blind` or `money-differs`) where it is about a hold or a pause (amendment 0028).
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
    /// The block up to which the network is read without a hole (`observation_watermarks`): it
    /// moves while the walk does, so a stale head with a moving walk is catching up, not a
    /// finality stall (amendment 0028).
    pub watermark: i64,
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

    fn json(&self, point: Option<&FinalPoint>, state: Option<Blind>) -> Value {
        json!({
            "point_block_number": point.map(|p| p.block_number),
            "head_block_number": self.head_block_number,
            "blocks": self.blocks,
            "seconds": self.seconds,
            "head_age_seconds": self.head_age_seconds,
            "bound_seconds": self.bound_seconds,
            "watermark": self.watermark,
            "state": state.map_or("current", Blind::as_str),
        })
    }
}

/// Why the check cannot see a network (amendment 0028; module documentation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blind {
    /// The network's final head is fresh, and the point the check can read at lags it beyond the
    /// bound: our walk is catching up. Held; paged only past the blind bound.
    CatchingUp,
    /// No newer final head within the bound: the chain is not finalizing, or nothing records its
    /// heads. Held and paged at once.
    FinalityStall,
}

impl Blind {
    /// As the page and the event name it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CatchingUp => "catching-up",
            Self::FinalityStall => "finality-stall",
        }
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
    /// The networks the check cannot see, and why (amendment 0028). Not empty means the asset's
    /// routes are held, whatever the outcome.
    pub blind: BTreeMap<String, Blind>,
    /// The terms re-derived from the networks.
    pub chain: Terms,
    /// The book's terms as of the same points.
    pub book: Terms,
    /// Every difference.
    pub differences: Vec<Difference>,
    /// The alerts that hold.
    pub alerts: Vec<String>,
}

/// What the reserve check could not even record (amendment 0028), shared with the core's gate
/// ([`crate::Core::watching`]): an asset whose failed evaluation the store refused is held in
/// memory, and so is every asset while the check cannot list them. Nothing here is durable: a
/// restarted core starts with nothing unseen, and the store's own rule (`reserve_check_holds`)
/// holds what the check has not seen since.
#[derive(Debug, Default)]
pub struct Unseen {
    all: std::sync::atomic::AtomicBool,
    assets: Mutex<HashSet<String>>,
}

impl Unseen {
    /// Whether new work of `asset` waits for the check.
    pub fn holds(&self, asset: &str) -> bool {
        self.all.load(std::sync::atomic::Ordering::SeqCst) || self.assets().contains(asset)
    }

    /// Whether the check could not list the assets at its last tick: every asset waits.
    pub fn all(&self) -> bool {
        self.all.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn set_all(&self, all: bool) {
        self.all.store(all, std::sync::atomic::Ordering::SeqCst);
    }

    fn assets(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.assets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The reserve check over the core's pool, with the readings it makes and whom it pages.
pub struct ReserveCheck<R, P> {
    pool: PgPool,
    readings: R,
    pager: P,
    /// Assets whose evaluation failed and could not even be recorded, since when, and whether the
    /// stall was paged: the store cannot hold the episode, so the check does (review SF1).
    unrecorded: Mutex<HashMap<String, (DateTime<Utc>, bool)>>,
    /// What the gate must hold that the store could not (amendment 0028).
    unseen: std::sync::Arc<Unseen>,
    /// Whether no tick has listed the assets yet: the first evaluates every asset.
    first: std::sync::atomic::AtomicBool,
    /// Each asset's blind bound as last read, for a stall the store cannot be asked about.
    bounds: Mutex<HashMap<String, i64>>,
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

/// The final points an evaluation rests on, how far each lags, and what was derived at them, or
/// why it could not be (the points and lags found are kept: amendment 0028).
struct Derivation {
    points: BTreeMap<String, FinalPoint>,
    lags: BTreeMap<String, Lag>,
    terms: Result<Derived, Incomplete>,
}

impl<R: Readings, P: Pager> ReserveCheck<R, P> {
    /// The check over `pool` (the core's login), `readings` and `pager`.
    pub fn new(pool: PgPool, readings: R, pager: P) -> Self {
        Self {
            pool,
            readings,
            pager,
            unrecorded: Mutex::default(),
            unseen: std::sync::Arc::default(),
            first: std::sync::atomic::AtomicBool::new(true),
            bounds: Mutex::default(),
        }
    }

    /// What the core's gate must hold that the store could not (amendment 0028): pass it to
    /// [`crate::Core::watching`].
    pub fn unseen(&self) -> std::sync::Arc<Unseen> {
        self.unseen.clone()
    }

    fn bounds(&self) -> std::sync::MutexGuard<'_, HashMap<String, i64>> {
        self.bounds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The readings it makes.
    pub fn readings(&self) -> &R {
        &self.readings
    }

    /// Evaluates every asset whose last evaluation is at least [`period`] old by `clock`, and
    /// returns the evaluations made (`NFR-5`). The first tick of a check evaluates every asset,
    /// however recent its last evaluation (amendment 0028). Each asset is evaluated on its own:
    /// one whose evaluation fails is recorded as incomplete and held when the store allows it, and
    /// held in memory ([`Unseen`]) when it does not, and the others are still evaluated; an asset
    /// with an observation just quarantined is evaluated at once (review M1). Only a store that
    /// cannot list the assets is an error, and while it is every asset is held ([`Unseen::all`]).
    pub async fn tick(&self, clock: &impl Clock) -> sqlx::Result<Vec<Evaluation>> {
        let now = clock.now();
        let quarantined = self.post().await;
        let assets: Vec<(String, Option<DateTime<Utc>>)> = match sqlx::query_as(
            "SELECT a.asset, (SELECT max(v.evaluated_at) FROM reserve_evaluations v \
                              WHERE v.asset = a.asset) \
             FROM assets a ORDER BY a.asset",
        )
        .fetch_all(&self.pool)
        .await
        {
            Ok(a) => {
                self.unseen.set_all(false);
                a
            }
            Err(e) => {
                self.unseen.set_all(true);
                return Err(e);
            }
        };
        let first = self.first.swap(false, std::sync::atomic::Ordering::SeqCst);
        let mut done = Vec::new();
        for (asset, last) in assets {
            if (first || last.is_none_or(|l| now - l >= period()) || quarantined.contains(&asset))
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

    /// Evaluates `asset`. On a failure it records an incomplete evaluation and holds the asset's
    /// routes, as an incomplete evaluation does, paging only once the hold outlasts its bound
    /// (review SF1; `REQ-IC-2`, amendment 0028). If even that cannot be recorded, nothing can be
    /// held, so the episode is kept here: paged at once, and paged as a stall once past the bound.
    async fn evaluate_or_page(&self, asset: &str, now: DateTime<Utc>) -> Option<Evaluation> {
        let error = match self.evaluated(asset, now).await {
            Ok(e) => {
                self.unrecorded_episode().remove(asset);
                self.unseen.assets().remove(asset);
                return Some(e);
            }
            Err(e) => e.to_string(),
        };
        match self.record_failure(asset, now, &error).await {
            Ok(acts) => {
                self.unrecorded_episode().remove(asset);
                self.unseen.assets().remove(asset);
                self.act(&acts);
            }
            Err(r) => {
                tracing::error!(asset, error = %r, "NFR-5: a failed evaluation could not be recorded");
                self.unrecorded(asset, now, &error);
            }
        }
        None
    }

    /// An evaluation of `asset` that could not be recorded at all: nothing durable can be held, so
    /// the core's gate holds the asset in memory, and it is paged at once and past the blind bound.
    fn unrecorded(&self, asset: &str, now: DateTime<Utc>, error: &str) {
        self.unseen.assets().insert(asset.to_owned());
        let bound = self
            .bounds()
            .get(asset)
            .copied()
            .unwrap_or(DEFAULT_BLIND_PAGE_SECONDS);
        let (first, stall) = {
            let mut episodes = self.unrecorded_episode();
            let first = !episodes.contains_key(asset);
            let (since, stall_paged) = episodes.entry(asset.to_owned()).or_insert((now, false));
            let stall = !*stall_paged && (now - *since).num_seconds() > bound;
            *stall_paged |= stall;
            (first, stall.then_some(*since))
        };
        if first {
            self.pager.page(&Page {
                kind: "reserve-check-failed".into(),
                asset: asset.to_owned(),
                detail: json!({
                    "spec": "NFR-5, REQ-IC-2 (amendment 0028)",
                    "class": "blind",
                    "stop": "hold",
                    "held_by": "the core's gate, in memory: no evaluation could be recorded, so no hold could be recorded",
                    "asset": asset,
                    "error": error,
                    "recorded": false,
                    "values": Value::Null,
                    "action": "the reserve equation could not be evaluated, nor the failure recorded: this core holds new work of the asset until it can; look at the store",
                }),
            });
        }
        if let Some(since) = stall {
            self.pager.page(&Page {
                kind: "reserve-check-stalled".into(),
                asset: asset.to_owned(),
                detail: json!({
                    "spec": "REQ-IC-2, NFR-5, INV-1 (amendment 0028)",
                    "class": "blind",
                    "stop": "hold",
                    "held_by": "the core's gate, in memory: no evaluation could be recorded, so no hold could be recorded",
                    "asset": asset,
                    "error": error,
                    "incomplete_since": since.to_rfc3339(),
                    "blind_seconds": (now - since).num_seconds(),
                    "bound_seconds": bound,
                    "recorded": false,
                    "values": Value::Null,
                    "action": "the reserve equation could not be evaluated, nor the failure recorded, for longer than the blind bound: this core holds new work of the asset in memory only, so another core, or a restart, would not; pause the asset's routes by hand if the store takes an operator's pause",
                }),
            });
        }
    }

    fn unrecorded_episode(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<String, (DateTime<Utc>, bool)>> {
        self.unrecorded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records a failed evaluation of `asset` as incomplete, holds the asset's routes (the check
    /// cannot see it: amendment 0028), and returns what it calls for: `reserve-check-failed` in the
    /// log when the previous evaluation was not incomplete, and the blind page once the hold is
    /// past its bound.
    async fn record_failure(
        &self,
        asset: &str,
        now: DateTime<Utc>,
        error: &str,
    ) -> sqlx::Result<Acts> {
        let mut tx = self.pool.begin().await?;
        lock_asset(&mut tx, asset).await?;
        let previous: Option<String> = sqlx::query_scalar(
            "SELECT outcome FROM reserve_evaluations WHERE asset = $1 ORDER BY id DESC LIMIT 1",
        )
        .bind(asset)
        .fetch_optional(&mut *tx)
        .await?;
        let walked = walked_watermarks(&mut tx, asset, now).await?;
        let bound = blind_bound(&mut tx, asset).await?;
        self.bounds().insert(asset.to_owned(), bound);
        let assessment = assess(&mut tx, asset, now, &walked).await?;
        let reason = format!("the evaluation failed: {error}");
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO reserve_evaluations (asset, evaluated_at, outcome, reason) \
             VALUES ($1, $2, 'incomplete', $3) RETURNING id",
        )
        .bind(asset)
        .bind(now)
        .bind(&reason)
        .fetch_one(&mut *tx)
        .await?;
        let mut acts = Acts::default();
        if previous.as_deref() != Some("incomplete") {
            acts.logged.push(Page {
                kind: "reserve-check-failed".into(),
                asset: asset.to_owned(),
                detail: json!({"spec": "NFR-5, REQ-IC-2", "class": "blind", "stop": "hold",
                               "asset": asset, "evaluation": id, "error": error,
                               "recorded": true, "values": Value::Null}),
            });
        }
        hold(
            &mut tx,
            &Seen {
                asset,
                evaluation: id,
                now,
                reason: &reason,
                points: &BTreeMap::new(),
                lags: &BTreeMap::new(),
                blind: &BTreeMap::new(),
                assessment: &assessment,
                bound,
                values: &Value::Null,
            },
            &mut acts,
        )
        .await?;
        for p in acts.paged.iter().chain(&acts.logged) {
            store::event(&mut tx, &p.kind, None, &p.detail).await?;
        }
        tx.commit().await?;
        Ok(acts)
    }

    /// Pages what is to be paged, and logs what is only recorded. A recorded entry is logged under
    /// `recorded=`, never `kind=`, so no alarm filter on a page kind matches it (amendment 0028;
    /// `deploy/runbooks/alerts.md`).
    fn act(&self, acts: &Acts) {
        for p in &acts.logged {
            tracing::warn!(recorded = %p.kind, asset = %p.asset, detail = %p.detail, "reserve check");
        }
        for p in &acts.paged {
            self.pager.page(p);
        }
    }

    /// Evaluates the reserve equation for `asset` at `now` and acts on the outcome. Posting what is
    /// pending first never stops it (review M1).
    pub async fn evaluate(&self, asset: &str, now: DateTime<Utc>) -> sqlx::Result<Evaluation> {
        self.post().await;
        self.evaluated(asset, now).await
    }

    async fn evaluated(&self, asset: &str, now: DateTime<Utc>) -> sqlx::Result<Evaluation> {
        // One evaluation of an asset at a time, decided and recorded under one lock (amendment
        // 0028): what an evaluation reads, the outcome it comes to and the hold it places or lifts
        // follow the evaluations' ids. The reads run in a savepoint, so a store error while reading
        // is an incomplete evaluation, recorded, not a failed one.
        let mut tx = self.pool.begin().await?;
        lock_asset(&mut tx, asset).await?;
        let mut read = sqlx::Acquire::begin(&mut *tx).await?;
        let mut broken = false;
        let derivation = match self.derive(&mut read, asset, now).await {
            Ok(d) => d,
            Err(e) => {
                broken = true;
                Derivation {
                    points: BTreeMap::new(),
                    lags: BTreeMap::new(),
                    terms: Err(format!("store: {e}")),
                }
            }
        };
        let Derivation {
            points,
            lags,
            terms,
        } = derivation;
        let mut entry_cut = None;
        let (derived, book_terms, reason) = match terms {
            Ok(derived) => {
                let cuts = points
                    .iter()
                    .map(|(n, p)| (n.clone(), p.block_number))
                    .collect();
                // The book as the evaluation reads it: no entry committed later, whatever its id
                // (review SF5; issue #42).
                let book = match book::entry_cut(&mut read).await {
                    Ok(cut) => {
                        let terms = book::as_of(&mut read, asset, &cuts, Some(&cut)).await;
                        entry_cut = Some(cut);
                        terms
                    }
                    Err(e) => Err(e),
                };
                match book {
                    Ok(b) => (derived, b, None),
                    Err(e) => {
                        broken = true;
                        (
                            Derived::default(),
                            Terms::default(),
                            Some(format!("store: {e}")),
                        )
                    }
                }
            }
            Err(why) => (Derived::default(), Terms::default(), Some(why)),
        };
        if broken {
            read.rollback().await?;
        } else {
            read.commit().await?;
        }
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
        let mismatches = readings::supply_mismatches(&mut tx, asset).await?;
        let made = reason.is_none();
        let alerts = if made {
            self.alerts(&mut tx, asset, &book_terms, now).await?
        } else {
            Vec::new()
        };
        let previous: Option<(String, Value, Value)> = sqlx::query_as(
            "SELECT outcome, alerts, points FROM reserve_evaluations WHERE asset = $1 \
             ORDER BY id DESC LIMIT 1",
        )
        .bind(asset)
        .fetch_optional(&mut *tx)
        .await?;
        let walked = walked_watermarks(&mut tx, asset, now).await?;
        let bound = blind_bound(&mut tx, asset).await?;
        self.bounds().insert(asset.to_owned(), bound);
        // Review M2: a point too far behind its network's final head is no check at all; nor,
        // review MF2, is one on a network with no final head recorded within the bound by the
        // clock (a finality stall, an observer that stopped): mints there would go unseen.
        // Amendment 0028: that is blindness, not a difference of value. It never holds, and it
        // holds the asset's routes; it diverges only if a value differs as well. Which networks
        // are in a finality stall is judged once ([`assess`]); the reason and the page say the same.
        let assessment = assess(&mut tx, asset, now, &walked).await?;
        let blind: BTreeMap<String, Blind> = lags
            .iter()
            .filter(|(_, l)| l.stale())
            .map(|(n, _)| (n.clone(), Blind::CatchingUp))
            .chain(
                assessment
                    .stalled
                    .iter()
                    .map(|n| (n.clone(), Blind::FinalityStall)),
            )
            .collect();
        let reason = reason.or_else(|| {
            (differences.is_empty() && !blind.is_empty()).then(|| blind_reason(&blind, &lags))
        });
        let outcome = match (&reason, differences.is_empty()) {
            (Some(_), _) => Outcome::Incomplete,
            (None, true) => Outcome::Holds,
            (None, false) => Outcome::Diverges,
        };
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

        // What a reader needs to tell at a glance whether money is at stake (amendment 0028).
        let values = if made {
            values_json(&derived.terms, &book_terms)
        } else {
            Value::Null
        };
        let mut acts = Acts {
            paged: supply_breach(
                &mut tx,
                asset,
                id,
                &mismatches,
                derived.supply_mismatch.as_ref(),
            )
            .await?,
            logged: Vec::new(),
        };
        // Amendment 0028, the second reading: one `diverges` evaluation already closes the gate
        // (`reserve_check_holds`) and holds signing, silently. The two-person pause and its page
        // need the same difference of value seen twice: the asset's previous evaluation by id
        // diverged too, and this one rests on a newer final point on every network (a node can
        // be behind, or on its own fork).
        let confirmed = outcome == Outcome::Diverges
            && previous
                .as_ref()
                .is_some_and(|(o, _, p)| o == "diverges" && newer_points(p, &points, &lags));
        if outcome == Outcome::Diverges
            && !confirmed
            && previous.as_ref().is_none_or(|(o, _, _)| o != "diverges")
        {
            acts.logged.push(Page {
                    kind: "reserve-divergence-unconfirmed".into(),
                    asset: asset.to_owned(),
                    detail: json!({
                        "spec": "REQ-BK-1, REQ-IC-2, AC-E2.1",
                        "class": "money-differs",
                        "stop": "hold",
                        "asset": asset,
                        "evaluation": id,
                        "differences": differences.iter().map(Difference::json).collect::<Vec<_>>(),
                        "values": values,
                        "lags": lags_json(&points, &lags, &blind),
                        "action": "a first reading differs: signing is held; the pause and the page wait for a second reading at a newer final point",
                    }),
                });
        }
        if confirmed {
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
                acts.paged.push(Page {
                    kind: "reserve-divergence".into(),
                    asset: asset.to_owned(),
                    detail: json!({
                        "spec": "REQ-BK-1, REQ-IC-2, AC-E2.1",
                        "class": "money-differs",
                        "stop": "pause",
                        "asset": asset,
                        "evaluation": id,
                        "differences": differences.iter().map(Difference::json).collect::<Vec<_>>(),
                        "values": values,
                        "lags": lags_json(&points, &lags, &blind),
                        "also_blind": blind.iter().map(|(n, b)| (n.clone(), json!(b.as_str()))).collect::<serde_json::Map<_, _>>(),
                        "action": "the money differs: the asset's routes are paused until two operators lift the pause (REQ-GV-2); transfers in flight are still observed",
                    }),
                });
            }
        }
        if outcome == Outcome::Incomplete
            && previous.as_ref().is_none_or(|(o, _, _)| o != "incomplete")
        {
            acts.logged.push(Page {
                kind: "reserve-check-incomplete".into(),
                asset: asset.to_owned(),
                detail: json!({"spec": "NFR-5, REQ-BK-1, REQ-IC-2", "class": "blind",
                               "stop": "hold", "asset": asset, "evaluation": id,
                               "reason": reason, "values": values,
                               "lags": lags_json(&points, &lags, &blind)}),
            });
        }
        if outcome == Outcome::Incomplete || !blind.is_empty() {
            let why = reason
                .clone()
                .unwrap_or_else(|| blind_reason(&blind, &lags));
            hold(
                &mut tx,
                &Seen {
                    asset,
                    evaluation: id,
                    now,
                    reason: &why,
                    points: &points,
                    lags: &lags,
                    blind: &blind,
                    assessment: &assessment,
                    bound,
                    values: &values,
                },
                &mut acts,
            )
            .await?;
        } else if outcome == Outcome::Holds {
            lift(&mut tx, asset, id, now, &values, &mut acts).await?;
        }
        let before: BTreeSet<String> = previous
            .as_ref()
            .and_then(|(_, a, _)| serde_json::from_value(a.clone()).ok())
            .unwrap_or_default();
        for alert in alerts.iter().filter(|a| !before.contains(*a)) {
            acts.paged.push(Page {
                kind: "reserve-alert".into(),
                asset: asset.to_owned(),
                detail: json!({"spec": "REQ-BK-10, REQ-BE-6", "asset": asset, "evaluation": id,
                               "alert": alert, "surplus": text(book_terms.surplus)}),
            });
        }
        for p in acts.paged.iter().chain(&acts.logged) {
            store::event(&mut tx, &p.kind, None, &p.detail).await?;
        }
        tx.commit().await?;
        self.act(&acts);
        Ok(Evaluation {
            id,
            asset: asset.to_owned(),
            evaluated_at: now,
            outcome,
            reason,
            points,
            lags,
            blind,
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

    /// Re-derives every term of `asset` at one final point per network (`REQ-BK-8`). The points
    /// and lags found are kept when a reading then fails (amendment 0028: a stale final head on the
    /// same evaluation is still a finality stall).
    async fn derive(
        &self,
        conn: &mut PgConnection,
        asset: &str,
        now: DateTime<Utc>,
    ) -> sqlx::Result<Derivation> {
        let failed = |why: String| Derivation {
            points: BTreeMap::new(),
            lags: BTreeMap::new(),
            terms: Err(why),
        };
        let Some(a) = Asset::load(conn, asset).await? else {
            return Ok(failed(format!("{asset} is not declared")));
        };
        let (Some(issuing), Some(issuing_asset), Some(issuing_ratio)) = (
            a.issuing_network.clone(),
            a.issuing_asset.clone(),
            a.issuing_ratio(),
        ) else {
            return Ok(failed(format!(
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
        let mut missing = None;
        for network in networks {
            match final_point(conn, &network, now).await? {
                Ok((f, lag)) => {
                    points.insert(network.clone(), f);
                    lags.insert(network, lag);
                }
                Err(why) => {
                    missing.get_or_insert(why);
                }
            }
        }
        let terms = match missing {
            Some(why) => Err(why),
            None => {
                self.terms_at(conn, &a, &issuing, &issuing_asset, issuing_ratio, &points)
                    .await?
            }
        };
        Ok(Derivation {
            points,
            lags,
            terms,
        })
    }

    /// Every term of asset `a` at `points`, one per network (`REQ-BK-8`).
    async fn terms_at(
        &self,
        conn: &mut PgConnection,
        a: &Asset,
        issuing: &str,
        issuing_asset: &str,
        issuing_ratio: u128,
        points: &BTreeMap<String, FinalPoint>,
    ) -> sqlx::Result<Result<Derived, Incomplete>> {
        let asset = a.asset.as_str();
        let k = points[issuing].clone();

        let mut d = Derived::default();
        // Supply (issuing network).
        let supply = match self.readings.supply(&k, issuing_asset).await {
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
        d.supply_mismatch = readings::supply_mismatch_at(conn, &k, issuing_asset).await?;

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
        if let Err(why) = owed(conn, a, issuing_asset, points, &mut d).await? {
            return Ok(Err(why));
        }
        // Surplus is the one term no network shows: the balance the others imply (§5.3).
        d.terms.surplus = d.terms.implied_surplus();
        Ok(Ok(d))
    }
}

/// Whether `points` are newer than the ones a previous evaluation recorded (`prev`, its `points`
/// column) on every network: a strictly higher block number, or a strictly higher final head
/// (amendment 0028, the second reading). A network the previous evaluation has no point for, or
/// no points at all, is not newer: there is nothing to have moved past.
fn newer_points(
    prev: &Value,
    points: &BTreeMap<String, FinalPoint>,
    lags: &BTreeMap<String, Lag>,
) -> bool {
    !points.is_empty()
        && points.iter().all(|(n, p)| {
            let Some(before) = prev.get(n) else {
                return false;
            };
            let block = before.get("block_number").and_then(Value::as_i64);
            let head = before
                .pointer("/lag/head_block_number")
                .and_then(Value::as_i64);
            block.is_some_and(|b| p.block_number > b)
                || head
                    .zip(lags.get(n))
                    .is_some_and(|(h, l)| l.head_block_number > h)
        })
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
                "class": "money-differs",
                "stop": "pause",
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
                "class": "money-differs",
                "stop": "pause",
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

/// What an evaluation calls for: pages to a person, and entries only recorded (in the event log and
/// the service's log). Both are written to the event log in the evaluation's transaction.
#[derive(Default)]
struct Acts {
    paged: Vec<Page>,
    logged: Vec<Page>,
}

/// What an evaluation that holds the asset's routes saw.
struct Seen<'a> {
    asset: &'a str,
    evaluation: i64,
    now: DateTime<Utc>,
    reason: &'a str,
    points: &'a BTreeMap<String, FinalPoint>,
    lags: &'a BTreeMap<String, Lag>,
    blind: &'a BTreeMap<String, Blind>,
    /// What the store says of the asset's networks now ([`assess`]).
    assessment: &'a Assessment,
    /// The asset's blind bound ([`blind_bound`]).
    bound: i64,
    values: &'a Value,
}

/// Takes the per-asset lock every evaluation and every record of a failure takes first, in its
/// own transaction (amendment 0028), as the engine's locks do. It waits as any statement does, up
/// to the session's `statement_timeout`; a timeout is a store error like any other, so a failed
/// evaluation is recorded, or, if even that cannot be, held in memory and paged.
async fn lock_asset(tx: &mut PgConnection, asset: &str) -> sqlx::Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("bezent/reserve-evaluation/{asset}"))
        .execute(&mut *tx)
        .await
        .map(|_| ())
}

/// Each network's watermark at the latest evaluation of `asset` at least that network's lag bound
/// old (by `evaluated_at`) that recorded one for it: what "the walk has stopped" is judged against
/// (amendment 0028). Records of failures, which carry no watermark, are passed over. A network with
/// no such evaluation is absent: unknown, not stopped. An evaluation seconds old (a restarted
/// core's first tick, an asset just quarantined) is never the baseline, so a walk that moves slowly
/// is not taken for a stopped one. By time, not id, so the lookup is an index seek on
/// `reserve_evaluations_latest`; under the lock an asset's evaluations are recorded in time order.
async fn walked_watermarks(
    tx: &mut PgConnection,
    asset: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<BTreeMap<String, i64>> {
    let rows: Vec<(String, Option<i64>)> = sqlx::query_as(&format!(
        "SELECT n.network, \
                (SELECT (v.points -> n.network -> 'lag' ->> 'watermark')::bigint \
                 FROM reserve_evaluations v \
                 WHERE v.asset = $1 \
                   AND v.evaluated_at <= $2 - make_interval(secs => n.reserve_point_lag_seconds) \
                   AND v.points -> n.network -> 'lag' ? 'watermark' \
                 ORDER BY v.evaluated_at DESC, v.id DESC LIMIT 1) \
         FROM networks n WHERE n.network IN ({ASSET_NETWORKS})"
    ))
    .bind(asset)
    .bind(now)
    .fetch_all(&mut *tx)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(n, w)| w.map(|w| (n, w)))
        .collect())
}

/// The networks an asset rests on: the issuing network, when the asset is issued there, and its
/// reserve networks (amendment 0028; never another asset's).
const ASSET_NETWORKS: &str = "SELECT n.network FROM networks n \
     WHERE (n.role = 'issuing' AND EXISTS (SELECT 1 FROM assets a \
                                           WHERE a.asset = $1 AND a.issuing_asset IS NOT NULL)) \
        OR n.network IN (SELECT r.network FROM reserves r WHERE r.asset = $1)";

/// How long `asset` may be blind before a person is paged: the smallest
/// `networks.reserve_blind_page_seconds` of its own networks.
async fn blind_bound(tx: &mut PgConnection, asset: &str) -> sqlx::Result<i64> {
    sqlx::query_scalar(&format!(
        "SELECT COALESCE(min(n.reserve_blind_page_seconds), {DEFAULT_BLIND_PAGE_SECONDS})::bigint \
         FROM networks n WHERE n.network IN ({ASSET_NETWORKS})"
    ))
    .bind(asset)
    .fetch_one(&mut *tx)
    .await
}

/// A network as the store has it now: its latest recorded final head (number, block time), its
/// watermark and its lag bound.
struct NetState {
    head: Option<(i64, DateTime<Utc>)>,
    watermark: Option<i64>,
    bound: i64,
}

async fn net_states(
    tx: &mut PgConnection,
    asset: &str,
) -> sqlx::Result<BTreeMap<String, NetState>> {
    type Row = (String, Option<i64>, Option<DateTime<Utc>>, Option<i64>, i64);
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT n.network, h.block_number, h.block_time, w.watermark, \
                n.reserve_point_lag_seconds::bigint \
         FROM networks n \
         LEFT JOIN LATERAL (SELECT f.block_number, f.block_time FROM final_heads f \
                            WHERE f.network = n.network ORDER BY f.block_number DESC LIMIT 1) h \
                ON true \
         LEFT JOIN observation_watermarks w ON w.network = n.network \
         WHERE n.network IN ({ASSET_NETWORKS}) ORDER BY n.network"
    ))
    .bind(asset)
    .fetch_all(&mut *tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(network, number, time, watermark, bound)| {
            (
                network,
                NetState {
                    head: number.zip(time),
                    watermark,
                    bound,
                },
            )
        })
        .collect())
}

/// Why an evaluation that saw every term alike is blind, in words (amendment 0028).
fn blind_reason(blind: &BTreeMap<String, Blind>, lags: &BTreeMap<String, Lag>) -> String {
    let each: Vec<String> = blind
        .iter()
        .map(|(network, b)| match (b, lags.get(network)) {
            (Blind::FinalityStall, Some(lag)) if lag.head_stale() => format!(
                "{network}: no final head recorded for {} s, beyond {} s, and the walk has \
                 stopped: a finality stall, or nothing records its heads",
                lag.head_age_seconds, lag.bound_seconds
            ),
            (Blind::FinalityStall, _) => format!(
                "{network}: the latest final head is the one recorded when the hold began, and the \
                 walk has stopped: a finality stall, or nothing records its heads"
            ),
            (Blind::CatchingUp, Some(lag)) if lag.head_stale() => format!(
                "{network}: no final head recorded for {} s, beyond {} s, while the walk is not \
                 known to have stopped (watermark {}): catching up",
                lag.head_age_seconds, lag.bound_seconds, lag.watermark
            ),
            (Blind::CatchingUp, Some(lag)) => format!(
                "{network}: the point lags the latest final head by {} s ({} blocks), beyond {} s: \
                 catching up",
                lag.seconds, lag.blocks, lag.bound_seconds
            ),
            (Blind::CatchingUp, None) => format!("{network}: catching up"),
        })
        .collect();
    format!(
        "blind (amendment 0028; NFR-5, NFR-6, REQ-IC-2): {}",
        each.join("; ")
    )
}

/// Every term, as the networks show it and as the book holds it, so that a page says at a glance
/// whether money is at stake (amendment 0028).
fn values_json(chain: &Terms, book: &Terms) -> Value {
    let pair = |c: i128, b: i128| json!({"chain": text(c), "book": text(b)});
    let networks: BTreeSet<&String> = chain.reserve.keys().chain(book.reserve.keys()).collect();
    let reserve: serde_json::Map<String, Value> = networks
        .into_iter()
        .map(|n| {
            (
                n.clone(),
                pair(
                    chain.reserve.get(n).copied().unwrap_or(0),
                    book.reserve.get(n).copied().unwrap_or(0),
                ),
            )
        })
        .collect();
    json!({
        "supply": pair(chain.supply, book.supply),
        "reserve": reserve,
        "in_transit": pair(chain.in_transit.values().sum(), book.in_transit.values().sum()),
        "owed_mint": pair(chain.owed_mint.values().sum(), book.owed_mint.values().sum()),
        "owed_payout": pair(chain.owed_payout.values().sum(), book.owed_payout.values().sum()),
        "surplus": pair(chain.implied_surplus(), book.surplus),
    })
}

/// Each network's point and lag, with whether the check can see it.
fn lags_json(
    points: &BTreeMap<String, FinalPoint>,
    lags: &BTreeMap<String, Lag>,
    blind: &BTreeMap<String, Blind>,
) -> Value {
    Value::Object(
        lags.iter()
            .map(|(n, l)| (n.clone(), l.json(points.get(n), blind.get(n).copied())))
            .collect(),
    )
}

/// What the store says of an asset's networks at an evaluation, and which of them are in a
/// finality stall (amendment 0028): one judgement, which the evaluation's reason and the hold's page
/// both read.
struct Assessment {
    /// The asset's hold, if it has one: its id, the evaluation it began at, when, its held heads.
    existing: Option<(i64, i64, DateTime<Utc>, Value)>,
    /// Each network's latest final head and watermark now, as a hold records them; empty when
    /// they could not be read (best effort: the hold is placed all the same).
    heads_now: serde_json::Map<String, Value>,
    /// The networks in a finality stall.
    stalled: Vec<String>,
}

/// Judges `asset`'s networks (see [`hold`] for the rule). "The walk has stopped" is judged against
/// `walked`, each network's watermark at the latest evaluation at least its lag bound old
/// ([`walked_watermarks`]). With none (no such evaluation, or one recorded before 0142) it is
/// unknown, not stopped: a stale head is then not yet a stall. Unknown can last (failures that
/// record no watermark, a hold with no `held_heads`); the asset is held all the same, and paged
/// `beyond-bound` at its blind bound.
async fn assess(
    tx: &mut PgConnection,
    asset: &str,
    now: DateTime<Utc>,
    walked: &BTreeMap<String, i64>,
) -> sqlx::Result<Assessment> {
    let existing: Option<(i64, i64, DateTime<Utc>, Value)> = sqlx::query_as(
        "SELECT p.id, p.evaluation_id, v.evaluated_at, p.held_heads FROM pauses p \
         JOIN reserve_evaluations v ON v.id = p.evaluation_id \
         WHERE p.asset = $1 AND p.cause = 'blind'",
    )
    .bind(asset)
    .fetch_optional(&mut *tx)
    .await?;
    // Best effort, in a savepoint: a read that fails here must not cost the durable hold.
    let mut sp = sqlx::Acquire::begin(&mut *tx).await?;
    let states = match net_states(&mut sp, asset).await {
        Ok(states) => {
            sp.commit().await?;
            states
        }
        Err(e) => {
            sp.rollback().await?;
            tracing::warn!(asset, error = %e, "the asset's final heads could not be read for its hold");
            BTreeMap::new()
        }
    };
    let heads_now: serde_json::Map<String, Value> = states
        .iter()
        .map(|(n, st)| {
            (
                n.clone(),
                json!({
                    "head_block_number": st.head.map(|(b, _)| b),
                    "head_block_time": st.head.map(|(_, t)| t.to_rfc3339()),
                    "watermark": st.watermark,
                }),
            )
        })
        .collect();
    let held_since = existing.as_ref().map(|(_, _, since, _)| *since);
    let stalled = states
        .iter()
        .filter(|(network, st)| {
            // Moved since `before`; an unknown `before` counts as moved (not known to have stopped).
            let moved_since = |before: Option<i64>| match (before, st.watermark) {
                (Some(b), Some(a)) => a > b,
                (None, Some(_)) => true,
                (_, None) => false,
            };
            let stale = st
                .head
                .is_some_and(|(_, t)| (now - t).num_seconds() > st.bound)
                && !match walked.get(network.as_str()) {
                    Some(w) => st.watermark.is_some_and(|a| a > *w),
                    None => true,
                };
            let at_hold = existing
                .as_ref()
                .and_then(|(_, _, _, held)| held.get(network.as_str()));
            let held_head = at_hold
                .and_then(|h| h.get("head_block_number"))
                .and_then(Value::as_i64);
            let held_watermark = at_hold
                .and_then(|h| h.get("watermark"))
                .and_then(Value::as_i64);
            let unchanged = held_since.is_some_and(|since| (now - since).num_seconds() > st.bound)
                && held_head.is_some()
                && held_head == st.head.map(|(b, _)| b)
                && !moved_since(held_watermark);
            stale || unchanged
        })
        .map(|(network, _)| network.clone())
        .collect();
    Ok(Assessment {
        existing,
        heads_now,
        stalled,
    })
}

/// Holds `asset`'s routes while the check cannot see it (amendment 0028; `REQ-IC-2`), within the
/// evaluation's transaction: records the hold if there is none, naming the evaluation it begins at
/// and each network's latest final head and watermark then (`held_heads`), and pages
/// `reserve-blind` once per hold and reason: at once on a finality stall, and otherwise once the
/// asset has been blind for longer than its bound.
///
/// A network is in a finality stall when either holds, and its walk has not moved meanwhile:
/// - its latest final head is older than the network's lag bound by the clock, and the watermark
///   has not moved since the latest evaluation at least that bound old;
/// - the hold is older than that bound and the latest final head is still the one recorded when
///   the hold began (whatever its block time says), and the watermark has not moved since then.
async fn hold(tx: &mut PgConnection, seen: &Seen<'_>, acts: &mut Acts) -> sqlx::Result<()> {
    let Seen {
        asset,
        evaluation,
        now,
        reason,
        points,
        lags,
        blind,
        assessment,
        bound,
        values,
    } = *seen;
    let held_now = &assessment.heads_now;
    let (hold_id, began, since, held) = match &assessment.existing {
        Some(h) => h.clone(),
        None => {
            let id: Option<i64> = sqlx::query_scalar(
                "INSERT INTO pauses (asset, cause, evaluation_id, held_heads) \
                 VALUES ($1, 'blind', $2, $3::text::jsonb) \
                 ON CONFLICT (asset) WHERE cause = 'blind' DO NOTHING RETURNING id",
            )
            .bind(asset)
            .bind(evaluation)
            .bind(Value::Object(held_now.clone()).to_string())
            .fetch_optional(&mut *tx)
            .await?;
            // Under the per-asset lock no other evaluation of the asset records one meanwhile;
            // a hold already there (an owner's, a restore's) is the hold.
            let Some(id) = id else {
                return Ok(());
            };
            acts.logged.push(Page {
                kind: "reserve-hold".into(),
                asset: asset.to_owned(),
                detail: json!({
                    "spec": "REQ-IC-2, NFR-5, AC-E2.3 (amendment 0028)",
                    "class": "blind",
                    "stop": "hold",
                    "hold": id,
                    "asset": asset,
                    "evaluation": evaluation,
                    "since": now.to_rfc3339(),
                    "reason": reason,
                    "values": values,
                    "lags": lags_json(points, lags, blind),
                    "held_heads": held_now,
                    "action": "the reserve check cannot see the asset: new deposits and withdrawals of it are refused, transfers in flight are still observed, and the hold lifts itself once an evaluation holds; no difference of value is known",
                }),
            });
            (id, evaluation, now, Value::Object(held_now.clone()))
        }
    };
    let blind_seconds = (now - since).num_seconds();
    let stalled = &assessment.stalled;
    let why = if !stalled.is_empty() {
        "finality-stall"
    } else if blind_seconds > bound {
        "beyond-bound"
    } else {
        return Ok(());
    };
    let paged: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM event_log WHERE kind = 'reserve-blind' \
                          AND payload ->> 'hold' = $1 AND payload ->> 'why' = $2)",
    )
    .bind(hold_id.to_string())
    .bind(why)
    .fetch_one(&mut *tx)
    .await?;
    if paged {
        return Ok(());
    }
    let action = if why == "finality-stall" {
        "the asset's routes are held (new deposits and withdrawals refused; transfers in flight still observed): a network's final head is not advancing and its walk has stopped, a finality stall or nothing recording its heads; check the chain and its endpoint. No difference of value is known. The hold lifts itself once an evaluation holds; do not lift it by hand"
    } else {
        "the reserve check has not seen the asset for longer than the bound: its routes are held (new deposits and withdrawals refused; transfers in flight still observed). No difference of value is known. Find why it cannot see (the reason, the lags); the hold lifts itself once an evaluation holds; do not lift it by hand"
    };
    acts.paged.push(Page {
        kind: "reserve-blind".into(),
        asset: asset.to_owned(),
        detail: json!({
            "spec": "REQ-IC-2, NFR-5, NFR-6, AC-E2.3 (amendment 0028)",
            "class": "blind",
            "stop": "hold",
            "why": why,
            "hold": hold_id,
            "asset": asset,
            "evaluation": evaluation,
            "began_at_evaluation": began,
            "since": since.to_rfc3339(),
            "blind_seconds": blind_seconds,
            "bound_seconds": bound,
            "stalled_networks": stalled,
            "reason": reason,
            "values": values,
            "lags": lags_json(points, lags, blind),
            "held_heads": held,
            "heads_now": held_now,
            "action": action,
        }),
    });
    Ok(())
}

/// Lifts `asset`'s hold, if it has one, in the transaction of evaluation `id`, which holds
/// (amendment 0028). The store refuses it unless that is so; it lifts no pause.
async fn lift(
    tx: &mut PgConnection,
    asset: &str,
    id: i64,
    now: DateTime<Utc>,
    values: &Value,
    acts: &mut Acts,
) -> sqlx::Result<()> {
    let lifted: Option<(i64, i64, DateTime<Utc>)> = sqlx::query_as(
        "DELETE FROM pauses p USING reserve_evaluations v \
         WHERE p.asset = $1 AND p.cause = 'blind' AND v.id = p.evaluation_id \
         RETURNING p.id, p.evaluation_id, v.evaluated_at",
    )
    .bind(asset)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((hold, began, since)) = lifted {
        acts.logged.push(Page {
            kind: "reserve-hold-lifted".into(),
            asset: asset.to_owned(),
            detail: json!({
                "spec": "REQ-IC-2, AC-E2.3 (amendment 0028)",
                "class": "blind",
                "stop": "hold",
                "hold": hold,
                "asset": asset,
                "evaluation": id,
                "began_at_evaluation": began,
                "since": since.to_rfc3339(),
                "held_seconds": (now - since).num_seconds(),
                "values": values,
                "action": "an evaluation holds again: the hold is lifted and new work of the asset resumes; no pause is lifted",
            }),
        });
    }
    Ok(())
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
            watermark,
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
