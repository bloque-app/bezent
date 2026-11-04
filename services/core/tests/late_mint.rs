// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #26: a mint of a reference after its proof of non-execution (`AC-I1.2`, `REQ-IC-4`,
//! `INV-8`, `INV-16`). Layer: service, through `Core::cycle` on the core's own login (SPEC §9:
//! `INV-8` is store + service + chain; `AC-I1.2` is the compromise path of F-005 and the core).
//!
//! Each case that must alert drives the whole core cycle, so removing the one call that wires the
//! check into the cycle fails it on an assertion. The cases that must not alert pass either way,
//! and are there to keep the check from over-reaching: the alarm is a global pause, so a false
//! alarm is an outage.
//!
//! What is real here: the core (`Core::cycle`, on its own login), the signer (`World::sign`), the
//! store's triggers and grants (the queue of migration 0075 is filled by the store, not by the
//! tests). What is a fixture: the endpoints (a prepared transaction, the signed bytes, an
//! inclusion, a proof) and the chain (a `Minted` observation), written as the endpoint writes them.

mod common;

use bezent_core_service::late_mint::{HELD_PAGE_AFTER_CYCLES, PAGE_AFTER_FAILURES};
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
    proven_deposit_from(w, 10, proof_at).await
}

/// [`proven_deposit`] of the arrival at origin block `block`.
async fn proven_deposit_from(w: &World, block: i64, proof_at: i64) -> (i64, Vec<u8>) {
    let a = arrival(&w.admin, block, 5_000).await;
    let id = deposit(&w.admin, block, 5_000).await;
    decide(&w.admin, a, "final", block + 2).await;
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
    mint_final_at(pool, block, reference, block + 2).await
}

/// A `Minted` of `reference` at `block`, decided final at the final head `at`: far past the block
/// when the observer is catching up on old history.
async fn mint_final_at(pool: &PgPool, block: i64, reference: &[u8], at: i64) -> i64 {
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
    decide(pool, o, "final", at).await;
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

/// The global pauses in force that rest on a late mint's alert, each with its record in the log
/// (issue #77).
async fn pauses(w: &World) -> i64 {
    let requests = w
        .count(
            "SELECT count(*) FROM event_log p WHERE p.kind = 'pause-requested' \
               AND p.payload ->> 'scope' = 'global' AND EXISTS (SELECT 1 FROM event_log a \
                 WHERE a.kind = 'key-compromise' AND a.payload ->> 'source' = 'late-mint' \
                   AND a.seq::text = p.payload ->> 'alert_seq')",
        )
        .await;
    let applied = w
        .count(
            "SELECT count(*) FROM pauses p JOIN event_log a ON a.seq = p.alert_seq \
             WHERE a.kind = 'key-compromise' AND a.payload ->> 'source' = 'late-mint' \
               AND p.cause = 'REQ-IC-4' AND p.asset IS NULL AND p.network IS NULL \
               AND p.direction IS NULL",
        )
        .await;
    assert_eq!(
        applied, requests,
        "AC-I1.2: a late mint's alert with no pause"
    );
    applied
}

/// Observations of the check still open (not settled).
async fn open_checks(w: &World) -> i64 {
    w.count("SELECT count(*) FROM late_mint_checks WHERE settled_at IS NULL")
        .await
}

async fn checked(w: &World, observation: i64) -> Option<chrono::DateTime<chrono::Utc>> {
    sqlx::query_scalar("SELECT checked_at FROM late_mint_checks WHERE observation_id = $1")
        .bind(observation)
        .fetch_one(&w.admin)
        .await
        .unwrap()
}

async fn pages(w: &World) -> i64 {
    w.count("SELECT count(*) FROM event_log WHERE kind = 'late-mint-check-failed'")
        .await
}

/// Pages of the prover not deciding an attempt a mint waits on.
async fn watch_pages(w: &World) -> i64 {
    w.count("SELECT count(*) FROM event_log WHERE kind = 'late-mint-watch-failed'")
        .await
}

async fn silent_pages(w: &World) -> i64 {
    w.count("SELECT count(*) FROM event_log WHERE kind = 'late-mint-prover-silent'")
        .await
}

/// Every pause the core requested, whatever raised it.
async fn all_pauses(w: &World) -> i64 {
    w.count("SELECT count(*) FROM event_log WHERE kind = 'pause-requested'")
        .await
}

async fn held_cycles(w: &World, observation: i64) -> i32 {
    sqlx::query_scalar("SELECT held_cycles FROM late_mint_checks WHERE observation_id = $1")
        .bind(observation)
        .fetch_one(&w.admin)
        .await
        .unwrap()
}

/// The prover records attempt `signed` refused on chain at `block` (its inclusion, not executed),
/// as it records a refusal (`REQ-OB-6`).
async fn prover_refuses(w: &World, signed: i64, block: i64, reference: &[u8]) {
    included(&w.admin, signed, block, reference, false).await;
}

/// The prover records a `delivery-anomaly` naming attempt `signed`: the contract says the
/// reference was used, so nothing is proven (`INV-8`).
async fn prover_flags(w: &World, signed: i64) {
    exec(
        &w.admin,
        &format!(
            "INSERT INTO event_log (kind, payload) VALUES ('delivery-anomaly', \
               '{{\"network\": \"{ISSUING}\", \"signed_transaction\": {signed}, \
                 \"reason\": \"reference used\"}}')"
        ),
    )
    .await;
}

/// The prover records a `delivery-anomaly` naming attempt `signed`, with `extra` in its payload (a
/// marker, as the endpoint writes it): the anomaly says nothing of whether the attempt minted.
async fn prover_flags_with(w: &World, signed: i64, extra: &str) {
    exec(
        &w.admin,
        &format!(
            "INSERT INTO event_log (kind, payload) VALUES ('delivery-anomaly', \
               '{{\"network\": \"{ISSUING}\", \"signed_transaction\": {signed}, \
                 \"reason\": \"unreadable\", {extra}}}')"
        ),
    )
    .await;
}

async fn verdict(w: &World, observation: i64) -> Option<String> {
    sqlx::query_scalar("SELECT verdict FROM late_mint_checks WHERE observation_id = $1")
        .bind(observation)
        .fetch_one(&w.admin)
        .await
        .unwrap()
}

/// A `Minted` of `reference` at (`ISSUING`, `block`, `hash`, log `index`), not yet decided.
async fn mint_seen<'e, E>(db: E, block: i64, hash: Vec<u8>, index: i64, reference: &[u8]) -> i64
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query_scalar(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, token, amount, recipient, on_chain_reference, executed, detail, kind) \
         VALUES ($1, $2, $3, $4, 'log', $5, $6, 5000, $7, $8, true, $9, \
             observation_kind_for_event('Minted')) RETURNING id",
    )
    .bind(ISSUING)
    .bind(block)
    .bind(hash)
    .bind(t0() + chrono::Duration::seconds(block))
    .bind(index)
    .bind(ISSUING_ASSET)
    .bind(DESTINATION_ACCOUNT)
    .bind(reference)
    .bind(json!({"event": "Minted"}))
    .fetch_one(db)
    .await
    .unwrap()
}

/// Observation `id` is final, decided at the issuing network's block `at`.
async fn finalise<'e, E>(db: E, id: i64, at: i64)
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(
        "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
             final_block_hash) VALUES ($1, 'final', $2, $3)",
    )
    .bind(id)
    .bind(at)
    .bind(hash(ISSUING, at))
    .execute(db)
    .await
    .unwrap();
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
    let m = mint_at(&w.admin, 105, &[0xee; 32]).await;
    w.cycle(now()).await;
    assert_eq!(alerts(&w).await, 0);
    assert_eq!(verdict(&w, m).await.as_deref(), Some("not-late"));
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
async fn ac_i1_2_req_ic_4_inv_9_a_mint_of_a_pending_deposits_reference_with_nothing_signed_raises(
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
    // The endpoint stores a transaction's exact bytes before it submits it (INV-9): a mint of a
    // reference with an order and no signed transaction is a signature bezent did not record.
    let m = mint_at(&w.admin, 105, &reference).await;
    w.cycle(now()).await;
    assert_eq!((alerts(&w).await, pauses(&w).await), (1, 1));
    assert_eq!(verdict(&w, m).await.as_deref(), Some("alerted"));
    let reason: String = sqlx::query_scalar(
        "SELECT payload ->> 'reason' FROM event_log WHERE kind = 'key-compromise' \
           AND payload ->> 'source' = 'late-mint'",
    )
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert!(reason.contains("signed nothing"), "{reason}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_9_a_delivery_bezent_signed_never_raises_whatever_is_recorded_first(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let a = arrival(&w.admin, 10, 5_000).await;
    let id = deposit(&w.admin, 10, 5_000).await;
    decide(&w.admin, a, "final", 12).await;
    w.settle(now()).await;
    deliver(&w, id, 1, now()).await;
    let signed = signed_of(&w.admin, id, 1).await;
    let reference = reference_of(&w.admin, id).await;
    // The mint is seen final before the endpoint records the transaction's inclusion: nothing is
    // raised, and the check keeps the observation until the inclusion is there.
    let m = mint_at(&w.admin, 105, &reference).await;
    w.cycle(now()).await;
    assert_eq!((alerts(&w).await, open_checks(&w).await), (0, 1));
    included(&w.admin, signed, 105, &reference, true).await;
    w.cycle(now()).await;
    assert_eq!((alerts(&w).await, open_checks(&w).await), (0, 0));
    assert_eq!(verdict(&w, m).await.as_deref(), Some("not-late"));
    assert_eq!(
        w.count(
            "SELECT count(*) FROM event_log WHERE kind IN ('pause-requested', 'key-compromise')"
        )
        .await,
        0
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_8_a_second_mint_in_the_block_of_an_executed_attempt_is_not_explained_by_it(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, reference) = proven_deposit(&w, 100).await;
    deliver(&w, id, 2, now()).await;
    let second = signed_of(&w.admin, id, 2).await;
    // Attempt 2 is included, and executed, at block 105 ...
    included(&w.admin, second, 105, &reference, true).await;
    // ... and block 105 holds two mint events of the reference: the contract mints one once
    // (INV-8), so one of them is not the attempt's. The observation does not say which extrinsic
    // emitted an event, so the two cannot be told apart; the surplus is what raises.
    for index in [0, 1] {
        let m = mint_seen(&w.admin, 105, hash(ISSUING, 105), index, &reference).await;
        decide(&w.admin, m, "final", 107).await;
    }
    w.cycle(now()).await;
    assert_eq!((alerts(&w).await, pauses(&w).await), (2, 2));
}

// The prover and the head (second review of PR #72). The wait on an undecided attempt ends on the
// prover's own record (a proof, an inclusion, an anomaly), never on the observer's final head: the
// two are written by separate workers, and the alarm is a global pause.

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_5_a_retry_in_its_eras_last_block_with_the_head_past_the_era_waits_for_the_prover(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, reference) = proven_deposit(&w, 100).await;
    deliver(&w, id, 2, now()).await;
    let second = signed_of(&w.admin, id, 2).await;
    // Attempt 2 is signed, and included at block 150, the last block of its era; the observer
    // records the mint at 150 as final under a head of 152, past the era. The prover has not
    // stepped yet: it has recorded nothing for attempt 2.
    set_era(&w, id, 2, 90, 150).await;
    let m = mint_at(&w.admin, 150, &reference).await;
    for _ in 0..3 {
        w.cycle(now()).await;
    }
    assert_eq!(
        (
            alerts(&w).await,
            all_pauses(&w).await,
            open_checks(&w).await
        ),
        (0, 0, 1),
        "the head is past the era, the prover is not: nothing is raised, the row stays open"
    );
    // The prover steps and records the inclusion, executed: the retry is the mint.
    included(&w.admin, second, 150, &reference, true).await;
    w.cycle(now()).await;
    assert_eq!(verdict(&w, m).await.as_deref(), Some("explained"));
    assert_eq!((alerts(&w).await, all_pauses(&w).await), (0, 0));
    assert_eq!(silent_pages(&w).await, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_5_an_observer_catching_up_on_old_history_raises_nothing_while_the_prover_has_not_decided(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, reference) = proven_deposit(&w, 100).await;
    deliver(&w, id, 2, now()).await;
    let second = signed_of(&w.admin, id, 2).await;
    set_era(&w, id, 2, 90, 150).await;
    // The observer reads old history and records the mint at 105 as final under a head of 5000:
    // far past the era from the first look. The prover is behind.
    let m = mint_final_at(&w.admin, 105, &reference, 5_000).await;
    for _ in 0..3 {
        w.cycle(now()).await;
    }
    assert_eq!((alerts(&w).await, all_pauses(&w).await), (0, 0));
    assert_eq!(verdict(&w, m).await, None);
    // The prover catches up: attempt 2 was included, and executed, at 105.
    included(&w.admin, second, 105, &reference, true).await;
    w.cycle(now()).await;
    assert_eq!(verdict(&w, m).await.as_deref(), Some("explained"));
    assert_eq!((alerts(&w).await, all_pauses(&w).await), (0, 0));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_real_late_mint_still_raises_once_when_the_prover_decides_against_the_retry(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // Refused on chain: the prover records the retry's inclusion, not executed; the mint at 105 is
    // another's.
    let w = World::new(options.clone(), connect.clone()).await;
    let (id, reference) = proven_deposit(&w, 100).await;
    deliver(&w, id, 2, now()).await;
    let second = signed_of(&w.admin, id, 2).await;
    set_era(&w, id, 2, 90, 150).await;
    let m = mint_final_at(&w.admin, 105, &reference, 5_000).await;
    w.cycle(now()).await;
    assert_eq!(alerts(&w).await, 0, "the prover has not decided: waits");
    prover_refuses(&w, second, 104, &reference).await;
    w.cycle(now()).await;
    assert_eq!((alerts(&w).await, pauses(&w).await), (1, 1));
    assert_eq!(verdict(&w, m).await.as_deref(), Some("alerted"));
    w.cycle(now()).await;
    w.restarted().await.cycle(now()).await.unwrap();
    assert_eq!((alerts(&w).await, pauses(&w).await), (1, 1), "once");
    assert_eq!(silent_pages(&w).await, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_real_late_mint_raises_once_when_the_prover_flags_the_retry_as_an_anomaly(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // A key minted the reference under another nonce: the contract says it was used, so the prover
    // proves nothing and records an anomaly for the retry (INV-8).
    let w = World::new(options, connect).await;
    let (id, reference) = proven_deposit(&w, 100).await;
    deliver(&w, id, 2, now()).await;
    let second = signed_of(&w.admin, id, 2).await;
    set_era(&w, id, 2, 90, 150).await;
    mint_final_at(&w.admin, 105, &reference, 5_000).await;
    w.cycle(now()).await;
    assert_eq!(alerts(&w).await, 0);
    prover_flags(&w, second).await;
    w.cycle(now()).await;
    w.cycle(now()).await;
    assert_eq!((alerts(&w).await, pauses(&w).await), (1, 1));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_prover_that_never_decides_pages_once_without_a_pause_and_the_alarm_still_raises_later(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let log = PageLog::start();
    let (id, reference) = proven_deposit(&w, 100).await;
    deliver(&w, id, 2, now()).await;
    let second = signed_of(&w.admin, id, 2).await;
    set_era(&w, id, 2, 90, 150).await;
    let m = mint_final_at(&w.admin, 105, &reference, 152).await;
    let pauses_before = all_pauses(&w).await;
    // Cycles that find the attempt undecided with its era over count; the page is at the bound.
    for _ in 0..HELD_PAGE_AFTER_CYCLES - 1 {
        w.cycle(now()).await;
    }
    assert_eq!(held_cycles(&w, m).await, HELD_PAGE_AFTER_CYCLES - 1);
    assert_eq!((silent_pages(&w).await, alerts(&w).await), (0, 0));
    w.cycle(now()).await;
    assert_eq!(silent_pages(&w).await, 1, "the page, at the bound");
    // Once within the day, however many cycles, and across a restart (the clock here is fixed; the
    // next test moves it); the row stays open; nothing is raised, and nothing is paused.
    for _ in 0..3 {
        w.cycle(now()).await;
    }
    w.restarted().await.cycle(now()).await.unwrap();
    assert_eq!(silent_pages(&w).await, 1, "once");
    assert_eq!(
        (
            alerts(&w).await,
            all_pauses(&w).await,
            open_checks(&w).await
        ),
        (0, pauses_before, 1)
    );
    let page: Value =
        sqlx::query_scalar("SELECT payload FROM event_log WHERE kind = 'late-mint-prover-silent'")
            .fetch_one(&w.admin)
            .await
            .unwrap();
    assert_eq!(page["observation"], json!(m.to_string()));
    assert_eq!(
        page["on_chain_reference"],
        json!(format!("0x{}", hex::encode(&reference)))
    );
    assert_eq!(page["final_head"], json!(152));
    assert_eq!(page["attempts"][0]["signed_transaction"], json!(second));
    assert_eq!(page["attempts"][0]["attempt"], json!(2));
    assert_eq!(page["attempts"][0]["valid_until"], json!(150));
    // The prover decides at last, against the mint: the alarm raises, once, and the page stands.
    prover_flags(&w, second).await;
    w.cycle(now()).await;
    w.cycle(now()).await;
    assert_eq!((alerts(&w).await, pauses(&w).await), (1, 1));
    assert_eq!(verdict(&w, m).await.as_deref(), Some("alerted"));
    assert_eq!(silent_pages(&w).await, 1);
    assert_page(&log, "late-mint-prover-silent", 1);
}

/// Review of the fix round of #77: an unbounded hold had a single page and nothing after it. While
/// the mint is still held with the prover still silent the page repeats once a day, from the log
/// (so a restart neither repeats early nor forgets), and it is never a pause and never the alert.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_prover_that_stays_silent_is_paged_again_each_day_across_restarts_and_never_pauses(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, reference) = proven_deposit(&w, 100).await;
    deliver(&w, id, 2, now()).await;
    set_era(&w, id, 2, 90, 150).await;
    mint_final_at(&w.admin, 105, &reference, 152).await;
    let pauses_before = all_pauses(&w).await;
    for _ in 0..HELD_PAGE_AFTER_CYCLES {
        w.cycle(now()).await;
    }
    assert_eq!(silent_pages(&w).await, 1, "the first page, at the bound");
    let day = chrono::Duration::days(1);
    w.cycle(now() + day - chrono::Duration::minutes(1)).await;
    assert_eq!(silent_pages(&w).await, 1, "quiet within the day");
    w.restarted()
        .await
        .cycle(now() + day - chrono::Duration::minutes(1))
        .await
        .unwrap();
    assert_eq!(silent_pages(&w).await, 1, "a restart repeats nothing early");
    w.cycle(now() + day).await;
    assert_eq!(silent_pages(&w).await, 2, "a day on, the page repeats");
    w.cycle(now() + day).await;
    w.restarted().await.cycle(now() + day).await.unwrap();
    assert_eq!(silent_pages(&w).await, 2, "once a day, restart or not");
    w.restarted().await.cycle(now() + day * 2).await.unwrap();
    assert_eq!(silent_pages(&w).await, 3, "and the day after");
    assert_eq!(
        (
            alerts(&w).await,
            all_pauses(&w).await,
            open_checks(&w).await
        ),
        (0, pauses_before, 1),
        "a page is not a pause, and the row stays open"
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_an_attempt_within_its_era_never_pages_however_long_it_waits(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, reference) = proven_deposit(&w, 100).await;
    deliver(&w, id, 2, now()).await;
    set_era(&w, id, 2, 90, 150).await;
    // The head is at the era's last block: the prover cannot have decided an open era yet.
    let m = mint_final_at(&w.admin, 105, &reference, 150).await;
    for _ in 0..HELD_PAGE_AFTER_CYCLES + 5 {
        w.cycle(now()).await;
    }
    assert_eq!(held_cycles(&w, m).await, 0, "an open era is not counted");
    assert_eq!((silent_pages(&w).await, alerts(&w).await), (0, 0));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_real_signing_rows_always_name_their_era_and_one_without_waits_on_the_prover_and_pages(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, reference) = proven_deposit(&w, 100).await;
    deliver(&w, id, 2, now()).await;
    // The signing path: the core records a signing intent that names its prepared transaction (a
    // number, checked against the order by migration 0037), the endpoint signs only such an
    // intent. So every real signed transaction has an era.
    assert_eq!(
        w.count(
            "SELECT count(*) FROM signed_transactions st JOIN intents i ON i.id = st.intent_id \
             WHERE NOT EXISTS (SELECT 1 FROM prepared_transactions p \
                               WHERE p.id::text = i.payload ->> 'prepared')"
        )
        .await,
        0
    );
    // And the store refuses, to the core's own login, a signing intent that names none.
    let core = login_pool(&w.admin, CORE_LOGIN, bezent_store::CORE_ROLE, 1).await;
    let refused = sqlx::query(
        "INSERT INTO intents (transfer_id, network, purpose, on_chain_reference, attempt, payload) \
         SELECT transfer_id, network, purpose, on_chain_reference, 2, \
                '{\"prepared\": 999999, \"message\": \"0x00\", \"amount\": \"1\"}'::jsonb \
         FROM intents WHERE transfer_id = $1 AND attempt = 2 AND payload ? 'order'",
    )
    .bind(id)
    .execute(&core)
    .await;
    let refused = refused.expect_err("migration 0037").to_string();
    assert!(refused.contains("signing intent refused"), "{refused}");

    // A fixture for the case that should not occur: a signed row on an intent with no prepared
    // transaction, so no era. It waits on the prover like any other attempt, whatever the head
    // (the head is not a decision), and counts toward the page from the first look.
    exec(
        &w.admin,
        &format!(
            "INSERT INTO signed_transactions (intent_id, attempt, bytes) \
             SELECT id, 1, '\\xbeef' FROM intents \
             WHERE transfer_id = {id} AND attempt = 2 AND payload ? 'order' AND NOT payload ? 'prepared'"
        ),
    )
    .await;
    // (Attempt 2's own signing intent also has a signed row: the unknown-era row is the other.)
    let m = mint_final_at(&w.admin, 105, &reference, 5_000).await;
    for _ in 0..HELD_PAGE_AFTER_CYCLES {
        w.cycle(now()).await;
    }
    assert_eq!((alerts(&w).await, open_checks(&w).await), (0, 1));
    assert_eq!(silent_pages(&w).await, 1);
    assert_eq!(held_cycles(&w, m).await, HELD_PAGE_AFTER_CYCLES);
}

/// Review of the fix round of #77: the streak counted as paged before the event was inserted, so a
/// failed insert left a log line and no page in the log, for good. It is paged when the event is in the log.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_page_that_could_not_be_recorded_is_tried_again_not_counted_as_made(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let log = PageLog::start();
    let (_, reference) = proven_deposit(&w, 100).await;
    mint_at(&w.admin, 105, &reference).await;
    exec(
        &w.admin,
        "REVOKE SELECT ON late_mint_checks FROM bezent_core",
    )
    .await;
    for _ in 0..PAGE_AFTER_FAILURES - 1 {
        w.cycle(now()).await;
    }
    // The streak reaches its bound while the core cannot write the log.
    exec(&w.admin, "REVOKE INSERT ON event_log FROM bezent_core").await;
    let _ = w.core.cycle(now()).await;
    assert_eq!(watch_pages(&w).await, 0, "the insert could not be made");
    let _ = w.core.cycle(now()).await;
    assert_eq!(watch_pages(&w).await, 0);
    // The log is writable again: the next cycle makes the page that was not made, and then no more.
    exec(&w.admin, "GRANT INSERT ON event_log TO bezent_core").await;
    let _ = w.core.cycle(now()).await;
    assert_eq!(watch_pages(&w).await, 1, "the page that was missed is made");
    let _ = w.core.cycle(now()).await;
    assert_eq!(watch_pages(&w).await, 1, "once");
    // And the log line is one per streak, not one per cycle while the insert kept failing.
    assert_page(&log, "late-mint-watch-failed", 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_core_that_cannot_list_the_mints_warns_then_pages_once_per_streak(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let log = PageLog::start();
    let (_, reference) = proven_deposit(&w, 100).await;
    let m = mint_at(&w.admin, 105, &reference).await;
    // The core's login cannot read the queue: the candidate query fails every cycle.
    exec(
        &w.admin,
        "REVOKE SELECT ON late_mint_checks FROM bezent_core",
    )
    .await;
    for _ in 0..PAGE_AFTER_FAILURES - 1 {
        w.cycle(now()).await;
    }
    assert_eq!(watch_pages(&w).await, 0, "a failure warns");
    w.cycle(now()).await;
    assert_eq!(watch_pages(&w).await, 1, "a streak pages");
    w.cycle(now()).await;
    w.cycle(now()).await;
    assert_eq!(watch_pages(&w).await, 1, "once");
    assert_eq!(verdict(&w, m).await, None, "nothing was checked meanwhile");
    // It recovers: the mints are checked, and a new streak would page again.
    exec(&w.admin, "GRANT SELECT ON late_mint_checks TO bezent_core").await;
    w.cycle(now()).await;
    assert!(verdict(&w, m).await.is_some());
    exec(
        &w.admin,
        "REVOKE SELECT ON late_mint_checks FROM bezent_core",
    )
    .await;
    for _ in 0..PAGE_AFTER_FAILURES {
        w.cycle(now()).await;
    }
    assert_eq!(watch_pages(&w).await, 2, "the next streak pages again");
    assert_page(&log, "late-mint-watch-failed", 2);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_mint_finalised_after_a_later_one_or_committed_after_it_is_still_looked_at(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (_, first_ref) = proven_deposit_from(&w, 10, 100).await;
    let (_, second_ref) = proven_deposit_from(&w, 11, 101).await;
    let (_, third_ref) = proven_deposit_from(&w, 12, 102).await;

    // A: the lowest id, seen now and finalised only later.
    let a = mint_seen(&w.admin, 105, hash(ISSUING, 105), 0, &first_ref).await;
    // C: an id between, whose transaction commits after B's (the observer's write is slow).
    let mut slow = w.admin.begin().await.unwrap();
    let c = mint_seen(&mut *slow, 107, hash(ISSUING, 107), 0, &third_ref).await;
    finalise(&mut *slow, c, 109).await;
    // B: the highest id, final and committed at once.
    let b = mint_at(&w.admin, 106, &second_ref).await;
    assert!(a < c && c < b);

    w.cycle(now()).await;
    assert_eq!(
        alerts(&w).await,
        1,
        "B only: A is not final, C not committed"
    );
    assert_eq!(verdict(&w, b).await.as_deref(), Some("alerted"));

    slow.commit().await.unwrap();
    w.cycle(now()).await;
    assert_eq!(alerts(&w).await, 2, "C, committed after B");
    assert_eq!(verdict(&w, c).await.as_deref(), Some("alerted"));

    finalise(&w.admin, a, 108).await;
    w.cycle(now()).await;
    assert_eq!(alerts(&w).await, 3, "A, finalised after both");
    assert_eq!(verdict(&w, a).await.as_deref(), Some("alerted"));
    assert_eq!(open_checks(&w).await, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn a_settled_mint_is_not_read_again_and_an_undecided_one_stays_in_the_set(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, reference) = proven_deposit(&w, 100).await;
    deliver(&w, id, 2, now()).await;
    set_era(&w, id, 2, 90, 500).await;
    let settled = mint_at(&w.admin, 100, &[0xee; 32]).await;
    let held = mint_at(&w.admin, 105, &reference).await;
    w.cycle(now()).await;
    let (settled_at, held_at) = (checked(&w, settled).await, checked(&w, held).await);
    assert_eq!(verdict(&w, settled).await.as_deref(), Some("not-late"));
    assert_eq!(verdict(&w, held).await, None);
    for _ in 0..3 {
        w.cycle(now()).await;
    }
    assert_eq!(
        checked(&w, settled).await,
        settled_at,
        "a settled mint is not read"
    );
    assert!(
        checked(&w, held).await > held_at,
        "an undecided one is looked at again"
    );
    assert_eq!((open_checks(&w).await, alerts(&w).await), (1, 0));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_the_dedupe_survives_a_restart_in_the_middle_of_a_raise_and_of_a_wait(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, reference) = proven_deposit(&w, 100).await;
    deliver(&w, id, 2, now()).await;
    let second = signed_of(&w.admin, id, 2).await;
    set_era(&w, id, 2, 90, 150).await;
    let m = mint_at(&w.admin, 105, &reference).await;
    // Waiting on the undecided attempt: a core restarted from the store waits too.
    w.cycle(now()).await;
    let restarted = w.restarted().await;
    restarted.cycle(now()).await.unwrap();
    assert_eq!((alerts(&w).await, open_checks(&w).await), (0, 1));
    // The attempt is refused; the restarted core raises, once.
    exec(
        &w.admin,
        &format!(
            "INSERT INTO event_log (kind, payload) VALUES ('delivery-anomaly', \
               '{{\"network\": \"{ISSUING}\", \"signed_transaction\": {second}, \"reason\": \"x\"}}')"
        ),
    )
    .await;
    restarted.cycle(now()).await.unwrap();
    assert_eq!((alerts(&w).await, pauses(&w).await), (1, 1));
    // A crash between the alert and the settling leaves the observation open: the next core finds
    // the alert recorded, settles it, and raises nothing more.
    exec(
        &w.admin,
        &format!("UPDATE late_mint_checks SET settled_at = NULL, verdict = NULL WHERE observation_id = {m}"),
    )
    .await;
    w.restarted().await.cycle(now()).await.unwrap();
    w.cycle(now()).await;
    assert_eq!((alerts(&w).await, pauses(&w).await), (1, 1));
    assert_eq!(verdict(&w, m).await.as_deref(), Some("alerted"));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_16_a_mint_withdrawn_in_a_reorganisation_and_final_again_elsewhere_raises_once(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (_, reference) = proven_deposit(&w, 100).await;
    let abandoned = mint_seen(&w.admin, 105, vec![0xdd; 8], 0, &reference).await;
    decide(&w.admin, abandoned, "withdrawn", 107).await;
    w.cycle(now()).await;
    assert_eq!(alerts(&w).await, 0);
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM late_mint_checks WHERE observation_id = {abandoned}"
        ))
        .await,
        0,
        "a withdrawn observation is never queued"
    );
    // The block is rebuilt with another hash, and the mint is in it, final.
    let again = mint_seen(&w.admin, 105, hash(ISSUING, 105), 0, &reference).await;
    decide(&w.admin, again, "final", 108).await;
    w.cycle(now()).await;
    w.cycle(now()).await;
    assert_eq!((alerts(&w).await, pauses(&w).await), (1, 1));
    let at: String = sqlx::query_scalar(
        "SELECT payload -> 'coordinate' ->> 'block_hash' FROM event_log \
         WHERE kind = 'key-compromise' AND payload ->> 'source' = 'late-mint'",
    )
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(at, format!("0x{}", hex::encode(hash(ISSUING, 105))));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_5_a_legitimate_retry_seen_again_after_a_reorganisation_raises_nothing(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, reference) = proven_deposit(&w, 100).await;
    deliver(&w, id, 2, now()).await;
    let second = signed_of(&w.admin, id, 2).await;
    included(&w.admin, second, 105, &reference, true).await;
    let abandoned = mint_seen(&w.admin, 105, vec![0xdd; 8], 0, &reference).await;
    decide(&w.admin, abandoned, "withdrawn", 107).await;
    let again = mint_seen(&w.admin, 105, hash(ISSUING, 105), 0, &reference).await;
    decide(&w.admin, again, "final", 108).await;
    w.cycle(now()).await;
    assert_eq!((alerts(&w).await, open_checks(&w).await), (0, 0));
    assert_eq!(verdict(&w, again).await.as_deref(), Some("explained"));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_the_first_walk_over_a_backlog_of_delivered_history_raises_nothing(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    // The deliveries first (the core cycles while they are made) ...
    let mut delivered = Vec::new();
    // Three deposits whose first attempt was proven not executed and whose retry was executed.
    for (k, block) in [10_i64, 11, 12].into_iter().enumerate() {
        let k = k as i64;
        let (id, reference) = proven_deposit_from(&w, block, 100 + k).await;
        deliver(&w, id, 2, now()).await;
        delivered.push((signed_of(&w.admin, id, 2).await, reference, 105 + k));
    }
    // Two delivered at the first attempt.
    for block in [13_i64, 14] {
        let a = arrival(&w.admin, block, 5_000).await;
        let id = deposit(&w.admin, block, 5_000).await;
        decide(&w.admin, a, "final", block + 2).await;
        w.settle(now()).await;
        deliver(&w, id, 1, now()).await;
        delivered.push((
            signed_of(&w.admin, id, 1).await,
            reference_of(&w.admin, id).await,
            200 + block,
        ));
    }
    // ... then the history the chain shows, all of it final, with no cycle of the core after it.
    let mut history = 0;
    for (signed, reference, at) in delivered {
        included(&w.admin, signed, at, &reference, true).await;
        mint_at(&w.admin, at, &reference).await;
        history += 1;
    }
    // A mint of an earlier deployment (no order here), and one seen and withdrawn.
    mint_at(&w.admin, 300, &[0xee; 32]).await;
    history += 1;
    let gone = mint_seen(&w.admin, 301, vec![0xdd; 8], 0, &[0xef; 32]).await;
    decide(&w.admin, gone, "withdrawn", 303).await;

    // The core has not looked at any of it. Its first walk, from a cold start.
    assert_eq!(open_checks(&w).await, history);
    assert_eq!(alerts(&w).await, 0);
    w.restarted().await.cycle(now()).await.unwrap();
    assert_eq!(
        w.count(
            "SELECT count(*) FROM event_log WHERE kind IN ('pause-requested', 'key-compromise')"
        )
        .await,
        0
    );
    assert_eq!(open_checks(&w).await, 0, "every one of them is settled");
    assert_eq!(
        w.count("SELECT count(*) FROM late_mint_checks WHERE verdict = 'alerted'")
            .await,
        0
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_mint_that_cannot_be_checked_pages_and_blocks_none_of_the_others(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let log = PageLog::start();
    let (_, first_ref) = proven_deposit_from(&w, 10, 100).await;
    let (_, second_ref) = proven_deposit_from(&w, 11, 101).await;
    // The first one's look settles it (at the proof's own block, no match), the second is a late
    // mint. The store refuses to settle the first: a persistent failure of one candidate (an
    // injected fixture), with the lowest id, so that it comes first every time.
    let stuck = mint_at(&w.admin, 100, &first_ref).await;
    let late = mint_at(&w.admin, 105, &second_ref).await;
    exec(
        &w.admin,
        &format!(
            "CREATE FUNCTION refuse_settling() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN IF NEW.settled_at IS NOT NULL THEN RAISE EXCEPTION 'injected failure'; END IF; \
             RETURN NEW; END $$; \
             CREATE TRIGGER refuse_settling BEFORE UPDATE ON late_mint_checks \
             FOR EACH ROW WHEN (OLD.observation_id = {stuck}) EXECUTE FUNCTION refuse_settling();"
        ),
    )
    .await;
    w.cycle(now()).await;
    assert_eq!(
        verdict(&w, late).await.as_deref(),
        Some("alerted"),
        "the failure of one does not stop the next"
    );
    assert_eq!((alerts(&w).await, pauses(&w).await), (1, 1));
    assert_eq!(pages(&w).await, 0, "one failure only warns");
    w.cycle(now()).await;
    assert_eq!(pages(&w).await, 0);
    w.cycle(now()).await;
    assert_eq!(pages(&w).await, 1, "a persistent one pages");
    w.cycle(now()).await;
    assert_eq!(pages(&w).await, 1, "once");
    let page: Value =
        sqlx::query_scalar("SELECT payload FROM event_log WHERE kind = 'late-mint-check-failed'")
            .fetch_one(&w.admin)
            .await
            .unwrap();
    assert_eq!(page["observation"], json!(stuck.to_string()));
    assert_page(&log, "late-mint-check-failed", 1);
    // When the failure clears, it settles.
    exec(&w.admin, "DROP TRIGGER refuse_settling ON late_mint_checks").await;
    w.cycle(now()).await;
    assert_eq!(verdict(&w, stuck).await.as_deref(), Some("not-late"));
}

/// Review of PR #80: an anomaly that says the prover cannot read the attempt (a recorded extrinsic
/// this build cannot decode took its nonce; its recorded bytes do not decode or do not answer their
/// rows) never says the attempt did not mint, so it does not decide it. The mint stays held and
/// pages. The kinds that do say so (`reference-used`, above) still decide.
async fn an_unreadable_attempt_holds(
    options: PgPoolOptions,
    connect: PgConnectOptions,
    marker: &str,
) {
    let w = World::new(options, connect).await;
    let (id, reference) = proven_deposit(&w, 100).await;
    deliver(&w, id, 2, now()).await;
    let second = signed_of(&w.admin, id, 2).await;
    set_era(&w, id, 2, 90, 150).await;
    let m = mint_final_at(&w.admin, 105, &reference, 152).await;
    let pauses_before = all_pauses(&w).await;
    prover_flags_with(&w, second, marker).await;
    for _ in 0..HELD_PAGE_AFTER_CYCLES + 3 {
        w.cycle(now()).await;
    }
    assert_eq!(
        (
            alerts(&w).await,
            all_pauses(&w).await,
            silent_pages(&w).await
        ),
        (0, pauses_before, 1)
    );
    assert_eq!(verdict(&w, m).await, None, "the row stays open");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_an_attempt_taken_by_a_recorded_extrinsic_nobody_can_read_stays_held_and_pages(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    an_unreadable_attempt_holds(options, connect, "\"recorded_undecodable\": true").await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_an_attempt_whose_recorded_bytes_cannot_be_read_stays_held_and_pages(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    an_unreadable_attempt_holds(options, connect, "\"bytes_unreadable\": true").await;
}

/// The marker is read as `payload @> {marker: true}`: a value an endpoint wrote that is not `true`
/// neither fails the check (no `failures`, no `late-mint-check-failed`) nor holds the mint: the
/// anomaly decides, as an unmarked one (an older row) does.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_marker_that_is_not_true_decides_like_no_marker_and_fails_nothing(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (id, reference) = proven_deposit(&w, 100).await;
    deliver(&w, id, 2, now()).await;
    let second = signed_of(&w.admin, id, 2).await;
    set_era(&w, id, 2, 90, 150).await;
    let m = mint_final_at(&w.admin, 105, &reference, 152).await;
    prover_flags_with(
        &w,
        second,
        "\"recorded_undecodable\": \"yes\", \"bytes_unreadable\": [1]",
    )
    .await;
    w.cycle(now()).await;
    w.cycle(now()).await;
    assert_eq!((alerts(&w).await, all_pauses(&w).await > 0), (1, true));
    assert_eq!(verdict(&w, m).await.as_deref(), Some("alerted"));
    assert_eq!(
        w.count("SELECT count(*) FROM event_log WHERE kind = 'late-mint-check-failed'")
            .await,
        0
    );
}

/// The anomaly the endpoint writes for a nonce taken by a recorded extrinsic nobody can read, with
/// the `coordinate` of the block that holds that extrinsic (`proof.rs`, `consumer`).
async fn prover_cannot_read_the_consumer(w: &World, signed: i64, coordinate: &str) {
    prover_flags_with(
        w,
        signed,
        &format!("\"recorded_undecodable\": true, \"coordinate\": {coordinate}"),
    )
    .await;
}

fn coordinate_at(block: i64) -> String {
    coordinate_with(
        block,
        &format!("\"0x{}\"", hex::encode(hash(ISSUING, block))),
    )
}

/// A coordinate at `block` whose `block_hash` is the JSON value `block_hash`, verbatim.
fn coordinate_with(block: i64, block_hash: &str) -> String {
    format!(
        "{{\"network\": \"{ISSUING}\", \"block_number\": {block}, \"block_hash\": {block_hash}, \
          \"position_kind\": \"transaction\", \"position_index\": 3}}"
    )
}

/// A reference proven once, with a second attempt whose era covers a mint at block 105.
async fn second_attempt_and_a_mint(w: &World) -> (i64, i64) {
    let (id, reference) = proven_deposit(w, 100).await;
    deliver(w, id, 2, now()).await;
    let second = signed_of(&w.admin, id, 2).await;
    set_era(w, id, 2, 90, 150).await;
    let m = mint_final_at(&w.admin, 105, &reference, 152).await;
    (second, m)
}

/// Review of PR #80 (item 2): the recorded extrinsic that took the nonce can have minted only in
/// its own block. A mint in another block is not its, so the anomaly decides the attempt and the
/// mint, after the first attempt's proof, is the late mint it is: the alert and the pause.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_inv_8_a_mint_in_another_block_than_the_unreadable_consumers_is_not_its(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (second, m) = second_attempt_and_a_mint(&w).await;
    prover_cannot_read_the_consumer(&w, second, &coordinate_at(104)).await;
    w.cycle(now()).await;
    w.cycle(now()).await;
    assert_eq!((alerts(&w).await, pauses(&w).await), (1, 1));
    assert_eq!(verdict(&w, m).await.as_deref(), Some("alerted"));
}

/// The guard against the false pause: the mint in the very block that holds the unreadable
/// consumer may be that consumer's own (bezent's), so it holds and pages. An anomaly without a
/// coordinate, or with one that cannot be read, cannot be compared, so it holds too.
async fn holds_with_coordinate(
    options: PgPoolOptions,
    connect: PgConnectOptions,
    coordinate: String,
) {
    let w = World::new(options, connect).await;
    let (second, m) = second_attempt_and_a_mint(&w).await;
    let before = all_pauses(&w).await;
    prover_cannot_read_the_consumer(&w, second, &coordinate).await;
    for _ in 0..HELD_PAGE_AFTER_CYCLES + 1 {
        w.cycle(now()).await;
    }
    assert_eq!(
        (
            alerts(&w).await,
            all_pauses(&w).await,
            silent_pages(&w).await
        ),
        (0, before, 1),
        "{coordinate}"
    );
    assert_eq!(verdict(&w, m).await, None, "{coordinate}");
    assert_eq!(
        w.count("SELECT count(*) FROM event_log WHERE kind = 'late-mint-check-failed'")
            .await,
        0
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_inv_8_a_mint_in_the_unreadable_consumers_own_block_holds(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    holds_with_coordinate(options, connect, coordinate_at(105)).await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_inv_8_the_block_hash_is_compared_whatever_its_case(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    holds_with_coordinate(
        options,
        connect,
        // Only the hex is uppercased; the keys and the `0x` stay, so the path resolves and
        // `lower()` is what makes it equal.
        coordinate_with(
            105,
            &format!("\"0x{}\"", hex::encode_upper(hash(ISSUING, 105))),
        ),
    )
    .await;
}

/// A `block_hash` that is not `0x` and hex of the mint's hash's length cannot be compared: the
/// attempt is not decided, the mint at that very block holds, and no pause is raised.
async fn holds_with_block_hash(options: PgPoolOptions, connect: PgConnectOptions, value: String) {
    holds_with_coordinate(options, connect, coordinate_with(105, &value)).await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_inv_8_a_numeric_block_hash_holds(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    holds_with_block_hash(options, connect, "105".to_owned()).await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_inv_8_a_block_hash_without_the_prefix_holds(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let v = format!("\"{}\"", hex::encode(hash(ISSUING, 105)));
    holds_with_block_hash(options, connect, v).await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_inv_8_an_empty_block_hash_holds(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    holds_with_block_hash(options, connect, "\"\"".to_owned()).await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_inv_8_a_block_hash_of_the_wrong_length_holds(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let v = format!("\"0x{}\"", &hex::encode(hash(ISSUING, 105))[..10]);
    holds_with_block_hash(options, connect, v).await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_inv_8_an_unreadable_consumer_with_no_coordinate_holds(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    holds_with_coordinate(options, connect, "{\"block_number\": 104}".to_owned()).await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_inv_8_an_unreadable_consumer_with_a_malformed_coordinate_holds_and_fails_nothing(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    holds_with_coordinate(options, connect, "\"105\"".to_owned()).await;
}

/// Review of PR #80 (item 1): the page's `meaning` is true of each attempt it names. An attempt
/// the prover wrote an anomaly for is not one it "recorded nothing" for.
async fn the_page_says(
    options: PgPoolOptions,
    connect: PgConnectOptions,
    extra: Option<String>,
    record: &str,
) -> Value {
    let w = World::new(options, connect).await;
    let (second, _) = second_attempt_and_a_mint(&w).await;
    if let Some(extra) = extra {
        prover_flags_with(&w, second, &extra).await;
    }
    for _ in 0..HELD_PAGE_AFTER_CYCLES + 1 {
        w.cycle(now()).await;
    }
    let page: Value =
        sqlx::query_scalar("SELECT payload FROM event_log WHERE kind = 'late-mint-prover-silent'")
            .fetch_one(&w.admin)
            .await
            .unwrap();
    assert_eq!(page["attempts"][0]["record"], json!(record));
    page
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_the_page_of_an_attempt_with_no_record_says_the_prover_recorded_nothing(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let page = the_page_says(options, connect, None, "none").await;
    let meaning = page["meaning"].as_str().unwrap();
    assert!(meaning.contains("recorded nothing"), "{meaning}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_req_nx_3_the_page_of_an_attempt_taken_by_an_unreadable_extrinsic_does_not_say_nothing_was_recorded(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let extra = format!(
        "\"recorded_undecodable\": true, \"coordinate\": {}",
        coordinate_at(105)
    );
    let page = the_page_says(options, connect, Some(extra), "recorded-undecodable").await;
    let meaning = page["meaning"].as_str().unwrap();
    assert!(!meaning.contains("recorded nothing"), "{meaning}");
    assert!(meaning.contains("recorded_undecodable"), "{meaning}");
    assert!(meaning.contains("may have minted"), "{meaning}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_req_nx_3_the_page_of_an_attempt_with_unreadable_bytes_does_not_say_nothing_was_recorded(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let page = the_page_says(
        options,
        connect,
        Some("\"bytes_unreadable\": true".to_owned()),
        "bytes-unreadable",
    )
    .await;
    let meaning = page["meaning"].as_str().unwrap();
    assert!(!meaning.contains("recorded nothing"), "{meaning}");
    assert!(meaning.contains("bytes_unreadable"), "{meaning}");
    assert!(meaning.contains("never read the chain"), "{meaning}");
}
