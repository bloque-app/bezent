// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-003 T-003-02, T-003-03, T-003-04, T-003-07: the core's cycle over durable state, deposits to
//! the mint, withdrawals to the payout and the refund mint, and the signatures the core verifies.
//! Each test is named after the acceptance criterion or invariant it verifies, at the service
//! layer (`INV-6`, `INV-7`, `INV-16` are the service's, SPEC §9).

mod common;

use bezent_core::State;
use chrono::Duration;
use common::*;
use serde_json::json;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

// ---------------------------------------------------------------------------------------------
// T-003-02: one table, idempotent cycles.

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_tr_6_the_core_and_the_store_hold_one_transition_table(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    for from in State::ALL {
        for to in State::ALL {
            let store: Option<bool> = sqlx::query_scalar("SELECT transfer_edge($1, $2)")
                .bind(from.as_str())
                .bind(to.as_str())
                .fetch_one(&w.admin)
                .await
                .unwrap();
            assert_eq!(bezent_core::edge(from, to), store, "{from} -> {to}");
        }
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ar_1_a_restart_reprocesses_nothing_twice(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let now = t0() + Duration::hours(1);
    let a = arrival(&w.admin, 10, 5_000).await;
    decide(&w.admin, a, "final", 12).await;
    let id = deposit(&w.admin, 10, 5_000).await;
    redemption(&w.admin, 20, &[9; 32], 700).await;
    reserve(&w.admin, 1_000_000).await;
    w.cycle(now).await;
    deliver(&w, id, 1, now).await;
    w.settle(now).await;
    let tally = || async {
        (
            w.count("SELECT count(*) FROM transitions").await,
            w.count("SELECT count(*) FROM intents").await,
            w.count("SELECT count(*) FROM transfers").await,
            w.count("SELECT count(*) FROM event_log").await,
        )
    };
    let before = tally().await;
    // The same core again, and a core restarted from the store: nothing new.
    let again = w.cycle(now).await;
    assert_eq!((again.moved, again.intents, again.withdrawals), (0, 0, 0));
    let restarted = w.restarted().await;
    let report = restarted.cycle(now).await.unwrap();
    assert_eq!(
        (report.moved, report.intents, report.withdrawals),
        (0, 0, 0)
    );
    assert_eq!(tally().await, before);
    assert_eq!(w.state(id).await.0, "Delivering");
}

// ---------------------------------------------------------------------------------------------
// T-003-03: deposits to the mint (INV-2, INV-6, AC-A2.*, AC-A4.*).

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a2_1_an_arrival_observed_but_not_final_mints_nothing(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    arrival(&w.admin, 10, 5_000).await;
    let id = deposit(&w.admin, 10, 5_000).await;
    head(&w.admin, ORIGIN, 11).await;
    w.settle(t0() + Duration::minutes(5)).await;
    assert_eq!(w.state(id).await, ("Accepted".into(), None));
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a2_2_a_final_bound_arrival_is_minted_in_full_with_its_reference(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let now = t0() + Duration::minutes(5);
    let a = arrival(&w.admin, 10, 5_000).await;
    let id = deposit(&w.admin, 10, 5_000).await;
    decide(&w.admin, a, "final", 12).await;

    // Source-Final at the final point, then the order: exactly its amount, to the request's
    // destination, carrying the reference derived from the arrival (REQ-IN-3, REQ-IN-4).
    w.cycle(now).await;
    assert_eq!(w.state(id).await.0, "Source-Final");
    w.cycle(now).await;
    let (_, payload) = order(&w.admin, id, 1).await.expect("the mint order");
    assert_eq!(
        payload,
        json!({"order": {"asset": ISSUING_ASSET, "to": DESTINATION_ACCOUNT}, "amount": "5000"})
    );
    let reference = reference_of(&w.admin, id).await;
    assert_eq!(reference.len(), 32);
    let coordinate = bezent_core_service::refs::Coordinate {
        network: ORIGIN.into(),
        block_number: 10,
        block_hash: hash(ORIGIN, 10),
        position_kind: "log".into(),
        position_index: 0,
    };
    assert_eq!(
        reference,
        bezent_core_service::refs::mint_reference(&coordinate).to_vec()
    );

    // The endpoint prepares it; the core records the signing intent over those exact bytes
    // (INV-9); the signer signs, the endpoint persists; the core verifies and moves (AC-I1.2).
    let (o, _) = order(&w.admin, id, 1).await.unwrap();
    let prepared = prepare(&w.admin, o, None).await;
    w.cycle(now).await;
    let (s, payload) = signing_intent(&w.admin, id, 1)
        .await
        .expect("signing intent");
    let message: Vec<u8> = scalar(
        &w.admin,
        &format!("SELECT message FROM prepared_transactions WHERE id = {prepared}"),
    )
    .await;
    assert_eq!(
        payload,
        json!({"message": format!("0x{}", hex::encode(&message)), "amount": "5000", "prepared": prepared})
    );
    assert_eq!(w.state(id).await.0, "Source-Final");
    sign(&w, s).await;
    w.cycle(now).await;
    assert_eq!(w.state(id).await.0, "Delivering");

    // Minted, included, then final: Completed, with three distinct milestones (REQ-TR-4).
    let minted = observe(
        &w.admin,
        ISSUING,
        40,
        0,
        ISSUING_ASSET,
        5_000,
        None,
        Some(DESTINATION_ACCOUNT),
        Some(&reference),
        true,
        json!({"event": "Minted"}),
    )
    .await;
    w.cycle(now).await;
    assert_eq!(w.state(id).await.0, "Delivered");
    decide(&w.admin, minted, "final", 45).await;
    w.cycle(now).await;
    assert_eq!(w.state(id).await.0, "Completed");
    let milestones: (i64, i64, i64) = sqlx::query_as(
        "SELECT source_final_block_number, destination_included_block_number, \
                destination_final_block_number FROM transfers WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(milestones, (12, 40, 45));
    // One order, one signing intent, one signature: minted once (REQ-EO-2).
    assert_eq!(w.count("SELECT count(*) FROM signatures").await, 1);
}

/// Alice under the Polkadot prefix: `DESTINATION`'s account, spelled differently.
const DESTINATION_OTHER_PREFIX: &str = "15oF4uVJwmo4TdGW7VfQxNLavjCXviqxT9S1MgbjMNHr6Sp5";

/// A final deposit to `DESTINATION` (Alice, prefix 42), with `entry` declared on the issuing
/// network; settled once. Returns the world and the transfer.
async fn deposit_with_ledger_entry(
    options: PgPoolOptions,
    connect: PgConnectOptions,
    entry: &str,
) -> (World, i64) {
    let w = World::new(options, connect).await;
    let a = arrival(&w.admin, 10, 5_000).await;
    let id = deposit(&w.admin, 10, 5_000).await;
    decide(&w.admin, a, "final", 12).await;
    // The destination was taken in before the account was declared (or the ingress was bypassed):
    // the core checks again before it orders a mint.
    exec(
        &w.admin,
        &format!(
            "INSERT INTO ledger_accounts (network, address, reason) \
             VALUES ('{ISSUING}', '{entry}', 'a ledger holds value here')"
        ),
    )
    .await;
    w.settle(t0() + Duration::minutes(5)).await;
    (w, id)
}

async fn assert_in_review_unminted(w: &World, id: i64, reason: &str) {
    // Not left in Source-Final: an operator matter, with the reason recorded. Never minted.
    assert_eq!(
        w.state(id).await,
        ("Needs-Review".into(), Some("CND-DestinationRefused".into()))
    );
    assert!(order(&w.admin, id, 1).await.is_none());
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM event_log WHERE kind = 'destination-unbindable' \
               AND payload ->> 'reason' = '{reason}'"
        ))
        .await,
        1
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_5_no_mint_is_ordered_to_a_listed_account_declared_as_a_raw_key(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let (w, id) = deposit_with_ledger_entry(options, connect, DESTINATION_ACCOUNT).await;
    assert_in_review_unminted(&w, id, "destination-is-a-ledger-account").await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_5_no_mint_is_ordered_to_a_listed_account_declared_under_another_prefix(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let (w, id) = deposit_with_ledger_entry(options, connect, DESTINATION_OTHER_PREFIX).await;
    assert_in_review_unminted(&w, id, "destination-is-a-ledger-account").await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_5_no_mint_is_ordered_to_a_listed_account_declared_as_the_same_ss58(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let (w, id) = deposit_with_ledger_entry(options, connect, DESTINATION).await;
    assert_in_review_unminted(&w, id, "destination-is-a-ledger-account").await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_5_a_list_the_core_cannot_read_back_sends_the_transfer_to_review(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // A typo the store cannot see (the SS58 checksum): nothing fails open.
    let typo = DESTINATION.replace('Q', "R");
    let (w, id) = deposit_with_ledger_entry(options, connect, &typo).await;
    assert_in_review_unminted(&w, id, "destination-or-list-unreadable").await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_5_an_unlisted_account_is_minted_to_again(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let a = arrival(&w.admin, 10, 5_000).await;
    let id = deposit(&w.admin, 10, 5_000).await;
    decide(&w.admin, a, "final", 12).await;
    for (kind, reason) in [
        ("declared", "a ledger holds value here"),
        ("unlisted", "by mistake"),
    ] {
        exec(
            &w.admin,
            &format!(
                "INSERT INTO ledger_accounts (network, address, kind, reason) \
                 VALUES ('{ISSUING}', '{DESTINATION_ACCOUNT}', '{kind}', '{reason}')"
            ),
        )
        .await;
    }
    w.settle(t0() + Duration::minutes(5)).await;
    assert_eq!(w.state(id).await.0, "Source-Final");
    assert!(order(&w.admin, id, 1).await.is_some());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a2_3_an_observation_withdrawn_before_finality_is_withdrawn_and_mints_nothing(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let a = arrival(&w.admin, 10, 5_000).await;
    let id = deposit(&w.admin, 10, 5_000).await;
    decide(&w.admin, a, "withdrawn", 12).await;
    w.settle(t0() + Duration::minutes(5)).await;
    assert_eq!(w.state(id).await, ("Withdrawn".into(), None));
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_6_inv_16_no_mint_rests_on_an_observation_that_is_not_final(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // A deposit inserted at Source-Final (REQ-IN-3) on an arrival that is not final: the store
    // refuses it whoever inserts it (review M1 of PR #56), and the core, which checks the arrival
    // again before it orders a mint, has nothing to mint against.
    let w = World::new(options, connect).await;
    arrival(&w.admin, 10, 5_000).await;
    let refused = sqlx::query(&format!(
            "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                 destination_network, destination, state, source_network, source_block_number, \
                 source_block_hash, source_position_kind, source_position_index, \
                 source_final_network, source_final_block_number, source_final_block_hash, \
                 source_final_block_time, source_final_seen_at) \
             VALUES ('bz-d-x', 'req-x', 'deposit', '{ASSET}', 5000, '{ISSUING}', '{DESTINATION}', \
                 'Source-Final', '{ORIGIN}', 10, '\\x{}', 'log', 0, '{ORIGIN}', 30, '\\x30', now(), now())",
        hex::encode(hash(ORIGIN, 10))
    ))
    .execute(&w.admin)
    .await
    .expect_err("a deposit at Source-Final on an arrival that is not final");
    assert!(refused.to_string().contains("INV-6"), "{refused}");
    w.settle(t0() + Duration::minutes(5)).await;
    assert_eq!(w.count("SELECT count(*) FROM transfers").await, 0);
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a4_1_an_arrival_that_cannot_become_a_deposit_is_unassociated_with_an_owner(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    // A token not admitted, at the reserve; and an admitted one that no request binds.
    let other = observe(
        &w.admin,
        ORIGIN,
        10,
        0,
        "usdc.e",
        5_000,
        Some("0xfunder"),
        Some(RESERVE),
        None,
        true,
        json!({}),
    )
    .await;
    decide(&w.admin, other, "final", 12).await;
    let unbound = arrival(&w.admin, 13, 7).await;
    decide(&w.admin, unbound, "final", 14).await;
    w.settle(t0() + Duration::minutes(5)).await;
    let rows: Vec<(i64, Option<String>, String)> = sqlx::query_as(
        "SELECT observation_id, admitted_for, owner FROM unassociated_arrivals ORDER BY observation_id",
    )
    .fetch_all(&w.admin)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            (other, None, "operations".to_owned()),
            (unbound, Some(ASSET.to_owned()), "operations".to_owned())
        ]
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a4_2_an_unassociated_arrival_is_never_minted_and_leaves_only_by_a_two_person_release(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let now = t0() + Duration::minutes(5);
    let other = observe(
        &w.admin,
        ORIGIN,
        10,
        0,
        "usdc.e",
        5_000,
        Some("0xfunder"),
        Some(RESERVE),
        None,
        true,
        json!({}),
    )
    .await;
    decide(&w.admin, other, "final", 12).await;
    w.settle(now).await;
    // Not owed mint, not minted, not absorbed (REQ-IN-6).
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
    assert_eq!(w.count("SELECT count(*) FROM book_postings").await, 0);
    // Its refund, as the ingress takes it in, waits for the operator's decision.
    let refund: i64 = scalar(
        &w.admin,
        &format!(
            "INSERT INTO transfers (bridge_reference, request_reference, kind, token, amount, \
                 destination_network, destination, state, source_network, source_block_number, \
                 source_block_hash, source_position_kind, source_position_index) \
             VALUES ('bz-u-1', 'r-1', 'refund', 'usdc.e', 5000, '{ORIGIN}', '{BENEFICIARY}', \
                 'Received', '{ORIGIN}', 10, '\\x{}', 'log', 0) RETURNING id",
            hex::encode(hash(ORIGIN, 10))
        ),
    )
    .await;
    let action: i64 = scalar(
        &w.ops[0],
        &format!(
            "INSERT INTO manual_actions (kind, transfer_id, justification) \
             VALUES ('release-unassociated', {refund}, 'the funder asked') RETURNING id"
        ),
    )
    .await;
    w.settle(now).await;
    assert_eq!(w.state(refund).await.0, "Received");
    approve_as(&w, 1, action).await.unwrap();
    w.cycle(now).await;
    w.cycle(now).await;
    assert_eq!(w.state(refund).await.0, "Source-Final");
}

// ---------------------------------------------------------------------------------------------
// T-003-04: withdrawals to the payout, and the refund mint (INV-7, INV-8, REQ-RF-*, AC-B1.*).

/// A withdrawal made from a final redemption, with reserve to cover it; returns its id.
async fn withdrawal(w: &World, block: i64, reference: [u8; 32], amount: u128) -> i64 {
    let r = redemption(&w.admin, block, &reference, amount).await;
    decide(&w.admin, r, "final", block + 2).await;
    w.cycle(t0()).await;
    scalar(
        &w.admin,
        &format!(
            "SELECT id FROM transfers WHERE kind = 'withdrawal' AND on_chain_reference = '\\x{}'",
            hex::encode(reference)
        ),
    )
    .await
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_b1_1_nothing_is_paid_out_before_the_redemption_is_final(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    let r = redemption(&w.admin, 20, &[1; 32], 700).await;
    w.settle(t0()).await;
    let (id, redeemer, destination, network): (i64, String, String, String) = sqlx::query_as(
        "SELECT id, redeemer, destination, destination_network FROM transfers WHERE kind = 'withdrawal'",
    )
    .fetch_one(&w.admin)
    .await
    .unwrap();
    // Bound to its redemption by (redeemer, reference), never by amount or time (REQ-OT-2).
    assert_eq!(
        (redeemer.as_str(), destination.as_str(), network.as_str()),
        (REDEEMER, BENEFICIARY, ORIGIN)
    );
    assert_eq!(w.state(id).await.0, "Accepted");
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
    decide(&w.admin, r, "final", 22).await;
    w.cycle(t0()).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Source-Final");
    let (_, payload) = order(&w.admin, id, 1).await.expect("the payout order");
    assert_eq!(payload["amount"], "700");
    assert_eq!(payload["order"]["to"], BENEFICIARY);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_b1_2_a_payout_final_and_executed_in_full_completes_the_withdrawal(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    let id = withdrawal(&w, 20, [2; 32], 700).await;
    deliver(&w, id, 1, t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    let paid = observe(
        &w.admin,
        ORIGIN,
        50,
        3,
        TOKEN,
        700,
        None,
        Some(BENEFICIARY),
        Some(&payout_reference_of(&w.admin, id).await),
        true,
        json!({"event": "Payout"}),
    )
    .await;
    decide(&w.admin, paid, "final", 60).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await, ("Completed".into(), None));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_b1_3_a_payout_proven_not_executed_gets_a_new_attempt_then_one_refund_mint(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    let reference = [3; 32];
    let id = withdrawal(&w, 20, reference, 700).await;
    deliver(&w, id, 1, t0()).await;

    // Attempt 1 proven not executed before the deadline: a new attempt, same reference, numbered
    // 2 (REQ-RF-5, REQ-EO-6).
    prove(&w.admin, id, 1, ORIGIN, 100).await;
    deliver(&w, id, 2, t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    let (attempt, refs): (i32, i64) = sqlx::query_as(
        "SELECT max(attempt), count(DISTINCT on_chain_reference) FROM intents WHERE transfer_id = $1",
    )
    .bind(id)
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!((attempt, refs), (2, 1));

    // Attempt 2 proven not executed past the deadline: every attempt is proven, so one refund
    // mint to the redeemer, tied to the redemption (REQ-OT-6, REQ-RF-4, INV-8).
    prove(&w.admin, id, 2, ORIGIN, 22 + BUDGET + 100).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Refunding");
    let (refund, destination, redeemer): (i64, String, String) =
        sqlx::query_as("SELECT id, destination, redeemer FROM transfers WHERE refund_of = $1")
            .bind(id)
            .fetch_one(&w.admin)
            .await
            .unwrap();
    assert_eq!(
        (destination.as_str(), redeemer.as_str()),
        (REDEEMER, REDEEMER)
    );
    w.cycle(t0()).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(refund).await.0, "Source-Final");
    deliver(&w, refund, 1, t0()).await;
    let (_, payload) = order(&w.admin, refund, 1).await.unwrap();
    assert_eq!(
        payload,
        json!({"order": {"redeemer": REDEEMER, "asset": ISSUING_ASSET}, "amount": "700"})
    );
    assert_eq!(w.state(refund).await.0, "Delivering");
    let refunded = observe(
        &w.admin,
        ISSUING,
        500,
        0,
        ISSUING_ASSET,
        700,
        None,
        Some(REDEEMER),
        Some(&reference),
        true,
        json!({"event": "RefundMinted"}),
    )
    .await;
    decide(&w.admin, refunded, "final", 505).await;
    w.settle(t0()).await;
    assert_eq!(w.state(refund).await.0, "Completed");
    assert_eq!(w.state(id).await.0, "Refunded");
    // One refund per redemption, ever.
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM transfers WHERE refund_of = {id}"
        ))
        .await,
        1
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_b1_4_a_delivery_unknown_at_its_deadline_goes_to_review_and_nothing_is_returned(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    let id = withdrawal(&w, 20, [4; 32], 700).await;
    deliver(&w, id, 1, t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    // Before the deadline (the source leg final at block 22, plus the route's budget, in chain
    // time): nothing. Past it, at a final block: review, no refund, no retry.
    let past = 22 + BUDGET + 10;
    head(&w.admin, ORIGIN, 30).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    head(&w.admin, ORIGIN, past).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(
        w.count("SELECT count(*) FROM transfers WHERE kind = 'refund'")
            .await,
        0
    );
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM intents WHERE transfer_id = {id} AND attempt > 1"
        ))
        .await,
        0
    );
    // Time alone never ends a review (REQ-RV-1).
    head(&w.admin, ORIGIN, past + 100_000).await;
    w.settle(t0() + Duration::days(30)).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_tr_5_a_delivery_of_another_amount_goes_to_review_reporting_both(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    let id = withdrawal(&w, 20, [5; 32], 700).await;
    deliver(&w, id, 1, t0()).await;
    observe(
        &w.admin,
        ORIGIN,
        50,
        3,
        TOKEN,
        650,
        None,
        Some(BENEFICIARY),
        Some(&payout_reference_of(&w.admin, id).await),
        true,
        json!({"event": "Payout"}),
    )
    .await;
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Needs-Review".into(), Some("CND-AmountMismatch".into()))
    );
    let both: (String, String) = sqlx::query_as(
        "SELECT payload ->> 'expected', payload ->> 'delivered' FROM event_log \
         WHERE kind = 'amount-mismatch' AND transfer_id = $1",
    )
    .bind(id)
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(both, ("700".into(), "650".into()));
}

// ---------------------------------------------------------------------------------------------
// T-003-07: the core verifies every returned signature (INV-9, AC-I1.2).

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_signature_not_over_the_recorded_bytes_raises_the_alert_and_moves_nothing(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let now = t0() + Duration::minutes(5);
    let a = arrival(&w.admin, 10, 5_000).await;
    decide(&w.admin, a, "final", 12).await;
    let id = deposit(&w.admin, 10, 5_000).await;
    w.cycle(now).await;
    w.cycle(now).await;
    let (o, _) = order(&w.admin, id, 1).await.unwrap();
    prepare(&w.admin, o, None).await;
    w.cycle(now).await;
    let (s, _) = signing_intent(&w.admin, id, 1).await.unwrap();
    // A signature over other bytes, recorded as if the signer had made it, with its bytes
    // persisted: the core must catch it.
    let good = w.signer.sign(o).await; // the order has no message: the signer refuses it
    assert!(matches!(good, bezent_signer::Outcome::Refused(_)));
    exec(
        &w.admin,
        &format!(
            "INSERT INTO signatures (intent_id, key_id, scheme, public_key, signature) \
             VALUES ({s}, 'kreivo-mint', 'sr25519', '\\x{}', '\\x{}'); \
             INSERT INTO signed_transactions (intent_id, attempt, bytes) VALUES ({s}, 1, '\\xbeef')",
            "11".repeat(32),
            "22".repeat(64)
        ),
    )
    .await;
    w.cycle(now).await;
    w.cycle(now).await;
    assert_eq!(w.state(id).await.0, "Source-Final");
    let alerts: Vec<String> = sqlx::query_scalar(
        "SELECT kind FROM event_log WHERE kind IN ('key-compromise', 'pause-requested') ORDER BY seq",
    )
    .fetch_all(&w.admin)
    .await
    .unwrap();
    // Raised once, with the pause request, however many cycles see it.
    assert_eq!(alerts, vec!["key-compromise", "pause-requested"]);
}

// ---------------------------------------------------------------------------------------------
// PR #39 second review MF1: the two steps of a delivery stay on one network (INV-9, PLAN DEC-3).

/// MF1 (`INV-9`): a preparation labelled with another network than its order's (the EVM
/// endpoint's `polygon` row for a Kreivo mint) is never signed over. The store refuses it
/// (migration 0036); with that guard off, as a drill could leave it, the core still records no
/// signing intent over it, and signs over the order's own network's preparation only.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn mf1_inv_9_the_core_records_no_signing_intent_over_another_networks_preparation(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let now = t0() + Duration::minutes(5);
    let a = arrival(&w.admin, 10, 5_000).await;
    let id = deposit(&w.admin, 10, 5_000).await;
    decide(&w.admin, a, "final", 12).await;
    w.cycle(now).await;
    w.cycle(now).await;
    let (o, _) = order(&w.admin, id, 1).await.expect("the mint order");
    let forged = format!(
        "INSERT INTO prepared_transactions (order_intent_id, attempt, network, lane, key_id, nonce, \
             valid_from_block, valid_until_block, checkpoint_hash, call, signing_payload, message, \
             dry_run_block_number, dry_run_block_hash, dry_run_passed, dry_run) \
         VALUES ({o}, 1, '{ORIGIN}', '\\x01', 'kreivo-mint', 0, 0, 9223372036854775807, '\\x02', \
             '\\x03', '\\x04', '\\xbadbad', 1, '\\x05', true, '{{\"outcome\": \"passed\"}}')"
    );
    let refused = sqlx::raw_sql(&forged).execute(&w.admin).await;
    assert_eq!(
        refused
            .expect_err("the store refuses another network's preparation")
            .as_database_error()
            .and_then(|d| d.code().map(|c| c.into_owned()))
            .as_deref(),
        Some(bezent_store::REFUSAL_SQLSTATE)
    );
    exec(
        &w.admin,
        &format!(
            "ALTER TABLE prepared_transactions DISABLE TRIGGER prepared_transactions_guard_order; \
             {forged}; \
             ALTER TABLE prepared_transactions ENABLE TRIGGER prepared_transactions_guard_order"
        ),
    )
    .await;
    w.cycle(now).await;
    assert_eq!(signing_intent(&w.admin, id, 1).await, None);
    assert_eq!(w.state(id).await.0, "Source-Final");

    // The order's own network's preparation is the one signed over.
    let prepared = prepare(&w.admin, o, None).await;
    w.cycle(now).await;
    let (_, payload) = signing_intent(&w.admin, id, 1)
        .await
        .expect("a signing intent over the order's own preparation");
    assert_eq!(payload["prepared"], json!(prepared));
}
