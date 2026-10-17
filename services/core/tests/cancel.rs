// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #41 (F-003 follow-up): `REQ-RV-1` and `REQ-NX-4` together. A payout unknown at its
//! deadline on an origin network goes to review at that moment, with its owner and service level
//! (`AC-B1.4`), and at the same time the core orders the cancel that forces its non-execution,
//! which it drives through the two steps while the transfer is in review. The cancel's final proof
//! is attached to the review as evidence (`REQ-RV-2`); the review ends by a resolution naming it,
//! and a resolution that moves value (the refund mint or a new attempt) waits for its second
//! approver (`REQ-GV-5`, `INV-8`). A cancel that stays unknown leaves the review open, and nothing
//! is refunded (`REQ-RF-1`). On the issuing network the era's expiry is the proof, no cancel is
//! ordered, and the expiry is attached the same way (`REQ-NX-1`). A payout over the reserve's
//! outflow allowance holds `CND-PayoutWindow` (amendment 0012).
//!
//! The EVM endpoint's half (preparing the cancel, persisting the signed bytes, proving the payout
//! replaced) is written as F-009 writes it; the signature comes from the real signer, which reads
//! a cancel's bytes before it signs.

mod common;

use chrono::Duration;
use common::*;
use serde_json::{Value, json};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// A withdrawal of `amount` from a redemption at (`ISSUING`, `block`), final two blocks later.
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

/// The withdrawal's payout signed and persisted (attempt 1), from a redemption final at block 22,
/// so its deadline is block 22 + `BUDGET`. Returns the transfer and the payout's signed
/// transaction.
async fn payout_in_flight(w: &World, reference: u8) -> (i64, i64) {
    reserve(&w.admin, 1_000_000).await;
    let id = withdrawal(w, 20, [reference; 32], 700).await;
    deliver(w, id, 1, t0()).await;
    assert_eq!(w.state(id).await, ("Delivering".into(), None));
    (id, signed_of(&w.admin, id, 1).await)
}

/// The final head past the payout's deadline.
const PAST: i64 = 22 + BUDGET + 10;

/// The payout of `payout_in_flight` past its deadline, in review, with its cancel ordered,
/// prepared at the payout's nonce and signed by the real signer.
async fn cancel_signed_in_review(w: &World, reference: u8) -> (i64, i64) {
    let (id, signed) = payout_in_flight(w, reference).await;
    head(&w.admin, ORIGIN, PAST).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    let (order, _) = cancel_order(&w.admin, signed)
        .await
        .expect("a cancel order");
    prepare_cancel(&w.admin, order, None).await;
    w.settle(t0()).await;
    let (intent, _) = cancel_signing_intent(&w.admin, signed)
        .await
        .expect("the cancel's signing intent");
    sign(w, intent).await;
    w.settle(t0()).await;
    (id, signed)
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

async fn refunds(w: &World) -> i64 {
    w.count("SELECT count(*) FROM transfers WHERE kind = 'refund'")
        .await
}

async fn later_attempts(w: &World, id: i64) -> i64 {
    w.count(&format!(
        "SELECT count(*) FROM intents WHERE transfer_id = {id} AND attempt > 1"
    ))
    .await
}

async fn cancels(w: &World) -> i64 {
    w.count("SELECT count(*) FROM intents WHERE purpose = 'cancel'")
        .await
}

async fn events(w: &World, id: i64, kind: &str) -> Vec<Value> {
    sqlx::query_scalar(
        "SELECT payload FROM event_log WHERE transfer_id = $1 AND kind = $2 ORDER BY seq",
    )
    .bind(id)
    .bind(kind)
    .fetch_all(&w.admin)
    .await
    .unwrap()
}

/// REQ-NX-4, REQ-RV-1, AC-B1.4: a payout not included by its deadline on an origin network goes
/// to review at that moment, resting on the final head past it, with its owner and service level;
/// at the same time the core orders its cancel (no transfer, no reference, nothing moved) and,
/// while the transfer is in review, drives it through the two steps of PLAN DEC-3: the endpoint's
/// preparation and dry run, a new order after a failed one, the signing intent over its bytes,
/// the signer, the signature verified. Nothing is refunded and no new attempt is made.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_nx_4_req_rv_1_at_the_deadline_review_opens_and_the_cancel_is_ordered_in_two_steps(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, signed) = payout_in_flight(&w, 41).await;

    // Before the deadline: nothing.
    head(&w.admin, ORIGIN, 30).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    assert_eq!(cancels(&w).await, 0);

    // At the deadline: review, and the cancel's order naming the payout's signed transaction.
    head(&w.admin, ORIGIN, PAST).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    let (owner, before, block): (String, String, i64) = sqlx::query_as(
        "SELECT r.owner, r.state_before_review::text, x.block_number FROM reviews r \
         JOIN transitions x ON x.id = r.entered_transition_id WHERE r.transfer_id = $1",
    )
    .bind(id)
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(
        (owner.as_str(), before.as_str(), block),
        ("operations", "Delivering", PAST)
    );
    let (first, payload) = cancel_order(&w.admin, signed)
        .await
        .expect("a cancel order");
    assert_eq!(payload, json!({"order": {"cancel": signed}, "amount": "0"}));
    let (transfer, reference, network): (Option<i64>, Option<Vec<u8>>, String) = sqlx::query_as(
        "SELECT transfer_id, on_chain_reference, network FROM intents WHERE id = $1",
    )
    .bind(first)
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(
        (transfer, reference, network.as_str()),
        (None, None, ORIGIN)
    );
    let ordered = events(&w, id, "review-cancel-ordered").await;
    assert_eq!(ordered.len(), 1);
    assert_eq!(ordered[0]["signed_transactions"], json!([signed]));
    assert_eq!(
        cancels(&w).await,
        1,
        "one order, waiting for its preparation"
    );

    // The endpoint's dry run fails (fees above the cap): a new order, nothing else.
    prepare_cancel(&w.admin, first, Some("fee: above the declared bound")).await;
    w.settle(t0()).await;
    let (second, _) = cancel_order(&w.admin, signed).await.unwrap();
    assert_ne!(second, first);
    assert_eq!(w.state(id).await, ("Needs-Review".into(), None));

    // Its dry run passes: the signing intent over exactly those bytes, amount nothing.
    let prepared = prepare_cancel(&w.admin, second, None).await;
    w.settle(t0()).await;
    let (intent, payload) = cancel_signing_intent(&w.admin, signed)
        .await
        .expect("the cancel's signing intent");
    assert_eq!(
        payload,
        json!({"message": format!("0x{}", hex::encode(cancel_payload(cancelled_nonce(&w.admin, second).await))), "amount": "0",
               "prepared": prepared})
    );

    // The real signer signs it (a transfer of nothing to the key itself); the core verifies the
    // signature and moves nothing, refunds nothing, attempts nothing.
    sign(&w, intent).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(refunds(&w).await, 0);
    assert_eq!(later_attempts(&w, id).await, 0);
    assert_eq!(cancels(&w).await, 3, "one signing intent per signed payout");
    assert_eq!(
        w.count("SELECT count(*) FROM event_log WHERE kind = 'key-compromise'")
            .await,
        0
    );
}

/// REQ-RV-2, REQ-GV-5, INV-8: the cancel's final proof that it consumed the payout's nonce is
/// attached to the review by itself, once, and moves nothing. The review ends by a resolution
/// naming it; the refund mint moves value, so it waits for the second approver, then runs as the
/// ordinary step, resting on the proof's final block.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rv_2_req_gv_5_the_cancels_proof_is_evidence_and_the_refund_waits_for_its_second_approver(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, _) = cancel_signed_in_review(&w, 42).await;
    assert!(events(&w, id, "review-evidence").await.is_empty());

    // The cancel consumed the payout's nonce at PAST + 5, final at PAST + 20.
    prove_replaced(&w.admin, id, 1, PAST + 5, PAST + 20).await;
    w.settle(t0()).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(refunds(&w).await, 0, "a proof alone moves nothing");
    let evidence = events(&w, id, "review-evidence").await;
    assert_eq!(evidence.len(), 1, "attached once");
    assert_eq!(
        (
            &evidence[0]["proof"],
            &evidence[0]["network"],
            &evidence[0]["block_number"],
            &evidence[0]["attempt"]
        ),
        (
            &json!(["nonce-consumed-by-other"]),
            &json!(ORIGIN),
            &json!(PAST + 20),
            &json!("1")
        )
    );

    // The refund mint, on that evidence: waits for its second approver, then runs.
    let action = resolution(&w, id, "Refunding", ORIGIN, PAST + 20, 0).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(refunds(&w).await, 0);
    approve_as(&w, 1, action).await.unwrap();
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Refunding");
    assert_eq!(refunds(&w).await, 1);
    let rests: (String, i64, Option<i64>) = sqlx::query_as(
        "SELECT network, block_number, manual_action_id FROM transitions \
         WHERE transfer_id = $1 AND to_state = 'Refunding'",
    )
    .bind(id)
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(rests, (ORIGIN.into(), PAST + 20, Some(action)));
}

/// REQ-RV-2, REQ-GV-5, REQ-EO-6: on the same evidence the resolution may be a new attempt
/// instead; it moves value too, so no order of attempt 2 exists before the second approver.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rv_2_req_gv_5_on_the_cancels_proof_a_new_attempt_waits_for_its_second_approver(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, _) = cancel_signed_in_review(&w, 46).await;
    prove_replaced(&w.admin, id, 1, PAST + 5, PAST + 20).await;
    w.settle(t0()).await;
    let action = resolution(&w, id, "Delivering", ORIGIN, PAST + 20, 0).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert!(order(&w.admin, id, 2).await.is_none());
    approve_as(&w, 1, action).await.unwrap();
    deliver(&w, id, 2, t0()).await;
    assert_eq!(w.state(id).await, ("Delivering".into(), None));
    assert_eq!(refunds(&w).await, 0);
}

/// REQ-NX-4, REQ-RV-1, REQ-RF-1: a cancel that stays unknown leaves the review open, with the
/// ordered cancel on record and no evidence attached. Time alone never ends it, no second cancel
/// is ordered, and a refund mint without the proof is refused even with its second approver.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_nx_4_req_rv_1_a_cancel_that_stays_unknown_leaves_the_review_open_and_refunds_nothing(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, _) = cancel_signed_in_review(&w, 43).await;
    let far = PAST + 100_000;
    head(&w.admin, ORIGIN, far).await;
    w.settle(t0() + Duration::days(30)).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(refunds(&w).await, 0);
    assert_eq!(later_attempts(&w, id).await, 0);
    assert_eq!(cancels(&w).await, 2, "the order and its one signing intent");
    assert_eq!(events(&w, id, "review-cancel-ordered").await.len(), 1);
    assert!(events(&w, id, "review-evidence").await.is_empty());

    let action = resolution(&w, id, "Refunding", ORIGIN, far, 0).await;
    approve_as(&w, 1, action).await.unwrap();
    w.settle(t0() + Duration::days(30)).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(refunds(&w).await, 0);
    assert_eq!(events(&w, id, "review-evidence-refused").await.len(), 1);
}

/// AC-I1.2: a cancel's signature that is not over its recorded bytes raises the compromise path
/// and the pause request, once, and nothing moves.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_cancel_signature_not_over_its_bytes_raises_the_alert(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, signed) = payout_in_flight(&w, 44).await;
    head(&w.admin, ORIGIN, PAST).await;
    w.settle(t0()).await;
    let (order, _) = cancel_order(&w.admin, signed).await.unwrap();
    prepare_cancel(&w.admin, order, None).await;
    w.settle(t0()).await;
    let (intent, _) = cancel_signing_intent(&w.admin, signed).await.unwrap();
    let public_key: Vec<u8> = scalar(
        &w.admin,
        &format!(
            "SELECT s.public_key FROM signatures s JOIN intents i ON i.id = s.intent_id \
             WHERE i.transfer_id = {id} LIMIT 1"
        ),
    )
    .await;
    exec(
        &w.admin,
        &format!(
            "INSERT INTO signatures (intent_id, key_id, scheme, public_key, signature) \
             VALUES ({intent}, 'polygon-payout', 'secp256k1-evm', '\\x{}', '\\x{}'); \
             INSERT INTO signed_transactions (intent_id, attempt, bytes) VALUES ({intent}, 1, '\\xbeef')",
            hex::encode(public_key),
            "22".repeat(65)
        ),
    )
    .await;
    w.cycle(t0()).await;
    w.cycle(t0()).await;
    let alerts: Vec<String> = sqlx::query_scalar(
        "SELECT kind FROM event_log WHERE kind IN ('key-compromise', 'pause-requested') ORDER BY seq",
    )
    .fetch_all(&w.admin)
    .await
    .unwrap();
    assert_eq!(alerts, vec!["key-compromise", "pause-requested"]);
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(refunds(&w).await, 0);
}

/// REQ-NX-1, REQ-RV-2: on the issuing network a transaction expires with its era, and the expiry
/// at finality is the proof: no cancel is ordered, before or past the deadline. A deposit proven
/// not executed gets its new attempt; one unknown at its deadline goes to review, and the era's
/// expiry, once final, is attached to that review as its evidence.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_nx_1_req_rv_2_on_the_issuing_network_era_expiry_is_the_proof_attached_to_the_review(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let a = arrival(&w.admin, 10, 5_000).await;
    decide(&w.admin, a, "final", 12).await;
    let id = deposit(&w.admin, 10, 5_000).await;
    w.cycle(t0()).await;
    deliver(&w, id, 1, t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");

    // The era ended at a final block with the nonce unconsumed: attempt 2, no cancel.
    prove(&w.admin, id, 1, ISSUING, 100).await;
    deliver(&w, id, 2, t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    assert_eq!(cancels(&w).await, 0);

    // Attempt 2 unknown at its deadline (from the proof it rests on): review, no cancel.
    head(&w.admin, ISSUING, 100 + BUDGET + 10).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(cancels(&w).await, 0);
    assert!(events(&w, id, "review-cancel-ordered").await.is_empty());

    // Its era expires, final at 100 + BUDGET + 50: the evidence, attached; nothing moves.
    prove(&w.admin, id, 2, ISSUING, 100 + BUDGET + 50).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    let evidence = events(&w, id, "review-evidence").await;
    assert_eq!(evidence.len(), 1);
    assert_eq!(
        (
            &evidence[0]["proof"],
            &evidence[0]["network"],
            &evidence[0]["attempt"]
        ),
        (&json!(["nonce-unconsumed"]), &json!(ISSUING), &json!("2"))
    );
}

/// The transitions of transfer `id` matching `filter`.
async fn transitions(w: &World, id: i64, filter: &str) -> i64 {
    w.count(&format!(
        "SELECT count(*) FROM transitions WHERE transfer_id = {id} AND {filter}"
    ))
    .await
}

/// A withdrawal held in CND-PayoutWindow: its first order's dry run refused over the allowance.
async fn held(w: &World, reference: u8) -> i64 {
    reserve(&w.admin, 1_000_000).await;
    let id = withdrawal(w, 20, [reference; 32], 700).await;
    w.cycle(t0()).await;
    let (first, _) = order(&w.admin, id, 1).await.expect("the payout order");
    prepare_allowance_spent(&w.admin, first, &allowance_key()).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-PayoutWindow".into()))
    );
    id
}

/// CND-PayoutWindow (amendment 0012), PR #52 review M-1: a payout the vault's outflow allowance
/// cannot take this window holds the withdrawal in its state, by a self-transition; far past its
/// deadline it is never sent to review and never refunded. Each new order is prepared again, and
/// the first whose dry run passes, once the allowance refills, resumes it: the hold is left
/// resting on the final head of that moment, signed, `Delivering`. The time it waited does not
/// count toward its deadline: on the cycles after the release, though far past the original
/// deadline, it is neither reviewed nor cancelled, and it completes normally.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_payout_window_m1_a_spent_allowance_holds_the_withdrawal_never_review_never_refund(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let id = held(&w, 45).await;
    let first = scalar::<i64>(
        &w.admin,
        &format!("SELECT min(id) FROM intents WHERE transfer_id = {id}"),
    )
    .await;

    // Far past the deadline, still spent: held, never review, never a refund.
    let released = 22 + 3 * BUDGET;
    head(&w.admin, ORIGIN, released).await;
    let (second, _) = order(&w.admin, id, 1).await.unwrap();
    assert_ne!(second, first, "a new order, to be prepared again");
    prepare_allowance_spent(&w.admin, second, &allowance_key()).await;
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-PayoutWindow".into()))
    );
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM transitions WHERE transfer_id = {id} AND to_state = 'Needs-Review'"
        ))
        .await,
        0
    );
    assert_eq!(refunds(&w).await, 0);
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM transitions WHERE transfer_id = {id} \
             AND from_state = 'Source-Final' AND to_state = 'Source-Final' \
             AND condition = 'CND-PayoutWindow'"
        ))
        .await,
        1,
        "held once, by a self-transition"
    );

    // The allowance refilled: the next order's dry run passes, and the payout goes out.
    let (third, _) = order(&w.admin, id, 1).await.unwrap();
    assert_ne!(third, second);
    prepare(&w.admin, third, None).await;
    w.cycle(t0()).await;
    let (s, _) = signing_intent(&w.admin, id, 1)
        .await
        .expect("a signing intent");
    sign(&w, s).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await, ("Delivering".into(), None));
    assert_eq!(
        transitions(
            &w,
            id,
            &format!(
                "from_state = 'Source-Final' AND to_state = 'Source-Final' \
                 AND condition IS NULL AND network = '{ORIGIN}' AND block_number = {released}"
            )
        )
        .await,
        1,
        "the hold is left resting on the final head at the release"
    );

    // Review M-1: the cycles after the release, far past the original deadline (22 + BUDGET) and
    // up to just before the restarted one (released + BUDGET), send nothing to review and order
    // no cancel.
    w.settle(t0()).await;
    w.settle(t0()).await;
    head(&w.admin, ORIGIN, released + BUDGET - 100).await;
    w.settle(t0()).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await, ("Delivering".into(), None));
    assert_eq!(transitions(&w, id, "to_state = 'Needs-Review'").await, 0);
    assert_eq!(cancels(&w).await, 0, "no cancel of a payout that waited");

    // The payout is included and final: it completes as any other.
    let paid = observe(
        &w.admin,
        ORIGIN,
        released + BUDGET - 50,
        0,
        TOKEN,
        700,
        None,
        Some(BENEFICIARY),
        Some(&payout_reference_of(&w.admin, id).await),
        true,
        json!({"event": "Payout"}),
    )
    .await;
    decide(&w.admin, paid, "final", released + BUDGET - 20).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await, ("Completed".into(), None));
    assert_eq!(transitions(&w, id, "to_state = 'Needs-Review'").await, 0);
    assert_eq!(cancels(&w).await, 0);
    assert_eq!(refunds(&w).await, 0);
}

/// CND-PayoutWindow, review M-1: the clock restarts at the release, it is not switched off. A
/// payout released from the hold and then unknown past the restarted deadline goes to review,
/// with its cancel, as any other (REQ-RV-1, REQ-NX-4).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_payout_window_m1_the_deadline_restarts_at_the_release(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let id = held(&w, 49).await;
    let released = 22 + 3 * BUDGET;
    head(&w.admin, ORIGIN, released).await;
    let (o, _) = order(&w.admin, id, 1).await.unwrap();
    prepare(&w.admin, o, None).await;
    w.cycle(t0()).await;
    let (s, _) = signing_intent(&w.admin, id, 1).await.unwrap();
    sign(&w, s).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await, ("Delivering".into(), None));
    head(&w.admin, ORIGIN, released + BUDGET + 10).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert!(
        cancel_order(&w.admin, signed_of(&w.admin, id, 1).await)
            .await
            .is_some()
    );
}

/// CND-PayoutWindow, PR #52 review S-2: a refusal is a window hold only from the structured
/// refusal both providers returned alike, over the token's own delivery allowance. The same words
/// in the reason with no structured refusal (one provider's text, or two that disagree), or a
/// structured refusal over another key, hold nothing; and such a failure releases an earlier
/// window hold rather than keeping it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_payout_window_s2_only_a_refusal_both_providers_agree_on_holds_the_window(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    let id = withdrawal(&w, 20, [50; 32], 700).await;
    w.cycle(t0()).await;
    let key = allowance_key();
    let words = format!("Roles: ConditionViolation(status 17, {key}) (execution reverted)");

    // The words alone: no hold.
    let (o, _) = order(&w.admin, id, 1).await.unwrap();
    prepare(&w.admin, o, Some(&words)).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await, ("Source-Final".into(), None));

    // A structured refusal over another allowance: no hold.
    let (o, _) = order(&w.admin, id, 1).await.unwrap();
    let other = format!("0x{}", "fe".repeat(32));
    prepare_allowance_spent(&w.admin, o, &other).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await, ("Source-Final".into(), None));

    // The token's own allowance, both providers alike: the window.
    let (o, _) = order(&w.admin, id, 1).await.unwrap();
    prepare_allowance_spent(&w.admin, o, &key).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-PayoutWindow".into()))
    );

    // Then a failure that is not the allowance's: the hold is left, not kept.
    let (o, _) = order(&w.admin, id, 1).await.unwrap();
    prepare(&w.admin, o, Some(&words)).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await, ("Source-Final".into(), None));
}

/// The pages of a payout window hold for transfer `id`.
async fn window_pages(w: &World, id: i64) -> Vec<Value> {
    events(w, id, "payout-window-alert").await
}

/// CND-PayoutWindow, PR #52 review S-3: a hold that lasts longer than its declared bound (two
/// allowance periods by default) pages a human, once, then once a day while it lasts, on the
/// destination network's clock. It stays a hold: no review, no refund.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_payout_window_s3_a_hold_past_its_bound_pages_once_then_daily(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let taken = 1_000;
    head(&w.admin, ORIGIN, taken).await;
    let id = held(&w, 51).await;
    assert_eq!(
        transitions(
            &w,
            id,
            &format!("condition = 'CND-PayoutWindow' AND block_number = {taken}")
        )
        .await,
        1,
        "the hold rests on the final head it was taken at"
    );
    let bound = 2 * 86_400;

    head(&w.admin, ORIGIN, taken + bound).await;
    w.settle(t0()).await;
    assert!(window_pages(&w, id).await.is_empty(), "not past the bound");

    head(&w.admin, ORIGIN, taken + bound + 1).await;
    w.settle(t0()).await;
    w.settle(t0()).await;
    let pages = window_pages(&w, id).await;
    assert_eq!(pages.len(), 1, "paged once");
    assert_eq!(
        (&pages[0]["head_block"], &pages[0]["bound_seconds"]),
        (&json!(taken + bound + 1), &json!(bound))
    );

    head(&w.admin, ORIGIN, taken + bound + 43_200).await;
    w.settle(t0()).await;
    assert_eq!(
        window_pages(&w, id).await.len(),
        1,
        "not again the same day"
    );

    head(&w.admin, ORIGIN, taken + bound + 1 + 86_400).await;
    w.settle(t0()).await;
    w.settle(t0()).await;
    assert_eq!(window_pages(&w, id).await.len(), 2, "then daily");
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-PayoutWindow".into()))
    );
    assert_eq!(transitions(&w, id, "to_state = 'Needs-Review'").await, 0);
    assert_eq!(refunds(&w).await, 0);
}

/// REQ-NX-4, REQ-RV-2, PR #52 review S-4: the race between the payout and its cancel, after the
/// cancel was signed. The payout is included first, so it consumed the nonce; the endpoint
/// records the cancel as not executed (its nonce consumed by the payout). From then on the core
/// records nothing more for the cancel, attaches the inclusion to the review as its evidence
/// (resolves as Delivered), and reads no proof for the payout: a refund is refused even with its
/// second approver, and the review closes only on Delivered, then completes.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_nx_4_s4_the_payout_included_after_its_cancel_was_signed_resolves_as_delivered(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, signed) = cancel_signed_in_review(&w, 52).await;
    let before = cancels(&w).await;
    assert_eq!(before, 2, "the order and its signing intent");

    // The payout lands at PAST + 5, final at PAST + 20; the cancel, at the same nonce, does not.
    let included = PAST + 5;
    let paid = observe(
        &w.admin,
        ORIGIN,
        included,
        0,
        TOKEN,
        700,
        None,
        Some(BENEFICIARY),
        Some(&payout_reference_of(&w.admin, id).await),
        true,
        json!({"event": "Payout"}),
    )
    .await;
    decide(&w.admin, paid, "final", PAST + 20).await;
    let (cancel_intent, _) = cancel_signing_intent(&w.admin, signed).await.unwrap();
    sqlx::query(
        "INSERT INTO non_execution_proofs (signed_transaction_id, network, signer, nonce, \
             valid_from_block, valid_until_block, final_block_number, final_block_hash, \
             nonce_at_final, kind, at_block_number, at_block_hash) \
         SELECT st.id, $2, $3, 1, 1, 9223372036854775807, $4, $5, 2, \
             'nonce-consumed-by-other', $6, $7 \
         FROM signed_transactions st WHERE st.intent_id = $1",
    )
    .bind(cancel_intent)
    .bind(ORIGIN)
    .bind(payout_lane().to_vec())
    .bind(PAST + 20)
    .bind(hash(ORIGIN, PAST + 20))
    .bind(included)
    .bind(hash(ORIGIN, included))
    .execute(&w.admin)
    .await
    .unwrap();

    w.settle(t0()).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(cancels(&w).await, before, "nothing more for the cancel");
    let evidence = events(&w, id, "review-evidence").await;
    assert_eq!(evidence.len(), 1, "attached once");
    assert_eq!(
        (
            &evidence[0]["kind"],
            &evidence[0]["resolves_to"],
            &evidence[0]["block_number"]
        ),
        (&json!("included"), &json!("Delivered"), &json!(included))
    );
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM non_execution_proofs n \
             JOIN signed_transactions st ON st.id = n.signed_transaction_id \
             JOIN intents i ON i.id = st.intent_id WHERE i.transfer_id = {id}"
        ))
        .await,
        0,
        "the payout executed: no proof of its non-execution"
    );

    // A refund, even with its second approver, is refused: no proof.
    let refund = resolution(&w, id, "Refunding", ORIGIN, PAST + 20, 0).await;
    approve_as(&w, 1, refund).await.unwrap();
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(refunds(&w).await, 0);

    // Delivered, on the leg's own coordinate; then Completed at its finality.
    resolution(&w, id, "Delivered", ORIGIN, included, 0).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await, ("Completed".into(), None));
    assert_eq!(refunds(&w).await, 0);
    assert_eq!(cancels(&w).await, before);
}

/// REQ-NX-4, PR #52 review S-4: a cancel prepared but not yet signed when the payout is observed
/// included is never signed: no signing intent is recorded for it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_nx_4_s4_once_the_payout_is_included_its_cancel_is_never_signed(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, signed) = payout_in_flight(&w, 53).await;
    head(&w.admin, ORIGIN, PAST).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    let (o, _) = cancel_order(&w.admin, signed)
        .await
        .expect("a cancel order");

    observe(
        &w.admin,
        ORIGIN,
        PAST + 5,
        0,
        TOKEN,
        700,
        None,
        Some(BENEFICIARY),
        Some(&payout_reference_of(&w.admin, id).await),
        true,
        json!({"event": "Payout"}),
    )
    .await;
    prepare_cancel(&w.admin, o, None).await;
    w.settle(t0()).await;
    w.settle(t0()).await;
    assert!(cancel_signing_intent(&w.admin, signed).await.is_none());
    assert_eq!(cancels(&w).await, 1, "the order alone");
    assert_eq!(
        events(&w, id, "review-evidence").await[0]["kind"],
        json!("included")
    );
}
