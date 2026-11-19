// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-018, T-018-07 to T-018-09 (amendment 0029, A2, A5, Open 1 to 3): the core's side of a directed
//! deposit, at the service layer (SPEC §9: the core's cycle over durable state).
//!
//! - T-018-07, directed intake: a final directed arrival becomes a deposit with no request, minted
//!   for the recorded recipient with the recorded refund address as its refund destination
//!   (`AC-A6.1`, `REQ-IN-10`); a pause, a hold and the mint window park it and nothing is returned
//!   (`AC-A6.3`, `REQ-IN-11`, `REQ-LM-3`); over what the route's window has left it waits in
//!   `CND-DepositWindow` and resumes by itself as the window rolls over, first come first served by
//!   coordinate, and is never returned (Open 1, ruled: wait).
//! - T-018-08, the return: an arrival that can never be minted, or whose recipient is refused by
//!   rule, is returned once, to its refund address only, by bezent itself and with no second approver
//!   (`AC-A6.4`, `REQ-IN-11`, `REQ-RF-2`, Open 3), paid with its own reference, and the book posts it
//!   (`REQ-BK-7`); a second return is refused (`INV-2`).
//! - T-018-09, the review edge: a directed deposit in review for `CND-DestinationRefused`, held from
//!   `Source-Final` with no mint attempt ever signed, resolves into `Refunding` only with a second
//!   approver (`REQ-RV-2`, `REQ-GV-5`, `INV-8`, Open 2); no other deposit gains the edge.

mod common;

use bezent_core_service::refs::{self, Coordinate};
use bezent_core_service::reserve::coordinate_key;
use chrono::{DateTime, Duration, Utc};
use common::reserve::balance;
use common::*;
use serde_json::json;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// The declared directing contract on the origin network.
const DIRECTING: &str = "0xD1Ec7ed00000000000000000000000000000d1ec";
/// The refund address the caller named (`d-refund-dest-swaps`): the original depositor.
const REFUND: &str = "0x52908400098527886E0F7030069857D2E4169EE7";
/// The funder: the account the value moved out of (an evmhot deposit address, say).
const FUNDER: &str = "0x00000000000000000000000000000000000000f1";

fn at(block: i64, index: i64) -> Coordinate {
    Coordinate {
        network: ORIGIN.into(),
        block_number: block,
        block_hash: hash(ORIGIN, block),
        position_kind: "log".into(),
        position_index: index,
    }
}

/// Declares [`DIRECTING`] for the reserve, for [`TOKEN`], from block 0 (`REQ-AS-11`; once): only
/// declared directing code directs (review M3 of PR #139).
async fn declare_directing(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO directing_code (asset, network, address, token, minimum, maximum, \
             from_block, reference) \
         VALUES ($1, $2, $3, $4, 1, 1000000000000000, 0, 'test declaration') \
         ON CONFLICT DO NOTHING",
    )
    .bind(ASSET)
    .bind(ORIGIN)
    .bind(DIRECTING)
    .bind(TOKEN)
    .execute(pool)
    .await
    .expect("declare the directing code");
}

/// What the EVM endpoint records for a directed arrival at (`ORIGIN`, `block`, log `index`): one
/// observation at the `Transfer`'s coordinate, at the reserve, with the issuing network's recipient
/// and the refund address in its detail (T-018-05), through declared directing code.
async fn directed_to(pool: &PgPool, block: i64, index: i64, amount: u128, recipient: &str) -> i64 {
    declare_directing(pool).await;
    observe(
        pool,
        ORIGIN,
        block,
        index,
        TOKEN,
        amount,
        Some(FUNDER),
        Some(RESERVE),
        None,
        true,
        json!({"event": "DirectedArrival", "directing": DIRECTING, "direction_log": index + 1,
               "recipient": recipient, "refund": REFUND}),
    )
    .await
}

/// A directed arrival of `amount` for [`DESTINATION_ACCOUNT`], final two blocks later.
async fn final_directed(pool: &PgPool, block: i64, index: i64, amount: u128) -> i64 {
    let id = directed_to(pool, block, index, amount, DESTINATION_ACCOUNT).await;
    decide(pool, id, "final", block + 2).await;
    id
}

/// The transfer that binds the directed arrival at (`block`, `index`), if any: (id, kind).
async fn bound(w: &World, block: i64, index: i64) -> Option<(i64, String)> {
    sqlx::query_as(
        "SELECT id, kind FROM transfers WHERE refund_of IS NULL AND source_network = $1 \
           AND source_block_number = $2 AND source_position_index = $3",
    )
    .bind(ORIGIN)
    .bind(block)
    .bind(index)
    .fetch_optional(&w.admin)
    .await
    .unwrap()
}

async fn returns(w: &World) -> i64 {
    w.count("SELECT count(*) FROM transfers WHERE kind = 'refund'")
        .await
}

/// Operator `by` records a resolution of `id` into `to`, resting on the final head (`network`,
/// `block`).
async fn resolution(w: &World, id: i64, to: &str, network: &str, block: i64, by: usize) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, \
             block_hash, block_time, justification) \
         VALUES ('review-resolution', $1, $2::transfer_state, $3, $4, $5, $6, 'cannot receive') \
         RETURNING id",
    )
    .bind(id)
    .bind(to)
    .bind(network)
    .bind(block)
    .bind(hash(network, block))
    .bind(t0() + Duration::seconds(block))
    .fetch_one(&w.ops[by])
    .await
    .unwrap()
}

/// Operator `by` records a resolution of `id` into `Refunding`, resting on the coordinate of attempt
/// 1's latest refused dry run on the issuing network, with the block time the endpoint recorded with
/// it (`common::prepare`: block 1, hash `\x05`, `t0()` + 1 s; no head is recorded there on its own):
/// the evidence of a directed deposit's review exit (review M1, L-a and L-b of PR #136).
async fn resolution_on_refusal(w: &World, id: i64, by: usize) -> i64 {
    resolution_on_refusal_at(w, id, by, None).await
}

/// As [`resolution_on_refusal`], naming block time `time` instead of the recorded one, if given.
async fn resolution_on_refusal_at(
    w: &World,
    id: i64,
    by: usize,
    time: Option<DateTime<Utc>>,
) -> i64 {
    let (network, block, hash, recorded): (String, i64, Vec<u8>, DateTime<Utc>) = sqlx::query_as(
        "SELECT p.network, p.dry_run_block_number, p.dry_run_block_hash, p.dry_run_block_time \
         FROM prepared_transactions p JOIN intents i ON i.id = p.order_intent_id \
         WHERE i.transfer_id = $1 AND i.attempt = 1 AND NOT p.dry_run_passed \
         ORDER BY p.id DESC LIMIT 1",
    )
    .bind(id)
    .fetch_one(&w.admin)
    .await
    .unwrap();
    sqlx::query_scalar(
        "INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, \
             block_hash, block_time, justification) \
         VALUES ('review-resolution', $1, 'Refunding', $2, $3, $4, $5, \
             'the issuing network refused the destination') RETURNING id",
    )
    .bind(id)
    .bind(network)
    .bind(block)
    .bind(hash)
    .bind(time.unwrap_or(recorded))
    .fetch_one(&w.ops[by])
    .await
    .unwrap()
}

async fn evidence_refused(w: &World, action: i64) -> i64 {
    w.count(&format!(
        "SELECT count(*) FROM event_log WHERE kind = 'review-evidence-refused' \
           AND payload ->> 'manual_action_id' = '{action}'"
    ))
    .await
}

/// A payout leg on the origin network, from the reserve to `to`, carrying `reference`, final two
/// blocks later.
async fn paid_out(w: &World, block: i64, amount: u128, to: &str, reference: &[u8]) {
    let leg = observe(
        &w.admin,
        ORIGIN,
        block,
        1,
        TOKEN,
        amount,
        Some(RESERVE),
        Some(to),
        Some(reference),
        true,
        json!({"event": "Payout"}),
    )
    .await;
    decide(&w.admin, leg, "final", block + 2).await;
}

// ---------------------------------------------------------------------------------------------
// T-018-07: directed intake.

/// `AC-A6.1`, `REQ-IN-10`, `REQ-IN-4`, `INV-6`: a directed arrival is nothing until it is final;
/// then it is a deposit at `Source-Final` with no request, its destination the recorded recipient
/// and its refund destination the refund address its caller named (not the funder), and it is
/// minted for exactly its amount, to that recipient, through to `Completed`.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_1_a_final_directed_arrival_is_minted_for_its_recorded_recipient(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let arrival = directed_to(&w.admin, 10, 0, 5_000_000, DESTINATION_ACCOUNT).await;
    w.settle(t0()).await;
    assert_eq!(
        bound(&w, 10, 0).await,
        None,
        "INV-6: nothing before it is final"
    );

    decide(&w.admin, arrival, "final", 12).await;
    w.cycle(t0()).await;
    let (id, kind) = bound(&w, 10, 0).await.expect("the deposit");
    assert_eq!(kind, "deposit");
    let (bridge, destination, refund_network, refund_to, answers): (
        String,
        String,
        String,
        String,
        i64,
    ) = sqlx::query_as(
        "SELECT bridge_reference, destination, refund_destination_network, refund_destination, \
             (SELECT count(*) FROM intake_answers a WHERE a.transfer_id = t.id) \
         FROM transfers t WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(bridge, refs::deposit(&at(10, 0)));
    assert_eq!(destination, DESTINATION_ACCOUNT);
    assert_eq!(
        (refund_network.as_str(), refund_to.as_str()),
        (ORIGIN, REFUND)
    );
    assert_eq!(answers, 0, "REQ-IN-10: no request");
    // It begins at Source-Final, with the milestone of its arrival's finality: never Received or
    // Accepted, which are a request's.
    assert_eq!(w.state(id).await, ("Source-Final".into(), None));
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM transfers WHERE id = {id} AND source_final_block_number = 12"
        ))
        .await,
        1
    );
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM transitions WHERE transfer_id = {id} \
               AND (from_state IN ('Received', 'Accepted') OR to_state = 'Accepted')"
        ))
        .await,
        0
    );

    let (_, payload) = order(&w.admin, id, 1).await.expect("the mint order");
    assert_eq!(payload["amount"], "5000000");
    assert_eq!(payload["order"]["to"], DESTINATION_ACCOUNT);
    deliver(&w, id, 1, t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    let minted = observe(
        &w.admin,
        ISSUING,
        40,
        0,
        ISSUING_ASSET,
        5_000_000,
        None,
        Some(DESTINATION_ACCOUNT),
        Some(&reference_of(&w.admin, id).await),
        true,
        json!({"event": "Minted"}),
    )
    .await;
    decide(&w.admin, minted, "final", 42).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await, ("Completed".into(), None));
    assert_eq!(returns(&w).await, 0);
    let item = coordinate_key(&at(10, 0));
    assert_eq!(balance(&w.admin, ASSET, "owed-mint", Some(&item)).await, 0);
}

/// `AC-A6.3`, `REQ-IN-11`, `REQ-GV-1`: a pause on the route parks a directed deposit in
/// `CND-Paused` with nothing ordered and nothing returned; it resumes by itself when the pause
/// lifts.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_3_a_pause_parks_a_directed_deposit_and_nothing_is_returned(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    exec(
        &w.admin,
        &format!("INSERT INTO pauses (asset) VALUES ('{ASSET}')"),
    )
    .await;
    final_directed(&w.admin, 10, 0, 5_000_000).await;
    w.settle(t0()).await;
    let (id, _) = bound(&w, 10, 0).await.expect("the deposit");
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-Paused".into()))
    );
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
    assert_eq!(returns(&w).await, 0);
    exec(&w.admin, "DELETE FROM pauses").await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await, ("Source-Final".into(), None));
    assert!(order(&w.admin, id, 1).await.is_some());
    assert_eq!(returns(&w).await, 0);
}

/// `AC-A6.3`, `REQ-IN-11` (amendment 0028): a hold of an asset the reserve check has not seen parks
/// a directed deposit as a pause does, and nothing is returned; it resumes once the check holds.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_3_a_hold_parks_a_directed_deposit_and_nothing_is_returned(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::unseen(options, connect).await;
    final_directed(&w.admin, 10, 0, 5_000_000).await;
    w.settle(t0()).await;
    let (id, _) = bound(&w, 10, 0).await.expect("the deposit");
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-Paused".into()))
    );
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
    assert_eq!(returns(&w).await, 0);
    seen(&w.admin).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await, ("Source-Final".into(), None));
    assert!(order(&w.admin, id, 1).await.is_some());
    assert_eq!(returns(&w).await, 0);
}

/// `AC-A6.3`, `REQ-IN-11`, `REQ-LM-3`: the issuing network's mint window refusing the mint parks a
/// directed deposit in `CND-MintWindow`, never a failure or a refund; a new order that passes
/// resumes it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_3_the_mint_window_parks_a_directed_deposit_and_nothing_is_returned(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    final_directed(&w.admin, 10, 0, 5_000_000).await;
    w.cycle(t0()).await;
    let (id, _) = bound(&w, 10, 0).await.expect("the deposit");
    let (first, _) = order(&w.admin, id, 1).await.expect("the mint order");
    prepare(&w.admin, first, Some("AboveWindow")).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-MintWindow".into()))
    );
    // Long past the route's deadline: the mint window clears by itself, so no review either.
    head(&w.admin, ISSUING, 12 + BUDGET + 100).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-MintWindow".into()))
    );
    assert_eq!(returns(&w).await, 0);
    let (second, _) = order(&w.admin, id, 1).await.unwrap();
    assert_ne!(first, second);
    prepare(&w.admin, second, None).await;
    w.cycle(t0()).await;
    let (s, _) = signing_intent(&w.admin, id, 1)
        .await
        .expect("signing intent");
    sign(&w, s).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await, ("Delivering".into(), None));
    assert_eq!(returns(&w).await, 0);
}

/// What `route_usage_locked()` says the deposit route's window of `seconds` has used, as the
/// ingress reads it.
async fn window_used(w: &World, seconds: i64) -> String {
    scalar(
        &w.admin,
        &format!(
            "SELECT used FROM route_usage_locked('{ASSET}', '{ORIGIN}', 'deposit', {seconds})"
        ),
    )
    .await
}

/// `AC-A6.3`, `REQ-IN-11`, `REQ-LM-1` (amendment 0029, Open 1, ruled: wait; Open 5: no separate
/// share): a directed deposit over what the route's window has left waits in `CND-DepositWindow`,
/// with nothing ordered and nothing returned, even past its deadline; one behind it by coordinate
/// waits behind it although it would fit alone (first come first served); while they wait their
/// amounts are a claim on the window, so a requester's deposit sees them as used. As the window
/// rolls over both resume by themselves, in coordinate order, and are minted.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_3_cnd_deposit_window_waits_first_come_first_served_and_resumes_as_it_rolls_over(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    exec(
        &w.admin,
        &format!(
            "UPDATE routes SET maximum = 6000000, window_limit = 10000000, \
                 window_seconds = 86400 \
             WHERE asset = '{ASSET}' AND network = '{ORIGIN}' AND direction = 'deposit'"
        ),
    )
    .await;
    final_directed(&w.admin, 10, 0, 6_000_000).await;
    final_directed(&w.admin, 11, 0, 6_000_000).await;
    final_directed(&w.admin, 12, 0, 1_000_000).await;
    w.cycle(t0()).await;
    let (a, _) = bound(&w, 10, 0).await.expect("deposit a");
    let (b, _) = bound(&w, 11, 0).await.expect("deposit b");
    let (c, _) = bound(&w, 12, 0).await.expect("deposit c");
    assert_eq!(w.state(a).await, ("Source-Final".into(), None));
    for id in [b, c] {
        assert_eq!(
            w.state(id).await,
            ("Source-Final".into(), Some("CND-DepositWindow".into())),
            "deposit {id} waits for the window"
        );
    }
    assert!(order(&w.admin, a, 1).await.is_some());
    // c alone would fit (6 + 1 <= 10), but b is ahead of it by coordinate.
    assert_eq!(
        window_used(&w, 86400).await,
        "13000000",
        "the waiting are a claim"
    );
    // Past their deadline, they still wait: the window resumes by itself, never review or refund.
    head(&w.admin, ISSUING, 14 + BUDGET + 100).await;
    w.settle(t0()).await;
    for id in [b, c] {
        assert_eq!(
            w.state(id).await,
            ("Source-Final".into(), Some("CND-DepositWindow".into()))
        );
        assert!(order(&w.admin, id, 1).await.is_none());
    }
    assert_eq!(returns(&w).await, 0);

    // The window rolls over: the route's window is made one second long, so a's share leaves it
    // a second on, and b, then c, take theirs (the requests' reserve is off, as it is on every route
    // until `d-f018-window-reserve` is ruled; amendment 0031).
    exec(
        &w.admin,
        &format!(
            "UPDATE routes SET window_seconds = 1 \
             WHERE asset = '{ASSET}' AND network = '{ORIGIN}' AND direction = 'deposit'"
        ),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    w.cycle(t0()).await;
    for id in [b, c] {
        assert_eq!(w.state(id).await, ("Source-Final".into(), None));
    }
    w.cycle(t0()).await;
    assert!(order(&w.admin, b, 1).await.is_some());
    assert!(order(&w.admin, c, 1).await.is_some());
    // Each counted once over the day: a from when it was made, b and c from when they left.
    assert_eq!(
        window_used(&w, 86400).await,
        "13000000",
        "each share counted once"
    );
    assert_eq!(returns(&w).await, 0);
}

// ---------------------------------------------------------------------------------------------
// T-018-08: the return of a never-mintable directed arrival.

/// `AC-A6.4`, `REQ-IN-11`, `REQ-RF-2`, `REQ-RF-3`, `INV-2` (Open 3, ruled: no second approver): an
/// arrival above the route's maximum can never be minted under its current declaration. bezent
/// returns it itself: a refund of an unassociated arrival with the coordinate's `bz-u-…` reference,
/// to the refund address the caller named and to no one else, accepted with no manual action, and
/// paid out with its own on-chain reference. No deposit is ever made of it, the book posts the
/// return against the arrival's owed mint, and a second return is refused.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_4_a_never_mintable_directed_arrival_is_returned_once_to_its_refund_address(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let now = t0() + Duration::minutes(5);
    final_directed(&w.admin, 10, 0, 2_000_000_000).await;
    w.cycle(now).await;
    let (id, kind) = bound(&w, 10, 0).await.expect("the return");
    assert_eq!(kind, "refund");
    let (bridge, network, destination, refund_of): (String, String, String, Option<i64>) =
        sqlx::query_as(
            "SELECT bridge_reference, destination_network, destination, refund_of \
             FROM transfers WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&w.admin)
        .await
        .unwrap();
    assert_eq!(bridge, refs::unassociated_refund(&at(10, 0)));
    assert_eq!((network.as_str(), destination.as_str()), (ORIGIN, REFUND));
    assert_eq!(refund_of, None);
    assert_eq!(
        w.count("SELECT count(*) FROM transfers WHERE kind = 'deposit'")
            .await,
        0
    );
    assert_eq!(w.count("SELECT count(*) FROM manual_actions").await, 0);
    let why: String = scalar(
        &w.admin,
        &format!(
            "SELECT payload ->> 'reason' FROM event_log WHERE kind = 'directed-return' \
               AND transfer_id = {id}"
        ),
    )
    .await;
    assert_eq!(why, "above-maximum");

    w.cycle(now).await;
    assert_eq!(w.state(id).await.0, "Source-Final");
    let (_, payload) = order(&w.admin, id, 1).await.expect("the payout order");
    assert_eq!(payload["amount"], "2000000000");
    assert_eq!(payload["order"]["to"], REFUND);
    deliver(&w, id, 1, now).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    let reference = reference_of(&w.admin, id).await;
    assert_eq!(
        reference,
        refs::unassociated_refund_reference(&at(10, 0)).to_vec()
    );
    paid_out(&w, 60, 2_000_000_000, REFUND, &reference).await;
    w.settle(now).await;
    assert_eq!(w.state(id).await, ("Completed".into(), None));

    // The book: the arrival was owed mint and reserve; the return leaves both.
    let item = coordinate_key(&at(10, 0));
    assert_eq!(balance(&w.admin, ASSET, "owed-mint", Some(&item)).await, 0);
    assert_eq!(balance(&w.admin, ASSET, "reserve", None).await, 0);
    assert_eq!(
        w.count("SELECT count(*) FROM book_entries WHERE kind = 'unassociated-refund'")
            .await,
        1
    );

    // Once: the core makes nothing more of it, the store refuses a second return and the
    // arrival's deposit, whoever asks (INV-2).
    w.settle(now).await;
    assert_eq!(w.count("SELECT count(*) FROM transfers").await, 1);
    let deposit: Option<i64> = sqlx::query_scalar(&format!(
        "SELECT directed_deposit('{ORIGIN}', 10, '\\x{}', 'log', 0)",
        hex::encode(hash(ORIGIN, 10))
    ))
    .fetch_one(w.core.pool())
    .await
    .unwrap();
    assert_eq!(deposit, None, "closed to minting");
    let again = sqlx::query(&format!(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index) \
         VALUES ('{bridge}', 'directed:again', 'refund', '{ASSET}', 2000000000, '{ORIGIN}', '{REFUND}', \
             'Received', '{ORIGIN}', 10, '\\x{}', 'log', 0)",
        hex::encode(hash(ORIGIN, 10))
    ))
    .execute(w.core.pool())
    .await
    .expect_err("a second return");
    assert!(
        again
            .as_database_error()
            .and_then(|e| e.code())
            .is_some_and(|c| c == "23505"),
        "{again}"
    );
}

/// `AC-A6.4`, `REQ-IN-11`, `REQ-IN-5` (PLAN `DEC-15`): a directed arrival whose recipient is a
/// declared ledger contract account is returned, not minted; the arrival next to it, to a recipient
/// off the list, is minted. A recipient off the list is never returned.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_4_a_refused_recipient_is_returned_not_minted(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    exec(
        &w.admin,
        &format!(
            "INSERT INTO ledger_accounts (network, address, reason) \
             VALUES ('{ISSUING}', '{DESTINATION}', 'a ledger''s contract account')"
        ),
    )
    .await;
    let refused = directed_to(&w.admin, 10, 0, 5_000_000, DESTINATION_ACCOUNT).await;
    decide(&w.admin, refused, "final", 12).await;
    let clear = directed_to(&w.admin, 11, 0, 5_000_000, REDEEMER).await;
    decide(&w.admin, clear, "final", 13).await;
    w.cycle(t0()).await;
    let (r, kind) = bound(&w, 10, 0).await.expect("the return");
    assert_eq!(kind, "refund");
    let why: String = scalar(
        &w.admin,
        &format!(
            "SELECT payload ->> 'reason' FROM event_log WHERE kind = 'directed-return' \
               AND transfer_id = {r}"
        ),
    )
    .await;
    assert_eq!(why, "recipient-refused");
    let (d, kind) = bound(&w, 11, 0).await.expect("the deposit");
    assert_eq!(kind, "deposit");
    assert!(order(&w.admin, d, 1).await.is_some());
    assert_eq!(returns(&w).await, 1);
}

// ---------------------------------------------------------------------------------------------
// T-018-09: the review edge (Open 2, ruled yes).

/// A directed deposit whose mint the issuing network refuses for its destination, past its
/// deadline: in review, entered from `Source-Final` holding `CND-DestinationRefused`, its first
/// attempt ordered and never signed. Returns it and the final head (`ISSUING`, block) past the
/// deadline.
async fn directed_in_review(w: &World) -> (i64, i64) {
    final_directed(&w.admin, 10, 0, 5_000_000).await;
    w.cycle(t0()).await;
    let (id, _) = bound(w, 10, 0).await.expect("the deposit");
    let (o, _) = order(&w.admin, id, 1).await.expect("the mint order");
    prepare(&w.admin, o, Some("Kreivo(Assets(CannotMint))")).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-DestinationRefused".into()))
    );
    let past = 12 + BUDGET + 1;
    head(&w.admin, ISSUING, past).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Needs-Review".into(), Some("CND-DestinationRefused".into()))
    );
    (id, past)
}

/// `REQ-RV-2`, `REQ-GV-5`, `INV-8`, `REQ-RF-2`, `REQ-IN-11` (amendment 0029, A5, Open 2): a directed
/// deposit that can never receive leaves review into `Refunding` only by a recorded resolution with
/// its second approver. Its mint, ordered and never signed, is closed for good, and its refund is a
/// transfer of its own, paid as its arrival's return is: the `bz-u-…` reference, to the refund
/// address the caller named, on the arrival's network. Once paid, the deposit is `Refunded` and the
/// book has left the arrival's owed mint.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rv_2_a_directed_deposit_that_cannot_receive_is_refunded_with_a_second_approver(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, past) = directed_in_review(&w).await;
    // Review M1 of PR #136: a resolution resting on just any final head of the issuing network is
    // refused, approved or not; the exit's evidence is the network's recorded refusal.
    let elsewhere = resolution(&w, id, "Refunding", ISSUING, past, 0).await;
    approve_as(&w, 1, elsewhere).await.unwrap();
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(evidence_refused(&w, elsewhere).await, 1);
    assert_eq!(returns(&w).await, 0);
    // The store refuses that edge too, whoever writes it: with the unsigned attempt cancelled
    // under that resolution (in a transaction rolled back after), the edge itself is refused for
    // its evidence.
    let mut tx = w.admin.begin().await.unwrap();
    sqlx::query(&format!(
        "INSERT INTO unsigned_attempt_cancellations (transfer_id, attempt, manual_action_id) \
         VALUES ({id}, 1, {elsewhere})"
    ))
    .execute(&mut *tx)
    .await
    .expect("the cancellation under the approved resolution");
    let refused = sqlx::query(&format!(
        "INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
             block_hash, block_time, manual_action_id) \
         SELECT {id}, 'Needs-Review', 'Refunding', network, block_number, block_hash, block_time, id \
         FROM manual_actions WHERE id = {elsewhere}"
    ))
    .execute(&mut *tx)
    .await
    .expect_err("the edge on another head");
    tx.rollback().await.unwrap();
    assert!(
        refused
            .as_database_error()
            .is_some_and(|e| e.code().as_deref() == Some("BZ001")
                && e.message().contains("recorded refusal")),
        "{refused}"
    );

    let action = resolution_on_refusal(&w, id, 0).await;
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await.0,
        "Needs-Review",
        "REQ-GV-5: one approver"
    );
    assert_eq!(returns(&w).await, 0);

    approve_as(&w, 1, action).await.unwrap();
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Refunding");
    let ran: Option<i64> = scalar(
        &w.admin,
        &format!(
            "SELECT manual_action_id FROM transitions WHERE transfer_id = {id} \
               AND to_state = 'Refunding'"
        ),
    )
    .await;
    assert_eq!(ran, Some(action));
    // Closed to minting: the unsigned attempt is cancelled, and nothing more is recorded for it.
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM unsigned_attempt_cancellations \
               WHERE transfer_id = {id} AND attempt = 1 AND manual_action_id = {action}"
        ))
        .await,
        1
    );
    let mints = w
        .count(&format!(
            "SELECT count(*) FROM intents WHERE transfer_id = {id}"
        ))
        .await;
    let (refund, bridge, network, destination): (i64, String, String, String) = sqlx::query_as(
        "SELECT id, bridge_reference, destination_network, destination FROM transfers \
         WHERE refund_of = $1",
    )
    .bind(id)
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(bridge, refs::unassociated_refund(&at(10, 0)));
    assert_eq!((network.as_str(), destination.as_str()), (ORIGIN, REFUND));

    w.cycle(t0()).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(refund).await.0, "Source-Final");
    w.cycle(t0()).await;
    let (_, payload) = order(&w.admin, refund, 1).await.expect("the payout order");
    assert_eq!(payload["amount"], "5000000");
    assert_eq!(payload["order"]["to"], REFUND);
    deliver(&w, refund, 1, t0()).await;
    assert_eq!(w.state(refund).await.0, "Delivering");
    let reference = reference_of(&w.admin, refund).await;
    assert_eq!(
        reference,
        refs::unassociated_refund_reference(&at(10, 0)).to_vec()
    );
    paid_out(&w, 60, 5_000_000, REFUND, &reference).await;
    w.settle(t0()).await;
    assert_eq!(w.state(refund).await, ("Completed".into(), None));
    assert_eq!(w.state(id).await, ("Refunded".into(), None));
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM intents WHERE transfer_id = {id}"
        ))
        .await,
        mints,
        "no mint after the refund"
    );
    let item = coordinate_key(&at(10, 0));
    assert_eq!(balance(&w.admin, ASSET, "owed-mint", Some(&item)).await, 0);
    assert_eq!(balance(&w.admin, ASSET, "reserve", None).await, 0);
}

/// `REQ-RV-2`, `REQ-TR-6` (amendment 0029, Open 2: "No other deposit gains it"): a requester's
/// deposit in the same review, with an approved resolution into `Refunding`, is not refunded: the
/// core refuses the resolution, and the store refuses the edge whoever writes it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rv_2_no_other_deposit_gains_the_directed_review_exit(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let a = arrival(&w.admin, 10, 5_000).await;
    decide(&w.admin, a, "final", 12).await;
    let id = deposit(&w.admin, 10, 5_000).await;
    w.cycle(t0()).await;
    w.cycle(t0()).await;
    let (o, _) = order(&w.admin, id, 1).await.expect("the mint order");
    prepare(&w.admin, o, Some("Kreivo(Assets(CannotMint))")).await;
    w.cycle(t0()).await;
    let past = 12 + BUDGET + 1;
    head(&w.admin, ISSUING, past).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Needs-Review".into(), Some("CND-DestinationRefused".into()))
    );
    let action = resolution(&w, id, "Refunding", ISSUING, past, 0).await;
    approve_as(&w, 1, action).await.unwrap();
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM event_log WHERE kind = 'review-evidence-refused' \
               AND payload ->> 'manual_action_id' = '{action}'"
        ))
        .await,
        1
    );
    assert_eq!(returns(&w).await, 0);
    let open: bool = scalar(
        &w.admin,
        &format!("SELECT directed_review_refund_open({id})"),
    )
    .await;
    assert!(!open);
    let refused = sqlx::query(&format!(
        "INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
             block_hash, block_time, manual_action_id) \
         SELECT {id}, 'Needs-Review', 'Refunding', network, block_number, block_hash, block_time, id \
         FROM manual_actions WHERE id = {action}"
    ))
    .execute(&w.admin)
    .await
    .expect_err("the edge, written by the owner");
    // Refused by the store (its first guard to see it names INV-8: the requester's mint attempt,
    // ordered and never signed, is not proven, and no resolution may cancel it).
    assert_eq!(
        refused
            .as_database_error()
            .and_then(|e| e.code())
            .as_deref(),
        Some("BZ001"),
        "{refused}"
    );
    assert_eq!(w.state(id).await.0, "Needs-Review");
}

/// `INV-8`, `REQ-RF-1`, `REQ-RV-2` (amendment 0029, Open 2: "with no mint ever signed"): a directed
/// deposit one of whose mint intents has a signature does not take the review exit, approved or not.
/// The signature exists outside the store's reach to prove; nothing is refunded on it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_8_a_directed_deposit_whose_mint_was_signed_never_takes_the_review_exit(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    final_directed(&w.admin, 10, 0, 5_000_000).await;
    w.cycle(t0()).await;
    let (id, _) = bound(&w, 10, 0).await.expect("the deposit");
    let (o, _) = order(&w.admin, id, 1).await.expect("the mint order");
    prepare(&w.admin, o, None).await;
    w.cycle(t0()).await;
    let (s, _) = signing_intent(&w.admin, id, 1)
        .await
        .expect("signing intent");
    // Signed, and the signed bytes not persisted yet: still Source-Final.
    assert!(matches!(
        w.signer.sign(s).await,
        bezent_signer::Outcome::Signed(_)
    ));
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Source-Final");
    // A review entered from Source-Final for CND-DestinationRefused, as the owner writes it.
    exec(
        &w.admin,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state, condition) \
             VALUES ({id}, 'Source-Final', 'Needs-Review', 'CND-DestinationRefused')"
        ),
    )
    .await;
    let past = 12 + BUDGET + 1;
    head(&w.admin, ISSUING, past).await;
    let action = resolution(&w, id, "Refunding", ISSUING, past, 0).await;
    approve_as(&w, 1, action).await.unwrap();
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(returns(&w).await, 0);
    assert_eq!(
        w.count("SELECT count(*) FROM unsigned_attempt_cancellations")
            .await,
        0
    );
}

// ---------------------------------------------------------------------------------------------
// Review M1 of PR #136: no refund on unknown (REQ-RF-1, guardrail 4).

/// `REQ-RF-1`, `REQ-IN-5`, `REQ-IN-9` (review M1 of PR #136): a directed deposit whose destination
/// the core cannot tell is clear (the declared list does not read back) goes to review holding
/// `CND-DestinationRefused`, with nothing ordered. That is an unknown, not the issuing network's
/// refusal: an approved resolution into `Refunding` is refused, and nothing is refunded.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_1_a_directed_deposit_reviewed_for_an_unknown_is_not_refunded(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let typo = DESTINATION.replace('Q', "R");
    exec(
        &w.admin,
        &format!(
            "INSERT INTO ledger_accounts (network, address, reason) \
             VALUES ('{ISSUING}', '{typo}', 'a ledger holds value here')"
        ),
    )
    .await;
    final_directed(&w.admin, 10, 0, 5_000_000).await;
    w.settle(t0()).await;
    let (id, kind) = bound(&w, 10, 0).await.expect("the deposit");
    assert_eq!(kind, "deposit");
    assert_eq!(
        w.state(id).await,
        ("Needs-Review".into(), Some("CND-DestinationRefused".into()))
    );
    assert!(order(&w.admin, id, 1).await.is_none());
    let open: bool = scalar(
        &w.admin,
        &format!("SELECT directed_review_refund_open({id})"),
    )
    .await;
    assert!(!open);
    let past = 12 + BUDGET + 1;
    head(&w.admin, ISSUING, past).await;
    let action = resolution(&w, id, "Refunding", ISSUING, past, 0).await;
    approve_as(&w, 1, action).await.unwrap();
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(evidence_refused(&w, action).await, 1);
    assert_eq!(returns(&w).await, 0);
}

/// `REQ-RF-1`, `REQ-IN-9` (review M1 of PR #136): even when the issuing network refused the
/// destination first, a review the deposit entered because the core could not read the declared
/// list (no coordinate, before the condition's deadline) is a review of an unknown. Resting on the
/// recorded refusal itself, its resolution into `Refunding` is still refused.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_1_a_refusal_then_an_unknown_review_is_not_refunded(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    final_directed(&w.admin, 10, 0, 5_000_000).await;
    w.cycle(t0()).await;
    let (id, _) = bound(&w, 10, 0).await.expect("the deposit");
    let (o, _) = order(&w.admin, id, 1).await.expect("the mint order");
    prepare(&w.admin, o, Some("Kreivo(Assets(CannotMint))")).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-DestinationRefused".into()))
    );
    let typo = DESTINATION.replace('Q', "R");
    exec(
        &w.admin,
        &format!(
            "INSERT INTO ledger_accounts (network, address, reason) \
             VALUES ('{ISSUING}', '{typo}', 'a ledger holds value here')"
        ),
    )
    .await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Needs-Review".into(), Some("CND-DestinationRefused".into()))
    );
    let action = resolution_on_refusal(&w, id, 0).await;
    approve_as(&w, 1, action).await.unwrap();
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(evidence_refused(&w, action).await, 1);
    assert_eq!(returns(&w).await, 0);
    assert_eq!(
        w.count("SELECT count(*) FROM unsigned_attempt_cancellations")
            .await,
        0
    );
}

/// A directed deposit whose first dry run the issuing network refused with `reason`, which is not
/// the refusal of its destination, taken to its deadline, with an approved resolution into
/// `Refunding` resting on that dry run: it is never refunded (review M1 of PR #136; `REQ-RF-1`,
/// guardrail 4).
async fn an_unknown_refusal_never_opens_the_exit(w: &World, reason: &str) {
    an_unknown_refusal_with_asset_never_opens_the_exit(w, reason, None).await;
}

/// As [`an_unknown_refusal_never_opens_the_exit`], the dry run recorded with the asset of `status`
/// at its head, if given (`common::prepare_refused_with_asset`).
async fn an_unknown_refusal_with_asset_never_opens_the_exit(
    w: &World,
    reason: &str,
    status: Option<serde_json::Value>,
) {
    final_directed(&w.admin, 10, 0, 5_000_000).await;
    w.cycle(t0()).await;
    let (id, _) = bound(w, 10, 0).await.expect("the deposit");
    let (o, _) = order(&w.admin, id, 1).await.expect("the mint order");
    // The first final head after the attempt was ordered: the order starts to age (§5.4).
    head(&w.admin, ISSUING, 13).await;
    match status {
        Some(status) => {
            prepare_refused_with_asset(&w.admin, o, reason, status).await;
        }
        None => {
            prepare(&w.admin, o, Some(reason)).await;
        }
    }
    w.cycle(t0()).await;
    let (state, condition) = w.state(id).await;
    assert_eq!(state, "Source-Final", "{reason}");
    assert_ne!(
        condition.as_deref(),
        Some("CND-DestinationRefused"),
        "{reason}: no refusal of the destination"
    );
    let past = 13 + BUDGET + 1;
    head(&w.admin, ISSUING, past).await;
    w.cycle(t0()).await;
    let (state, condition) = w.state(id).await;
    assert_eq!(
        state, "Needs-Review",
        "{reason}: the refusal that names nothing, at its deadline"
    );
    assert_ne!(condition.as_deref(), Some("CND-DestinationRefused"));
    let open: bool = scalar(
        &w.admin,
        &format!("SELECT directed_review_refund_open({id})"),
    )
    .await;
    assert!(!open, "{reason}");
    let action = resolution_on_refusal(w, id, 0).await;
    approve_as(w, 1, action).await.unwrap();
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review", "{reason}");
    assert_eq!(evidence_refused(w, action).await, 1, "{reason}");
    assert_eq!(returns(w).await, 0, "{reason}");
}

/// `REQ-RF-1`, `REQ-IN-9`, guardrail 4 (review M1 of PR #136): the contract's community not
/// authorised by the issuing network is no refusal of the destination.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_1_a_kreivo_not_authorised_refusal_never_opens_the_exit(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    an_unknown_refusal_never_opens_the_exit(&w, "Kreivo(Assets(NotAuthorised))").await;
}

/// `REQ-RF-1`, `REQ-IN-9`, guardrail 4 (review M1 of PR #136): an error the issuing network does not
/// name is an unknown, and never refunds.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_1_a_kreivo_unknown_error_never_opens_the_exit(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    an_unknown_refusal_never_opens_the_exit(&w, "Kreivo(UnknownError)").await;
}

/// `REQ-RF-1`, `REQ-IN-8`, `REQ-IN-9` (review of PR #136): Kreivo answers `CannotMint` for the whole
/// asset's errors too. A refusal to mint while the asset is frozen, as the endpoint read it at the
/// dry run's head, is no refusal of the destination: no `CND-DestinationRefused`, the refusal that
/// names nothing at its deadline, and no exit. (A `Live` asset's refusal is the exit's evidence:
/// `req_rv_2_a_directed_deposit_that_cannot_receive_is_refunded_with_a_second_approver`.)
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_1_a_refusal_to_mint_a_frozen_asset_never_opens_the_exit(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    an_unknown_refusal_with_asset_never_opens_the_exit(
        &w,
        "Kreivo(Assets(CannotMint))",
        Some(json!("Frozen")),
    )
    .await;
}

/// `REQ-RF-1` (review of PR #136): a refusal to mint whose asset status the endpoint could not read
/// is an unknown, and never opens the exit.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_1_a_refusal_to_mint_an_asset_of_unknown_status_never_opens_the_exit(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    an_unknown_refusal_with_asset_never_opens_the_exit(
        &w,
        "Kreivo(Assets(CannotMint))",
        Some(serde_json::Value::Null),
    )
    .await;
}

/// `REQ-RF-1`, `REQ-IN-9` (review of PR #136): a refusal of the destination, then the asset frozen
/// and the next order's mint refused for it. The frozen asset's refusal does not keep the
/// destination's hold, so the deposit's review at its deadline is not the destination's, and an
/// approved resolution resting on the first refusal is refused.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_1_a_refusal_then_a_frozen_asset_drops_the_destination_hold(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    final_directed(&w.admin, 10, 0, 5_000_000).await;
    w.cycle(t0()).await;
    let (id, _) = bound(&w, 10, 0).await.expect("the deposit");
    let (first, _) = order(&w.admin, id, 1).await.expect("the mint order");
    // The first final head after the attempt was ordered: the order starts to age (§5.4).
    head(&w.admin, ISSUING, 13).await;
    prepare(&w.admin, first, Some("Kreivo(Assets(CannotMint))")).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-DestinationRefused".into()))
    );
    let (second, _) = order(&w.admin, id, 1).await.expect("a new order");
    assert_ne!(first, second);
    prepare_refused_with_asset(
        &w.admin,
        second,
        "Kreivo(Assets(CannotMint))",
        json!("Frozen"),
    )
    .await;
    w.cycle(t0()).await;
    let (state, condition) = w.state(id).await;
    assert_eq!(state, "Source-Final");
    assert_ne!(condition.as_deref(), Some("CND-DestinationRefused"));
    let past = 13 + BUDGET + 1;
    head(&w.admin, ISSUING, past).await;
    w.cycle(t0()).await;
    let (state, condition) = w.state(id).await;
    assert_eq!(state, "Needs-Review");
    assert_ne!(condition.as_deref(), Some("CND-DestinationRefused"));
    let open: bool = scalar(
        &w.admin,
        &format!("SELECT directed_review_refund_open({id})"),
    )
    .await;
    assert!(!open);
    let action = resolution_on_refusal(&w, id, 0).await;
    approve_as(&w, 1, action).await.unwrap();
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(evidence_refused(&w, action).await, 1);
    assert_eq!(returns(&w).await, 0);
}

/// `REQ-RF-1`, `REQ-IN-9` (review L-a of PR #136): a refusal of the destination, then a dry run that
/// passed, then the review's resolution resting on the refusal: the refusal is stale, what the
/// network says now is that the mint applies, and nothing is refunded.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_1_a_refusal_then_a_passing_dry_run_is_not_refunded(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, _) = directed_in_review(&w).await;
    let (o, _) = order(&w.admin, id, 1).await.expect("the mint order");
    prepare(&w.admin, o, None).await;
    let open: bool = scalar(
        &w.admin,
        &format!("SELECT directed_review_refund_open({id})"),
    )
    .await;
    assert!(!open);
    let action = resolution_on_refusal(&w, id, 0).await;
    approve_as(&w, 1, action).await.unwrap();
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(evidence_refused(&w, action).await, 1);
    assert_eq!(returns(&w).await, 0);
}

/// `REQ-RV-2`, `REQ-RF-1` (review L-b of PR #136): the directed exit's evidence names the block time
/// the endpoint read from the issuing network and recorded with the refused dry run. A resolution on
/// the right block with another time is refused by the core and, whoever writes the edge, by the
/// store; the one naming the recorded time runs.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rv_2_a_directed_exit_naming_another_block_time_is_refused(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, _) = directed_in_review(&w).await;
    let typed = resolution_on_refusal_at(&w, id, 0, Some(t0() + Duration::hours(1))).await;
    approve_as(&w, 1, typed).await.unwrap();
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(evidence_refused(&w, typed).await, 1);
    assert_eq!(returns(&w).await, 0);
    let mut tx = w.admin.begin().await.unwrap();
    sqlx::query(&format!(
        "INSERT INTO unsigned_attempt_cancellations (transfer_id, attempt, manual_action_id) \
         VALUES ({id}, 1, {typed})"
    ))
    .execute(&mut *tx)
    .await
    .expect("the cancellation under the approved resolution");
    let refused = sqlx::query(&format!(
        "INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
             block_hash, block_time, manual_action_id) \
         SELECT {id}, 'Needs-Review', 'Refunding', network, block_number, block_hash, block_time, id \
         FROM manual_actions WHERE id = {typed}"
    ))
    .execute(&mut *tx)
    .await
    .expect_err("the edge at another block time");
    tx.rollback().await.unwrap();
    assert!(
        refused
            .as_database_error()
            .is_some_and(|e| e.code().as_deref() == Some("BZ001")
                && e.message().contains("recorded refusal")),
        "{refused}"
    );

    let action = resolution_on_refusal(&w, id, 0).await;
    approve_as(&w, 1, action).await.unwrap();
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Refunding");
}

/// `REQ-IN-10`, `AC-A6.2`, `INV-2` (review, info, of PR #136): a directed arrival bound by a row that
/// is not bezent's own, here a requester's deposit bound to the coordinate before the arrival was
/// observed as directed, is never decided by the intake and is no unassociated arrival either. The
/// core pages `directed-binding-alert` once for it, and changes nothing.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_10_a_directed_arrival_bound_by_another_row_pages(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let requested = deposit(&w.admin, 10, 5_000_000).await;
    final_directed(&w.admin, 10, 0, 5_000_000).await;
    // The requester's deposit cannot become final on a directed arrival (the store refuses it,
    // review H1 of PR #125), so its step fails every cycle; the page comes all the same.
    for _ in 0..2 {
        let report = w.core.cycle(t0()).await.expect("cycle");
        assert_eq!(report.failed, 1, "{report:?}");
        assert_eq!(report.directed, 0, "{report:?}");
    }
    let pages = w
        .count(&format!(
            "SELECT count(*) FROM event_log WHERE kind = 'directed-binding-alert' \
               AND transfer_id = {requested}"
        ))
        .await;
    assert_eq!(pages, 1, "once");
    assert_eq!(bound(&w, 10, 0).await.map(|b| b.0), Some(requested));
    assert_eq!(returns(&w).await, 0);
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM intents WHERE transfer_id = {requested}"
        ))
        .await,
        0,
        "nothing minted for it"
    );
    // bezent's own deposit of another directed arrival does not page.
    final_directed(&w.admin, 20, 0, 5_000_000).await;
    let report = w.core.cycle(t0()).await.expect("cycle");
    assert_eq!(report.directed, 1, "{report:?}");
    let (own, _) = bound(&w, 20, 0).await.expect("the directed deposit");
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM event_log WHERE kind = 'directed-binding-alert' \
               AND transfer_id = {own}"
        ))
        .await,
        0
    );
}

// ---------------------------------------------------------------------------------------------
// Reviews L2, L3 and L5 of PR #136.

/// A token at the reserve that is admitted for no bridged asset.
const UNADMITTED: &str = "0x00000000000000000000000000000000000000bb";

/// `REQ-AS-11`, `REQ-IN-11` (review L3 of PR #136, as review M3 of PR #139 now reads it): a directed
/// arrival of a token admitted for no asset can never be minted, but no directing code is declared
/// for such a token either (a declaration names an admitted one), so only declared directing code
/// directs and it is neither minted nor returned: it waits, final and unbound, and the core says so.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_as_11_a_directed_arrival_of_an_unadmitted_token_waits_undeclared(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    declare_directing(&w.admin).await;
    let arrival = observe(
        &w.admin,
        ORIGIN,
        10,
        0,
        UNADMITTED,
        7_000,
        Some(FUNDER),
        Some(RESERVE),
        None,
        true,
        json!({"event": "DirectedArrival", "directing": DIRECTING, "direction_log": 1,
               "recipient": DESTINATION_ACCOUNT, "refund": REFUND}),
    )
    .await;
    decide(&w.admin, arrival, "final", 12).await;
    w.cycle(t0()).await;
    w.settle(t0()).await;
    assert_eq!(bound(&w, 10, 0).await, None, "neither minted nor returned");
    assert_eq!(w.count("SELECT count(*) FROM transfers").await, 0);
    assert_eq!(
        w.count("SELECT count(*) FROM event_log WHERE kind = 'directed-return'")
            .await,
        0
    );
}

/// `REQ-AS-11` (a MUST; review M3 of PR #139): only declared directing code directs. A final
/// directed arrival whose directing contract is not declared in `directing_code`, or is declared from
/// a later block than its own, is neither made a deposit nor returned, however mintable or returnable
/// it is otherwise; once a declaration covers it, the next cycle decides it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_as_11_an_undeclared_directing_contracts_arrival_is_never_minted_or_returned(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    const UNDECLARED: &str = "0x00000000000000000000000000000000000000dd";
    let w = World::new(options, connect).await;
    let through = |block: i64, amount: u128| {
        let admin = w.admin.clone();
        async move {
            let id = observe(
                &admin,
                ORIGIN,
                block,
                0,
                TOKEN,
                amount,
                Some(FUNDER),
                Some(RESERVE),
                None,
                true,
                json!({"event": "DirectedArrival", "directing": UNDECLARED, "direction_log": 1,
                       "recipient": DESTINATION_ACCOUNT, "refund": REFUND}),
            )
            .await;
            decide(&admin, id, "final", block + 2).await;
        }
    };
    // One that would be minted, and one that would be returned (above the route's maximum).
    through(10, 5_000_000).await;
    through(11, 2_000_000_000).await;
    w.cycle(t0()).await;
    w.settle(t0()).await;
    assert_eq!(bound(&w, 10, 0).await, None, "never minted");
    assert_eq!(bound(&w, 11, 0).await, None, "never returned");
    // Declared from a block after theirs: still not theirs.
    sqlx::query(
        "INSERT INTO directing_code (asset, network, address, token, minimum, maximum, \
             from_block, reference) \
         VALUES ($1, $2, $3, $4, 1, 1000000000000000, 11, 'declared from block 11')",
    )
    .bind(ASSET)
    .bind(ORIGIN)
    .bind(UNDECLARED.to_uppercase().replace("0X", "0x"))
    .bind(TOKEN)
    .execute(&w.admin)
    .await
    .unwrap();
    w.cycle(t0()).await;
    assert_eq!(
        bound(&w, 10, 0).await,
        None,
        "below the declaration's first block"
    );
    // From its first block on, it directs (its address compared without case): the arrival at
    // block 11 is decided, here returned, above the route's maximum.
    let (_, kind) = bound(&w, 11, 0).await.expect("decided once declared");
    assert_eq!(kind, "refund");
    assert_eq!(
        w.count("SELECT count(*) FROM transfers WHERE kind = 'deposit'")
            .await,
        0
    );
}

/// `REQ-AS-11`, `REQ-IN-6`, `REQ-GV-5` (review M3 of PR #139, second fix round; migration 0149): a
/// directed arrival below its contract's declaration (the endpoint's `[[directing]]` `from_block`
/// lower than the declaration's) is never minted and the core never returns it; an operator's
/// release, approved by a second login, returns it, once, to the refund address it recorded, paid
/// out as the refund of an unassociated arrival is and posted by the book as one.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_6_an_undeclared_directed_arrival_is_returned_only_by_a_two_person_release(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    const GAP: &str = "0x00000000000000000000000000000000000000de";
    let w = World::new(options, connect).await;
    let now = t0() + Duration::minutes(5);
    // Declared from block 100; its arrival at block 10, mintable otherwise, is in the gap.
    sqlx::query(
        "INSERT INTO directing_code (asset, network, address, token, minimum, maximum, \
             from_block, reference) \
         VALUES ($1, $2, $3, $4, 1, 1000000000000000, 100, 'declared from block 100')",
    )
    .bind(ASSET)
    .bind(ORIGIN)
    .bind(GAP)
    .bind(TOKEN)
    .execute(&w.admin)
    .await
    .unwrap();
    let arrival = observe(
        &w.admin,
        ORIGIN,
        10,
        0,
        TOKEN,
        5_000_000,
        Some(FUNDER),
        Some(RESERVE),
        None,
        true,
        json!({"event": "DirectedArrival", "directing": GAP, "direction_log": 1,
               "recipient": DESTINATION_ACCOUNT, "refund": REFUND}),
    )
    .await;
    decide(&w.admin, arrival, "final", 12).await;
    w.cycle(now).await;
    w.settle(now).await;
    assert_eq!(bound(&w, 10, 0).await, None, "never minted, never returned");

    // One operator releases it: its return waits in Received for the second login.
    let action: i64 = sqlx::query_scalar(&format!(
        "SELECT operator_release_directed_arrival('{ORIGIN}', 10, '\\x{}', 'log', 0, \
             'declared from a later block: never mintable')",
        hex::encode(hash(ORIGIN, 10))
    ))
    .fetch_one(&w.ops[0])
    .await
    .expect("the release");
    let (id, kind) = bound(&w, 10, 0).await.expect("the return");
    assert_eq!(kind, "refund");
    w.cycle(now).await;
    w.cycle(now).await;
    assert_eq!(w.state(id).await.0, "Received", "not on one person's word");
    approve_as(&w, 0, action)
        .await
        .expect_err("approved by its own login");
    approve_as(&w, 1, action).await.expect("the second login");

    // Accepted under the release, paid to the recorded refund address, once.
    w.cycle(now).await;
    w.cycle(now).await;
    assert_eq!(w.state(id).await.0, "Source-Final");
    w.cycle(now).await;
    let (_, payload) = order(&w.admin, id, 1).await.expect("the payout order");
    assert_eq!(payload["amount"], "5000000");
    assert_eq!(payload["order"]["to"], REFUND);
    deliver(&w, id, 1, now).await;
    let reference = reference_of(&w.admin, id).await;
    assert_eq!(
        reference,
        refs::unassociated_refund_reference(&at(10, 0)).to_vec()
    );
    paid_out(&w, 60, 5_000_000, REFUND, &reference).await;
    w.settle(now).await;
    assert_eq!(w.state(id).await, ("Completed".into(), None));
    assert_eq!(
        w.count("SELECT count(*) FROM book_entries WHERE kind = 'unassociated-refund'")
            .await,
        1
    );
    let accepted_under: Option<i64> = scalar(
        &w.admin,
        &format!(
            "SELECT manual_action_id FROM transitions WHERE transfer_id = {id} \
               AND from_state = 'Received' AND to_state = 'Accepted'"
        ),
    )
    .await;
    assert_eq!(accepted_under, Some(action));
    w.settle(now).await;
    assert_eq!(w.count("SELECT count(*) FROM transfers").await, 1, "once");
    assert_eq!(
        w.count("SELECT count(*) FROM transfers WHERE kind = 'deposit'")
            .await,
        0,
        "never minted"
    );
}

/// `REQ-IN-6`, `REQ-GV-5`, `REQ-IN-11` (review M1 of PR #139, third round; migration 0149): the
/// return an operator's release made stays the release's, whatever happens to the declaration
/// afterwards. A declaration that lands after the release must not turn it into the core's own
/// return, accepted with no second approver: it is not paid until a second operator approves, and
/// then once.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_6_a_declaration_after_a_release_does_not_waive_its_second_approver(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    const LATE: &str = "0x00000000000000000000000000000000000000d7";
    let w = World::new(options, connect).await;
    let now = t0() + Duration::minutes(5);
    let arrival = observe(
        &w.admin,
        ORIGIN,
        10,
        0,
        TOKEN,
        5_000_000,
        Some(FUNDER),
        Some(RESERVE),
        None,
        true,
        json!({"event": "DirectedArrival", "directing": LATE, "direction_log": 1,
               "recipient": DESTINATION_ACCOUNT, "refund": REFUND}),
    )
    .await;
    decide(&w.admin, arrival, "final", 12).await;
    w.cycle(now).await;
    assert_eq!(bound(&w, 10, 0).await, None, "undeclared: never decided");

    let action: i64 = sqlx::query_scalar(&format!(
        "SELECT operator_release_directed_arrival('{ORIGIN}', 10, '\\x{}', 'log', 0, \
             'undeclared at the time: never mintable')",
        hex::encode(hash(ORIGIN, 10))
    ))
    .fetch_one(&w.ops[0])
    .await
    .expect("the release");
    let (id, kind) = bound(&w, 10, 0).await.expect("the return");
    assert_eq!(kind, "refund");

    // The declaration lands after the release, covering the arrival.
    sqlx::query(
        "INSERT INTO directing_code (asset, network, address, token, minimum, maximum, \
             from_block, reference) \
         VALUES ($1, $2, $3, $4, 1, 1000000000000000, 0, 'declared after the release')",
    )
    .bind(ASSET)
    .bind(ORIGIN)
    .bind(LATE)
    .bind(TOKEN)
    .execute(&w.admin)
    .await
    .unwrap();
    for _ in 0..3 {
        w.cycle(now).await;
    }
    assert_eq!(
        w.state(id).await.0,
        "Received",
        "the release's return waits for its second approver, declared or not"
    );
    assert_eq!(
        w.count("SELECT count(*) FROM intents").await,
        0,
        "nothing is paid before the approval"
    );

    approve_as(&w, 1, action).await.expect("the second login");
    w.cycle(now).await;
    w.cycle(now).await;
    assert_eq!(w.state(id).await.0, "Source-Final");
    w.cycle(now).await;
    let (_, payload) = order(&w.admin, id, 1).await.expect("the payout order");
    assert_eq!(payload["order"]["to"], REFUND);
    deliver(&w, id, 1, now).await;
    let reference = reference_of(&w.admin, id).await;
    paid_out(&w, 60, 5_000_000, REFUND, &reference).await;
    w.settle(now).await;
    assert_eq!(w.state(id).await, ("Completed".into(), None));
    assert_eq!(
        w.count("SELECT count(*) FROM book_entries WHERE kind = 'unassociated-refund'")
            .await,
        1,
        "paid once"
    );
    assert_eq!(w.count("SELECT count(*) FROM transfers").await, 1, "once");
}

/// `REQ-IN-9`, `CND-DepositWindow` (review L5 of PR #136): a directed deposit that waited for the
/// route's window gets the whole of the route's deadline for `CND-DestinationRefused`, counted from
/// when it left the window (on the issuing network's head it rested on then), not from its arrival's
/// finality, which the wait had already used up.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_9_a_deposit_that_waited_for_the_window_gets_its_whole_refusal_deadline(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    exec(
        &w.admin,
        &format!(
            "UPDATE routes SET maximum = 6000000, window_limit = 10000000, \
                 window_seconds = 86400 \
             WHERE asset = '{ASSET}' AND network = '{ORIGIN}' AND direction = 'deposit'"
        ),
    )
    .await;
    final_directed(&w.admin, 10, 0, 6_000_000).await;
    final_directed(&w.admin, 11, 0, 6_000_000).await;
    w.cycle(t0()).await;
    let (b, _) = bound(&w, 11, 0).await.expect("deposit b");
    assert_eq!(
        w.state(b).await,
        ("Source-Final".into(), Some("CND-DepositWindow".into()))
    );
    // It waits past its arrival's deadline; then the window rolls over and it leaves, on this head.
    let left = 13 + BUDGET + 100;
    head(&w.admin, ISSUING, left).await;
    exec(
        &w.admin,
        &format!(
            "UPDATE routes SET window_seconds = 1 \
             WHERE asset = '{ASSET}' AND network = '{ORIGIN}' AND direction = 'deposit'"
        ),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(b).await, ("Source-Final".into(), None));
    w.cycle(t0()).await;
    let (o, _) = order(&w.admin, b, 1).await.expect("the mint order");
    prepare(&w.admin, o, Some("Kreivo(Assets(CannotMint))")).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(b).await,
        ("Source-Final".into(), Some("CND-DestinationRefused".into())),
        "not sent to review at once: its clock started when it left the window"
    );
    head(&w.admin, ISSUING, left + BUDGET - 1).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(b).await.0, "Source-Final");
    head(&w.admin, ISSUING, left + BUDGET + 1).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(b).await,
        ("Needs-Review".into(), Some("CND-DestinationRefused".into()))
    );
}

/// A second directed deposit that waited in `CND-DepositWindow` past its arrival's deadline, then
/// left it on the issuing network's head `left`, its first mint ordered, and the issuing network's
/// next head, `left + 1`, seen after the order: returns it, `left` and the order (as
/// `req_in_9_a_deposit_that_waited_for_the_window_gets_its_whole_refusal_deadline`).
async fn waited_for_the_window(w: &World) -> (i64, i64, i64) {
    exec(
        &w.admin,
        &format!(
            "UPDATE routes SET maximum = 6000000, window_limit = 10000000, \
                 window_seconds = 86400 \
             WHERE asset = '{ASSET}' AND network = '{ORIGIN}' AND direction = 'deposit'"
        ),
    )
    .await;
    final_directed(&w.admin, 10, 0, 6_000_000).await;
    final_directed(&w.admin, 11, 0, 6_000_000).await;
    w.cycle(t0()).await;
    let (b, _) = bound(w, 11, 0).await.expect("deposit b");
    assert_eq!(
        w.state(b).await,
        ("Source-Final".into(), Some("CND-DepositWindow".into()))
    );
    let left = 13 + BUDGET + 100;
    head(&w.admin, ISSUING, left).await;
    exec(
        &w.admin,
        &format!(
            "UPDATE routes SET window_seconds = 1 \
             WHERE asset = '{ASSET}' AND network = '{ORIGIN}' AND direction = 'deposit'"
        ),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(b).await, ("Source-Final".into(), None));
    w.cycle(t0()).await;
    let (o, _) = order(&w.admin, b, 1).await.expect("the mint order");
    head(&w.admin, ISSUING, left + 1).await;
    (b, left, o)
}

/// Takes `b`, refused for nothing it names after it left the window on head `left` (and was
/// ordered before head `left + 1`), to its deadline: still `Source-Final` with no condition just
/// before `left + 1` plus the route's time, in review without `CND-DestinationRefused` just after.
async fn the_refusal_that_names_nothing_waits_from_the_window(w: &World, b: i64, left: i64) {
    let (state, condition) = w.state(b).await;
    assert_eq!(
        state, "Source-Final",
        "not sent to review at once: the first attempt's clock started when it left the window"
    );
    assert_eq!(condition, None);
    let aged = left + 1;
    head(&w.admin, ISSUING, aged + BUDGET - 1).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(b).await, ("Source-Final".into(), None));
    head(&w.admin, ISSUING, aged + BUDGET + 1).await;
    w.cycle(t0()).await;
    let (state, condition) = w.state(b).await;
    assert_eq!(
        state, "Needs-Review",
        "the refusal that names nothing, at its deadline"
    );
    assert_ne!(condition.as_deref(), Some("CND-DestinationRefused"));
}

/// `REQ-IN-9`, `REQ-RV-1`, `CND-DepositWindow` (review M1 of PR #136): a directed deposit that
/// waited for the route's window, refused for nothing it names, gets the whole of the route's
/// deadline counted from when it left the window, not from its arrival's finality, which the wait
/// had already used up.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_9_a_deposit_that_waited_for_the_window_gets_its_whole_unnamed_refusal_deadline(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (b, left, o) = waited_for_the_window(&w).await;
    prepare(&w.admin, o, Some("Kreivo(UnknownError)")).await;
    w.cycle(t0()).await;
    w.cycle(t0()).await;
    the_refusal_that_names_nothing_waits_from_the_window(&w, b, left).await;
}

/// `REQ-IN-9`, `REQ-RV-1`, `CND-DepositWindow` (review M1 of PR #136): a directed deposit that
/// waited for the route's window, then held in `CND-DestinationRefused`, whose hold a refusal to
/// mint a frozen asset drops: the refusal that names nothing has the whole of the route's deadline
/// from when it left the window too.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_9_a_dropped_destination_hold_after_the_window_gets_its_whole_deadline(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (b, left, first) = waited_for_the_window(&w).await;
    prepare(&w.admin, first, Some("Kreivo(Assets(CannotMint))")).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(b).await,
        ("Source-Final".into(), Some("CND-DestinationRefused".into()))
    );
    let (second, _) = order(&w.admin, b, 1).await.expect("a new order");
    assert_ne!(first, second);
    prepare_refused_with_asset(
        &w.admin,
        second,
        "Kreivo(Assets(CannotMint))",
        json!("Frozen"),
    )
    .await;
    w.cycle(t0()).await;
    w.cycle(t0()).await;
    the_refusal_that_names_nothing_waits_from_the_window(&w, b, left).await;
}

/// `REQ-IN-9`, `REQ-RF-1` (review L1 of PR #136): the core holds `CND-DestinationRefused` on the
/// dry run the store reads for the directed exit, the newest of the first attempt's by block, then
/// row. A late row of a superseded order, at a later block, refused for a frozen asset, drops the
/// hold the current order's own row (the destination refused at an earlier block) would keep; so
/// the deposit is never in review for the destination while the store would refuse its exit.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_1_a_late_row_of_a_superseded_order_decides_the_destination_hold(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    final_directed(&w.admin, 10, 0, 5_000_000).await;
    w.cycle(t0()).await;
    let (id, _) = bound(&w, 10, 0).await.expect("the deposit");
    let (first, _) = order(&w.admin, id, 1).await.expect("the mint order");
    prepare(&w.admin, first, Some("Kreivo(Assets(CannotMint))")).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-DestinationRefused".into()))
    );
    let (second, _) = order(&w.admin, id, 1).await.expect("a new order");
    assert_ne!(first, second);
    prepare(&w.admin, second, Some("Kreivo(Assets(CannotMint))")).await;
    // The superseded order's late row, at a later head.
    let dry_run = json!({"outcome": "refused", "reason": "Kreivo(Assets(CannotMint))",
                         "asset": {"id": "Here(1)", "status": "Frozen", "supply": "0"}});
    prepare_dry_run_at(&w.admin, first, false, dry_run, 2).await;
    let recorded: bool = scalar(
        &w.admin,
        &format!("SELECT directed_refusal_recorded({id}, NULL, NULL, NULL, NULL)"),
    )
    .await;
    assert!(
        !recorded,
        "the store reads the newest row: the frozen asset's"
    );
    w.cycle(t0()).await;
    let (state, condition) = w.state(id).await;
    assert_eq!(state, "Source-Final");
    assert_ne!(condition.as_deref(), Some("CND-DestinationRefused"));
}

/// `CND-DepositWindow`, `REQ-LM-1` (review L2 of PR #136): a directed deposit waiting for a window
/// that, as its route is declared now, can never take it (the window's limit lowered below its
/// amount, or the route removed) is not left to wait in silence: it pages (`deposit-window-alert`,
/// in the event log too), once per reason and then once a day, and nothing else changes: it keeps
/// waiting, is not returned and not reviewed.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_deposit_window_a_window_that_can_never_fit_pages(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    exec(
        &w.admin,
        &format!(
            "UPDATE routes SET maximum = 6000000, window_limit = 10000000, \
                 window_seconds = 86400 \
             WHERE asset = '{ASSET}' AND network = '{ORIGIN}' AND direction = 'deposit'"
        ),
    )
    .await;
    final_directed(&w.admin, 10, 0, 6_000_000).await;
    final_directed(&w.admin, 11, 0, 6_000_000).await;
    w.cycle(t0()).await;
    let (b, _) = bound(&w, 11, 0).await.expect("deposit b");
    let pages = |reason: &'static str| {
        let w = &w;
        async move {
            w.count(&format!(
                "SELECT count(*) FROM event_log WHERE kind = 'deposit-window-alert' \
                   AND transfer_id = {b} AND payload ->> 'reason' = '{reason}'"
            ))
            .await
        }
    };
    // Waiting for a window that will roll over pages nothing.
    w.cycle(t0()).await;
    assert_eq!(
        w.count("SELECT count(*) FROM event_log WHERE kind = 'deposit-window-alert'")
            .await,
        0
    );
    exec(
        &w.admin,
        &format!(
            "UPDATE routes SET maximum = 5000000, window_limit = 5000000 \
             WHERE asset = '{ASSET}' AND network = '{ORIGIN}' AND direction = 'deposit'"
        ),
    )
    .await;
    w.cycle(t0()).await;
    w.cycle(t0() + Duration::hours(1)).await;
    assert_eq!(pages("above-window-limit").await, 1, "once");
    w.cycle(t0() + Duration::hours(25)).await;
    assert_eq!(pages("above-window-limit").await, 2, "then once a day");
    assert_eq!(
        w.state(b).await,
        ("Source-Final".into(), Some("CND-DepositWindow".into()))
    );
    assert!(order(&w.admin, b, 1).await.is_none());
    assert_eq!(returns(&w).await, 0);
}

/// `REQ-IN-9`, `REQ-RV-1`, `CND-DepositWindow` (review M-a of PR #136): a directed deposit that
/// waited for the route's window, whose mint then passed its dry run and was signed, is not sent to
/// review on the next head for the time it waited: the signed attempt's deadline in `Delivering`
/// counts from when it left the window, as the unsigned one's does, and is reviewed only past it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_9_a_signed_mint_after_the_window_gets_its_whole_delivering_deadline(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (b, left, o) = waited_for_the_window(&w).await;
    prepare(&w.admin, o, None).await;
    w.cycle(t0()).await;
    let (s, _) = signing_intent(&w.admin, b, 1)
        .await
        .expect("signing intent");
    sign(&w, s).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(b).await, ("Delivering".into(), None));
    // The next head, before the inclusion is seen: still delivering.
    head(&w.admin, ISSUING, left + 2).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(b).await,
        ("Delivering".into(), None),
        "not sent to review at once: the mint's clock started when it left the window"
    );
    head(&w.admin, ISSUING, left + BUDGET - 1).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(b).await, ("Delivering".into(), None));
    head(&w.admin, ISSUING, left + BUDGET + 1).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(b).await.0,
        "Needs-Review",
        "unknown past its deadline (REQ-RV-1)"
    );
    assert_eq!(returns(&w).await, 0, "REQ-RF-1: no refund on unknown");
}

/// `REQ-EO-3`, `REQ-RV-1`, `REQ-IN-11`, SPEC §5.4's `Source-Final` → `Needs-Review` row (review L-a
/// of PR #136): a directed deposit held in `CND-Paused` past the route's budget, whose first order,
/// made when the pause lifts, is refused for nothing it names, is not reviewed on the next head: the
/// route's deadline counts from the later of the source leg's finality and the first final head
/// seen after the attempt was ordered, for a refused preparation as for one that never happened.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_eo_3_a_refusal_after_a_long_pause_counts_from_the_first_head_after_the_order(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    exec(
        &w.admin,
        &format!("INSERT INTO pauses (asset) VALUES ('{ASSET}')"),
    )
    .await;
    final_directed(&w.admin, 10, 0, 5_000_000).await;
    w.settle(t0()).await;
    let (id, _) = bound(&w, 10, 0).await.expect("the deposit");
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-Paused".into()))
    );
    // Held past the route's budget, counted from the arrival's finality (block 12).
    let lifted = 12 + BUDGET + 100;
    head(&w.admin, ISSUING, lifted).await;
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-Paused".into()))
    );
    exec(&w.admin, "DELETE FROM pauses").await;
    w.settle(t0()).await;
    let (o, _) = order(&w.admin, id, 1).await.expect("the first order");
    // The first final head after the order, then the refusal that names nothing.
    let aged = lifted + 1;
    head(&w.admin, ISSUING, aged).await;
    prepare(&w.admin, o, Some("Kreivo(UnknownError)")).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), None),
        "not sent to review on the next head: the order has only started to age"
    );
    head(&w.admin, ISSUING, aged + BUDGET - 1).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await, ("Source-Final".into(), None));
    head(&w.admin, ISSUING, aged + BUDGET + 1).await;
    w.cycle(t0()).await;
    let (state, condition) = w.state(id).await;
    assert_eq!(
        state, "Needs-Review",
        "the refusal that names nothing, at its deadline"
    );
    assert_ne!(condition.as_deref(), Some("CND-DestinationRefused"));
    assert_eq!(returns(&w).await, 0);
}

/// `REQ-EO-3`, `REQ-RV-1` (review L-a of PR #136): the first final head after the attempt's FIRST
/// order sets the clock of a refusal that names nothing; the new order each refusal makes after
/// its retry interval does not restart it, however late that order's own first head comes.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_eo_3_a_retry_does_not_restart_the_refused_attempts_clock(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    final_directed(&w.admin, 10, 0, 5_000_000).await;
    w.cycle(t0()).await;
    let (id, _) = bound(&w, 10, 0).await.expect("the deposit");
    let (first, _) = order(&w.admin, id, 1).await.expect("the first order");
    // The first final head after the first order: later than the arrival's finality (block 12).
    let aged = 20;
    head(&w.admin, ISSUING, aged).await;
    prepare(&w.admin, first, Some("Kreivo(UnknownError)")).await;
    w.cycle(t0()).await;
    let (second, _) = order(&w.admin, id, 1).await.expect("a retry");
    assert_ne!(first, second);
    // The retry's own first head comes much later.
    head(&w.admin, ISSUING, aged + 1_000).await;
    prepare(&w.admin, second, Some("Kreivo(UnknownError)")).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await, ("Source-Final".into(), None));
    head(&w.admin, ISSUING, aged + BUDGET - 1).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), None),
        "counted from the first head after the first order, not the arrival's finality"
    );
    head(&w.admin, ISSUING, aged + BUDGET + 1).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await.0,
        "Needs-Review",
        "the retry did not restart the clock"
    );
    assert_eq!(returns(&w).await, 0);
}

// ---------------------------------------------------------------------------------------------
// Amendment 0031 (SPEC v0.5.14): capacity, the window's queue, the hold on an upgrade of the
// origin token, and the review edge judged at resolution.

/// Sets the deposit route's declared capacity.
async fn capacity(w: &World, n: i64) {
    exec(
        &w.admin,
        &format!(
            "UPDATE routes SET capacity = {n} \
             WHERE asset = '{ASSET}' AND network = '{ORIGIN}' AND direction = 'deposit'"
        ),
    )
    .await;
}

/// The deposit route's transfers in flight, as the ingress reads them (`route_usage_locked()`).
async fn in_flight(w: &World) -> i64 {
    scalar(
        &w.admin,
        &format!(
            "SELECT in_flight FROM route_usage_locked('{ASSET}', '{ORIGIN}', 'deposit', 86400)"
        ),
    )
    .await
}

/// `AC-A6.3`, `REQ-LM-4`, `REQ-IN-11` (amendment 0031, `d-f018-capacity`, ruled: wait): a directed
/// deposit over its route's capacity waits in `CND-DepositCapacity`, with nothing ordered and nothing
/// returned, even past its deadline; one behind it by coordinate waits behind it. While they wait
/// they are in flight, so a request sees the route full (`ERR-Overloaded`, first come first served).
/// As room comes, they resume by themselves, in coordinate order, and are minted.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_3_cnd_deposit_capacity_waits_in_order_resumes_and_is_never_returned(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    capacity(&w, 2).await;
    for block in [10, 11, 12, 13] {
        final_directed(&w.admin, block, 0, 5_000_000).await;
    }
    w.cycle(t0()).await;
    let ids: Vec<i64> = {
        let mut v = Vec::new();
        for block in [10, 11, 12, 13] {
            v.push(bound(&w, block, 0).await.expect("a deposit").0);
        }
        v
    };
    let (a, b, c, d) = (ids[0], ids[1], ids[2], ids[3]);
    for id in [a, b] {
        assert_eq!(w.state(id).await, ("Source-Final".into(), None));
    }
    for id in [c, d] {
        assert_eq!(
            w.state(id).await,
            ("Source-Final".into(), Some("CND-DepositCapacity".into())),
            "deposit {id} waits for room"
        );
    }
    assert_eq!(in_flight(&w).await, 4, "the waiting are in flight");
    // Past their deadline they still wait: no order, no review, no return.
    head(&w.admin, ISSUING, 15 + BUDGET + 100).await;
    w.settle(t0()).await;
    for id in [c, d] {
        assert_eq!(
            w.state(id).await,
            ("Source-Final".into(), Some("CND-DepositCapacity".into()))
        );
        assert!(order(&w.admin, id, 1).await.is_none());
    }
    assert_eq!(returns(&w).await, 0);

    // Room for one more: c, first by coordinate, resumes; d still waits behind it.
    capacity(&w, 3).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(c).await, ("Source-Final".into(), None));
    assert_eq!(
        w.state(d).await,
        ("Source-Final".into(), Some("CND-DepositCapacity".into()))
    );
    capacity(&w, 4).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(d).await, ("Source-Final".into(), None));
    w.cycle(t0()).await;
    for id in [a, b, c, d] {
        assert!(order(&w.admin, id, 1).await.is_some(), "{id} is minted");
    }
    assert_eq!(returns(&w).await, 0);
}

/// `REQ-IN-11`, `REQ-IN-9`, `CND-DepositCapacity` (amendment 0031): a directed deposit that waited
/// for its route's capacity gets the whole of the route's deadline for `CND-DestinationRefused`,
/// counted from when it left the wait, not from its arrival's finality.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_11_a_deposit_that_waited_for_capacity_gets_its_whole_refusal_deadline(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    capacity(&w, 1).await;
    final_directed(&w.admin, 10, 0, 5_000_000).await;
    final_directed(&w.admin, 11, 0, 5_000_000).await;
    w.cycle(t0()).await;
    let (b, _) = bound(&w, 11, 0).await.expect("deposit b");
    assert_eq!(
        w.state(b).await,
        ("Source-Final".into(), Some("CND-DepositCapacity".into()))
    );
    let left = 13 + BUDGET + 100;
    head(&w.admin, ISSUING, left).await;
    capacity(&w, 2).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(b).await, ("Source-Final".into(), None));
    w.cycle(t0()).await;
    let (o, _) = order(&w.admin, b, 1).await.expect("the mint order");
    prepare(&w.admin, o, Some("Kreivo(Assets(CannotMint))")).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(b).await,
        ("Source-Final".into(), Some("CND-DestinationRefused".into())),
        "not sent to review at once: its clock started when it left the wait"
    );
    head(&w.admin, ISSUING, left + BUDGET - 1).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(b).await.0, "Source-Final");
    head(&w.admin, ISSUING, left + BUDGET + 1).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(b).await,
        ("Needs-Review".into(), Some("CND-DestinationRefused".into()))
    );
    assert_eq!(returns(&w).await, 0);
}

/// Sets the deposit route's window: its limit and maximum to `limit`, its length to `seconds`.
async fn window(w: &World, limit: u128, seconds: i64) {
    exec(
        &w.admin,
        &format!(
            "UPDATE routes SET maximum = {limit}, window_limit = {limit}, window_seconds = {seconds} \
             WHERE asset = '{ASSET}' AND network = '{ORIGIN}' AND direction = 'deposit'"
        ),
    )
    .await;
}

/// The deposit route's window made `seconds` long, its limit unchanged.
async fn window_seconds(w: &World, seconds: i64) {
    exec(
        &w.admin,
        &format!(
            "UPDATE routes SET window_seconds = {seconds} \
             WHERE asset = '{ASSET}' AND network = '{ORIGIN}' AND direction = 'deposit'"
        ),
    )
    .await;
}

/// A second and a half: past a window made one second long.
async fn past_a_window() {
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
}

/// The deposit route's requests' reserve set to `share` (`routes.request_reserve`). Not in force: 0
/// on every route until the owner rules on `d-f018-window-reserve` (amendment 0031); only the tests
/// of the reserve's own behaviour set it.
async fn request_reserve(w: &World, share: &str) {
    exec(
        &w.admin,
        &format!(
            "UPDATE routes SET request_reserve = {share} \
             WHERE asset = '{ASSET}' AND network = '{ORIGIN}' AND direction = 'deposit'"
        ),
    )
    .await;
}

/// `AC-A6.7`, `REQ-IN-11`, `REQ-LM-1` (amendment 0031, `d-f018-window-queue`, ruled: cap what waits
/// to one window): a queue of directed deposits three windows deep claims, against requests, one
/// window's limit at most, and only for one window from when it began to wait. After that window a
/// request sees room and its deposit is taken, and the queue still drains in coordinate order,
/// behind it, never past the window's limit. The requests' reserve is off (0, as on every route until
/// `d-f018-window-reserve` is ruled); the request comes before the core's next cycle. Had the core
/// cycled first, the queue's head would have taken the room
/// ([`req_in_11_with_the_reserve_off_the_queue_head_takes_the_freed_room_first`]).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_7_a_queue_of_several_windows_leaves_requesters_room_after_one_window(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    window(&w, 10_000_000, 86400).await;
    for block in [10, 11, 12, 13] {
        final_directed(&w.admin, block, 0, 10_000_000).await;
    }
    w.cycle(t0()).await;
    let mut ids = Vec::new();
    for block in [10, 11, 12, 13] {
        ids.push(bound(&w, block, 0).await.expect("a deposit").0);
    }
    let (a, queue) = (ids[0], ids[1..].to_vec());
    assert_eq!(w.state(a).await, ("Source-Final".into(), None));
    for id in &queue {
        assert_eq!(
            w.state(*id).await,
            ("Source-Final".into(), Some("CND-DepositWindow".into()))
        );
    }
    // Within the window: a's share and the queue's claim, capped at one window (30 waiting, 10
    // counted): a request is refused, as before.
    assert_eq!(window_used(&w, 86400).await, "20000000");

    // One window on (the route's window made one second long): a's share has left it, and so has
    // every claim of the queue, which began to wait more than a window ago. A request sees room.
    window_seconds(&w, 1).await;
    past_a_window().await;
    assert_eq!(
        window_used(&w, 1).await,
        "0",
        "the queue no longer locks requests out"
    );
    // A requester's deposit is taken in that room (as the ingress does, under the route's lock).
    let r_arrival = arrival(&w.admin, 30, 6_000_000).await;
    decide(&w.admin, r_arrival, "final", 32).await;
    let requested = deposit(&w.admin, 30, 6_000_000).await;
    assert_eq!(window_used(&w, 1).await, "6000000");
    // The queue's head waits behind it: 6 taken plus its 10 is over the window.
    w.cycle(t0()).await;
    for id in &queue {
        assert_eq!(
            w.state(*id).await.1.as_deref(),
            Some("CND-DepositWindow"),
            "deposit {id} waits behind the request"
        );
    }
    assert_ne!(
        w.state(requested).await.0,
        "Refused",
        "the request was taken"
    );

    // Window by window the queue drains in coordinate order, one at a time.
    for (n, id) in queue.iter().enumerate() {
        past_a_window().await;
        w.cycle(t0()).await;
        assert_eq!(
            w.state(*id).await,
            ("Source-Final".into(), None),
            "{n}: the queue's next leaves the window"
        );
        for later in &queue[n + 1..] {
            assert_eq!(
                w.state(*later).await.1.as_deref(),
                Some("CND-DepositWindow"),
                "{n}: {later} still waits"
            );
        }
    }
    assert_eq!(returns(&w).await, 0);
}

/// `REQ-IN-11`, `REQ-LM-1` (amendment 0031, `d-f018-window-queue`): the behaviour as built, with the
/// requests' reserve off (0, on every route until `d-f018-window-reserve` is ruled), stated so that
/// the ruling changes a test and not a surprise. Right after the window rolls over the core cycles
/// first, as it does in production every few seconds, and the queue's head takes the window's
/// freed room before any request can come: what a request sees then is the head's share, the whole
/// window. The queue's claims no longer count against requests, but its head may still take each
/// window's room as it frees.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_11_with_the_reserve_off_the_queue_head_takes_the_freed_room_first(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    window(&w, 10_000_000, 86400).await;
    for block in [10, 11, 12] {
        final_directed(&w.admin, block, 0, 10_000_000).await;
    }
    w.cycle(t0()).await;
    let mut ids = Vec::new();
    for block in [10, 11, 12] {
        ids.push(bound(&w, block, 0).await.expect("a deposit").0);
    }
    window_seconds(&w, 1).await;
    past_a_window().await;
    assert_eq!(window_used(&w, 1).await, "0");
    w.cycle(t0()).await;
    assert_eq!(
        w.state(ids[1]).await,
        ("Source-Final".into(), None),
        "the head took the room"
    );
    assert_eq!(
        window_used(&w, 1).await,
        "10000000",
        "a request finds none, pending d-f018-window-reserve"
    );
    assert_eq!(
        w.state(ids[2]).await.1.as_deref(),
        Some("CND-DepositWindow")
    );
}

/// `REQ-IN-11`, `REQ-LM-1`: the requests' reserve's own behaviour, set explicitly to 0.5. NOT IN
/// FORCE: `routes.request_reserve` is 0 on every route until the owner rules on
/// `d-f018-window-reserve` (amendment 0031; review M3 of PR #142 proposed it, and the second review
/// found it starves old deposits, [`req_in_11_reserve_h1_an_old_deposit_over_the_share_starves`]
/// and [`req_in_11_reserve_m1_a_smaller_old_deposit_starves_too`]). With it set, right after the
/// window rolls over the core cycles first and the queue's head takes its share; but a queue older
/// than a window takes no more than half the window, so a request still finds room. The rest of the
/// queue waits behind it and drains window by window, in coordinate order, never past the half.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_11_the_requests_reserve_when_set_leaves_requests_half_the_window(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    window(&w, 10_000_000, 86400).await;
    request_reserve(&w, "0.5").await;
    for block in [10, 11, 12, 13, 14] {
        final_directed(&w.admin, block, 0, 4_000_000).await;
    }
    w.cycle(t0()).await;
    let mut ids = Vec::new();
    for block in [10, 11, 12, 13, 14] {
        ids.push(bound(&w, block, 0).await.expect("a deposit").0);
    }
    let queue = ids[2..].to_vec();
    for id in &ids[..2] {
        assert_eq!(w.state(*id).await, ("Source-Final".into(), None));
    }
    for id in &queue {
        assert_eq!(
            w.state(*id).await,
            ("Source-Final".into(), Some("CND-DepositWindow".into()))
        );
    }
    assert_eq!(window_used(&w, 86400).await, "18000000");

    // One window on: the core cycles first; the queue's head takes its 4, within half the window;
    // the next, 4 more, would pass the half.
    window_seconds(&w, 1).await;
    past_a_window().await;
    w.cycle(t0()).await;
    assert_eq!(w.state(queue[0]).await, ("Source-Final".into(), None));
    for id in &queue[1..] {
        assert_eq!(
            w.state(*id).await.1.as_deref(),
            Some("CND-DepositWindow"),
            "deposit {id} waits behind the head, under the requests' reserve"
        );
    }
    let used: u128 = window_used(&w, 1).await.parse().unwrap();
    assert_eq!(used, 4_000_000, "only the head's share");
    // A request still finds room: a requester's deposit of 6 is within what is left, and is taken.
    let r_arrival = arrival(&w.admin, 30, 6_000_000).await;
    decide(&w.admin, r_arrival, "final", 32).await;
    let requested = deposit(&w.admin, 30, 6_000_000).await;
    assert_eq!(window_used(&w, 1).await, "10000000");
    w.cycle(t0()).await;
    assert_ne!(w.state(requested).await.0, "Refused");

    // Window by window the rest of the queue drains in coordinate order, each leaving the requests
    // their half.
    for (n, id) in queue.iter().enumerate().skip(1) {
        past_a_window().await;
        w.cycle(t0()).await;
        assert_eq!(
            w.state(*id).await,
            ("Source-Final".into(), None),
            "{n}: the queue's next leaves the window"
        );
        for later in &queue[n + 1..] {
            assert_eq!(
                w.state(*later).await.1.as_deref(),
                Some("CND-DepositWindow"),
                "{n}: {later} still waits"
            );
        }
        let used: u128 = window_used(&w, 1).await.parse().unwrap();
        assert!(
            used <= 5_000_000,
            "{n}: the queue took {used}, past its half"
        );
    }
    assert_eq!(returns(&w).await, 0);
}

/// A requester's deposit of `amount` at `block`, taken only if the window of `seconds` has room as
/// the ingress sees it (`route_usage_locked()`'s `used`, against a limit of 10); whether it was.
async fn request_if_room(w: &World, block: i64, amount: u128, seconds: i64) -> bool {
    let used: u128 = window_used(w, seconds).await.parse().unwrap();
    if used + amount > 10_000_000 {
        return false;
    }
    let a = arrival(&w.admin, block, amount).await;
    decide(&w.admin, a, "final", block + 2).await;
    deposit(&w.admin, block, amount).await;
    true
}

/// `secs` seconds of a route with a window of `window_secs`: a requester's deposit of `amount` every
/// `every_ms` (taken only if it fits, as the ingress does), the core cycling in between as fast as
/// it can.
async fn request_trickle(
    w: &World,
    secs: u64,
    window_secs: i64,
    amount: u128,
    every_ms: u64,
    block: &mut i64,
) {
    let start = std::time::Instant::now();
    let mut next = start;
    while start.elapsed() < std::time::Duration::from_secs(secs) {
        if std::time::Instant::now() >= next {
            request_if_room(w, *block, amount, window_secs).await;
            *block += 5;
            next += std::time::Duration::from_millis(every_ms);
        }
        w.cycle(t0()).await;
    }
}

/// KNOWN FAULT of the requests' reserve, which keeps it off (H1 of the second review of PR #142;
/// awaiting `d-f018-window-reserve`). Ignored: it takes some 30 seconds of wall time and documents a
/// starvation, it does not guard a promise. With the reserve set to 0.5, an old deposit larger than
/// (1 - 0.5) of the limit may leave only into a completely empty window, so under any trickle of
/// requests (here 1 of 10 every second, a window of 4 seconds) it waits for ever, and the deposit
/// behind it with it, while the core cycles throughout. Once requests stop, one window on, it
/// leaves. Run it with `--ignored`.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
#[ignore = "documents H1, a known starvation of the requests' reserve (off until d-f018-window-reserve is ruled); about 30 s"]
async fn req_in_11_reserve_h1_an_old_deposit_over_the_share_starves(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    window(&w, 10_000_000, 86400).await;
    request_reserve(&w, "0.5").await;
    final_directed(&w.admin, 10, 0, 10_000_000).await;
    final_directed(&w.admin, 11, 0, 6_000_000).await;
    final_directed(&w.admin, 12, 0, 6_000_000).await;
    final_directed(&w.admin, 13, 0, 1_000_000).await;
    w.cycle(t0()).await;
    let mut ids = Vec::new();
    for b in [10, 11, 12, 13] {
        ids.push(bound(&w, b, 0).await.expect("a deposit").0);
    }
    for id in &ids[1..] {
        assert_eq!(w.state(*id).await.1.as_deref(), Some("CND-DepositWindow"));
    }
    window_seconds(&w, 4).await;
    tokio::time::sleep(std::time::Duration::from_millis(4500)).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(ids[1]).await.1, None, "the head leaves");
    let mut block = 100;
    request_trickle(&w, 20, 4, 1_000_000, 1000, &mut block).await;
    assert_eq!(
        w.state(ids[2]).await.1.as_deref(),
        Some("CND-DepositWindow"),
        "the old 6, over the unreserved half, starves"
    );
    assert_eq!(
        w.state(ids[3]).await.1.as_deref(),
        Some("CND-DepositWindow"),
        "the 1 behind it starves with it"
    );
    tokio::time::sleep(std::time::Duration::from_millis(4500)).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(ids[2]).await.1,
        None,
        "requests stopped: it leaves into the empty window"
    );
}

/// KNOWN FAULT of the requests' reserve, which keeps it off (M1 of the second review of PR #142;
/// awaiting `d-f018-window-reserve`). Ignored: it takes some 30 seconds of wall time, depends on
/// timing, and documents a starvation, it does not guard a promise. With the reserve set to 0.5, an
/// old deposit within the unreserved half (3 of 10) starves too: requests are not bounded by old
/// waiters, so a trickle of them (1 every second, a window of 4 seconds) keeps the window's use
/// above what the deposit may join. Run it with `--ignored`.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
#[ignore = "documents M1, a known starvation of the requests' reserve (off until d-f018-window-reserve is ruled); about 30 s, timing-dependent"]
async fn req_in_11_reserve_m1_a_smaller_old_deposit_starves_too(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    window(&w, 10_000_000, 86400).await;
    request_reserve(&w, "0.5").await;
    final_directed(&w.admin, 10, 0, 10_000_000).await;
    final_directed(&w.admin, 11, 0, 6_000_000).await;
    final_directed(&w.admin, 12, 0, 3_000_000).await;
    w.cycle(t0()).await;
    let b = bound(&w, 11, 0).await.expect("a deposit").0;
    let c = bound(&w, 12, 0).await.expect("a deposit").0;
    assert_eq!(w.state(c).await.1.as_deref(), Some("CND-DepositWindow"));
    window_seconds(&w, 4).await;
    tokio::time::sleep(std::time::Duration::from_millis(4500)).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(b).await.1, None, "the head leaves");
    assert_eq!(w.state(c).await.1.as_deref(), Some("CND-DepositWindow"));
    let mut block = 100;
    request_trickle(&w, 20, 4, 1_000_000, 1000, &mut block).await;
    assert_eq!(
        w.state(c).await.1.as_deref(),
        Some("CND-DepositWindow"),
        "the old 3, within the unreserved half, starves under requests"
    );
}

/// `REQ-LM-4`, `REQ-IN-11`, `AC-A6.3` (amendment 0031, `d-f018-capacity`; review M1 of PR #142): a
/// directed deposit waiting for the window carries nothing, so it is not in flight, and neither a
/// request nor a deposit that leaves the window waits behind it for capacity. Capacity 2, five
/// deposits of a whole window each: the first moves, the other four wait for the window, and the
/// route has one transfer in flight. When the window rolls over, the second leaves the window and
/// finds room beside the first, though the last three still wait for the window behind it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_lm_4_deposits_waiting_for_the_window_are_not_in_flight(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    capacity(&w, 2).await;
    window(&w, 10_000_000, 86400).await;
    for block in [10, 11, 12, 13, 14] {
        final_directed(&w.admin, block, 0, 10_000_000).await;
    }
    w.cycle(t0()).await;
    let mut ids = Vec::new();
    for block in [10, 11, 12, 13, 14] {
        ids.push(bound(&w, block, 0).await.expect("a deposit").0);
    }
    assert_eq!(w.state(ids[0]).await, ("Source-Final".into(), None));
    for id in &ids[1..] {
        assert_eq!(
            w.state(*id).await,
            ("Source-Final".into(), Some("CND-DepositWindow".into()))
        );
    }
    assert_eq!(
        in_flight(&w).await,
        1,
        "the window's waiters are not in flight: a request finds room"
    );

    window_seconds(&w, 1).await;
    past_a_window().await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(ids[1]).await,
        ("Source-Final".into(), None),
        "b moves: only a is in flight"
    );
    for id in &ids[2..] {
        assert_eq!(
            w.state(*id).await,
            ("Source-Final".into(), Some("CND-DepositWindow".into()))
        );
    }
    assert_eq!(in_flight(&w).await, 2);
    assert_eq!(returns(&w).await, 0);
}

/// An upgrade of the origin token at (`ORIGIN`, `block`), final two blocks later, as the EVM
/// endpoint records it (`token_upgrade`, migration 0147).
async fn upgraded(w: &World, block: i64) -> i64 {
    let id = observe(
        &w.admin,
        ORIGIN,
        block,
        0,
        TOKEN,
        0,
        None,
        None,
        None,
        true,
        json!({"event": "TokenUpgraded", "token": TOKEN,
               "implementation": "0x0000000000000000000000000000000000000abc"}),
    )
    .await;
    decide(&w.admin, id, "final", block + 2).await;
    id
}

/// `AC-A6.8`, `REQ-AS-12`, `REQ-GV-5` (amendment 0031, `d-f018-usdc-upgrade`, ruled: hold and
/// alert): after an upgrade of the origin token, a directed deposit of it waits in
/// `CND-TokenUpgraded` with nothing ordered, and one that could never be minted is not returned;
/// a directed deposit from before the upgrade, and a requester's deposit, are not held. The store
/// refuses a mint order for the held deposit. One operator alone cannot lift the hold; once a
/// second approves the lift, the held deposit resumes and is minted, and the never-mintable arrival
/// is returned.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_8_an_upgrade_holds_directed_intake_until_two_operators_lift_it(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    final_directed(&w.admin, 20, 0, 5_000_000).await;
    let upgrade = upgraded(&w, 30).await;
    final_directed(&w.admin, 40, 0, 5_000_000).await;
    // Above the route's maximum: never mintable, and returned only once the hold is lifted.
    final_directed(&w.admin, 41, 0, 2_000_000_000).await;
    let r = arrival(&w.admin, 42, 5_000_000).await;
    decide(&w.admin, r, "final", 44).await;
    let requested = deposit(&w.admin, 42, 5_000_000).await;
    w.settle(t0()).await;

    let (before, _) = bound(&w, 20, 0)
        .await
        .expect("the deposit before the upgrade");
    assert!(
        order(&w.admin, before, 1).await.is_some(),
        "made before the upgrade: not held"
    );
    assert!(
        order(&w.admin, requested, 1).await.is_some(),
        "a requester's deposit: not held"
    );
    let (held, _) = bound(&w, 40, 0)
        .await
        .expect("the deposit after the upgrade");
    assert_eq!(
        w.state(held).await,
        ("Source-Final".into(), Some("CND-TokenUpgraded".into()))
    );
    assert!(order(&w.admin, held, 1).await.is_none(), "nothing ordered");
    assert!(bound(&w, 41, 0).await.is_none(), "nothing returned");
    assert_eq!(returns(&w).await, 0);

    // The store refuses a mint order for the held deposit, whoever but the owner writes it: the
    // order made for the deposit before the upgrade, copied onto it, as the core's own login.
    let (_, payload) = order(&w.admin, before, 1).await.unwrap();
    let refused = sqlx::query(
        "INSERT INTO intents (transfer_id, network, purpose, on_chain_reference, \
             reference_scope, attempt, payload) \
         SELECT id, $2, 'mint', on_chain_reference, redeemer, 1, $3 FROM transfers \
         WHERE id = $1",
    )
    .bind(held)
    .bind(ISSUING)
    .bind(&payload)
    .execute(w.core.pool())
    .await
    .expect_err("a mint order for a held deposit");
    assert!(
        refused.as_database_error().is_some_and(
            |e| e.code().as_deref() == Some("BZ001") && e.message().contains("REQ-AS-12")
        ),
        "{refused}"
    );

    // One operator requests the lift; the same login cannot approve it.
    let request: i64 = sqlx::query_scalar(
        "SELECT operator_request_token_hold_lift($1, 'fork suite run against the new implementation')",
    )
    .bind(upgrade)
    .fetch_one(&w.ops[0])
    .await
    .unwrap();
    sqlx::query_scalar::<_, i64>("SELECT operator_approve_token_hold_lift($1, 'checked')")
        .bind(request)
        .fetch_one(&w.ops[0])
        .await
        .expect_err("the requester approves its own lift");
    w.settle(t0()).await;
    assert_eq!(
        w.state(held).await,
        ("Source-Final".into(), Some("CND-TokenUpgraded".into())),
        "one operator is not enough"
    );
    sqlx::query_scalar::<_, i64>("SELECT operator_approve_token_hold_lift($1, 'checked')")
        .bind(request)
        .fetch_one(&w.ops[1])
        .await
        .unwrap();
    w.settle(t0()).await;
    assert_eq!(w.state(held).await.1, None, "the hold is lifted");
    assert!(
        order(&w.admin, held, 1).await.is_some(),
        "and the deposit is minted"
    );
    let (returned, kind) = bound(&w, 41, 0).await.expect("the return, now");
    assert_eq!(kind, "refund");
    assert!(returned > 0);
}

/// `REQ-AS-12`, `REQ-LM-4` (amendment 0031; review M2 of PR #142): directed deposits held by an
/// upgrade of their token carry nothing, so they are not in flight: with capacity 2 and two held, a
/// requester's deposit of the token still finds room (the ingress answers `ERR-Overloaded` only at
/// `in_flight >= capacity`). Once two operators lift the hold, the held deposits take their places
/// in flight only as the route has room: the first beside the requester's, the second waiting in
/// `CND-DepositCapacity`.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_as_12_deposits_held_by_an_upgrade_are_not_in_flight(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    capacity(&w, 2).await;
    let upgrade = upgraded(&w, 30).await;
    final_directed(&w.admin, 40, 0, 5_000_000).await;
    final_directed(&w.admin, 41, 0, 5_000_000).await;
    w.settle(t0()).await;
    let mut held = Vec::new();
    for block in [40, 41] {
        let (id, _) = bound(&w, block, 0).await.expect("a deposit");
        assert_eq!(
            w.state(id).await,
            ("Source-Final".into(), Some("CND-TokenUpgraded".into()))
        );
        held.push(id);
    }
    assert_eq!(in_flight(&w).await, 0, "the held carry nothing");
    // A requester's deposit of the token is taken in that room.
    let r = arrival(&w.admin, 42, 5_000_000).await;
    decide(&w.admin, r, "final", 44).await;
    let requested = deposit(&w.admin, 42, 5_000_000).await;
    assert_eq!(in_flight(&w).await, 1, "the request is accepted: 1 of 2");

    let request: i64 = sqlx::query_scalar(
        "SELECT operator_request_token_hold_lift($1, 'fork suite run against the new implementation')",
    )
    .bind(upgrade)
    .fetch_one(&w.ops[0])
    .await
    .unwrap();
    sqlx::query_scalar::<_, i64>("SELECT operator_approve_token_hold_lift($1, 'checked')")
        .bind(request)
        .fetch_one(&w.ops[1])
        .await
        .unwrap();
    w.cycle(t0()).await;
    assert_eq!(
        w.state(held[0]).await,
        ("Source-Final".into(), None),
        "room beside the request"
    );
    assert_eq!(
        w.state(held[1]).await,
        ("Source-Final".into(), Some("CND-DepositCapacity".into())),
        "no room left: it waits for capacity, not past it"
    );
    assert_eq!(in_flight(&w).await, 3, "two moving, one waiting in turn");
    assert_ne!(w.state(requested).await.0, "Refused");
    assert_eq!(returns(&w).await, 0);
}

/// `REQ-LM-4`, `REQ-AS-12` (amendment 0031; review L-a of PR #142): a directed deposit held by an
/// upgrade of its token after it left `Source-Final` (here in `Needs-Review`, on a resolution's
/// delivery, the upgrade observed late) was in flight already, and stays in flight while held, as
/// the ingress counts it: the route at capacity 1 stays full, so no request takes its place, and the
/// lift, which checks no capacity outside `Source-Final`, orders the resolution's delivery and never
/// puts the route past it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_lm_4_a_deposit_held_by_an_upgrade_after_source_final_stays_in_flight(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, past) = directed_in_review_for_an_unknown(&w).await;
    capacity(&w, 1).await;
    assert_eq!(in_flight(&w).await, 1);
    // The upgrade, at a block before the deposit's arrival, is recorded only now.
    let upgrade = upgraded(&w, 5).await;
    let action = resolution(&w, id, "Delivering", ISSUING, past, 0).await;
    approve_as(&w, 1, action).await.unwrap();
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Needs-Review".into(), Some("CND-TokenUpgraded".into()))
    );
    assert_eq!(
        in_flight(&w).await,
        1,
        "held after Source-Final, still in flight: a request finds the route full"
    );
    let request: i64 = sqlx::query_scalar(
        "SELECT operator_request_token_hold_lift($1, 'fork suite run against the new implementation')",
    )
    .bind(upgrade)
    .fetch_one(&w.ops[0])
    .await
    .unwrap();
    sqlx::query_scalar::<_, i64>("SELECT operator_approve_token_hold_lift($1, 'checked')")
        .bind(request)
        .fetch_one(&w.ops[1])
        .await
        .unwrap();
    let before = order(&w.admin, id, 1).await.map(|(o, _)| o);
    w.settle(t0()).await;
    assert_eq!(w.state(id).await, ("Needs-Review".into(), None));
    let after = order(&w.admin, id, 1).await.map(|(o, _)| o);
    assert!(
        after.is_some() && after != before,
        "the resolution's delivery is ordered once the hold lifts"
    );
    assert_eq!(in_flight(&w).await, 1, "never past the capacity");
    assert_eq!(returns(&w).await, 0);
}

/// A directed deposit whose first order was never prepared, past the route's deadline: in review
/// from `Source-Final`, with no condition (`REQ-EO-3`), an unknown. Returns it and the final head
/// (`ISSUING`, block) past the deadline.
async fn directed_in_review_for_an_unknown(w: &World) -> (i64, i64) {
    final_directed(&w.admin, 10, 0, 5_000_000).await;
    w.cycle(t0()).await;
    let (id, _) = bound(w, 10, 0).await.expect("the deposit");
    assert!(order(&w.admin, id, 1).await.is_some());
    head(&w.admin, ISSUING, 20).await;
    let past = 20 + BUDGET + 1;
    head(&w.admin, ISSUING, past).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await, ("Needs-Review".into(), None));
    let open: bool = scalar(
        &w.admin,
        &format!("SELECT directed_review_refund_open({id})"),
    )
    .await;
    assert!(!open, "an unknown opens nothing");
    (id, past)
}

/// A resolution of `id`'s review into `Delivering`, resting on the issuing network's head `past`,
/// with its second approver, run: its mint order is recorded. Returns the action and the order.
async fn resolved_into_delivering(w: &World, id: i64, past: i64) -> (i64, i64) {
    let before = order(&w.admin, id, 1).await.map(|(o, _)| o);
    let action = resolution(w, id, "Delivering", ISSUING, past, 0).await;
    approve_as(w, 1, action).await.unwrap();
    w.cycle(t0()).await;
    let (o, _) = order(&w.admin, id, 1)
        .await
        .expect("the resolution's order");
    assert_ne!(Some(o), before, "a new order, the resolution's");
    (action, o)
}

/// `AC-A6.9`, `REQ-RV-2`, `REQ-GV-5`, `REQ-RF-1` (amendment 0031, `d-f018-review-exit-later`): a
/// directed deposit that entered review for an unknown, and whose resolution's delivery the issuing
/// network then refuses for its destination (the asset Live), is judged on that evidence: it may be
/// refunded, by a resolution with its second approver, which supersedes the resolution into
/// `Delivering` that waits on the refusal.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_9_a_review_for_an_unknown_then_a_live_refusal_is_refundable_with_two_approvers(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, past) = directed_in_review_for_an_unknown(&w).await;
    let (delivering, o) = resolved_into_delivering(&w, id, past).await;
    prepare(&w.admin, o, Some("Kreivo(Assets(CannotMint))")).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    let open: bool = scalar(
        &w.admin,
        &format!("SELECT directed_review_refund_open({id})"),
    )
    .await;
    assert!(
        open,
        "the refusal made since the review began is the evidence"
    );

    let action = resolution_on_refusal(&w, id, 0).await;
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await.0,
        "Needs-Review",
        "REQ-GV-5: one approver"
    );
    assert_eq!(returns(&w).await, 0);
    approve_as(&w, 1, action).await.unwrap();
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Refunding");
    assert_eq!(evidence_refused(&w, delivering).await, 1, "superseded");
    let (refund, destination): (i64, String) =
        sqlx::query_as("SELECT id, destination FROM transfers WHERE refund_of = $1")
            .bind(id)
            .fetch_one(&w.admin)
            .await
            .unwrap();
    assert!(refund > 0);
    assert_eq!(destination, REFUND);
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM unsigned_attempt_cancellations \
               WHERE transfer_id = {id} AND attempt = 1 AND manual_action_id = {action}"
        ))
        .await,
        1,
        "the unsigned order is closed to signing"
    );
}

/// `AC-A6.9`, `REQ-RF-1` (amendment 0031): once a later dry run of the first mint passes, the
/// refusal before it is no longer the evidence: the edge closes, and an approved resolution into
/// `Refunding` does not run.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_9_a_later_passing_dry_run_closes_the_exit(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, past) = directed_in_review_for_an_unknown(&w).await;
    let (_, o) = resolved_into_delivering(&w, id, past).await;
    prepare(&w.admin, o, Some("Kreivo(Assets(CannotMint))")).await;
    let open: bool = scalar(
        &w.admin,
        &format!("SELECT directed_review_refund_open({id})"),
    )
    .await;
    assert!(open);
    // The account can receive now: the next dry run, at a later block, passes.
    prepare_dry_run_at(
        &w.admin,
        o,
        true,
        json!({"outcome": "passed", "reason": null}),
        2,
    )
    .await;
    let open: bool = scalar(
        &w.admin,
        &format!("SELECT directed_review_refund_open({id})"),
    )
    .await;
    assert!(!open, "a passing dry run closes the edge");
    let action = resolution_on_refusal(&w, id, 0).await;
    approve_as(&w, 1, action).await.unwrap();
    w.settle(t0()).await;
    assert_ne!(w.state(id).await.0, "Refunding");
    assert_eq!(returns(&w).await, 0);
    assert_eq!(
        w.count("SELECT count(*) FROM transfers WHERE refund_of IS NOT NULL")
            .await,
        0
    );
}

/// `AC-A6.9`, `REQ-RF-1`, guardrail 4 (amendment 0031): a refusal of the destination recorded
/// before a review that began for an unknown is stale; and a resolution's dry run refused for
/// something bezent cannot name is newer still. Neither opens the edge: an approved resolution into
/// `Refunding` does not run, and nothing is refunded.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_9_a_stale_refusal_followed_by_an_unknown_keeps_the_exit_closed(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    final_directed(&w.admin, 10, 0, 5_000_000).await;
    w.cycle(t0()).await;
    let (id, _) = bound(&w, 10, 0).await.expect("the deposit");
    let (o, _) = order(&w.admin, id, 1).await.expect("the mint order");
    prepare(&w.admin, o, Some("Kreivo(Assets(CannotMint))")).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-DestinationRefused".into()))
    );
    // Then the declared list stops reading back: the review begins for an unknown.
    let typo = DESTINATION.replace('Q', "R");
    exec(
        &w.admin,
        &format!(
            "INSERT INTO ledger_accounts (network, address, reason) \
             VALUES ('{ISSUING}', '{typo}', 'a ledger holds value here')"
        ),
    )
    .await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    let open: bool = scalar(
        &w.admin,
        &format!("SELECT directed_review_refund_open({id})"),
    )
    .await;
    assert!(!open, "the refusal before the unknown is stale");

    // A resolution's delivery, refused for an error bezent cannot name, at a later block.
    let past = 12 + BUDGET + 1;
    head(&w.admin, ISSUING, past).await;
    let (_, o2) = resolved_into_delivering(&w, id, past).await;
    prepare_dry_run_at(
        &w.admin,
        o2,
        false,
        json!({"outcome": "refused", "reason": "Kreivo(UnknownError)"}),
        2,
    )
    .await;
    w.cycle(t0()).await;
    let open: bool = scalar(
        &w.admin,
        &format!("SELECT directed_review_refund_open({id})"),
    )
    .await;
    assert!(!open, "the newest dry run is an unknown");
    let action = resolution(&w, id, "Refunding", ISSUING, past, 0).await;
    approve_as(&w, 1, action).await.unwrap();
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(returns(&w).await, 0);
    assert_eq!(
        w.count("SELECT count(*) FROM unsigned_attempt_cancellations")
            .await,
        0
    );
}

/// `REQ-RV-2`, `REQ-RF-1` (amendment 0031, `d-f018-review-exit-later`; review L1 of PR #142): a
/// resolution into `Refunding` that fails its own evidence check (it names another block time than
/// the recorded refusal's) is refused and supersedes nothing: the earlier resolution into
/// `Delivering`, waiting on the refusal, stands, and the deposit stays in review. One naming the
/// recorded time then supersedes it and runs.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rv_2_a_refund_refused_for_its_evidence_supersedes_nothing(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, past) = directed_in_review_for_an_unknown(&w).await;
    let (delivering, o) = resolved_into_delivering(&w, id, past).await;
    prepare(&w.admin, o, Some("Kreivo(Assets(CannotMint))")).await;
    w.cycle(t0()).await;
    let open: bool = scalar(
        &w.admin,
        &format!("SELECT directed_review_refund_open({id})"),
    )
    .await;
    assert!(open);

    let typed = resolution_on_refusal_at(&w, id, 0, Some(t0() + Duration::hours(1))).await;
    approve_as(&w, 1, typed).await.unwrap();
    w.cycle(t0()).await;
    assert_eq!(evidence_refused(&w, typed).await, 1, "its evidence fails");
    assert_eq!(
        evidence_refused(&w, delivering).await,
        0,
        "the earlier resolution is untouched"
    );
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(evidence_refused(&w, delivering).await, 0);
    assert_eq!(returns(&w).await, 0);
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM transfers WHERE refund_of = {id}"
        ))
        .await,
        0
    );

    let action = resolution_on_refusal(&w, id, 0).await;
    approve_as(&w, 1, action).await.unwrap();
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Refunding");
    assert_eq!(evidence_refused(&w, delivering).await, 1, "superseded now");
}

/// The refund destination of transfer `id` set to (`network`, `destination`), or cleared, past the
/// store's guards (the triggers off for this one transaction, as only a superuser can): a state the
/// store never allows, made to test the core's defence when a refund cannot be made.
async fn force_refund_destination(w: &World, id: i64, to: Option<(&str, &str)>) {
    let mut tx = w.admin.begin().await.unwrap();
    sqlx::query("SET LOCAL session_replication_role = replica")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE transfers SET refund_destination_network = $2, refund_destination = $3 \
         WHERE id = $1",
    )
    .bind(id)
    .bind(to.map(|(n, _)| n))
    .bind(to.map(|(_, d)| d))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

/// `REQ-RV-2`, `REQ-RF-2`, `INV-8` (amendment 0031, `d-f018-review-exit-later`; review L1 of PR
/// #142): an approved resolution into `Refunding` that passes its checks, but whose refund cannot be
/// made (here the deposit has no refund destination), commits nothing: the step fails and rolls
/// back, so the earlier resolution into `Delivering` is not superseded and its order is not
/// cancelled without the refund. Once the refund can be made, it runs and supersedes.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rv_2_a_supersede_is_never_committed_without_its_refund(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, past) = directed_in_review_for_an_unknown(&w).await;
    let (delivering, o) = resolved_into_delivering(&w, id, past).await;
    prepare(&w.admin, o, Some("Kreivo(Assets(CannotMint))")).await;
    w.cycle(t0()).await;
    let (network, destination): (String, String) = sqlx::query_as(
        "SELECT refund_destination_network, refund_destination FROM transfers WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&w.admin)
    .await
    .unwrap();
    force_refund_destination(&w, id, None).await;

    let action = resolution_on_refusal(&w, id, 0).await;
    approve_as(&w, 1, action).await.unwrap();
    let report = w.core.cycle(t0()).await.expect("cycle");
    assert_eq!(report.failed, 1, "the step fails: {report:?}");
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(
        evidence_refused(&w, delivering).await,
        0,
        "not superseded without the refund"
    );
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM unsigned_attempt_cancellations WHERE transfer_id = {id}"
        ))
        .await,
        0,
        "its order not cancelled without the refund"
    );
    assert_eq!(returns(&w).await, 0);

    force_refund_destination(&w, id, Some((&network, &destination))).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Refunding");
    assert_eq!(evidence_refused(&w, delivering).await, 1, "superseded now");
    assert_eq!(returns(&w).await, 1);
}
