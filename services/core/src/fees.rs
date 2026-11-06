// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The fee position floor and its page (`F-014`; `REQ-ST-6`, `AC-H3.1`; PLAN `DEC-18`; amendment 0025,
//! `d-fee-position-scope`).
//!
//! A network's delivery account needs native token to pay network costs, and a delivery that finds it short is held
//! in `CND-FeeShort` (`REQ-TR-5`). The endpoints record the account's fee position at each final head they record
//! (EVM networks: the balance; the issuing network: free balance less frozen, migrations 0110 and 0130), and
//! `ops_observables()` exposes the latest as `fee_position_amount`, exact. This step compares it with the floor the
//! deployment set **in the core's own configuration** ([`crate::Config::fee_floors`]) and pages when it is below:
//! a low balance is a page like any other, `kind=fee-position-low`, with its runbook
//! (`deploy/runbooks/pages/float-top-up.md`).
//!
//! - **Exact.** The comparison is made on the value as text from the store, not on the float of the metric.
//! - **Once on crossing, then daily.** The page is also an `event_log` entry, and the log is the memory: the
//!   last of this network's `fee-position-low` and `fee-position-recovered` entries says whether a page is
//!   standing. A restart repeats nothing early and forgets nothing. A balance back at or above its floor appends
//!   `fee-position-recovered`, so a second drop is a new crossing and pages at once.
//! - **Unknown is not low.** A fee position that cannot be read, or has never been read, is not compared: it is
//!   the missing-data alarm on `fee_position_amount` and the growing `fee_position_age_seconds` that catch it
//!   (`deploy/runbooks/alerts.md`), and the state of the last page is left as it was.
//! - **A network with no floor** is named in a warning the first time it is seen, and is never paged on.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::PgPool;

use crate::store;

/// The page's kind, as logged (`tracing::error!` below spells it as a literal, which the runbooks' test scans for).
pub const KIND: &str = "fee-position-low";
/// The event log entry that says a network's fee position is back at or above its floor.
pub const RECOVERED: &str = "fee-position-recovered";
/// How often the floors are checked.
pub const EVERY: Duration = Duration::from_secs(60);
/// How long after a page the next is made, while the position is still below its floor.
pub const REPAGE_EVERY: chrono::Duration = chrono::Duration::days(1);

/// What one check did, per network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Below its floor: paged now (`repeat` says it is a daily repeat).
    Paged {
        /// The network.
        network: String,
        /// Whether an earlier page stands.
        repeat: bool,
    },
    /// Below its floor, and a page of the last day stands.
    Standing(String),
    /// Back at or above its floor after a page.
    Recovered(String),
}

/// Checks every network that has a floor against its latest fee position, and pages the ones below it. At least
/// once: the page is logged before the entry that remembers it, so a failure between the two repeats the page
/// rather than losing it.
pub async fn check(
    pool: &PgPool,
    floors: &BTreeMap<String, u128>,
    now: DateTime<Utc>,
) -> sqlx::Result<Vec<Outcome>> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT network, value::text FROM ops_observables() \
         WHERE metric = 'fee_position_amount' AND network IS NOT NULL ORDER BY network",
    )
    .fetch_all(pool)
    .await?;
    let mut outcomes = Vec::new();
    for (network, amount) in rows {
        let (Some(floor), Some(amount)) = (floors.get(&network).copied(), amount) else {
            continue;
        };
        let Ok(amount) = amount.parse::<u128>() else {
            tracing::warn!(network = %network, "REQ-ST-6: a fee position the core cannot read as a whole number is not compared with its floor");
            continue;
        };
        // The last of this network's page and recovery, from the log.
        let last: Option<(String, DateTime<Utc>)> = sqlx::query_as(
            "SELECT kind, COALESCE((payload ->> 'at')::timestamptz, appended_at) FROM event_log \
             WHERE kind IN ($1, $2) AND transfer_id IS NULL AND payload ->> 'network' = $3 \
             ORDER BY seq DESC LIMIT 1",
        )
        .bind(KIND)
        .bind(RECOVERED)
        .bind(&network)
        .fetch_optional(pool)
        .await?;
        let standing = last.as_ref().filter(|(k, _)| k == KIND);
        let mut conn = pool.acquire().await?;
        if amount >= floor {
            if standing.is_some() {
                tracing::info!(network = %network, amount = %amount, floor = %floor, "REQ-ST-6: the fee position is back at or above its floor");
                store::event(
                    &mut conn,
                    RECOVERED,
                    None,
                    &json!({"spec": "REQ-ST-6", "network": network, "amount": amount.to_string(),
                            "floor": floor.to_string(), "at": now.to_rfc3339()}),
                )
                .await?;
                outcomes.push(Outcome::Recovered(network));
            }
            continue;
        }
        if standing.is_some_and(|(_, at)| now < *at + REPAGE_EVERY) {
            outcomes.push(Outcome::Standing(network));
            continue;
        }
        let repeat = standing.is_some();
        tracing::error!(
            kind = %"fee-position-low", network = %network, amount = %amount, floor = %floor, repeat,
            "page: the delivery account's fee position is below its floor; deliveries on this network will be held in CND-FeeShort when it cannot pay. Top it up (deploy/runbooks/pages/float-top-up.md)"
        );
        store::event(
            &mut conn,
            KIND,
            None,
            &json!({"spec": "REQ-ST-6, REQ-TR-5", "network": network, "amount": amount.to_string(),
                    "floor": floor.to_string(), "repeat": repeat, "at": now.to_rfc3339()}),
        )
        .await?;
        outcomes.push(Outcome::Paged { network, repeat });
    }
    Ok(outcomes)
}

/// Runs [`check`] once a minute, and names the networks the store declares that have no floor, once.
pub struct Watch {
    floors: BTreeMap<String, u128>,
    last: Option<Instant>,
    named: BTreeSet<String>,
}

impl Watch {
    /// A watch over `floors`.
    pub fn new(floors: BTreeMap<String, u128>) -> Self {
        Self {
            floors,
            last: None,
            named: BTreeSet::new(),
        }
    }

    /// Checks the floors if a minute has passed since the last time. A failure is logged and tried again at the
    /// next cycle; it never stops the others.
    pub async fn tick(&mut self, pool: &PgPool, at: Instant, now: DateTime<Utc>) {
        if self.last.is_some_and(|l| at.duration_since(l) < EVERY) {
            return;
        }
        self.last = Some(at);
        match check(pool, &self.floors, now).await {
            Ok(outcomes) => tracing::debug!(?outcomes, "fee positions"),
            Err(e) => {
                tracing::warn!(error = %e, "REQ-ST-6: the fee positions could not be checked against their floors; tried again next minute");
                return;
            }
        }
        if let Ok(networks) = sqlx::query_scalar::<_, String>(
            "SELECT network FROM ops_observables() WHERE metric = 'fee_position_amount' AND network IS NOT NULL",
        )
        .fetch_all(pool)
        .await
        {
            for network in networks {
                if !self.floors.contains_key(&network) && self.named.insert(network.clone()) {
                    tracing::warn!(network = %network, "REQ-ST-6: no fee position floor is configured for this network ([fee_floors]); its fee position is observed and never paged on");
                }
            }
        }
    }
}
