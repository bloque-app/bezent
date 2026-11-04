// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #26: a mint of a reference after its proof of non-execution (`AC-I1.2`, `REQ-IC-4`,
//! `INV-8`, `INV-16`). Layer: service, through `Core::cycle` on the core's own login (SPEC §9:
//! `INV-8` is store + service + chain; `AC-I1.2` is the compromise path of F-005 and the core).
//!
//! Each case that must alert drives the whole core cycle, so removing the one call that wires the
//! check into the cycle fails it on an assertion. The cases that must not alert pass either way,
//! and are there to keep the check from over-reaching.

mod common;

use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

fn now() -> chrono::DateTime<chrono::Utc> {
    t0()
}

/// A deposit whose first mint attempt is signed and proven not executed at the issuing network's
/// final block `proof_at`. The core has then ordered attempt 2 (`REQ-RF-5`, same reference).
async fn proven_deposit(w: &World, proof_at: i64) -> (i64, Vec<u8>) {
    let a = arrival(&w.admin, 10, 5_000).await;
    let id = deposit(&w.admin, 10, 5_000).await;
    decide(&w.admin, a, "final", 12).await;
    w.settle(now()).await;
    deliver(w, id, 1, now()).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    prove(&w.admin, id, 1, ISSUING, proof_at).await;
    w.cycle(now()).await;
    assert!(order(&w.admin, id, 2).await.is_some(), "attempt 2 ordered");
    let reference = reference_of(&w.admin, id).await;
    (id, reference)
}

/// A final `Minted` of `reference` at the issuing network's `block`.
async fn mint_at(pool: &PgPool, block: i64, reference: &[u8]) -> i64 {
    let o = observe(
        pool,
        ISSUING,
        block,
        0,
        ISSUING_ASSET,
        5_000,
        None,
        Some(DESTINATION_ACCOUNT),
        Some(reference),
        true,
        json!({"event": "Minted"}),
    )
    .await;
    decide(pool, o, "final", block + 2).await;
    o
}

/// A `RefundMinted` of `reference` to `to` at the issuing network's `block`, final.
async fn refund_mint_at(pool: &PgPool, block: i64, reference: &[u8], to: &str) -> i64 {
    let o = observe(
        pool,
        ISSUING,
        block,
        0,
        ISSUING_ASSET,
        700,
        None,
        Some(to),
        Some(reference),
        true,
        json!({"event": "RefundMinted"}),
    )
    .await;
    decide(pool, o, "final", block + 2).await;
    o
}

/// The endpoint records signed transaction `signed` included, and executed, in the block of the
/// observation (`ISSUING`, `block`), as it records an inclusion (`REQ-OB-6`).
async fn included(pool: &PgPool, signed: i64, block: i64, reference: &[u8], executed: bool) {
    sqlx::query(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, sender, on_chain_reference, executed, detail, kind) \
         VALUES ($1, $2, $3, $4, 'transaction', 9, '0xlane', $5, $6, $7, 'transaction')",
    )
    .bind(ISSUING)
    .bind(block)
    .bind(hash(ISSUING, block))
    .bind(t0() + chrono::Duration::seconds(block))
    .bind(reference)
    .bind(executed)
    .bind(json!({"signed_transaction": signed.to_string()}))
    .execute(pool)
    .await
    .unwrap();
}

/// The era of attempt `attempt` of transfer `id`'s prepared transaction.
async fn set_era(w: &World, id: i64, attempt: i32, from: i64, until: i64) {
    exec(
        &w.admin,
        &format!(
            "ALTER TABLE prepared_transactions DISABLE TRIGGER USER; \
             UPDATE prepared_transactions SET valid_from_block = {from}, valid_until_block = {until} \
             WHERE id::text IN (SELECT payload ->> 'prepared' FROM intents \
                                WHERE transfer_id = {id} AND attempt = {attempt} AND payload ? 'prepared'); \
             ALTER TABLE prepared_transactions ENABLE TRIGGER USER;"
        ),
    )
    .await;
}

async fn alerts(w: &World) -> i64 {
    w.count(
        "SELECT count(*) FROM event_log WHERE kind = 'key-compromise' \
           AND payload ->> 'source' = 'late-mint'",
    )
    .await
}

async fn pauses(w: &World) -> i64 {
    w.count(
        "SELECT count(*) FROM event_log p WHERE p.kind = 'pause-requested' \
           AND p.payload ->> 'scope' = 'global' AND EXISTS (SELECT 1 FROM event_log a \
             WHERE a.kind = 'key-compromise' AND a.payload ->> 'source' = 'late-mint' \
               AND a.seq::text = p.payload ->> 'alert_seq')",
    )
    .await
}

/// A withdrawal's refund, up to the refund mint's order: attempts 1 and 2 proven not executed, the
/// refund created and ordered. Returns the refund's id and the redemption's reference.
async fn refund_ordered(w: &World, reference: [u8; 32], redeemer: &str) -> (i64, i64) {
    reserve(&w.admin, 1_000_000).await;
    let r = redemption_by(&w.admin, 20, redeemer, &reference, 700, BENEFICIARY).await;
    decide(&w.admin, r, "final", 22).await;
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
    prove(&w.admin, id, 1, ORIGIN, 100).await;
    deliver(w, id, 2, t0()).await;
    prove(&w.admin, id, 2, ORIGIN, 22 + BUDGET + 100).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Refunding");
    let refund: i64 = scalar(
        &w.admin,
        &format!("SELECT id FROM transfers WHERE refund_of = {id}"),
    )
    .await;
    w.cycle(t0()).await;
    w.cycle(t0()).await;
    w.cycle(t0()).await;
    assert!(order(&w.admin, refund, 1).await.is_some(), "refund ordered");
    (id, refund)
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_inv_8_a_mint_after_the_proof_raises_the_alert_and_the_global_pause_once(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (_, reference) = proven_deposit(&w, 100).await;
    // Minted at block 105, final, after the proof's final block 100, by nothing bezent signed.
    mint_at(&w.admin, 105, &reference).await;
    w.cycle(now()).await;
    assert_eq!((alerts(&w).await, pauses(&w).await), (1, 1));
    let (detector, key): (String, String) = sqlx::query_as(
        "SELECT payload ->> 'detector', payload ->> 'detection_key' FROM event_log \
         WHERE kind = 'key-compromise' AND payload ->> 'source' = 'late-mint'",
    )
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(detector, "core");
    assert!(key.starts_with("dedupe:late-mint:"), "{key}");
    let proof: Value = sqlx::query_scalar(
        "SELECT payload -> 'proof' FROM event_log WHERE kind = 'key-compromise' \
           AND payload ->> 'source' = 'late-mint'",
    )
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(proof, json!({"final_block_number": 100}));

    // The same core again, and a core restarted from nothing but the store: nothing more.
    w.cycle(now()).await;
    w.restarted().await.cycle(now()).await.unwrap();
    assert_eq!((alerts(&w).await, pauses(&w).await), (1, 1));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_replayed_observation_raises_nothing_more(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (_, reference) = proven_deposit(&w, 100).await;
    mint_at(&w.admin, 105, &reference).await;
    w.cycle(now()).await;
    assert_eq!(alerts(&w).await, 1);
    // The observer replays the block (a restart, a refilled gap): the store records an observation
    // once per coordinate (INV-10), and the core finds the alert there.
    let again = sqlx::query(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, token, amount, recipient, on_chain_reference, executed, detail, kind) \
         SELECT network, block_number, block_hash, block_time, position_kind, position_index, \
             token, amount, recipient, on_chain_reference, executed, detail, kind \
         FROM observations WHERE detail ->> 'event' = 'Minted'",
    )
    .execute(&w.admin)
    .await;
    assert!(again.is_err(), "INV-10: one observation per coordinate");
    w.cycle(now()).await;
    assert_eq!((alerts(&w).await, pauses(&w).await), (1, 1));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_8_a_mint_at_or_before_the_proofs_final_block_is_not_a_match(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (_, reference) = proven_deposit(&w, 100).await;
    // At the proof's final block itself, the endpoint's read of the contract covers it.
    mint_at(&w.admin, 100, &reference).await;
    w.cycle(now()).await;
    assert_eq!(alerts(&w).await, 0);
    assert_eq!(
        w.count("SELECT count(*) FROM event_log WHERE kind = 'pause-requested'")
            .await,
        0
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_16_a_mint_that_is_not_final_raises_nothing(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (_, reference) = proven_deposit(&w, 100).await;
    // Observed with no outcome yet, and one withdrawn: neither is final (INV-16).
    observe(
        &w.admin,
        ISSUING,
        105,
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
    let withdrawn = observe(
        &w.admin,
        ISSUING,
        106,
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
    decide(&w.admin, withdrawn, "withdrawn", 108).await;
    w.cycle(now()).await;
    assert_eq!(alerts(&w).await, 0);
    // The one that becomes final then raises it.
    let pending: i64 = scalar(
        &w.admin,
        "SELECT id FROM observations WHERE block_number = 105",
    )
    .await;
    decide(&w.admin, pending, "final", 107).await;
    w.cycle(now()).await;
    assert_eq!(alerts(&w).await, 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_5_a_later_attempt_executed_in_the_mints_block_is_legitimate(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, reference) = proven_deposit(&w, 100).await;
    deliver(&w, id, 2, now()).await;
    let second = signed_of(&w.admin, id, 2).await;
    // Attempt 2 is included, and executes, at block 105, which is when the mint is seen.
    included(&w.admin, second, 105, &reference, true).await;
    mint_at(&w.admin, 105, &reference).await;
    w.settle(now()).await;
    assert_eq!(alerts(&w).await, 0);
    assert_eq!(w.state(id).await.0, "Completed");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_5_a_later_attempt_not_yet_decided_waits_and_then_the_mint_is_late(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, reference) = proven_deposit(&w, 100).await;
    deliver(&w, id, 2, now()).await;
    let second = signed_of(&w.admin, id, 2).await;
    // Its era covers the mint's block, and the endpoint has not decided it: the mint may be its.
    set_era(&w, id, 2, 90, 150).await;
    mint_at(&w.admin, 105, &reference).await;
    w.cycle(now()).await;
    assert_eq!(alerts(&w).await, 0, "undecided: waits");
    // The endpoint decides it: the contract says the reference was used, so it proves nothing and
    // records an anomaly. The mint was no attempt of bezent's.
    exec(
        &w.admin,
        &format!(
            "INSERT INTO event_log (kind, payload) VALUES ('delivery-anomaly', \
               '{{\"network\": \"{ISSUING}\", \"signed_transaction\": {second}, \
                 \"reason\": \"reference used\"}}')"
        ),
    )
    .await;
    w.cycle(now()).await;
    assert_eq!((alerts(&w).await, pauses(&w).await), (1, 1));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_later_attempt_whose_era_has_ended_holds_nothing_back(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, reference) = proven_deposit(&w, 100).await;
    deliver(&w, id, 2, now()).await;
    // Attempt 2's era ended before the mint's block: it cannot be the mint.
    set_era(&w, id, 2, 90, 99).await;
    mint_at(&w.admin, 105, &reference).await;
    w.cycle(now()).await;
    assert_eq!((alerts(&w).await, pauses(&w).await), (1, 1));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_later_attempt_included_but_refused_does_not_explain_the_mint(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, reference) = proven_deposit(&w, 100).await;
    deliver(&w, id, 2, now()).await;
    let second = signed_of(&w.admin, id, 2).await;
    set_era(&w, id, 2, 90, 150).await;
    // Attempt 2 was included at 104 and refused by the contract; the reference was minted at 105
    // by something else.
    included(&w.admin, second, 104, &reference, false).await;
    mint_at(&w.admin, 105, &reference).await;
    w.cycle(now()).await;
    assert_eq!((alerts(&w).await, pauses(&w).await), (1, 1));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ic_2_a_mint_with_no_reference_of_bezents_is_not_this_alert_and_diverges_instead(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (_, _) = proven_deposit(&w, 100).await;
    // A reference no order, no proof and no signature names: the supply the book does not explain
    // is a divergence of the reserve equation (`req_bk_1_a_mint_no_deposit_explains_diverges_on_supply`
    // in reserve_check.rs), not this check's: backfilled history may hold such mints.
    mint_at(&w.admin, 105, &[0xee; 32]).await;
    w.cycle(now()).await;
    assert_eq!(alerts(&w).await, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_8_purposes_never_cross(options: PgPoolOptions, connect: PgConnectOptions) {
    let w = World::new(options, connect).await;
    let (_, reference) = proven_deposit(&w, 100).await;
    // A refund mint of a deposit's mint reference is no mint of it: another purpose, another scope.
    refund_mint_at(&w.admin, 105, &reference, DESTINATION_ACCOUNT).await;
    w.cycle(now()).await;
    assert_eq!(alerts(&w).await, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_inv_8_a_refund_mint_after_the_refund_mints_proof_raises_the_alert(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let reference = [7u8; 32];
    let (_, refund) = refund_ordered(&w, reference, REDEEMER).await;
    let (o, _) = order(&w.admin, refund, 1).await.unwrap();
    prepare(&w.admin, o, None).await;
    w.cycle(t0()).await;
    let (s, _) = signing_intent(&w.admin, refund, 1).await.unwrap();
    sign(&w, s).await;
    w.cycle(t0()).await;
    prove(&w.admin, refund, 1, ISSUING, 300).await;
    w.cycle(t0()).await;
    // Another redeemer's refund mint of the same reference is another redemption's: nothing.
    refund_mint_at(&w.admin, 305, &reference, DESTINATION_ACCOUNT).await;
    w.cycle(t0()).await;
    assert_eq!(alerts(&w).await, 0);
    // A mint of the reference as a deposit's is another purpose: nothing.
    mint_at(&w.admin, 306, &reference).await;
    w.cycle(t0()).await;
    assert_eq!(alerts(&w).await, 0);
    // The redeemer's refund mint, after the proof of the refund mint's only transaction: late.
    refund_mint_at(&w.admin, 310, &reference, REDEEMER).await;
    w.cycle(t0()).await;
    assert_eq!((alerts(&w).await, pauses(&w).await), (1, 1));
    w.cycle(t0()).await;
    assert_eq!(alerts(&w).await, 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_inv_8_a_refund_mint_while_the_refund_is_ordered_and_unsigned_raises_the_alert(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let reference = [8u8; 32];
    refund_ordered(&w, reference, REDEEMER).await;
    // The refund is ordered, and nothing is signed for it (INV-9): a refund mint of the
    // redemption cannot be bezent's.
    refund_mint_at(&w.admin, 400, &reference, REDEEMER).await;
    w.cycle(t0()).await;
    assert_eq!((alerts(&w).await, pauses(&w).await), (1, 1));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_deposits_mint_order_with_nothing_signed_is_not_matched(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let a = arrival(&w.admin, 10, 5_000).await;
    let id = deposit(&w.admin, 10, 5_000).await;
    decide(&w.admin, a, "final", 12).await;
    w.settle(now()).await;
    assert!(order(&w.admin, id, 1).await.is_some(), "mint ordered");
    let reference = reference_of(&w.admin, id).await;
    // The issue names a proof or a refund order; a pending deposit's mint order is neither. The
    // case is left open (see the pull request): nothing is raised here.
    mint_at(&w.admin, 105, &reference).await;
    w.cycle(now()).await;
    assert_eq!(alerts(&w).await, 0);
}
