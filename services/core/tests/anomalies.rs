// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #24 (F-007 review: "the core must surface `delivery-anomaly` rows"): an endpoint records a
//! `delivery-anomaly` when what it reads contradicts itself, and proves nothing then. The core
//! surfaces it for the transfer it belongs to: `Needs-Review` at once, with the anomaly attached
//! as an event, once; never a refund or a retry (`INV-8`, `REQ-NX-3`, `REQ-RV-1`, `REQ-RF-1`).

mod common;

use common::*;
use serde_json::{Value, json};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// A withdrawal of 700 delivered (signed) in attempt 1: in `Delivering`, with its signed
/// transaction's id.
async fn delivering(w: &World, reference: [u8; 32], block: i64) -> (i64, i64) {
    reserve(&w.admin, 1_000_000).await;
    let r = redemption(&w.admin, block, &reference, 700).await;
    decide(&w.admin, r, "final", block + 2).await;
    w.cycle(t0()).await;
    let id: i64 = scalar(
        &w.admin,
        &format!(
            "SELECT id FROM transfers WHERE kind = 'withdrawal' AND on_chain_reference = '\\x{}'",
            hex::encode(reference)
        ),
    )
    .await;
    deliver(w, id, 1, t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    let signed = signed_of(&w.admin, id, 1).await;
    (id, signed)
}

/// An endpoint records an anomaly (the owner writes it here; the store lets an endpoint, the core
/// and the owner).
async fn anomaly(w: &World, payload: Value) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO event_log (kind, payload) VALUES ('delivery-anomaly', $1::text::jsonb) \
         RETURNING seq",
    )
    .bind(payload.to_string())
    .fetch_one(&w.admin)
    .await
    .unwrap()
}

async fn surfaced(w: &World, id: i64) -> i64 {
    w.count(&format!(
        "SELECT count(*) FROM event_log WHERE kind = 'delivery-anomaly-surfaced' \
           AND transfer_id = {id}"
    ))
    .await
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_8_an_anomaly_of_a_transfers_signed_transaction_sends_it_to_review_at_once(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, signed) = delivering(&w, [1; 32], 20).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");

    let seq = anomaly(
        &w,
        json!({"network": ORIGIN, "signed": signed, "reason": "the nonce was consumed by \
               another transaction that carries this reference", "spec": "INV-8, REQ-NX-3"}),
    )
    .await;
    // At once, long before the deadline; with its owner and service level (REQ-RV-1).
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    let (before, owner): (String, String) = sqlx::query_as(
        "SELECT state_before_review::text, owner FROM reviews WHERE transfer_id = $1",
    )
    .bind(id)
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(
        (before.as_str(), owner.as_str()),
        ("Delivering", "operations")
    );
    // The anomaly is attached to the review, once.
    let attached: String = scalar(
        &w.admin,
        &format!(
            "SELECT payload ->> 'anomaly' FROM event_log \
             WHERE kind = 'delivery-anomaly-surfaced' AND transfer_id = {id}"
        ),
    )
    .await;
    assert_eq!(attached, seq.to_string());
    w.settle(t0()).await;
    assert_eq!(surfaced(&w, id).await, 1);
    // Nothing is refunded or ordered again because of it (REQ-RF-1).
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
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_8_an_anomaly_is_a_transfers_only_if_it_is_of_its_network_and_names_its_records(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, signed) = delivering(&w, [2; 32], 20).await;

    // Another network's endpoint cannot send this transfer to review, whatever id it names.
    anomaly(
        &w,
        json!({"network": ISSUING, "signed_transaction": signed}),
    )
    .await;
    anomaly(
        &w,
        json!({"network": ISSUING, "signed": signed, "intent": 1}),
    )
    .await;
    // Another transaction than any of this transfer's.
    anomaly(&w, json!({"network": ORIGIN, "signed": signed + 1000})).await;
    anomaly(&w, json!({"network": ORIGIN, "intent": 999_999})).await;
    // Payloads that are not what an endpoint writes never fail a step.
    anomaly(&w, json!({"network": ORIGIN, "signed": "x", "intent": 1.5})).await;
    anomaly(&w, json!({"signed": signed})).await;
    anomaly(&w, json!({"network": ORIGIN, "signed": 1e30})).await;
    anomaly(&w, json!({"network": ORIGIN, "reference": "0x1234"})).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    assert_eq!(surfaced(&w, id).await, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_8_an_anomaly_of_a_signing_intent_reaches_its_transfer(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // As the endpoints record a signing check that fails: the signing intent is named.
    let w = World::new(options, connect).await;
    let (id, _) = delivering(&w, [3; 32], 20).await;
    let (intent, _) = signing_intent(&w.admin, id, 1).await.unwrap();
    anomaly(
        &w,
        json!({"network": ORIGIN, "intent": intent, "reason": "the intent does not answer its order"}),
    )
    .await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(surfaced(&w, id).await, 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_8_an_anomaly_of_the_legs_reference_reaches_its_transfer(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // As the EVM endpoint records "the reference is on chain, though this transaction did not
    // execute it": the payout's own reference, in either case of hex.
    let w = World::new(options, connect).await;
    let (id, _) = delivering(&w, [4; 32], 20).await;
    let reference = format!("0x{}", hex::encode(payout_reference_of(&w.admin, id).await));
    anomaly(
        &w,
        json!({"network": ORIGIN, "reference": reference.to_uppercase().replace("0X", "0x"),
               "reason": "the reference is on chain, though this transaction did not execute it"}),
    )
    .await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    // Another reference is not this transfer's, and an `on_chain_reference` alone is no match
    // (a refund mint's is scoped by its redeemer, amendment 0008).
    let (other, _) = delivering(&w, [5; 32], 40).await;
    let theirs = format!(
        "0x{}",
        hex::encode(payout_reference_of(&w.admin, other).await)
    );
    anomaly(
        &w,
        json!({"network": ORIGIN, "reference": format!("0x{}", "ab".repeat(32))}),
    )
    .await;
    anomaly(&w, json!({"network": ORIGIN, "on_chain_reference": theirs})).await;
    w.settle(t0()).await;
    assert_eq!(w.state(other).await.0, "Delivering");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_8_a_mints_reference_alone_is_no_match_for_an_anomaly(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // Only a withdrawal's payout reference is matched by `reference`. A mint's on-chain reference
    // is scoped by its redeemer (amendment 0008), so another namespace's anomaly carrying the
    // same bytes must not send a deposit's mint to review.
    let w = World::new(options, connect).await;
    let a = arrival(&w.admin, 10, 5_000).await;
    decide(&w.admin, a, "final", 12).await;
    let id = deposit(&w.admin, 10, 5_000).await;
    w.cycle(t0()).await;
    deliver(&w, id, 1, t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    let reference = format!("0x{}", hex::encode(reference_of(&w.admin, id).await));
    anomaly(
        &w,
        json!({"network": ISSUING, "reference": reference,
               "reason": "the reference is on chain, though this transaction did not execute it"}),
    )
    .await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    assert_eq!(surfaced(&w, id).await, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_8_a_transfer_already_in_review_gets_the_anomaly_attached_and_nothing_else(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, signed) = delivering(&w, [6; 32], 20).await;
    head(&w.admin, ORIGIN, 22 + BUDGET + 10).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(surfaced(&w, id).await, 0);
    anomaly(
        &w,
        json!({"network": ORIGIN, "signed": signed, "reason": "late"}),
    )
    .await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(surfaced(&w, id).await, 1);
    // One review, not two: the transfer moved into review once.
    let entered: i64 = scalar(
        &w.admin,
        &format!(
            "SELECT count(*) FROM transitions WHERE transfer_id = {id} AND to_state = 'Needs-Review'"
        ),
    )
    .await;
    assert_eq!(entered, 1);
}
