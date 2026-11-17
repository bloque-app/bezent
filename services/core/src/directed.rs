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
//! An arrival of a token admitted for no asset cannot be either: nothing is done, it stays listed as
//! unassociated with its owner (`CND-Unassociated`), and the log says why (`REQ-RF-1`: no refund on
//! unknown). A recipient or a list that cannot be read is not a refusal by rule either: the deposit
//! is made, and the core's check before the mint sends it to review (`REQ-IN-5`).
//!
//! Nothing here reads a request, matches by amount or time, or chooses a destination: each decision
//! is a function of the observation and the declarations (`REQ-OT-2`, `REQ-RF-2`).

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

/// Decides every pending directed arrival (see the module documentation), each in a transaction of
/// its own. Returns how many it decided; one whose step fails is logged and tried again next cycle.
pub async fn intake(pool: &PgPool) -> sqlx::Result<usize> {
    let mut decided = 0;
    for o in pending(pool).await? {
        let mut tx = pool.begin().await?;
        match intake_one(&mut tx, &o).await {
            Ok(true) => {
                tx.commit().await?;
                decided += 1;
            }
            Ok(false) => tx.rollback().await?,
            Err(e) => {
                tx.rollback().await?;
                tracing::warn!(observation = o.id, error = %e, "REQ-IN-10: a directed arrival was not decided; tried again next cycle");
            }
        }
    }
    Ok(decided)
}

fn parse(amount: Option<String>) -> Option<u128> {
    amount.and_then(|a| a.parse().ok())
}

async fn intake_one(conn: &mut PgConnection, o: &Observation) -> sqlx::Result<bool> {
    let (Some(token), Some(amount)) = (o.token.as_deref(), o.amount()) else {
        return Ok(false);
    };
    let (Some(recipient), Some(refund)) = (
        o.detail.get("recipient").and_then(Value::as_str),
        o.detail.get("refund").and_then(Value::as_str),
    ) else {
        tracing::error!(
            observation = o.id,
            "REQ-IN-10: a directed arrival with no recipient or refund address; nothing is done"
        );
        return Ok(false);
    };
    let asset: Option<String> =
        sqlx::query_scalar("SELECT asset FROM admitted_tokens WHERE network = $1 AND token = $2")
            .bind(&o.network)
            .bind(token)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(asset) = asset else {
        tracing::error!(
            observation = o.id,
            token,
            "REQ-IN-10, REQ-RF-1: a directed arrival of a token admitted for no asset; it stays unassociated"
        );
        return Ok(false);
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
            // A declaration the core cannot read is no route to mint on, and no rule to return by.
            _ => return Ok(false),
        },
        None => None,
    };
    let listing = recipient_listing(conn, recipient).await?;
    match decide(amount, limits.as_ref(), listing) {
        Decision::Deposit => deposit(conn, o).await,
        Decision::Return(why) => {
            give_back(conn, o, &asset, amount, refund, why).await?;
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

/// The return (`REQ-IN-11`, `AC-A6.4`): the refund of an unassociated arrival, bezent's own, to the
/// recorded refund address, accepted at once with no second approver (Open 3). The store refuses any
/// other destination, a second return, and a return of an arrival something else binds.
async fn give_back(
    conn: &mut PgConnection,
    o: &Observation,
    asset: &str,
    amount: u128,
    refund: &str,
    why: &str,
) -> sqlx::Result<()> {
    let c = o.coordinate();
    let bridge_reference = refs::unassociated_refund(&c);
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index, state) \
         VALUES ($1, $2, 'refund', $3, $4::numeric, $5, $6, $5, $7, $8, $9::position_kind, $10, \
             'Received') \
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
