// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #25 (F-003 follow-up, review note N7 and the verification's F3): an attempt whose
//! preparation never passes has a deadline like any other leg, and an attempt that was ordered and
//! never signed no longer shuts a withdrawal's refund for good.
//!
//! - `REQ-RV-1`, `REQ-EO-3`: a dry run that keeps failing without naming a condition sends the
//!   transfer to review at the route's deadline, from `Source-Final` for the first attempt and from
//!   `Delivering` for a later one, never to a refund or a retry on its own; a refusal that names a
//!   condition waits as that condition says (`CND-FeeShort`, `CND-PayoutWindow`).
//! - `INV-8`, `REQ-RF-1`, `REQ-GV-5`: the operator's approved resolution into `Refunding` cancels
//!   the attempt that was never signed, recorded, and refunds on the proof of the last attempt that
//!   was; an attempt that was signed and is not proven blocks the refund as before.

mod common;

use chrono::Duration;
use common::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// A refusal the core names no condition for (`dry_run_condition`): it is retried, and holds
/// nothing.
const UNNAMED: &str = "Unspecified";

/// A withdrawal of 700 whose redemption is at `block` (final two blocks later), accepted.
async fn withdrawal(w: &World, block: i64, reference: [u8; 32]) -> i64 {
    let r = redemption(&w.admin, block, &reference, 700).await;
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

/// The core orders attempt `attempt`, the endpoint prepares it and its dry run is refused with
/// `reason` (several times over: each refusal is followed by a new order, and the core cycles).
async fn refused(w: &World, id: i64, attempt: i32, reason: &str) {
    for _ in 0..3 {
        w.cycle(t0()).await;
        w.cycle(t0()).await;
        let (o, _) = order(&w.admin, id, attempt).await.expect("an order");
        prepare(&w.admin, o, Some(reason)).await;
    }
    w.cycle(t0()).await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rv_1_a_first_attempt_whose_preparation_never_passes_goes_to_review_at_its_deadline(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    let id = withdrawal(&w, 20, [1; 32]).await;
    refused(&w, id, 1, UNNAMED).await;
    assert_eq!(w.state(id).await, ("Source-Final".into(), None));

    // Before the deadline (the source leg final at block 22, plus the route's budget, on the
    // destination network's clock): nothing.
    head(&w.admin, ORIGIN, 22 + BUDGET - 10).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Source-Final");
    // Past it, at a final block: review, entered from Source-Final, with its owner and service
    // level; no refund (REQ-RF-1) and no further order (REQ-RV-1).
    head(&w.admin, ORIGIN, 22 + BUDGET + 10).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    let orders = w.count("SELECT count(*) FROM intents").await;
    let before: String = scalar(
        &w.admin,
        &format!("SELECT state_before_review::text FROM reviews WHERE transfer_id = {id}"),
    )
    .await;
    assert_eq!(before, "Source-Final");
    w.settle(t0()).await;
    assert_eq!(w.count("SELECT count(*) FROM intents").await, orders);
    assert_eq!(
        w.count("SELECT count(*) FROM transfers WHERE kind = 'refund'")
            .await,
        0
    );
    // Time alone never ends a review (REQ-RV-1).
    head(&w.admin, ORIGIN, 22 + BUDGET + 100_000).await;
    w.settle(t0() + Duration::days(30)).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rv_1_a_refusal_that_names_a_condition_waits_as_the_condition_says(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // CND-FeeShort resumes by itself (§10): it is no unknown, and the review is not its end.
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    let fee = withdrawal(&w, 20, [2; 32]).await;
    refused(&w, fee, 1, "InsufficientBalance").await;
    assert_eq!(
        w.state(fee).await,
        ("Source-Final".into(), Some("CND-FeeShort".into()))
    );
    head(&w.admin, ORIGIN, 22 + BUDGET + 10).await;
    w.settle(t0()).await;
    assert_eq!(
        w.state(fee).await,
        ("Source-Final".into(), Some("CND-FeeShort".into()))
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_payout_window_a_spent_allowance_is_never_reviewed_and_its_clock_restarts_when_it_refills(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // Amendment 0012: the hold is no review and no refund; the time held does not count.
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    let id = withdrawal(&w, 20, [3; 32]).await;
    head(&w.admin, ORIGIN, 30).await;
    w.cycle(t0()).await;
    let (o, _) = order(&w.admin, id, 1).await.unwrap();
    prepare_allowance_spent(&w.admin, o, &allowance_key()).await;
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-PayoutWindow".into()))
    );
    // Held far past the deadline: still held.
    let released = 22 + 5 * BUDGET;
    head(&w.admin, ORIGIN, released).await;
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-PayoutWindow".into()))
    );
    // The allowance refills and a later preparation is refused for an unnamed reason: the hold is
    // left, resting on the final head, and the attempt's deadline counts from there.
    w.cycle(t0()).await;
    let (o, _) = order(&w.admin, id, 1).await.unwrap();
    prepare(&w.admin, o, Some(UNNAMED)).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await, ("Source-Final".into(), None));
    head(&w.admin, ORIGIN, released + BUDGET - 10).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Source-Final");
    head(&w.admin, ORIGIN, released + BUDGET + 10).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
}

/// A withdrawal whose attempt 1 was signed and proven not executed at block 100 (inside the
/// deadline), and whose attempt 2 was ordered and never prepared successfully: in `Delivering`.
async fn second_attempt_never_prepared(w: &World, reference: [u8; 32]) -> i64 {
    reserve(&w.admin, 1_000_000).await;
    let id = withdrawal(w, 20, reference).await;
    deliver(w, id, 1, t0()).await;
    prove(&w.admin, id, 1, ORIGIN, 100).await;
    refused(w, id, 2, UNNAMED).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    id
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rv_1_a_later_attempt_whose_preparation_never_passes_goes_to_review_from_delivering(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let id = second_attempt_never_prepared(&w, [4; 32]).await;
    // The attempt's clock starts at the proof it rests on (block 100), not at the source leg.
    head(&w.admin, ORIGIN, 100 + BUDGET - 10).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    head(&w.admin, ORIGIN, 100 + BUDGET + 10).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    let before: String = scalar(
        &w.admin,
        &format!("SELECT state_before_review::text FROM reviews WHERE transfer_id = {id}"),
    )
    .await;
    assert_eq!(before, "Delivering");
    assert_eq!(
        w.count("SELECT count(*) FROM transfers WHERE kind = 'refund'")
            .await,
        0
    );
}

/// Operator `by` records, as their own login, a resolution of transfer `id` into `to` resting on
/// the final head (`network`, `block`).
async fn resolution(w: &World, id: i64, to: &str, network: &str, block: i64, by: usize) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, \
             block_hash, block_time, justification) \
         VALUES ('review-resolution', $1, $2::transfer_state, $3, $4, $5, $6, 'see the block') \
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

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_8_an_approved_resolution_cancels_the_attempt_never_signed_and_refunds_on_the_proof_of_the_last_signed(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let id = second_attempt_never_prepared(&w, [5; 32]).await;
    head(&w.admin, ORIGIN, 100 + BUDGET + 10).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");

    // The resolution waits for its second approver; nothing is cancelled meanwhile (REQ-GV-5).
    let action = resolution(&w, id, "Refunding", ORIGIN, 100, 0).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(
        w.count("SELECT count(*) FROM unsigned_attempt_cancellations")
            .await,
        0
    );
    approve_as(&w, 1, action).await.unwrap();
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Refunding");
    // Attempt 2 is cancelled, recorded under the resolution, and nothing is recorded for it any
    // more: the order, a signing intent and a signature are all refused (INV-8).
    let (attempt, under): (i32, i64) = sqlx::query_as(
        "SELECT attempt, manual_action_id FROM unsigned_attempt_cancellations WHERE transfer_id = $1",
    )
    .bind(id)
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!((attempt, under), (2, action));
    let again = sqlx::query(
        "INSERT INTO intents (transfer_id, network, purpose, on_chain_reference, reference_scope, \
             attempt, payload) \
         SELECT id, destination_network, 'payout', destination_reference, NULL, 2, \
             '{\"order\": {}, \"amount\": \"1\"}' FROM transfers WHERE id = $1",
    )
    .bind(id)
    .execute(&w.admin)
    .await
    .expect_err("an order for the cancelled attempt");
    assert!(again.to_string().contains("INV-8"), "{again}");
    // One refund mint, to the redeemer (REQ-OT-6, REQ-RF-4).
    let refund: i64 = scalar(
        &w.admin,
        &format!(
            "SELECT count(*) FROM transfers WHERE refund_of = {id} AND redeemer = '{REDEEMER}'"
        ),
    )
    .await;
    assert_eq!(refund, 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_8_a_signed_attempt_that_is_not_proven_still_blocks_the_refund(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // Attempt 2 is signed and unknown at its deadline: review, and a resolution into Refunding
    // naming attempt 1's proof is refused, as before: nothing is cancelled, nothing refunded.
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    let id = withdrawal(&w, 20, [6; 32]).await;
    deliver(&w, id, 1, t0()).await;
    prove(&w.admin, id, 1, ORIGIN, 100).await;
    deliver(&w, id, 2, t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    head(&w.admin, ORIGIN, 100 + BUDGET + 10).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");

    let action = resolution(&w, id, "Refunding", ORIGIN, 100, 0).await;
    approve_as(&w, 1, action).await.unwrap();
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(
        w.count("SELECT count(*) FROM unsigned_attempt_cancellations")
            .await,
        0
    );
    assert_eq!(
        w.count("SELECT count(*) FROM transfers WHERE kind = 'refund'")
            .await,
        0
    );
    let refused: i64 = w
        .count(&format!(
            "SELECT count(*) FROM event_log WHERE kind = 'review-evidence-refused' \
               AND payload ->> 'manual_action_id' = '{action}'"
        ))
        .await;
    assert_eq!(refused, 1);
}
