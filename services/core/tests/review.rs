// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-003 T-003-06: review (`REQ-RV-1`, `REQ-RV-2`, `REQ-GV-5`, `AC-C1.1`, `AC-C1.2`).
//!
//! A transfer in review has an owner and a service level; it leaves review only by a recorded
//! resolution whose evidence the core verifies itself from what the endpoints recorded at final
//! blocks, after which the ordinary step runs; a resolution that moves value waits for its second
//! approver. Every disposition is executed, not executed, or unknown: a timeout or a missing
//! observation is unknown and goes to review, never a refund (`AC-C1.1`, `AC-C1.2`).

mod common;

use chrono::Duration;
use common::*;
use serde_json::json;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// A deposit delivered (included at block 40) whose finality is unknown past its deadline.
async fn delivered_in_review(w: &World) -> (i64, i64) {
    let a = arrival(&w.admin, 10, 5_000).await;
    decide(&w.admin, a, "final", 12).await;
    let id = deposit(&w.admin, 10, 5_000).await;
    w.cycle(t0()).await;
    deliver(w, id, 1, t0()).await;
    let reference = reference_of(&w.admin, id).await;
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
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Delivered");
    head(&w.admin, ISSUING, 40 + BUDGET + 1).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    (id, minted)
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

/// The other operator approves (the resolutions here are recorded by operator 0).
async fn approve(w: &World, action: i64) {
    approve_as(w, 1, action).await.unwrap();
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rv_1_a_transfer_in_review_has_an_owner_and_a_service_level(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, _) = delivered_in_review(&w).await;
    let (owner, before, on_time): (String, String, bool) = sqlx::query_as(
        "SELECT owner, state_before_review::text, due_at > entered_at FROM reviews WHERE transfer_id = $1",
    )
    .bind(id)
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(
        (owner.as_str(), before.as_str(), on_time),
        ("operations", "Delivered", true)
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rv_2_a_review_ends_by_evidence_bezent_verifies_then_the_ordinary_step_runs(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, minted) = delivered_in_review(&w).await;
    // Evidence the chain does not show (no final outcome at that block): refused, recorded,
    // still in review.
    head(&w.admin, ISSUING, 5_000).await;
    let wrong = resolution(&w, id, "Completed", ISSUING, 5_000, 0).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    let refused: i64 = w
        .count(&format!(
            "SELECT count(*) FROM event_log WHERE kind = 'review-evidence-refused' \
               AND payload ->> 'manual_action_id' = '{wrong}'"
        ))
        .await;
    assert_eq!(refused, 1);
    // The endpoint records the inclusion final at 6000; a resolution naming that final point is
    // verified and runs Delivered -> Completed, with the actor in the record.
    decide(&w.admin, minted, "final", 6_000).await;
    let right = resolution(&w, id, "Completed", ISSUING, 6_000, 0).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await, ("Completed".into(), None));
    let ran: Option<i64> = scalar(
        &w.admin,
        &format!(
            "SELECT manual_action_id FROM transitions WHERE transfer_id = {id} \
               AND from_state = 'Needs-Review'"
        ),
    )
    .await;
    assert_eq!(ran, Some(right));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_gv_5_a_resolution_that_moves_value_waits_for_its_second_approver(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, _) = delivered_in_review(&w).await;
    // The delivery is proven not executed at 7000: a new attempt moves value.
    prove(&w.admin, id, 1, ISSUING, 7_000).await;
    let action = resolution(&w, id, "Delivering", ISSUING, 7_000, 0).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert!(order(&w.admin, id, 2).await.is_none());
    approve(&w, action).await;
    deliver(&w, id, 2, t0()).await;
    assert_eq!(w.state(id).await, ("Delivering".into(), None));
    let (attempt, ran): (Option<i32>, Option<i64>) = sqlx::query_as(
        "SELECT attempt, manual_action_id FROM transitions WHERE transfer_id = $1 \
           AND from_state = 'Needs-Review'",
    )
    .bind(id)
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!((attempt, ran), (Some(2), Some(action)));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m3_req_gv_5_one_login_cannot_be_its_own_second_approver_under_another_name(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // The reviewer's scenario: one person on one operator login records the action as "alice"
    // and the approval as "bob". The store names neither: it stamps the login, and refuses an
    // approval by the action's own login; a value-moving resolution stays in review.
    let w = World::new(options, connect).await;
    let (id, _) = delivered_in_review(&w).await;
    prove(&w.admin, id, 1, ISSUING, 7_000).await;
    let named = sqlx::query(
        "INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, \
             block_hash, block_time, actor, justification) \
         VALUES ('review-resolution', $1, 'Delivering', $2, 7000, $3, $4, 'alice', 'x')",
    )
    .bind(id)
    .bind(ISSUING)
    .bind(hash(ISSUING, 7_000))
    .bind(t0() + Duration::seconds(7_000))
    .execute(&w.ops[0])
    .await
    .expect_err("the operator named the actor");
    assert_eq!(
        named.as_database_error().and_then(|d| d.code()).as_deref(),
        Some("42501")
    );
    let action = resolution(&w, id, "Delivering", ISSUING, 7_000, 0).await;
    let backdated = sqlx::query(
        "INSERT INTO manual_approvals (action_id, approver, approved_at, justification) \
         VALUES ($1, 'bob', now() - interval '1 day', 'x')",
    )
    .bind(action)
    .execute(&w.ops[0])
    .await
    .expect_err("the operator named the approver and the time");
    assert_eq!(
        backdated
            .as_database_error()
            .and_then(|d| d.code())
            .as_deref(),
        Some("42501")
    );
    let own = approve_as(&w, 0, action)
        .await
        .expect_err("the actor's own login approved");
    assert!(own.to_string().contains("REQ-GV-5"), "{own}");
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert!(order(&w.admin, id, 2).await.is_none());
    // Another operator's login approves; both are recorded under their sessions.
    approve_as(&w, 1, action).await.unwrap();
    let (actor, approver): (String, String) = sqlx::query_as(
        "SELECT a.actor, p.approver FROM manual_actions a \
         JOIN manual_approvals p ON p.action_id = a.id WHERE a.id = $1",
    )
    .bind(action)
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(
        (actor.as_str(), approver.as_str()),
        (OPERATORS[0], OPERATORS[1])
    );
    deliver(&w, id, 2, t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s5_inv_15_a_review_resolution_passes_the_delivery_gates(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, _) = delivered_in_review(&w).await;
    prove(&w.admin, id, 1, ISSUING, 7_000).await;
    let action = resolution(&w, id, "Delivering", ISSUING, 7_000, 0).await;
    approve(&w, action).await;
    exec(
        &w.admin,
        &format!("INSERT INTO pauses (asset) VALUES ('{ASSET}')"),
    )
    .await;
    w.settle(t0()).await;
    // A pause stops new work: the resolution waits in review, signing nothing.
    assert_eq!(
        w.state(id).await,
        ("Needs-Review".into(), Some("CND-Paused".into()))
    );
    assert!(order(&w.admin, id, 2).await.is_none());
    exec(&w.admin, "DELETE FROM pauses").await;
    w.cycle(t0()).await;
    deliver(&w, id, 2, t0()).await;
    assert_eq!(w.state(id).await, ("Delivering".into(), None));
    let ran: Option<i64> = scalar(
        &w.admin,
        &format!(
            "SELECT manual_action_id FROM transitions WHERE transfer_id = {id} \
               AND to_state = 'Delivering' AND attempt = 2"
        ),
    )
    .await;
    assert_eq!(ran, Some(action));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_c1_2_a_missing_observation_is_unknown_never_not_executed(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // A timeout, a node error or a dropped transaction leaves no observation and no proof: the
    // disposition is unknown, and unknown goes to review, never to a refund (REQ-NX-3).
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    let r = redemption(&w.admin, 20, &[6; 32], 700).await;
    decide(&w.admin, r, "final", 22).await;
    w.cycle(t0()).await;
    let id: i64 = scalar(
        &w.admin,
        "SELECT id FROM transfers WHERE kind = 'withdrawal'",
    )
    .await;
    deliver(&w, id, 1, t0()).await;
    head(&w.admin, ORIGIN, 22 + BUDGET + 1).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(
        w.count("SELECT count(*) FROM transfers WHERE kind = 'refund'")
            .await,
        0
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_c1_1_every_disposition_is_executed_not_executed_or_unknown(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // Executed: Completed. Not executed (proven): a new attempt. Unknown: review.
    let w = World::new(options, connect).await;
    let (unknown, _) = delivered_in_review(&w).await;
    assert_eq!(w.state(unknown).await.0, "Needs-Review");
    let a = arrival(&w.admin, 11, 9).await;
    decide(&w.admin, a, "final", 13).await;
    let proven = deposit(&w.admin, 11, 9).await;
    w.cycle(t0()).await;
    deliver(&w, proven, 1, t0()).await;
    prove(&w.admin, proven, 1, ISSUING, 50).await;
    w.cycle(t0()).await;
    assert!(order(&w.admin, proven, 2).await.is_some());
    assert_eq!(w.state(proven).await.0, "Delivering");
}
