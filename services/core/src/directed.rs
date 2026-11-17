// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Directed intake (`F-018`, T-018-07 and T-018-08; amendment 0029, A2 and A5).
//!
//! A directed arrival names its own destination and its own refund address in the transaction that
//! moved its value, so no request binds it (`REQ-IN-10`). In each cycle, for every final directed
//! arrival nothing binds yet, in coordinate order, the core decides it once, in one transaction:
//!
//! - **A deposit** (`REQ-IN-10`, `AC-A6.1`), when the route of its token's asset is declared, its
//!   amount is within the route's minimum and maximum as declared now, and its recipient is not on
//!   the refused list (the declared ledger contract accounts of PLAN `DEC-15`). The store's
//!   `directed_deposit()` makes it at `Source-Final`, minted for the recorded recipient, with the
//!   recorded refund address as its refund destination and no request. From there it is an ordinary
//!   deposit: a pause, a hold, the mint window, a fee position park it in their conditions, and the
//!   issuing network's refusal of its destination waits for review (`REQ-IN-9`). When its amount is
//!   over what the route's window has left, it holds `CND-DepositWindow` from the start and leaves
//!   it by itself once it fits, behind every directed deposit waiting ahead of it by coordinate
//!   ([`crate::engine`], `directed_window_fits()`); it is never returned for it (Open 1, ruled:
//!   wait). There is no share of the window and no count cap (Open 5 and 6).
//! - **A return** (`REQ-IN-11`, `AC-A6.4`), when it can never be minted under the route's current
//!   declaration (the route is not declared, or the amount is outside its limits) or its recipient
//!   is refused by rule: the refund of an unassociated arrival (`REQ-RQ-6`), which bezent orders
//!   itself, `bz-u-…` from the coordinate, to the recorded refund address only, accepted at once with
//!   no second approver (Open 3, ruled). Binding the coordinate closes the arrival to minting: the
//!   store lets one transfer consume it (`INV-2`), and refuses any deposit of it afterwards, so a
//!   second return, or a deposit after the return, is refused. From there it is paid out as any
//!   refund of an unassociated arrival is, and the book posts it as one.
//!
//! An arrival of a token admitted for no asset can never be minted either (no route can name it), so
//! it is returned the same way (review L3 of PR #136), its refund naming the observed token and no
//! asset, as the refund of an unassociated arrival of such a token does (`REQ-IN-6`). A recipient or
//! a list that cannot be read is not a refusal by rule: the deposit is made, and the core's check
//! before the mint sends it to review (`REQ-IN-5`), which is never refunded (`REQ-RF-1`).
//!
//! An arrival the core cannot decide (its detail lacks a recipient or a refund address, its route's
//! declaration does not parse, or its step fails) stays as it is and is tried again every cycle; the
//! log says so once an hour per arrival, not every cycle ([`Undecided`]; review L4).
//!
//! Nothing here reads a request, matches by amount or time, or chooses a destination: each decision
//! is a function of the observation and the declarations (`REQ-OT-2`, `REQ-RF-2`).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bezent_core::{Condition, Listing, State};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};

use crate::refs;
use crate::store::{self, Move, Observation};

/// What the core does with a final directed arrival.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// It becomes a deposit (`REQ-IN-10`).
    Deposit,
    /// It can never be minted, or its recipient is refused by rule: it is returned (`REQ-IN-11`),
    /// for the reason named.
    Return(&'static str),
}

/// A route's per-transfer limits, as declared now (`REQ-LM-1`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The minimum per transfer.
    pub minimum: u128,
    /// The maximum per transfer.
    pub maximum: u128,
    /// A minimum a venue imposes, if any.
    pub venue_minimum: Option<u128>,
}

/// The decision for an arrival of `amount` to `recipient`, given its route's `limits` (`None`: the
/// route is not declared) and how the refused list reads `recipient`. Only a recipient the list
/// names is a refusal by rule; one that cannot be read is decided at the mint (`REQ-IN-5`).
pub fn decide(amount: u128, limits: Option<&Limits>, recipient: Listing) -> Decision {
    let Some(l) = limits else {
        return Decision::Return("route-not-declared");
    };
    if amount < l.minimum || l.venue_minimum.is_some_and(|v| amount < v) {
        return Decision::Return("below-minimum");
    }
    if amount > l.maximum {
        return Decision::Return("above-maximum");
    }
    if recipient == Listing::Listed {
        return Decision::Return("recipient-refused");
    }
    Decision::Deposit
}

/// How often an arrival the core cannot decide is logged again for the same reason (review L4).
pub const UNDECIDED_LOG_EVERY: Duration = Duration::from_secs(3600);

/// The arrivals the intake could not decide and when it last logged each, so that an arrival that
/// stays undecidable is logged once an hour for its reason, not once a cycle (review L4). Held by the
/// core across cycles; an arrival that is decided, or no longer pending, is forgotten.
#[derive(Debug, Default)]
pub struct Undecided(Mutex<HashMap<i64, (&'static str, Instant)>>);

impl Undecided {
    /// Whether to log that observation `id` is undecided for `why` now: the first time, when the
    /// reason changed, or [`UNDECIDED_LOG_EVERY`] after the last time.
    pub fn due(&self, id: i64, why: &'static str) -> bool {
        let mut seen = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        match seen.get(&id) {
            Some((last, at)) if *last == why && now.duration_since(*at) < UNDECIDED_LOG_EVERY => {
                false
            }
            _ => {
                seen.insert(id, (why, now));
                true
            }
        }
    }

    fn forget(&self, id: i64) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    }

    fn keep_only(&self, pending: &HashSet<i64>) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|id, _| pending.contains(id));
    }
}

/// The final, executed directed arrivals that no transfer binds yet, in coordinate order.
async fn pending(pool: &PgPool) -> sqlx::Result<Vec<Observation>> {
    sqlx::query_as(&format!(
        "SELECT {} FROM observations o \
         LEFT JOIN observation_outcomes x ON x.observation_id = o.id \
         WHERE o.kind = 'directed_arrival' AND o.executed AND x.outcome = 'final' \
           AND NOT EXISTS (SELECT 1 FROM transfers t \
                 WHERE t.refund_of IS NULL AND t.source_network = o.network \
                   AND t.source_block_number = o.block_number \
                   AND t.source_block_hash = o.block_hash \
                   AND t.source_position_kind = o.position_kind \
                   AND t.source_position_index = o.position_index \
                   AND t.state NOT IN ('Refused', 'Expired', 'Withdrawn')) \
         ORDER BY o.network, o.block_number, o.position_index, o.id",
        store::OBSERVATION
    ))
    .fetch_all(pool)
    .await
}

/// Pages a human, once per transfer, for every directed arrival a transfer binds that is not one of
/// bezent's own rows (review, info, of PR #136; `REQ-IN-10`, `AC-A6.2`, `INV-2`): a transfer whose
/// request reference is not the coordinate's `directed:` label, or that a request answers. The store
/// refuses to write one (0144), but one can exist: a requester's deposit bound to the coordinate
/// before the endpoint observed the arrival as directed, which the store then refuses to make final
/// (review H1 of PR #125), or a row the owner wrote. The arrival is then bound, so the intake never
/// decides it and it is not an unassociated arrival either: nothing would show it. It changes
/// nothing; what to do is the operator's (`directed-binding-alert`, in the event log too).
async fn foreign_bindings(pool: &PgPool) -> sqlx::Result<usize> {
    let rows: Vec<(i64, String, String, String, i64)> = sqlx::query_as(
        "SELECT t.id, t.bridge_reference, t.kind, t.state::text, o.id FROM observations o \
         JOIN transfers t ON t.source_network = o.network AND t.source_block_number = o.block_number \
          AND t.source_block_hash = o.block_hash AND t.source_position_kind = o.position_kind \
          AND t.source_position_index = o.position_index \
         WHERE o.kind = 'directed_arrival' AND t.refund_of IS NULL \
           AND (NOT starts_with(t.request_reference, 'directed:') \
                OR EXISTS (SELECT 1 FROM intake_answers a WHERE a.transfer_id = t.id)) \
           AND NOT EXISTS (SELECT 1 FROM event_log e \
                           WHERE e.kind = 'directed-binding-alert' AND e.transfer_id = t.id) \
         ORDER BY t.id",
    )
    .fetch_all(pool)
    .await?;
    let mut conn = pool.acquire().await?;
    for (id, bridge_reference, kind, state, observation) in &rows {
        let detail = json!({"spec": "REQ-IN-10, AC-A6.2, INV-2",
                            "bridge_reference": bridge_reference, "kind": kind, "state": state,
                            "observation": observation});
        store::event(&mut conn, "directed-binding-alert", Some(*id), &detail).await?;
        tracing::error!(kind = %"directed-binding-alert", transfer = %bridge_reference, %detail,
            "page: a directed arrival is bound by a transfer that is not bezent's own deposit or return; bezent will not decide it");
    }
    Ok(rows.len())
}

/// Decides every pending directed arrival (see the module documentation), each in a transaction of
/// its own. Returns how many it decided; one whose step fails is logged (once an hour, `undecided`)
/// and tried again next cycle. First, a directed arrival another row binds pages
/// ([`foreign_bindings`]); a failure of that is logged and the intake runs.
pub async fn intake(pool: &PgPool, undecided: &Undecided) -> sqlx::Result<usize> {
    if let Err(e) = foreign_bindings(pool).await {
        tracing::warn!(error = %e, "REQ-IN-10: the directed bindings were not checked this cycle; tried again next cycle");
    }
    let mut decided = 0;
    let pending = pending(pool).await?;
    undecided.keep_only(&pending.iter().map(|o| o.id).collect());
    for o in pending {
        let mut tx = pool.begin().await?;
        match intake_one(&mut tx, &o, undecided).await {
            Ok(true) => {
                tx.commit().await?;
                undecided.forget(o.id);
                decided += 1;
            }
            Ok(false) => tx.rollback().await?,
            Err(e) => {
                tx.rollback().await?;
                if undecided.due(o.id, "step-failed") {
                    tracing::warn!(observation = o.id, error = %e, "REQ-IN-10: a directed arrival was not decided; tried again next cycle");
                }
            }
        }
    }
    Ok(decided)
}

fn parse(amount: Option<String>) -> Option<u128> {
    amount.and_then(|a| a.parse().ok())
}

async fn intake_one(
    conn: &mut PgConnection,
    o: &Observation,
    undecided: &Undecided,
) -> sqlx::Result<bool> {
    let (Some(token), Some(amount)) = (o.token.as_deref(), o.amount()) else {
        if undecided.due(o.id, "no-token-or-amount") {
            tracing::error!(
                observation = o.id,
                "REQ-IN-10: a directed arrival with no token or no amount; nothing is done"
            );
        }
        return Ok(false);
    };
    let (Some(recipient), Some(refund)) = (
        o.detail.get("recipient").and_then(Value::as_str),
        o.detail.get("refund").and_then(Value::as_str),
    ) else {
        if undecided.due(o.id, "no-recipient-or-refund") {
            tracing::error!(
                observation = o.id,
                "REQ-IN-10: a directed arrival with no recipient or refund address; nothing is done"
            );
        }
        return Ok(false);
    };
    let asset: Option<String> =
        sqlx::query_scalar("SELECT asset FROM admitted_tokens WHERE network = $1 AND token = $2")
            .bind(&o.network)
            .bind(token)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(asset) = asset else {
        // Review L3 of PR #136 (REQ-IN-11): admitted for no asset, it can never be minted under
        // any route's declaration, so it is returned; the refund names the token, not an asset
        // (REQ-IN-6), and the store holds it to the observed one (0048).
        give_back(
            conn,
            o,
            Paid::Token(token),
            amount,
            refund,
            "token-not-admitted",
        )
        .await?;
        return Ok(true);
    };
    type Row = (Option<String>, Option<String>, Option<String>);
    let row: Option<Row> = sqlx::query_as(
        "SELECT minimum::text, maximum::text, venue_minimum::text FROM routes \
         WHERE asset = $1 AND network = $2 AND direction = 'deposit'",
    )
    .bind(&asset)
    .bind(&o.network)
    .fetch_optional(&mut *conn)
    .await?;
    let limits = match row {
        Some((minimum, maximum, venue_minimum)) => match (parse(minimum), parse(maximum)) {
            (Some(minimum), Some(maximum)) => Some(Limits {
                minimum,
                maximum,
                venue_minimum: parse(venue_minimum),
            }),
            // A declaration the core cannot read is no route to mint on, and no rule to return by
            // (review L4: said in the log, once an hour, not passed over in silence).
            _ => {
                if undecided.due(o.id, "route-unreadable") {
                    tracing::error!(
                        observation = o.id,
                        %asset,
                        network = %o.network,
                        "REQ-IN-10, REQ-LM-1: the deposit route's minimum or maximum does not parse; the directed arrival waits, undecided"
                    );
                }
                return Ok(false);
            }
        },
        None => None,
    };
    let listing = recipient_listing(conn, recipient).await?;
    match decide(amount, limits.as_ref(), listing) {
        Decision::Deposit => deposit(conn, o).await,
        Decision::Return(why) => {
            give_back(conn, o, Paid::Asset(&asset), amount, refund, why).await?;
            Ok(true)
        }
    }
}

/// How the refused list reads `recipient` on the issuing network (`REQ-IN-5`, PLAN `DEC-15`).
async fn recipient_listing(conn: &mut PgConnection, recipient: &str) -> sqlx::Result<Listing> {
    let issuing: Option<(String, String)> =
        sqlx::query_as("SELECT network, address_format FROM networks WHERE role = 'issuing'")
            .fetch_optional(&mut *conn)
            .await?;
    let Some((network, format)) = issuing else {
        return Ok(Listing::Unknown);
    };
    let declared: Vec<(String, Vec<u8>)> =
        sqlx::query_as("SELECT address, account FROM ledger_accounts_listed WHERE network = $1")
            .bind(&network)
            .fetch_all(&mut *conn)
            .await?;
    Ok(bezent_core::listing(&format, recipient, &declared))
}

/// The deposit (`REQ-IN-10`): made by the store from the observation, and, over what the route's
/// window has left, held in `CND-DepositWindow` in the same transaction, under the route's lock.
async fn deposit(conn: &mut PgConnection, o: &Observation) -> sqlx::Result<bool> {
    let c = o.coordinate();
    let id: Option<i64> =
        sqlx::query_scalar("SELECT directed_deposit($1, $2, $3, $4::position_kind, $5)")
            .bind(&c.network)
            .bind(c.block_number)
            .bind(&c.block_hash)
            .bind(&c.position_kind)
            .bind(c.position_index)
            .fetch_one(&mut *conn)
            .await?;
    let Some(id) = id else {
        // Something else binds the coordinate (its return): nothing to do.
        return Ok(false);
    };
    let fits: bool = sqlx::query_scalar("SELECT directed_window_fits($1)")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    if !fits {
        tracing::info!(
            transfer = id,
            "CND-DepositWindow: a directed deposit over what its route's window has left waits for it"
        );
        store::append(
            conn,
            id,
            &Move {
                from: State::SourceFinal,
                to: State::SourceFinal,
                condition: Some(Condition::DepositWindow),
                point: None,
                attempt: None,
                manual_action: None,
            },
        )
        .await?;
    }
    Ok(true)
}

/// What a return names: the bridged asset its token is admitted for, or, for a token admitted for
/// none, the token itself (`REQ-IN-6`; review L3).
#[derive(Clone, Copy, Debug)]
enum Paid<'a> {
    Asset(&'a str),
    Token(&'a str),
}

/// The return (`REQ-IN-11`, `AC-A6.4`): the refund of an unassociated arrival, bezent's own, to the
/// recorded refund address, accepted at once with no second approver (Open 3). The store refuses any
/// other destination, a second return, and a return of an arrival something else binds.
async fn give_back(
    conn: &mut PgConnection,
    o: &Observation,
    paid: Paid<'_>,
    amount: u128,
    refund: &str,
    why: &str,
) -> sqlx::Result<()> {
    let c = o.coordinate();
    let bridge_reference = refs::unassociated_refund(&c);
    let (asset, token) = match paid {
        Paid::Asset(a) => (Some(a), None),
        Paid::Token(t) => (None, Some(t)),
    };
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index, state, token) \
         VALUES ($1, $2, 'refund', $3, $4::numeric, $5, $6, $5, $7, $8, $9::position_kind, $10, \
             'Received', $11) \
         ON CONFLICT DO NOTHING RETURNING id",
    )
    .bind(&bridge_reference)
    .bind(format!(
        "directed:{}:{}:0x{}:{}:{}:return",
        c.network,
        c.block_number,
        hex::encode(&c.block_hash),
        c.position_kind,
        c.position_index
    ))
    .bind(asset)
    .bind(amount.to_string())
    .bind(&c.network)
    .bind(refund)
    .bind(c.block_number)
    .bind(&c.block_hash)
    .bind(&c.position_kind)
    .bind(c.position_index)
    .bind(token)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(id) = id else {
        return Ok(());
    };
    store::append(
        conn,
        id,
        &Move {
            from: State::Received,
            to: State::Accepted,
            condition: None,
            point: None,
            attempt: None,
            manual_action: None,
        },
    )
    .await?;
    let detail = json!({"spec": "REQ-IN-11, AC-A6.4", "bridge_reference": bridge_reference,
                        "observation": o.id, "reason": why, "refund": refund,
                        "network": c.network, "block_number": c.block_number,
                        "position_index": c.position_index});
    store::event(conn, "directed-return", Some(id), &detail).await?;
    tracing::warn!(transfer = %bridge_reference, why, %detail, "REQ-IN-11: a directed arrival that can never be minted is returned to its refund address");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review L4 of PR #136: an arrival that stays undecidable is logged once for its reason, again
    /// when the reason changes, and not again until [`UNDECIDED_LOG_EVERY`] has passed; one that is
    /// decided, or no longer pending, is forgotten.
    #[test]
    fn req_in_10_an_undecided_arrival_is_logged_once_not_every_cycle() {
        let u = Undecided::default();
        assert!(u.due(1, "route-unreadable"));
        assert!(!u.due(1, "route-unreadable"));
        assert!(u.due(2, "route-unreadable"), "per arrival");
        assert!(u.due(1, "step-failed"), "a new reason");
        assert!(!u.due(1, "step-failed"));
        u.forget(1);
        assert!(u.due(1, "step-failed"), "decided, then undecided again");
        u.keep_only(&HashSet::from([1]));
        assert!(u.due(2, "route-unreadable"), "no longer pending: forgotten");
        u.0.lock().unwrap().insert(
            1,
            (
                "step-failed",
                Instant::now() - UNDECIDED_LOG_EVERY - Duration::from_secs(1),
            ),
        );
        assert!(u.due(1, "step-failed"), "an hour on");
    }

    fn limits() -> Limits {
        Limits {
            minimum: 1_000_000,
            maximum: 50_000_000_000,
            venue_minimum: None,
        }
    }

    /// `REQ-IN-10`, `REQ-IN-11`: what is mintable under the route as declared now becomes a
    /// deposit; what never can, or whose recipient is refused by rule, is returned; a recipient the
    /// list cannot read is no refusal by rule (`REQ-RF-1`).
    #[test]
    fn req_in_11_only_the_never_mintable_and_the_refused_are_returned() {
        let l = limits();
        assert_eq!(
            decide(1_000_000, Some(&l), Listing::Clear),
            Decision::Deposit
        );
        assert_eq!(
            decide(50_000_000_000, Some(&l), Listing::Clear),
            Decision::Deposit
        );
        assert_eq!(
            decide(999_999, Some(&l), Listing::Clear),
            Decision::Return("below-minimum")
        );
        assert_eq!(
            decide(50_000_000_001, Some(&l), Listing::Clear),
            Decision::Return("above-maximum")
        );
        assert_eq!(
            decide(1_000_000, None, Listing::Clear),
            Decision::Return("route-not-declared")
        );
        assert_eq!(
            decide(1_000_000, Some(&l), Listing::Listed),
            Decision::Return("recipient-refused")
        );
        assert_eq!(
            decide(1_000_000, Some(&l), Listing::Unknown),
            Decision::Deposit
        );
        let venue = Limits {
            venue_minimum: Some(2_000_000),
            ..l
        };
        assert_eq!(
            decide(1_500_000, Some(&venue), Listing::Clear),
            Decision::Return("below-minimum")
        );
    }
}
