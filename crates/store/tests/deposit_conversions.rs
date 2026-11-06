// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #10 (F-010), migration 0120, amendment 0018 as ruled (`d-cv-conversion-link`, A1b): a
//! deposit that names a convertible arrival moves its source, once, to the conversion's output its
//! requester names, and records the arrival with both amounts (`REQ-CV-2`, `REQ-RQ-4`, `INV-2`).
//!
//! Store layer (SPEC §9: `INV-2` is chain + store). Everything is written as the migration owner,
//! whom no writer guard exempts from these rules: the move is `deposit_conversion_record()` alone,
//! the arrival and the output are each consumed once, and nothing binds a recorded arrival again.
//! The ingress's refusals by name, and the core's mint against the output, are
//! `services/core/tests/conversions.rs`. The deposit's requester declared the convertible arrival's
//! sender (`REQ-PN-3`, migration 0135), so its answer binds it.

mod common;

use common::*;
use sqlx::PgPool;

const RESERVE: &str = "0xreserve";
const POOL: &str = "0xpool";

async fn declarations(pool: &PgPool) {
    exec(
        pool,
        &format!(
            "INSERT INTO networks (network, role, address_format) VALUES \
               ('issuing', 'issuing', 'opaque'), ('origin-a', 'origin', 'opaque'), \
               ('origin-b', 'origin', 'opaque'); \
             INSERT INTO assets (asset, precision, assurance_class) VALUES \
               ('asset-a', 6, 'public'), ('asset-b', 6, 'public'); \
             INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
               ('origin-a', 'token-a', 'asset-a', 6), ('origin-b', 'token-a', 'asset-a', 6), \
               ('origin-a', 'token-b', 'asset-b', 6); \
             INSERT INTO reserves (asset, network, address) VALUES \
               ('asset-a', 'origin-a', '{RESERVE}'), ('asset-a', 'origin-b', '{RESERVE}'), \
               ('asset-b', 'origin-a', '0xreserve-b'); \
             INSERT INTO requesters (requester) VALUES ('swaps'); \
             INSERT INTO requester_senders (requester, network, address) VALUES \
               ('swaps', 'origin-a', '0xfunder')"
        ),
    )
    .await;
}

/// Records, as an endpoint would, an executed movement at (`network`, `block`, log `index`) with
/// the block hash `block` as one byte, and makes it final at `block + 20` unless `open`.
#[allow(clippy::too_many_arguments)]
async fn observed(
    pool: &PgPool,
    network: &str,
    block: i64,
    index: i64,
    kind: &str,
    token: &str,
    amount: u64,
    sender: &str,
    recipient: &str,
    open: bool,
) -> i64 {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, token, amount, sender, recipient, executed, kind) \
         VALUES ($1, $2, decode(lpad(to_hex($2), 2, '0'), 'hex'), $3::timestamptz, 'log', $4, $5, \
             $6::numeric, $7, $8, TRUE, $9) RETURNING id",
    )
    .bind(network)
    .bind(block)
    .bind(BLOCK_TIME)
    .bind(index)
    .bind(token)
    .bind(amount.to_string())
    .bind(sender)
    .bind(recipient)
    .bind(kind)
    .fetch_one(pool)
    .await
    .expect("observe");
    if !open {
        finalise(pool, id).await;
    }
    id
}

async fn finalise(pool: &PgPool, observation: i64) {
    exec(
        pool,
        &format!(
            "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
               SELECT network, block_number + 20, '\\xff', '{BLOCK_TIME}' FROM observations \
               WHERE id = {observation} ON CONFLICT DO NOTHING; \
             INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
               final_block_hash) \
               SELECT id, 'final', block_number + 20, '\\xff' FROM observations WHERE id = {observation}"
        ),
    )
    .await;
}

/// A request of `requester`, recorded as the ingress records one (`REQ-RQ-1`); its id.
async fn request(pool: &PgPool, requester: &str, reference: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO intake (request_reference, requester, kind, body, body_digest) \
         VALUES ($1, $2, 'deposit', '{}', '\\x01') RETURNING id",
    )
    .bind(reference)
    .bind(requester)
    .fetch_one(pool)
    .await
    .expect("intake")
}

/// The deposit `swaps` requests for the convertible arrival at (`origin-a`, `block`, log 0), taken
/// to Source-Final holding CND-AwaitingConversion as the core takes it (resting on the arrival's
/// final head). Its id.
async fn awaiting(pool: &PgPool, reference: &str, block: i64) -> i64 {
    let intake = request(pool, "swaps", reference).await;
    let id: i64 = sqlx::query_scalar(&format!(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index) \
         SELECT 'bz-d-{reference}', '{reference}', 'deposit', 'asset-a', amount, 'issuing', \
             'dest-1', 'Received', network, block_number, block_hash, 'log', 0 \
         FROM observations WHERE network = 'origin-a' AND block_number = {block} \
           AND position_index = 0 RETURNING id"
    ))
    .fetch_one(pool)
    .await
    .expect("the deposit naming a convertible arrival (REQ-RQ-3, REQ-CV-1)");
    exec(
        pool,
        &format!(
            "INSERT INTO intake_answers (intake_id, transfer_id, bridge_reference) \
               VALUES ({intake}, {id}, 'bz-d-{reference}'); \
             INSERT INTO transitions (transfer_id, from_state, to_state) \
               VALUES ({id}, 'Received', 'Accepted'); \
             INSERT INTO transitions (transfer_id, from_state, to_state, condition, network, \
                 block_number, block_hash, block_time) \
               VALUES ({id}, 'Accepted', 'Source-Final', 'CND-AwaitingConversion', 'origin-a', \
                 {block} + 20, '\\xff', '{BLOCK_TIME}')"
        ),
    )
    .await;
    id
}

/// `deposit_conversion_record()` for deposit `id`, in request `intake`, naming the output at
/// (`network`, `block`, log `index`).
async fn record(
    pool: &PgPool,
    id: i64,
    intake: i64,
    network: &str,
    block: i64,
    index: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "SELECT deposit_conversion_record($1, $2, $3, $4, decode(lpad(to_hex($4), 2, '0'), 'hex'), \
             'log', $5)",
    )
    .bind(id)
    .bind(intake)
    .bind(network)
    .bind(block)
    .bind(index)
    .execute(pool)
    .await
    .map(|_| ())
}

/// The deposit's source block and index, and its amount.
async fn source_of(pool: &PgPool, id: i64) -> (i64, i64, String) {
    sqlx::query_as(
        "SELECT source_block_number, source_position_index, amount::text FROM transfers WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

/// `REQ-CV-2`, `REQ-RQ-4`, `INV-2`: the happy path at the store. The deposit's source moves from the
/// convertible arrival to the output, with the output's amount; the arrival is recorded with its own
/// amount, and neither is listed as unassociated (`REQ-IN-6`). The move happens once: a second
/// output is refused, and the source and the amount cannot be rewritten by hand, even by the owner.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_cv_2_req_rq_4_inv_2_a_deposits_source_moves_once_from_its_convertible_arrival_to_its_output(
    pool: PgPool,
) {
    declarations(&pool).await;
    let arrival = observed(
        &pool,
        "origin-a",
        10,
        0,
        "convertible_arrival",
        "convertible-a",
        1_000,
        "0xfunder",
        RESERVE,
        false,
    )
    .await;
    let id = awaiting(&pool, "dep-1", 10).await;
    assert_eq!(source_of(&pool, id).await, (10, 0, "1000".into()));
    let output = observed(
        &pool,
        "origin-a",
        12,
        2,
        "conversion_output",
        "token-a",
        990,
        POOL,
        RESERVE,
        false,
    )
    .await;

    let named = request(&pool, "swaps", "conv-1").await;
    record(&pool, id, named, "origin-a", 12, 2)
        .await
        .expect("the requester names its deposit's output");
    assert_eq!(source_of(&pool, id).await, (12, 2, "990".into()));
    let (token, arrival_amount, output_amount): (String, String, String) = sqlx::query_as(
        "SELECT arrival_token, arrival_amount::text, output_amount::text FROM deposit_conversions \
         WHERE transfer_id = $1 AND intake_id = $2 AND arrival_block_number = 10 \
           AND output_block_number = 12",
    )
    .bind(id)
    .bind(named)
    .fetch_one(&pool)
    .await
    .expect("the record of REQ-CV-2");
    assert_eq!(
        (
            token.as_str(),
            arrival_amount.as_str(),
            output_amount.as_str()
        ),
        ("convertible-a", "1000", "990")
    );
    for o in [arrival, output] {
        assert_eq!(
            count(
                &pool,
                &format!("SELECT count(*) FROM unassociated_arrivals WHERE observation_id = {o}")
            )
            .await,
            0,
            "observation {o} is consumed by the deposit"
        );
    }

    // Once: another output, in another request, is refused; the deposit no longer names a
    // convertible arrival.
    observed(
        &pool,
        "origin-a",
        14,
        2,
        "conversion_output",
        "token-a",
        5,
        POOL,
        RESERVE,
        false,
    )
    .await;
    let again = request(&pool, "swaps", "conv-2").await;
    let err = record(&pool, id, again, "origin-a", 14, 2)
        .await
        .expect_err("a second move");
    assert_refusal(&err, "REQ-CV-1", "a second move");
    assert_eq!(source_of(&pool, id).await, (12, 2, "990".into()));

    // And no one rewrites the source or the amount by hand (REQ-ST-7, INV-2).
    assert_refused(
        &pool,
        &format!("UPDATE transfers SET source_block_number = 14, source_block_hash = '\\x0e' WHERE id = {id}"),
        "INV-2",
    )
    .await;
    assert_refused(
        &pool,
        &format!("UPDATE transfers SET amount = 5 WHERE id = {id}"),
        "REQ-ST-7",
    )
    .await;
    // Nor once the condition is cleared (a NULL condition is no exemption).
    exec(
        &pool,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state) \
             VALUES ({id}, 'Source-Final', 'Source-Final')"
        ),
    )
    .await;
    assert_refused(
        &pool,
        &format!("UPDATE transfers SET amount = 5 WHERE id = {id}"),
        "REQ-ST-7",
    )
    .await;
}

/// `INV-2`: a convertible arrival a deposit recorded stays consumed after its source moved on.
/// Nothing binds it again: not another deposit, not the refund of an unassociated arrival.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_2_an_arrival_recorded_by_a_conversion_is_never_bound_again(pool: PgPool) {
    declarations(&pool).await;
    observed(
        &pool,
        "origin-a",
        10,
        0,
        "convertible_arrival",
        "convertible-a",
        1_000,
        "0xfunder",
        RESERVE,
        false,
    )
    .await;
    let id = awaiting(&pool, "dep-1", 10).await;
    observed(
        &pool,
        "origin-a",
        12,
        2,
        "conversion_output",
        "token-a",
        990,
        POOL,
        RESERVE,
        false,
    )
    .await;
    let named = request(&pool, "swaps", "conv-1").await;
    record(&pool, id, named, "origin-a", 12, 2)
        .await
        .expect("the move");

    for (kind, reference) in [("deposit", "bz-d-twice"), ("refund", "bz-u-twice")] {
        assert_refused(
            &pool,
            &format!(
                "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                     destination_network, destination, state, source_network, source_block_number, \
                     source_block_hash, source_position_kind, source_position_index) \
                 VALUES ('{reference}', 'req-{reference}', '{kind}', 'asset-a', 1000, 'issuing', \
                     'dest-1', 'Received', 'origin-a', 10, '\\x0a', 'log', 0)"
            ),
            "INV-2",
        )
        .await;
    }
    assert_eq!(count(&pool, "SELECT count(*) FROM transfers").await, 1);
}

/// `INV-2`: an output already consumed is refused. Here another deposit named it directly first; the
/// move then fails on the source's unique index, and the awaiting deposit and the record are as they
/// were.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_2_an_output_already_consumed_is_refused(pool: PgPool) {
    declarations(&pool).await;
    observed(
        &pool,
        "origin-a",
        10,
        0,
        "convertible_arrival",
        "convertible-a",
        1_000,
        "0xfunder",
        RESERVE,
        false,
    )
    .await;
    let id = awaiting(&pool, "dep-1", 10).await;
    observed(
        &pool,
        "origin-a",
        12,
        2,
        "conversion_output",
        "token-a",
        990,
        POOL,
        RESERVE,
        false,
    )
    .await;
    exec(
        &pool,
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index) \
         VALUES ('bz-d-first', 'req-first', 'deposit', 'asset-a', 990, 'issuing', 'dest-2', \
             'Received', 'origin-a', 12, '\\x0c', 'log', 2)",
    )
    .await;

    let named = request(&pool, "swaps", "conv-1").await;
    let err = record(&pool, id, named, "origin-a", 12, 2)
        .await
        .expect_err("an output another deposit consumed");
    assert_error_sqlstate(&err, "23505", "the output's second use");
    assert_eq!(source_of(&pool, id).await, (10, 0, "1000".into()));
    assert_eq!(
        count(&pool, "SELECT count(*) FROM deposit_conversions").await,
        0
    );
}

/// `REQ-RQ-4`, `REQ-ID-4`: only the requester that made the deposit names its output, in a request of
/// its own that is not answered yet. Another requester's request is refused, and so is reusing the
/// deposit's own request.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rq_4_only_the_deposits_requester_names_its_output(pool: PgPool) {
    declarations(&pool).await;
    observed(
        &pool,
        "origin-a",
        10,
        0,
        "convertible_arrival",
        "convertible-a",
        1_000,
        "0xfunder",
        RESERVE,
        false,
    )
    .await;
    let id = awaiting(&pool, "dep-1", 10).await;
    observed(
        &pool,
        "origin-a",
        12,
        2,
        "conversion_output",
        "token-a",
        990,
        POOL,
        RESERVE,
        false,
    )
    .await;

    let other = request(&pool, "someone-else", "conv-1").await;
    let err = record(&pool, id, other, "origin-a", 12, 2)
        .await
        .expect_err("another requester names the output");
    assert_refusal(&err, "REQ-RQ-4", "another requester");
    let own: i64 = sqlx::query_scalar("SELECT id FROM intake WHERE request_reference = 'dep-1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let err = record(&pool, id, own, "origin-a", 12, 2)
        .await
        .expect_err("the deposit's own, answered request");
    assert_refusal(&err, "REQ-RQ-4", "an answered request");
    assert_eq!(source_of(&pool, id).await, (10, 0, "1000".into()));

    // A conversion answer names only the deposit its request recorded a conversion for.
    assert_refused(
        &pool,
        &format!(
            "INSERT INTO intake_answers (intake_id, transfer_id, bridge_reference, conversion) \
             VALUES ({other}, {id}, 'bz-d-dep-1', TRUE)"
        ),
        "REQ-RQ-4",
    )
    .await;
}

/// `REQ-CV-2`, `INV-6` (amendment 0018, A2 as A1b needs it): the output is a final conversion output,
/// at the reserve the convertible arrived at, of a token admitted for the deposit's asset. Not final,
/// an ordinary arrival, another network, another asset's token: each is refused and nothing moves.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_cv_2_inv_6_the_output_is_final_at_the_same_reserve_and_of_the_deposits_asset(
    pool: PgPool,
) {
    declarations(&pool).await;
    observed(
        &pool,
        "origin-a",
        10,
        0,
        "convertible_arrival",
        "convertible-a",
        1_000,
        "0xfunder",
        RESERVE,
        false,
    )
    .await;
    let id = awaiting(&pool, "dep-1", 10).await;
    observed(
        &pool,
        "origin-a",
        12,
        2,
        "conversion_output",
        "token-a",
        990,
        POOL,
        RESERVE,
        true,
    )
    .await;
    observed(
        &pool,
        "origin-a",
        13,
        0,
        "arrival",
        "token-a",
        990,
        "0xsomeone",
        RESERVE,
        false,
    )
    .await;
    observed(
        &pool,
        "origin-b",
        14,
        2,
        "conversion_output",
        "token-a",
        990,
        POOL,
        RESERVE,
        false,
    )
    .await;
    observed(
        &pool,
        "origin-a",
        15,
        2,
        "conversion_output",
        "token-b",
        990,
        POOL,
        "0xreserve-b",
        false,
    )
    .await;

    for (n, (network, block, index, spec)) in [
        ("origin-a", 12, 2, "INV-6"),
        ("origin-a", 13, 0, "INV-6"),
        ("origin-b", 14, 2, "REQ-CV-2"),
        ("origin-a", 15, 2, "REQ-CV-2"),
    ]
    .into_iter()
    .enumerate()
    {
        let named = request(&pool, "swaps", &format!("conv-{n}")).await;
        let err = record(&pool, id, named, network, block, index)
            .await
            .expect_err("not a final output of this deposit's asset at its reserve");
        assert_refusal(&err, spec, &format!("{network} block {block}"));
    }
    assert_eq!(source_of(&pool, id).await, (10, 0, "1000".into()));
    assert_eq!(
        count(&pool, "SELECT count(*) FROM deposit_conversions").await,
        0
    );
}

/// Sends deposit `id` to review from `from`, keeping `condition`, on a final head of the issuing
/// network, as the core does at the deadline (`REQ-CV-6`).
async fn to_review(pool: &PgPool, id: i64, from: &str, condition: &str) {
    exec(
        pool,
        &format!(
            "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
               VALUES ('issuing', 500, '\\x01', '{BLOCK_TIME}') ON CONFLICT DO NOTHING; \
             INSERT INTO transitions (transfer_id, from_state, to_state, condition, network, \
                 block_number, block_hash, block_time) \
               VALUES ({id}, '{from}', 'Needs-Review', '{condition}', 'issuing', 500, '\\x01', \
                 '{BLOCK_TIME}')"
        ),
    )
    .await;
}

/// A convertible arrival of 1 000 at block 10 and a final conversion output of 990 at block 12.
async fn arrival_and_output(pool: &PgPool) {
    observed(
        pool,
        "origin-a",
        10,
        0,
        "convertible_arrival",
        "convertible-a",
        1_000,
        "0xfunder",
        RESERVE,
        false,
    )
    .await;
    observed(
        pool,
        "origin-a",
        12,
        2,
        "conversion_output",
        "token-a",
        990,
        POOL,
        RESERVE,
        false,
    )
    .await;
}

/// `REQ-CV-2`, `REQ-RV-2`, `INV-2`, amendment 0018 Open 6 (`d-cv-open-choices` (a)): the output is
/// named for a deposit sent to review from `Source-Final` for its deadline, still holding
/// `CND-AwaitingConversion`. The store moves the source and the amount and records both amounts, as
/// at `Source-Final`, and the deposit stays in `Needs-Review`: nothing here ends the review (that is
/// a recorded resolution with a second approver, `REQ-GV-5`; `services/core/tests/conversions.rs`).
/// The move is still once, and the source still cannot be rewritten by hand.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_cv_2_req_rv_2_inv_2_an_output_is_named_for_a_deposit_in_review(pool: PgPool) {
    declarations(&pool).await;
    arrival_and_output(&pool).await;
    let id = awaiting(&pool, "dep-1", 10).await;
    to_review(&pool, id, "Source-Final", "CND-AwaitingConversion").await;

    // By hand, even by the owner, the source does not move without the record.
    let err = sqlx::query(
        "UPDATE transfers SET source_block_number = 12, source_position_index = 2, \
             source_block_hash = '\\x0c', amount = 990 WHERE id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .expect_err("a source moved by hand in review");
    assert_refusal(&err, "REQ-ST-7", "a source moved by hand in review");

    let named = request(&pool, "swaps", "conv-1").await;
    record(&pool, id, named, "origin-a", 12, 2)
        .await
        .expect("a deposit in review takes the output its requester names");
    assert_eq!(source_of(&pool, id).await, (12, 2, "990".into()));
    assert_eq!(
        count(
            &pool,
            &format!(
                "SELECT count(*) FROM deposit_conversions WHERE transfer_id = {id} \
                   AND arrival_amount = 1000 AND output_amount = 990 AND intake_id = {named}"
            )
        )
        .await,
        1
    );
    let (state, condition): (String, Option<String>) =
        sqlx::query_as("SELECT state::text, condition::text FROM transfers WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (state.as_str(), condition.as_deref()),
        ("Needs-Review", Some("CND-AwaitingConversion"))
    );

    // Once: another output is refused, and the arrival is not unassociated.
    observed(
        &pool,
        "origin-a",
        14,
        2,
        "conversion_output",
        "token-a",
        5,
        POOL,
        RESERVE,
        false,
    )
    .await;
    let again = request(&pool, "swaps", "conv-2").await;
    assert!(record(&pool, id, again, "origin-a", 14, 2).await.is_err());
    assert_eq!(source_of(&pool, id).await, (12, 2, "990".into()));
    assert_eq!(
        count(&pool, "SELECT count(*) FROM unassociated_arrivals").await,
        1,
        "only the later, unnamed output is an unassociated arrival"
    );
}

/// `REQ-CV-2`, `REQ-RF-1`, `REQ-RV-2`, amendment 0018 Open 6 (`d-cv-open-choices` (a)): an output is
/// not named for a deposit after a refund of it is recorded (a transfer whose `refund_of` is the
/// deposit) or started (a review resolution into `Refunding` whose evidence the core did not refuse).
/// A resolution the core refused starts nothing. Nothing is recorded by a refused request.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_cv_2_req_rf_1_req_rv_2_no_output_is_named_after_a_refund(pool: PgPool) {
    declarations(&pool).await;
    arrival_and_output(&pool).await;
    let id = awaiting(&pool, "dep-1", 10).await;
    to_review(&pool, id, "Source-Final", "CND-AwaitingConversion").await;

    // A review resolution into Refunding, recorded and not refused: a refund is started.
    let action: i64 = sqlx::query_scalar(
        "INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, \
             block_hash, block_time, actor, justification) \
         VALUES ('review-resolution', $1, 'Refunding', 'issuing', 500, '\\x01', $2::timestamptz, \
             'ops', 'refund it') RETURNING id",
    )
    .bind(id)
    .bind(BLOCK_TIME)
    .fetch_one(&pool)
    .await
    .expect("a recorded resolution");
    let named = request(&pool, "swaps", "conv-1").await;
    let err = record(&pool, id, named, "origin-a", 12, 2)
        .await
        .expect_err("a refund is started");
    assert_refusal(&err, "REQ-RF-1", "a refund started");
    assert_eq!(source_of(&pool, id).await, (10, 0, "1000".into()));
    assert_eq!(
        count(&pool, "SELECT count(*) FROM deposit_conversions").await,
        0
    );

    // The core refused that evidence: it started nothing.
    exec(
        &pool,
        &format!(
            "INSERT INTO event_log (kind, transfer_id, payload) \
               VALUES ('review-evidence-refused', {id}, \
                       '{{\"manual_action_id\": \"{action}\"}}'::jsonb)"
        ),
    )
    .await;
    let again = request(&pool, "swaps", "conv-2").await;
    record(&pool, id, again, "origin-a", 12, 2)
        .await
        .expect("a refused resolution starts no refund");
    assert_eq!(source_of(&pool, id).await, (12, 2, "990".into()));

    // A refund recorded: a transfer whose refund_of is the deposit. The store takes one only in the
    // shape of a refund of a withdrawal or a deposit's payout; whichever it takes is refused here.
    observed(
        &pool,
        "origin-a",
        16,
        0,
        "convertible_arrival",
        "convertible-a",
        1_000,
        "0xfunder",
        RESERVE,
        false,
    )
    .await;
    let other = awaiting(&pool, "dep-2", 16).await;
    to_review(&pool, other, "Source-Final", "CND-AwaitingConversion").await;
    sqlx::query(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, refund_of, state) \
         SELECT 'bz-r-dep-2', 'r-dep-2', 'refund', asset, 1000, 'origin-a', 'dest-1', id, 'Received' \
         FROM transfers WHERE id = $1",
    )
    .bind(other)
    .execute(&pool)
    .await
    .expect("a refund transfer of the deposit");
    observed(
        &pool,
        "origin-a",
        18,
        2,
        "conversion_output",
        "token-a",
        990,
        POOL,
        RESERVE,
        false,
    )
    .await;
    let named = request(&pool, "swaps", "conv-3").await;
    let err = record(&pool, other, named, "origin-a", 18, 2)
        .await
        .expect_err("a refund is recorded");
    assert_refusal(&err, "REQ-RF-1", "a refund recorded");
}

/// `REQ-CV-2`, `REQ-CV-6`, `REQ-RV-2`: a deposit in review for another reason than its deadline, or
/// sent there from a state other than `Source-Final`, takes no output.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_cv_2_req_cv_6_a_review_that_is_not_the_conversions_takes_no_output(pool: PgPool) {
    declarations(&pool).await;
    arrival_and_output(&pool).await;
    // In review from Source-Final, but for another condition.
    let id = awaiting(&pool, "dep-1", 10).await;
    to_review(&pool, id, "Source-Final", "CND-DestinationRefused").await;
    let named = request(&pool, "swaps", "conv-1").await;
    let err = record(&pool, id, named, "origin-a", 12, 2)
        .await
        .expect_err("another review");
    assert_refusal(&err, "REQ-CV-6", "a review for another condition");
    assert_eq!(source_of(&pool, id).await, (10, 0, "1000".into()));
}
