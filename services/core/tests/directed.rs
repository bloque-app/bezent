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
use chrono::Duration;
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

/// What the EVM endpoint records for a directed arrival at (`ORIGIN`, `block`, log `index`): one
/// observation at the `Transfer`'s coordinate, at the reserve, with the issuing network's recipient
/// and the refund address in its detail (T-018-05).
async fn directed_to(pool: &PgPool, block: i64, index: i64, amount: u128, recipient: &str) -> i64 {
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
    // a second on, and b, then c, take theirs.
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
         VALUES ('{bridge}', 'again', 'refund', '{ASSET}', 2000000000, '{ORIGIN}', '{REFUND}', \
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
    prepare(&w.admin, o, Some("Kreivo(CannotCreate)")).await;
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
    let action = resolution(&w, id, "Refunding", ISSUING, past, 0).await;
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
    prepare(&w.admin, o, Some("Kreivo(CannotCreate)")).await;
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
