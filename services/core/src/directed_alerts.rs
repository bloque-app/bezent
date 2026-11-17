// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The alerts on directed deposits (F-018, T-018-11; amendment 0029, "New exposures", Open 5 and 6:
//! **alerts only**, no count cap and no share of the window; `REQ-IC-2`, `REQ-ST-6`).
//!
//! Directed deposits are open to anyone, so what they cost bezent is watched, not capped. Each
//! minute the core reads, per deposit route, from the store:
//!
//! | Page | What | Against |
//! |---|---|---|
//! | `directed-mints-alert` | the directed deposits minted (`Delivered`) in the trailing 24 hours: each new mint reference takes storage deposit from the issuing network's delivery account | [`Thresholds::mints_per_day`] |
//! | `directed-returns-alert` | the returns of directed arrivals, and refunds of directed deposits, made in the trailing 24 hours: each is a payout from the reserve's delivery allowance, which withdrawals share | [`Thresholds::returns_per_day`] |
//! | `directed-window-alert` | the share of the route's window that directed deposits hold: those it counts now (made, or out of `CND-DepositWindow`, within the window, as `route_usage_locked()` counts them) and those waiting in `CND-DepositWindow`, which claim it ahead of anything later (migration 0145) | [`Thresholds::window_share_percent`] |
//! | `directed-wait-alert` | the directed deposits that have held `CND-TokenUpgraded` or `CND-DepositCapacity` for longer than one of the route's windows (amendment 0031; review L2 of PR #142): a hold no operator has lifted, or a route that stays full | more than none |
//!
//! A count or share **above** its threshold pages, once on crossing and then once a day while it
//! stays above; back at or below, `directed-alert-cleared` is logged, so a second crossing pages at
//! once. The log is the memory, as for `fee-position-low` (`crate::fees`): a restart repeats nothing
//! early and forgets nothing.
//!
//! Two observations page one by one, each once, with no threshold:
//!
//! - `unpaired-direction`: a declared directing contract's `Directed` log that paired with no
//!   transfer into the reserve (`AC-A6.6`). With the declared code it should never happen; it binds
//!   nothing (migration 0143), and a person must find out why.
//! - `origin-token-upgraded`: an admitted origin token's proxy emitted `Upgraded` (EIP-1967): its
//!   implementation changed (migration 0147). It pages and does not pause. It **holds directed
//!   intake** of that token (amendment 0031, `d-f018-usdc-upgrade`, ruled: hold and alert): the
//!   store reads the observation itself (`directed_arrival_token_held()`, migration 0150), so the
//!   hold needs nothing from this page and starts with the observation, not with the page. A
//!   directed deposit of that token at or after the upgrade waits in `CND-TokenUpgraded`, and
//!   nothing of it is returned, until two operators lift the hold after the fork suite has been run
//!   against the new implementation (`operator_request_token_hold_lift`,
//!   `operator_approve_token_hold_lift`). A requester's deposit is not held.
//!
//! The returns alert also watches the exposure amendment 0031 accepted (`d-f018-return-routing`):
//! a funder can route value through the reserve to a third party's refund address, at bezent's gas
//! cost, by sending what can never be minted; every such return counts here.
//!
//! At least once: a page is logged before the entry that remembers it, so a failure between the two
//! repeats the page rather than losing it.

use std::time::{Duration, Instant};

use serde_json::json;
use sqlx::PgPool;

use crate::store;

/// Directed deposits minted in the trailing day above the threshold.
pub const MINTS: &str = "directed-mints-alert";
/// Returns and refunds of directed deposits in the trailing day above the threshold.
pub const RETURNS: &str = "directed-returns-alert";
/// Directed deposits' share of a route's window above the threshold.
pub const WINDOW: &str = "directed-window-alert";
/// Directed deposits held by an upgrade of their token, or waiting for capacity, for longer than one
/// of the route's windows.
pub const WAITS: &str = "directed-wait-alert";
/// A `Directed` log that paired with no transfer (`AC-A6.6`).
pub const UNPAIRED: &str = "unpaired-direction";
/// An admitted origin token's implementation changed.
pub const UPGRADED: &str = "origin-token-upgraded";
/// A count or share back at or below its threshold, after a page.
pub const CLEARED: &str = "directed-alert-cleared";
/// How often the alerts are checked.
pub const EVERY: Duration = Duration::from_secs(60);

/// The thresholds, from the core's configuration (`[directed_alerts]`). A count or share **above**
/// its threshold pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thresholds {
    /// Directed deposits minted per route in the trailing 24 hours.
    pub mints_per_day: u64,
    /// Returns of directed arrivals and refunds of directed deposits per route in the trailing 24
    /// hours.
    pub returns_per_day: u64,
    /// The share of a route's window directed deposits hold, in percent (1 to 100).
    pub window_share_percent: u32,
}

impl Default for Thresholds {
    /// 200 mints a day (the count amendment 0029 put to the owner as a cap, Open 6, and which he
    /// ruled an alert instead), 3 returns a day (a return is rare by construction: only what can never
    /// mint is returned, amendment 0029 A5), and half of the window (Open 5: no share, so an alert well
    /// before directed deposits crowd requesters out).
    fn default() -> Self {
        Self {
            mints_per_day: 200,
            returns_per_day: 3,
            window_share_percent: 50,
        }
    }
}

/// What one check did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Above its threshold: paged now (`repeat` says an earlier page of the last day stands).
    Paged {
        /// The page's kind.
        kind: &'static str,
        /// The route's asset.
        asset: String,
        /// The route's network.
        network: String,
        /// The count, or the share in basis points.
        value: u128,
        /// Whether it is the daily repeat.
        repeat: bool,
    },
    /// Above its threshold, and a page of the last day stands.
    Standing {
        /// The page's kind.
        kind: &'static str,
        /// The route's asset.
        asset: String,
        /// The route's network.
        network: String,
    },
    /// Back at or below its threshold after a page.
    Cleared {
        /// The page's kind.
        kind: &'static str,
        /// The route's asset.
        asset: String,
        /// The route's network.
        network: String,
    },
    /// An observation paged (`kind` is [`UNPAIRED`] or [`UPGRADED`]).
    Observation {
        /// The page's kind.
        kind: &'static str,
        /// The observation.
        id: i64,
    },
}

/// Whether the transfer `d` is bezent's own, bound to a directed arrival: no request answered with
/// it, and the observation at its source is a `directed_arrival` (migration 0144's rule).
const DIRECTED: &str = "\
    NOT EXISTS (SELECT 1 FROM intake_answers a WHERE a.transfer_id = d.id) \
    AND EXISTS (SELECT 1 FROM observations o \
                WHERE o.network = d.source_network AND o.block_number = d.source_block_number \
                  AND o.block_hash = d.source_block_hash \
                  AND o.position_kind = d.source_position_kind \
                  AND o.position_index = d.source_position_index \
                  AND o.kind = 'directed_arrival')";

/// An observation to page: (id, network, block, hash, log index, sender, detail).
type PendingRow = (
    i64,
    String,
    i64,
    Vec<u8>,
    i64,
    Option<String>,
    serde_json::Value,
);

/// Per deposit route: (asset, network, window limit, mints, returns, held, long waits).
type RouteRow = (String, String, String, i64, i64, String, i64);

async fn routes(pool: &PgPool) -> sqlx::Result<Vec<RouteRow>> {
    let directed_p = DIRECTED.replace("d.", "p.");
    sqlx::query_as(&format!(
        "SELECT r.asset, r.network, r.window_limit::text, \
           (SELECT count(DISTINCT d.id) FROM transitions x JOIN transfers d ON d.id = x.transfer_id \
             WHERE x.to_state = 'Delivered' AND x.seen_at > now() - interval '1 day' \
               AND d.kind = 'deposit' AND d.refund_of IS NULL AND d.asset = r.asset \
               AND d.source_network = r.network AND {DIRECTED}) AS mints, \
           (SELECT count(*) FROM transfers d LEFT JOIN transfers p ON p.id = d.refund_of \
             WHERE d.asset = r.asset AND d.kind = 'refund' \
               AND d.created_at > now() - interval '1 day' \
               AND COALESCE(d.source_network, p.source_network) = r.network \
               AND ((d.refund_of IS NULL AND {DIRECTED}) \
                    OR (p.kind = 'deposit' AND {directed_p}))) AS returns, \
           (SELECT COALESCE(sum(d.amount), 0)::text FROM transfers d \
             WHERE d.id IN (SELECT t.id FROM transfers t \
                              WHERE t.asset = r.asset AND t.kind = 'deposit' \
                                AND t.created_at > now() - make_interval(secs => r.window_seconds) \
                            UNION \
                            SELECT w.transfer_id FROM transitions w \
                              WHERE w.condition = 'CND-DepositWindow') \
               AND d.asset = r.asset AND d.kind = 'deposit' AND d.refund_of IS NULL \
               AND d.source_network = r.network \
               AND d.state NOT IN ('Refused', 'Expired', 'Withdrawn') AND {DIRECTED} \
               AND CASE \
                   WHEN d.condition = 'CND-DepositWindow' THEN TRUE \
                   WHEN EXISTS (SELECT 1 FROM transitions w WHERE w.transfer_id = d.id \
                                  AND w.condition = 'CND-DepositWindow') THEN \
                       (SELECT min(x.seen_at) FROM transitions x \
                         WHERE x.transfer_id = d.id \
                           AND x.condition IS DISTINCT FROM 'CND-DepositWindow' \
                           AND x.id > (SELECT min(w.id) FROM transitions w \
                                        WHERE w.transfer_id = d.id \
                                          AND w.condition = 'CND-DepositWindow')) \
                       > now() - make_interval(secs => r.window_seconds) \
                   ELSE d.created_at > now() - make_interval(secs => r.window_seconds) \
               END) AS held, \
           (SELECT count(*) FROM transfers d \
             WHERE d.asset = r.asset AND d.kind = 'deposit' AND d.refund_of IS NULL \
               AND d.source_network = r.network AND NOT is_terminal_state(d.state) \
               AND d.condition IN ('CND-TokenUpgraded', 'CND-DepositCapacity') \
               AND (SELECT min(x.seen_at) FROM transitions x \
                     WHERE x.transfer_id = d.id AND x.condition = d.condition \
                       AND x.id > COALESCE((SELECT max(y.id) FROM transitions y \
                                             WHERE y.transfer_id = d.id \
                                               AND y.condition IS DISTINCT FROM d.condition), 0)) \
                   <= now() - make_interval(secs => r.window_seconds)) AS waits \
         FROM routes r WHERE r.direction = 'deposit' ORDER BY r.asset, r.network"
    ))
    .fetch_all(pool)
    .await
}

/// The key a threshold alert's entries carry, so its last page or clearance is one index read.
fn key(kind: &str, asset: &str, network: &str) -> String {
    format!("{kind}/{asset}/{network}")
}

/// Checks every deposit route against the thresholds, and pages each directed observation not paged
/// yet. See the module documentation.
pub async fn check(pool: &PgPool, thresholds: &Thresholds) -> sqlx::Result<Vec<Outcome>> {
    let mut outcomes = Vec::new();
    for (asset, network, limit, mints, returns, held, waits) in routes(pool).await? {
        let limit: u128 = limit.parse().unwrap_or(0);
        let held: u128 = held.parse().unwrap_or(0);
        // The share in basis points, for the page; the comparison is exact, in integers.
        let share_bps = held.saturating_mul(10_000).checked_div(limit).unwrap_or(0);
        let over_window = limit > 0
            && held.saturating_mul(100)
                > u128::from(thresholds.window_share_percent).saturating_mul(limit);
        for (kind, value, above, threshold) in [
            (
                MINTS,
                u128::try_from(mints).unwrap_or(0),
                u64::try_from(mints).unwrap_or(0) > thresholds.mints_per_day,
                u128::from(thresholds.mints_per_day),
            ),
            (
                RETURNS,
                u128::try_from(returns).unwrap_or(0),
                u64::try_from(returns).unwrap_or(0) > thresholds.returns_per_day,
                u128::from(thresholds.returns_per_day),
            ),
            (
                WINDOW,
                share_bps,
                over_window,
                u128::from(thresholds.window_share_percent) * 100,
            ),
            (WAITS, u128::try_from(waits).unwrap_or(0), waits > 0, 0),
        ] {
            if let Some(o) =
                threshold_alert(pool, kind, &asset, &network, value, threshold, above).await?
            {
                outcomes.push(o);
            }
        }
    }
    for kind in [UNPAIRED, UPGRADED] {
        let observation_kind = if kind == UNPAIRED {
            "unpaired_direction"
        } else {
            "token_upgrade"
        };
        let pending: Vec<PendingRow> =
            sqlx::query_as(
                "SELECT o.id, o.network, o.block_number, o.block_hash, o.position_index, o.sender, \
                        o.detail FROM observations o \
                 WHERE o.kind IN ('unpaired_direction', 'token_upgrade') AND o.kind = $1 \
                   AND NOT EXISTS (SELECT 1 FROM event_log s \
                                   WHERE (s.payload ->> 'directed_alert') = 'observation/' || o.id) \
                 ORDER BY o.id LIMIT 50",
            )
            .bind(observation_kind)
            .fetch_all(pool)
            .await?;
        for (id, network, block, hash, index, sender, detail) in pending {
            page_observation(
                pool, kind, id, &network, block, &hash, index, sender, &detail,
            )
            .await?;
            outcomes.push(Outcome::Observation { kind, id });
        }
    }
    Ok(outcomes)
}

async fn threshold_alert(
    pool: &PgPool,
    kind: &'static str,
    asset: &str,
    network: &str,
    value: u128,
    threshold: u128,
    above: bool,
) -> sqlx::Result<Option<Outcome>> {
    let k = key(kind, asset, network);
    let last: Option<(String, bool)> = sqlx::query_as(
        "SELECT kind, appended_at > now() - interval '1 day' FROM event_log \
         WHERE (payload ->> 'directed_alert') = $1 ORDER BY seq DESC LIMIT 1",
    )
    .bind(&k)
    .fetch_optional(pool)
    .await?;
    let standing = last.as_ref().filter(|(last, _)| last == kind);
    let mut conn = pool.acquire().await?;
    if !above {
        if standing.is_none() {
            return Ok(None);
        }
        tracing::info!(alert = %kind, asset = %asset, network = %network, value = %value, threshold = %threshold, "F-018: back at or below its threshold");
        store::event(
            &mut conn,
            CLEARED,
            None,
            &json!({"spec": "REQ-IC-2", "directed_alert": k, "alert": kind, "asset": asset,
                    "network": network, "value": value.to_string(),
                    "threshold": threshold.to_string()}),
        )
        .await?;
        return Ok(Some(Outcome::Cleared {
            kind,
            asset: asset.to_owned(),
            network: network.to_owned(),
        }));
    }
    if standing.is_some_and(|(_, recent)| *recent) {
        return Ok(Some(Outcome::Standing {
            kind,
            asset: asset.to_owned(),
            network: network.to_owned(),
        }));
    }
    let repeat = standing.is_some();
    match kind {
        MINTS => tracing::error!(
            kind = %"directed-mints-alert", asset = %asset, network = %network, mints = %value, threshold = %threshold, repeat,
            "page: more directed deposits were minted on this route in the last 24 hours than the threshold; each new mint reference takes storage deposit from the issuing network's delivery account. Look at who is depositing and at the fee position (deploy/runbooks/pages/directed-mints.md)"
        ),
        RETURNS => tracing::error!(
            kind = %"directed-returns-alert", asset = %asset, network = %network, returns = %value, threshold = %threshold, repeat,
            "page: more directed arrivals were returned on this route in the last 24 hours than the threshold; each is a payout from the reserve's delivery allowance, which withdrawals share (deploy/runbooks/pages/directed-returns.md)"
        ),
        WINDOW => tracing::error!(
            kind = %"directed-window-alert", asset = %asset, network = %network, share_bps = %value, threshold_bps = %threshold, repeat,
            "page: directed deposits hold more of this route's window than the threshold; requesters' deposits are refused ERR-WindowExceeded when it is full (deploy/runbooks/pages/directed-window.md)"
        ),
        _ => tracing::error!(
            kind = %"directed-wait-alert", asset = %asset, network = %network, deposits = %value, repeat,
            "page: directed deposits on this route have been held by an upgrade of their token (CND-TokenUpgraded) or waited for its capacity (CND-DepositCapacity) for longer than one window; is a lift of the hold pending, or the route full? (deploy/runbooks/conditions/CND-TokenUpgraded.md, deploy/runbooks/conditions/CND-DepositCapacity.md)"
        ),
    }
    store::event(
        &mut conn,
        kind,
        None,
        &json!({"spec": "REQ-IC-2, REQ-ST-6", "directed_alert": k, "asset": asset,
                "network": network, "value": value.to_string(),
                "threshold": threshold.to_string(), "repeat": repeat}),
    )
    .await?;
    Ok(Some(Outcome::Paged {
        kind,
        asset: asset.to_owned(),
        network: network.to_owned(),
        value,
        repeat,
    }))
}

/// Pages one observation of [`UNPAIRED`] or [`UPGRADED`] and remembers it. It pauses nothing; the
/// hold on directed intake an upgrade makes is the store's, from the observation itself (see the
/// module documentation).
#[allow(clippy::too_many_arguments)]
async fn page_observation(
    pool: &PgPool,
    kind: &'static str,
    id: i64,
    network: &str,
    block: i64,
    hash: &[u8],
    index: i64,
    sender: Option<String>,
    detail: &serde_json::Value,
) -> sqlx::Result<()> {
    let at = format!("{network}:{block}:0x{}:log:{index}", hex::encode(hash));
    if kind == UNPAIRED {
        let directing = detail["directing"].as_str().unwrap_or("unknown");
        let reason = detail["reason"].as_str().unwrap_or("");
        tracing::error!(
            kind = %"unpaired-direction", observation = id, at = %at, directing = %directing,
            sender = %sender.as_deref().unwrap_or(""), reason = %reason,
            "page: a declared directing contract's Directed log paired with no transfer into the reserve (AC-A6.6). It binds nothing and nothing is minted; with the declared code it should never happen (deploy/runbooks/pages/unpaired-direction.md)"
        );
    } else {
        let token = detail["token"].as_str().unwrap_or("unknown");
        let implementation = detail["implementation"].as_str().unwrap_or("unknown");
        tracing::error!(
            kind = %"origin-token-upgraded", observation = id, at = %at, token = %token,
            implementation = %implementation,
            "page: an admitted origin token's implementation changed (its proxy emitted Upgraded). Directed intake of the token is held (CND-TokenUpgraded) until two operators lift it after the fork suite has been run against the new implementation; nothing is paused. Review the new implementation now (deploy/runbooks/pages/token-upgraded.md)"
        );
    }
    let mut conn = pool.acquire().await?;
    store::event(
        &mut conn,
        kind,
        None,
        &json!({"spec": if kind == UNPAIRED { "AC-A6.6, REQ-IC-2" } else { "REQ-AS-11, REQ-IC-2" },
                "directed_alert": format!("observation/{id}"), "observation_id": id,
                "coordinate": at, "detail": detail}),
    )
    .await
}

/// The `token_upgrade` observations of `token` on `network` after observation `after` (by id),
/// final: the upgrades a runbook names when it requests the lift of their hold (amendment 0031,
/// `d-f018-usdc-upgrade`). The hold itself is the store's (`directed_arrival_token_held()`).
pub async fn token_upgrades_since(
    pool: &PgPool,
    network: &str,
    token: &str,
    after: i64,
) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar(
        "SELECT o.id FROM observations o \
         JOIN observation_outcomes x ON x.observation_id = o.id AND x.outcome = 'final' \
         WHERE o.kind IN ('unpaired_direction', 'token_upgrade') AND o.kind = 'token_upgrade' \
           AND o.network = $1 AND o.detail ->> 'token' = $2 AND o.id > $3 ORDER BY o.id",
    )
    .bind(network)
    .bind(token)
    .bind(after)
    .fetch_all(pool)
    .await
}

/// Runs [`check`] once a minute.
pub struct Watch {
    thresholds: Thresholds,
    last: Option<Instant>,
}

impl Watch {
    /// A watch with `thresholds`.
    pub fn new(thresholds: Thresholds) -> Self {
        Self {
            thresholds,
            last: None,
        }
    }

    /// Checks if a minute has passed since the last time. A failure is logged and tried again at the
    /// next cycle; it never stops the others.
    pub async fn tick(&mut self, pool: &PgPool, at: Instant) {
        if self.last.is_some_and(|l| at.duration_since(l) < EVERY) {
            return;
        }
        self.last = Some(at);
        match check(pool, &self.thresholds).await {
            Ok(outcomes) => tracing::debug!(?outcomes, "F-018 alerts"),
            Err(e) => {
                tracing::warn!(error = %e, "F-018: the directed deposits' alerts could not be checked; tried again next minute")
            }
        }
    }
}
