// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The findings of the review of PR #32 (F-003), each reproduced as the reviewer described it:
//! M1 (`REQ-OT-2`, `INV-2`), M2 (`INV-8`, `REQ-RF-1`, `REQ-EO-6`), S1 (`REQ-OT-5`), S2
//! (`REQ-RQ-9`), S3 (`REQ-TR-3`), S4, S6 (PLAN `DEC-3`) and S7 (`REQ-TR-2`). M3 and S5 are in
//! `review.rs` and the store's `lifecycle_graph.rs`.

mod common;

use std::time::Duration as StdDuration;

use common::*;
use serde_json::json;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// A second redeemer and its beneficiary.
const REDEEMER_B: &str = "0x1cbd2d43530a44705ad088af313e18f80b53ef16b36177cd4b77b846f2a5f07c";
const BENEFICIARY_B: &str = "0x00000000000000000000000000000000000000bb";

/// A withdrawal by `redeemer` with `reference`, made from a final redemption at `block`.
async fn withdrawal_by(
    w: &World,
    block: i64,
    redeemer: &str,
    reference: [u8; 32],
    beneficiary: &str,
) -> i64 {
    let r = redemption_by(&w.admin, block, redeemer, &reference, 700, beneficiary).await;
    decide(&w.admin, r, "final", block + 2).await;
    w.settle(t0()).await;
    sqlx::query_scalar(
        "SELECT id FROM transfers WHERE kind = 'withdrawal' AND redeemer = $1 \
           AND on_chain_reference = $2",
    )
    .bind(redeemer)
    .bind(reference.to_vec())
    .fetch_one(&w.admin)
    .await
    .unwrap()
}

/// A payout observed on the origin network at (`block`, log `index`), carrying `reference`.
async fn payout(w: &World, block: i64, index: i64, to: &str, reference: &[u8]) -> i64 {
    observe(
        &w.admin,
        ORIGIN,
        block,
        index,
        TOKEN,
        700,
        None,
        Some(to),
        Some(reference),
        true,
        json!({"event": "Payout"}),
    )
    .await
}

async fn refunds(w: &World) -> i64 {
    w.count("SELECT count(*) FROM transfers WHERE kind = 'refund'")
        .await
}

// ---------------------------------------------------------------------------------------------
// M1: a payout is matched by a reference unique to its withdrawal, and never by a pick.

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m1_req_ot_2_two_redeemers_with_one_reference_are_each_paid_by_their_own_payout(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // The reviewer's scenario: W_A (redeemer A, reference R -> X) and W_B (redeemer B, reference R
    // -> Y) live on one origin network; A's payout is observed. W_B must not take it.
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    let a = withdrawal_by(&w, 20, REDEEMER, [9; 32], BENEFICIARY).await;
    let b = withdrawal_by(&w, 30, REDEEMER_B, [9; 32], BENEFICIARY_B).await;
    deliver(&w, a, 1, t0()).await;
    deliver(&w, b, 1, t0()).await;
    assert_eq!(w.state(a).await.0, "Delivering");
    assert_eq!(w.state(b).await.0, "Delivering");
    let (ra, rb) = (
        payout_reference_of(&w.admin, a).await,
        payout_reference_of(&w.admin, b).await,
    );
    assert_ne!(ra, rb, "each withdrawal's payout writes its own reference");
    // Every payout intent carries its withdrawal's destination reference, unscoped.
    let carried: i64 = w
        .count(&format!(
            "SELECT count(*) FROM intents i JOIN transfers t ON t.id = i.transfer_id \
             WHERE t.id IN ({a}, {b}) AND (i.on_chain_reference <> t.destination_reference \
               OR i.reference_scope IS NOT NULL)"
        ))
        .await;
    assert_eq!(carried, 0);

    let paid = payout(&w, 50, 3, BENEFICIARY, &ra).await;
    w.settle(t0()).await;
    assert_eq!(w.state(a).await.0, "Delivered");
    assert_eq!(w.state(b).await, ("Delivering".into(), None));
    decide(&w.admin, paid, "final", 60).await;
    w.settle(t0()).await;
    assert_eq!(w.state(a).await.0, "Completed");
    assert_eq!(w.state(b).await, ("Delivering".into(), None));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m1_inv_2_two_legs_with_one_reference_are_unknown_never_a_pick(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    let id = withdrawal_by(&w, 20, REDEEMER, [10; 32], BENEFICIARY).await;
    deliver(&w, id, 1, t0()).await;
    let r = payout_reference_of(&w.admin, id).await;
    payout(&w, 50, 3, BENEFICIARY, &r).await;
    payout(&w, 51, 4, BENEFICIARY, &r).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    // Unknown past its deadline: review, as any unknown leg (REQ-RV-1).
    head(&w.admin, ORIGIN, 22 + BUDGET + 10).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM transitions WHERE transfer_id = {id} AND to_state = 'Delivered'"
        ))
        .await,
        0
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s4_delivered_reads_back_the_leg_its_inclusion_rested_on(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // Another event in the inclusion's block carries the same reference, recorded first: the old
    // read (by block and reference, LIMIT 1) chose it.
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    let id = withdrawal_by(&w, 20, REDEEMER, [11; 32], BENEFICIARY).await;
    deliver(&w, id, 1, t0()).await;
    let r = payout_reference_of(&w.admin, id).await;
    let decoy = observe(
        &w.admin,
        ORIGIN,
        50,
        0,
        TOKEN,
        1,
        None,
        Some(BENEFICIARY_B),
        Some(&r),
        true,
        json!({"event": "Reference"}),
    )
    .await;
    let paid = payout(&w, 50, 3, BENEFICIARY, &r).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Delivered");
    decide(&w.admin, decoy, "final", 60).await;
    decide(&w.admin, paid, "final", 60).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await, ("Completed".into(), None));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s3_req_tr_3_a_leg_to_another_recipient_is_not_executed_in_full(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    let id = withdrawal_by(&w, 20, REDEEMER, [12; 32], BENEFICIARY).await;
    deliver(&w, id, 1, t0()).await;
    let r = payout_reference_of(&w.admin, id).await;
    payout(&w, 50, 3, BENEFICIARY_B, &r).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await, ("Needs-Review".into(), None));
    let both: (String, String) = sqlx::query_as(
        "SELECT payload ->> 'expected_recipient', payload ->> 'delivered_recipient' FROM event_log \
         WHERE kind = 'recipient-mismatch' AND transfer_id = $1",
    )
    .bind(id)
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(both, (BENEFICIARY.into(), BENEFICIARY_B.into()));
}

// ---------------------------------------------------------------------------------------------
// M2: a refund only once every attempt is proven, and no attempt after it.

/// Records, as an owner would, a refund transfer of `id` and its `Refunding` transition resting
/// on the final head (`ORIGIN`, `block`), inside `tx`.
async fn refund_by_hand(
    tx: &mut sqlx::PgConnection,
    id: i64,
    from: &str,
    block: i64,
    action: Option<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, refund_of, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index, on_chain_reference, \
             redeemer, state) \
         SELECT 'bz-f-by-hand-' || id, request_reference || ':refund', 'refund', asset, amount, \
             $2, redeemer, id, source_network, source_block_number, source_block_hash, \
             source_position_kind, source_position_index, on_chain_reference, redeemer, 'Received' \
         FROM transfers WHERE id = $1",
    )
    .bind(id)
    .bind(ISSUING)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
             block_hash, block_time, manual_action_id) \
         VALUES ($1, $2::transfer_state, 'Refunding', $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(from)
    .bind(ORIGIN)
    .bind(block)
    .bind(hash(ORIGIN, block))
    .bind(t0() + chrono::Duration::seconds(block))
    .bind(action)
    .execute(&mut *tx)
    .await
    .map(|_| ())
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m2_inv_8_review_cannot_refund_while_a_later_attempt_is_signed(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // The reviewer's first entry: in Delivered, attempt 1's proof lands, attempt 2 is ordered,
    // prepared and signed; then attempt 1's inclusion is withdrawn and the transfer goes to review;
    // a resolution to Refunding resting on attempt 1's proof is refused, by the core and the store.
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    let id = withdrawal_by(&w, 20, REDEEMER, [13; 32], BENEFICIARY).await;
    deliver(&w, id, 1, t0()).await;
    let r = payout_reference_of(&w.admin, id).await;
    let paid = payout(&w, 50, 3, BENEFICIARY, &r).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Delivered");
    prove(&w.admin, id, 1, ORIGIN, 100).await;
    w.cycle(t0()).await;
    let (o2, _) = order(&w.admin, id, 2).await.expect("attempt 2 ordered");
    prepare(&w.admin, o2, None).await;
    w.cycle(t0()).await;
    let (s2, _) = signing_intent(&w.admin, id, 2)
        .await
        .expect("attempt 2's signing intent");
    sign(&w, s2).await;
    decide(&w.admin, paid, "withdrawn", 110).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Needs-Review".into(), Some("CND-Withdrawn".into()))
    );

    let action: i64 = sqlx::query_scalar(
        "INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, \
             block_hash, block_time, justification) \
         VALUES ('review-resolution', $1, 'Refunding', $2, 100, $3, $4, 'attempt 1 did not run') \
         RETURNING id",
    )
    .bind(id)
    .bind(ORIGIN)
    .bind(hash(ORIGIN, 100))
    .bind(t0() + chrono::Duration::seconds(100))
    .fetch_one(&w.ops[0])
    .await
    .unwrap();
    approve_as(&w, 1, action).await.unwrap();
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(refunds(&w).await, 0);
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM event_log WHERE kind = 'review-evidence-refused' \
               AND payload ->> 'manual_action_id' = '{action}'"
        ))
        .await,
        1
    );
    // The store refuses it too, whoever writes it.
    let mut tx = w.admin.begin().await.unwrap();
    let err = refund_by_hand(&mut tx, id, "Needs-Review", 100, Some(action))
        .await
        .expect_err("a refund beside a signed attempt");
    assert!(
        err.to_string().contains("INV-8"),
        "the refusal names INV-8: {err}"
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m2_req_rf_5_the_retry_or_refund_decision_is_made_once(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // The reviewer's second entry: attempt 1 is proven before the deadline, so attempt 2 is
    // ordered; then the route's budget is lowered. The decision stands: attempt 2, no refund.
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    let id = withdrawal_by(&w, 20, REDEEMER, [14; 32], BENEFICIARY).await;
    deliver(&w, id, 1, t0()).await;
    prove(&w.admin, id, 1, ORIGIN, 100).await;
    w.cycle(t0()).await;
    let (o2, _) = order(&w.admin, id, 2).await.expect("attempt 2 ordered");
    exec(
        &w.admin,
        "UPDATE networks SET reserve_point_lag_seconds = 1; \
         UPDATE routes SET completion_budget_seconds = 1 WHERE direction = 'withdrawal'",
    )
    .await;
    w.cycle(t0()).await;
    assert_eq!(refunds(&w).await, 0);
    prepare(&w.admin, o2, None).await;
    w.cycle(t0()).await;
    let (s2, _) = signing_intent(&w.admin, id, 2)
        .await
        .expect("signing intent");
    sign(&w, s2).await;
    w.settle(t0()).await;
    assert_eq!(refunds(&w).await, 0);
    assert_eq!(store_attempt(&w.admin, id).await, 2);
    assert_eq!(w.state(id).await.0, "Delivering");
}

async fn store_attempt(pool: &PgPool, id: i64) -> i32 {
    scalar(
        pool,
        &format!("SELECT COALESCE(max(attempt), 0) FROM transitions WHERE transfer_id = {id}"),
    )
    .await
}

/// A payout intent of attempt `attempt` for withdrawal `id`, as the core records one.
async fn record_attempt(pool: &PgPool, id: i64, attempt: i32) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO intents (transfer_id, network, purpose, on_chain_reference, reference_scope, \
             attempt, payload) \
         SELECT id, destination_network, 'payout', destination_reference, NULL, $2, \
             '{\"order\": {}, \"amount\": \"700\"}' FROM transfers WHERE id = $1",
    )
    .bind(id)
    .bind(attempt)
    .execute(pool)
    .await
    .map(|_| ())
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m2_inv_8_the_refund_closes_the_attempt_set_atomically(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;

    // A refund decided first: an attempt recorded meanwhile waits for it, then is refused.
    let a = withdrawal_by(&w, 20, REDEEMER, [15; 32], BENEFICIARY).await;
    deliver(&w, a, 1, t0()).await;
    prove(&w.admin, a, 1, ORIGIN, 100).await;
    let mut refund = w.admin.begin().await.unwrap();
    refund_by_hand(&mut refund, a, "Delivering", 100, None)
        .await
        .expect("the refund");
    let admin = w.admin.clone();
    let late = tokio::spawn(async move { record_attempt(&admin, a, 2).await });
    tokio::time::sleep(StdDuration::from_millis(300)).await;
    assert!(!late.is_finished(), "the new attempt waits for the refund");
    refund.commit().await.unwrap();
    let err = late
        .await
        .unwrap()
        .expect_err("an attempt after the refund");
    assert!(err.to_string().contains("INV-8"), "{err}");
    assert_eq!(w.state(a).await.0, "Refunding");

    // An attempt recorded first: the refund waits for it, then is refused (attempt 2 is not
    // proven).
    let b = withdrawal_by(&w, 30, REDEEMER_B, [15; 32], BENEFICIARY_B).await;
    deliver(&w, b, 1, t0()).await;
    prove(&w.admin, b, 1, ORIGIN, 101).await;
    let mut attempt = w.admin.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO intents (transfer_id, network, purpose, on_chain_reference, reference_scope, \
             attempt, payload) \
         SELECT id, destination_network, 'payout', destination_reference, NULL, 2, \
             '{\"order\": {}, \"amount\": \"700\"}' FROM transfers WHERE id = $1",
    )
    .bind(b)
    .execute(&mut *attempt)
    .await
    .expect("attempt 2");
    let admin = w.admin.clone();
    let late = tokio::spawn(async move {
        let mut tx = admin.begin().await?;
        refund_by_hand(&mut tx, b, "Delivering", 101, None).await?;
        tx.commit().await
    });
    tokio::time::sleep(StdDuration::from_millis(300)).await;
    assert!(!late.is_finished(), "the refund waits for the attempt");
    attempt.commit().await.unwrap();
    let err = late
        .await
        .unwrap()
        .expect_err("a refund beside an unproven attempt");
    assert!(err.to_string().contains("INV-8"), "{err}");
    assert_eq!(w.state(b).await.0, "Delivering");
}

// ---------------------------------------------------------------------------------------------
// S1, S2, S6, S7.

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s1_req_ot_5_a_withdrawal_ordered_but_not_signed_counts_against_the_reserve(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // Reserve for one of two: the first is ordered in Source-Final, the second holds.
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000).await;
    for (block, reference) in [(20, [16u8; 32]), (30, [17u8; 32])] {
        let r = redemption(&w.admin, block, &reference, 700).await;
        decide(&w.admin, r, "final", block + 2).await;
    }
    w.settle(t0()).await;
    assert_eq!(
        w.count("SELECT count(DISTINCT transfer_id) FROM intents WHERE purpose = 'payout'")
            .await,
        1
    );
    assert_eq!(
        w.count(
            "SELECT count(*) FROM transfers WHERE kind = 'withdrawal' \
               AND condition = 'CND-ReserveShort'"
        )
        .await,
        1
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s2_req_rq_9_a_commitment_never_becomes_a_payout_destination(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    for (block, backend) in [
        (20, json!({"kind": "attested", "id": 1})),
        (30, json!({"kind": "evm", "chain_id": 80002})),
    ] {
        let r = observe(
            &w.admin,
            ISSUING,
            block,
            0,
            ISSUING_ASSET,
            700,
            Some(REDEEMER),
            None,
            Some(&[18; 32]),
            true,
            json!({"event": "Redeemed", "backend": backend,
                   "payout": {"commitment": "0x5e7c"}}),
        )
        .await;
        decide(&w.admin, r, "final", block + 2).await;
    }
    w.settle(t0()).await;
    assert_eq!(
        w.count("SELECT count(*) FROM transfers WHERE kind = 'withdrawal'")
            .await,
        0
    );
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s6_the_cores_login_cannot_write_final_heads_or_proofs(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    let id = withdrawal_by(&w, 20, REDEEMER, [19; 32], BENEFICIARY).await;
    deliver(&w, id, 1, t0()).await;
    let denied = |e: sqlx::Error| {
        e.as_database_error()
            .and_then(|d| d.code())
            .is_some_and(|c| c == "42501")
    };
    let err = sqlx::query(
        "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
         VALUES ($1, 9999, '\\x01', now())",
    )
    .bind(ORIGIN)
    .execute(w.core.pool())
    .await
    .expect_err("the core wrote a final head");
    assert!(denied(err));
    let err = sqlx::query(
        "INSERT INTO non_execution_proofs (signed_transaction_id, network, signer, nonce, \
             valid_from_block, valid_until_block, final_block_number, final_block_hash, \
             nonce_at_final, kind) \
         SELECT st.id, $1, '\\x01', 0, 0, 64, 9999, '\\x01', 0, 'nonce-unconsumed' \
         FROM signed_transactions st LIMIT 1",
    )
    .bind(ORIGIN)
    .execute(w.core.pool())
    .await
    .expect_err("the core wrote a proof");
    assert!(denied(err));
    // An endpoint's role may.
    let endpoint: bool = scalar(
        &w.admin,
        "SELECT has_table_privilege('bezent_kreivo_endpoint', 'final_heads', 'INSERT') \
            AND has_table_privilege('bezent_evm_endpoint', 'non_execution_proofs', 'INSERT') \
            AND NOT has_table_privilege('bezent_service', 'final_heads', 'INSERT') \
            AND NOT has_table_privilege('bezent_core', 'final_heads', 'INSERT')",
    )
    .await;
    assert!(endpoint);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s7_req_tr_2_a_deadline_move_records_the_final_head_it_rests_on(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    let id = withdrawal_by(&w, 20, REDEEMER, [20; 32], BENEFICIARY).await;
    deliver(&w, id, 1, t0()).await;
    let past = 22 + BUDGET + 10;
    head(&w.admin, ORIGIN, past).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    let rests: (Option<String>, Option<i64>, Option<Vec<u8>>, bool) = sqlx::query_as(
        "SELECT network, block_number, block_hash, block_time = $2 FROM transitions \
         WHERE transfer_id = $1 AND to_state = 'Needs-Review'",
    )
    .bind(id)
    .bind(t0() + chrono::Duration::seconds(past))
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(
        rests,
        (
            Some(ORIGIN.into()),
            Some(past),
            Some(hash(ORIGIN, past)),
            true
        )
    );
}
