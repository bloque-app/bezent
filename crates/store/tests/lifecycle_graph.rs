// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-003 T-003-01: the store's state graph is the §5.4 transition table of amendment 0009
//! (`REQ-TR-6`), every allowed edge applied and every other refused, at the store layer; numbered
//! attempts with one signature per (reference, attempt) and the proof gate (`REQ-EO-6`, issue #24);
//! review exits by evidence with their second approver (`REQ-RV-2`, `REQ-GV-5`); per-attempt
//! milestones (`REQ-TR-4`, `NFR-3`); and the redeemer-scoped reference (amendment 0008, `INV-2`).

mod common;

use common::*;
use sqlx::PgPool;

const UNIQUE_VIOLATION: &str = "23505";
const INSUFFICIENT_PRIVILEGE: &str = "42501";

const STATES: [&str; 12] = [
    "Received",
    "Refused",
    "Accepted",
    "Source-Final",
    "Delivering",
    "Delivered",
    "Completed",
    "Refunding",
    "Refunded",
    "Expired",
    "Withdrawn",
    "Needs-Review",
];

const TERMINAL: [&str; 5] = ["Refused", "Completed", "Refunded", "Expired", "Withdrawn"];

/// The ordinary edges of the table (amendment 0009), besides `S -> S` for a non-terminal state.
const EDGES: [(&str, &str); 17] = [
    ("Received", "Refused"),
    ("Received", "Accepted"),
    ("Accepted", "Source-Final"),
    ("Accepted", "Expired"),
    ("Accepted", "Withdrawn"),
    ("Accepted", "Needs-Review"),
    ("Source-Final", "Delivering"),
    ("Source-Final", "Needs-Review"),
    ("Delivering", "Delivered"),
    ("Delivered", "Delivering"),
    ("Delivering", "Refunding"),
    ("Delivered", "Refunding"),
    ("Delivering", "Needs-Review"),
    ("Delivered", "Needs-Review"),
    ("Delivered", "Completed"),
    ("Refunding", "Refunded"),
    // Delivering -> Delivering as a new attempt is its own test below.
    ("Delivering", "Delivering"),
];

/// Gives transfer `id` the on-chain reference `n` (4 bytes), so its attempts have a lane.
async fn reference(pool: &PgPool, id: i64, n: i64) {
    exec(
        pool,
        &format!(
            "UPDATE transfers SET on_chain_reference = int4send({n}::int) WHERE id = {id} \
               AND on_chain_reference IS NULL"
        ),
    )
    .await;
}

/// A deposit brought to `state` along the table; `n` keeps its coordinate and reference apart.
async fn at(pool: &PgPool, n: i64, state: &str) -> i64 {
    let br = format!("g-{n}");
    let block = 1000 + n;
    if TERMINAL.contains(&state) {
        return terminal(pool, &br, state, block).await;
    }
    let id = transfer(pool, &br).await;
    reference(pool, id, n).await;
    if state == "Received" {
        return id;
    }
    if state == "Accepted" {
        bare_transition(pool, id, "Received", "Accepted")
            .await
            .unwrap();
        return id;
    }
    to_source_final(pool, id, block).await;
    match state {
        "Source-Final" => {}
        "Delivering" => walk(pool, id, &["Source-Final", "Delivering"]).await,
        "Delivered" => walk(pool, id, &["Source-Final", "Delivering", "Delivered"]).await,
        "Refunding" => {
            walk(pool, id, &["Source-Final", "Delivering"]).await;
            refund_of(pool, id, &format!("{br}-refund")).await;
            walk(pool, id, &["Delivering", "Refunding"]).await;
        }
        "Needs-Review" => {
            walk(pool, id, &["Source-Final", "Delivering"]).await;
            bare_transition(pool, id, "Delivering", "Needs-Review")
                .await
                .unwrap();
        }
        other => panic!("{other}"),
    }
    id
}

/// Makes ready what an edge into `to` needs besides the edge itself, so that only the edge decides.
async fn prepare(pool: &PgPool, id: i64, n: i64, from: &str, to: &str) {
    if TERMINAL.contains(&from) || from == to {
        return;
    }
    match to {
        "Source-Final" => {
            exec(
                pool,
                &format!(
                    "UPDATE transfers SET source_network = 'origin-a', source_block_number = {}, \
                   source_block_hash = '\\xaa', source_position_kind = 'log', \
                   source_position_index = 0 WHERE id = {id} AND source_network IS NULL",
                    1000 + n
                ),
            )
            .await;
            // Review M1 of PR #56: only on a final source (INV-6).
            finalise_source(pool, id).await;
        }
        "Expired" => {
            exec(
                pool,
                &format!(
                    "UPDATE transfers SET expires_at = '2026-10-12T00:00:00Z' WHERE id = {id} \
                   AND expires_at IS NULL"
                ),
            )
            .await
        }
        "Refunding" => {
            let has: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM transfers WHERE refund_of = $1)")
                    .bind(id)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            if !has {
                refund_of(pool, id, &format!("g-{n}-refund")).await;
            }
            if !proven(pool, id).await {
                prove_not_executed(pool, id).await;
            }
        }
        "Refunded" => {
            let refund: Option<(i64, String)> =
                sqlx::query_as("SELECT id, state::text FROM transfers WHERE refund_of = $1")
                    .bind(id)
                    .fetch_optional(pool)
                    .await
                    .unwrap();
            let refund = match refund {
                Some(r) => r,
                None => (
                    refund_of(pool, id, &format!("g-{n}-refund")).await,
                    "Received".into(),
                ),
            };
            if refund.1 == "Received" {
                to_source_final(pool, refund.0, 1000 + n).await;
                walk(
                    pool,
                    refund.0,
                    &["Source-Final", "Delivering", "Delivered", "Completed"],
                )
                .await;
            }
        }
        "Delivering" if from == "Delivered" || from == "Needs-Review" => {
            prove_not_executed(pool, id).await
        }
        _ => {}
    }
}

/// Appends `from -> to` the way the edge would be appended when it is allowed: a refusal names
/// its error, a self-transition takes a condition, everything else rests on a coordinate.
async fn attempt(pool: &PgPool, id: i64, from: &str, to: &str) -> Result<(), sqlx::Error> {
    if to == "Refused" {
        return sqlx::query(
            "INSERT INTO transitions (transfer_id, from_state, to_state, error) \
             VALUES ($1, $2, 'Refused', 'ERR-BelowMinimum')",
        )
        .bind(id)
        .bind(from)
        .execute(pool)
        .await
        .map(|_| ());
    }
    if from == to {
        return sqlx::query(
            "INSERT INTO transitions (transfer_id, from_state, to_state, condition) \
             VALUES ($1, $2, $2, CASE WHEN $2::text IN ('Refused', 'Completed', 'Refunded', \
                 'Expired', 'Withdrawn') THEN NULL ELSE 'CND-Paused' END)",
        )
        .bind(id)
        .bind(from)
        .execute(pool)
        .await
        .map(|_| ());
    }
    if TERMINAL.contains(&from) {
        // Nothing to prepare: the store refuses any edge out of a terminal state first.
        return sqlx::query(
            "INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
                 block_hash, block_time) \
             VALUES ($1, $2, $3, 'issuing', 10, '\\x0a', '2026-10-13T00:00:00Z')",
        )
        .bind(id)
        .bind(from)
        .bind(to)
        .execute(pool)
        .await
        .map(|_| ());
    }
    transition(pool, id, from, to).await
}

/// Every `from -> to` of one row of the table: allowed exactly when the table says so.
async fn row(pool: &PgPool, from: &str) {
    for (k, to) in STATES.into_iter().enumerate() {
        let n = (STATES.iter().position(|s| *s == from).unwrap() * 100 + k) as i64;
        let id = at(pool, n, from).await;
        prepare(pool, id, n, from, to).await;
        let allowed = (from == to && !TERMINAL.contains(&from)) || EDGES.contains(&(from, to));
        let got = attempt(pool, id, from, to).await;
        if allowed {
            got.unwrap_or_else(|e| panic!("{from} -> {to} was refused: {e}"));
            assert_eq!(
                text(
                    pool,
                    &format!("SELECT state::text FROM transfers WHERE id = {id}")
                )
                .await,
                to,
                "{from} -> {to}"
            );
        } else {
            let err = got.expect_err(&format!("{from} -> {to} was applied"));
            let spec = if TERMINAL.contains(&from) {
                "INV-5"
            } else if from == "Needs-Review" {
                "REQ-RV-2"
            } else {
                "REQ-TR-6"
            };
            assert_refusal(&err, spec, &format!("{from} -> {to}"));
        }
    }
}

macro_rules! rows {
    ($($name:ident => $from:literal),* $(,)?) => {
        $(
            #[sqlx::test(migrator = "bezent_store::MIGRATOR")]
            async fn $name(pool: PgPool) {
                row(&pool, $from).await;
            }
        )*
    };
}

rows! {
    req_tr_6_edges_out_of_received => "Received",
    req_tr_6_edges_out_of_refused => "Refused",
    req_tr_6_edges_out_of_accepted => "Accepted",
    req_tr_6_edges_out_of_source_final => "Source-Final",
    req_tr_6_edges_out_of_delivering => "Delivering",
    req_tr_6_edges_out_of_delivered => "Delivered",
    req_tr_6_edges_out_of_completed => "Completed",
    req_tr_6_edges_out_of_refunding => "Refunding",
    req_tr_6_edges_out_of_refunded => "Refunded",
    req_tr_6_edges_out_of_expired => "Expired",
    req_tr_6_edges_out_of_withdrawn => "Withdrawn",
    req_tr_6_edges_out_of_needs_review => "Needs-Review",
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_tr_6_the_two_pairs_that_are_not_edges_on_purpose(pool: PgPool) {
    // Received -> Source-Final: a deposit bound to a final arrival is created at Source-Final.
    let id = at(&pool, 1, "Received").await;
    prepare(&pool, id, 1, "Received", "Source-Final").await;
    let err = transition(&pool, id, "Received", "Source-Final")
        .await
        .expect_err("Received -> Source-Final");
    assert_refusal(&err, "REQ-TR-6", "Received -> Source-Final");
    bound_deposit(&pool, "g-bound", 2).await;
    // Refunding -> Needs-Review: the refund has its own lifecycle and review.
    let id = at(&pool, 3, "Refunding").await;
    let err = bare_transition(&pool, id, "Refunding", "Needs-Review")
        .await
        .expect_err("Refunding -> Needs-Review");
    assert_refusal(&err, "REQ-TR-6", "Refunding -> Needs-Review");
}

// ---------------------------------------------------------------------------------------------
// REQ-EO-6: numbered attempts, same reference, the next only after the previous is proven.

/// Appends `from -> Delivering` as attempt `attempt`, resting on (issuing, 10, \x0a).
async fn new_attempt(pool: &PgPool, id: i64, from: &str, attempt: i32) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
             block_hash, block_time, attempt) \
         VALUES ($1, $2, 'Delivering', 'issuing', 10, '\\x0a', '2026-10-13T00:00:00Z', $3)",
    )
    .bind(id)
    .bind(from)
    .bind(attempt)
    .execute(pool)
    .await
    .map(|_| ())
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_eo_6_new_attempt_is_refused_before_the_previous_is_proven(pool: PgPool) {
    for (n, from) in [(1, "Delivering"), (2, "Delivered")] {
        let id = at(&pool, n, from).await;
        // The next attempt's intent cannot even be recorded before the proof.
        let err = sqlx::query(
            "INSERT INTO intents (transfer_id, network, purpose, on_chain_reference, attempt, payload) \
             SELECT id, 'issuing', 'mint', on_chain_reference, 2, '{\"amount\": \"1\"}' \
             FROM transfers WHERE id = $1",
        )
        .bind(id)
        .execute(&pool)
        .await
        .expect_err("attempt 2 recorded before attempt 1 was proven");
        assert_refusal(&err, "REQ-EO-6", "intent of attempt 2");
        // Nor can the transfer move to it.
        let err = new_attempt(&pool, id, from, 2)
            .await
            .expect_err("a new attempt before the proof");
        assert_refusal(
            &err,
            "REQ-EO-6",
            &format!("{from} -> Delivering, attempt 2"),
        );
        // Once proven, it can, and the attempt keeps the reference.
        prove_not_executed(&pool, id).await;
        signed_attempt(&pool, id, 2).await;
        new_attempt(&pool, id, from, 2).await.unwrap();
        let (attempts, references): (i64, i64) = sqlx::query_as(
            "SELECT count(DISTINCT attempt), count(DISTINCT on_chain_reference) FROM intents \
             WHERE transfer_id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((attempts, references), (2, 1), "{from}");
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_eo_6_attempts_are_numbered_in_order(pool: PgPool) {
    let id = at(&pool, 1, "Source-Final").await;
    signed_attempt(&pool, id, 1).await;
    // Source-Final -> Delivering is attempt 1, not 2, and not none.
    for wrong in [None, Some(2)] {
        let err = sqlx::query(
            "INSERT INTO transitions (transfer_id, from_state, to_state, attempt) \
             VALUES ($1, 'Source-Final', 'Delivering', $2)",
        )
        .bind(id)
        .bind(wrong)
        .execute(&pool)
        .await
        .expect_err("a wrong attempt number");
        assert_refusal(&err, "REQ-EO-6", "attempt number");
    }
    // An attempt number on any other edge is refused by the table's shape.
    let err = sqlx::query(
        "INSERT INTO transitions (transfer_id, from_state, to_state, attempt) \
         VALUES ($1, 'Source-Final', 'Needs-Review', 1)",
    )
    .bind(id)
    .execute(&pool)
    .await
    .expect_err("an attempt on Needs-Review");
    assert_error_sqlstate(&err, "23514", "attempt on another edge");
    // Delivering needs the attempt's signature and its signed bytes (INV-9).
    let bare = at(&pool, 2, "Source-Final").await;
    let err = sqlx::query(
        "INSERT INTO transitions (transfer_id, from_state, to_state, attempt) \
         VALUES ($1, 'Source-Final', 'Delivering', 1)",
    )
    .bind(bare)
    .execute(&pool)
    .await
    .expect_err("Delivering with nothing signed");
    assert_refusal(&err, "INV-9", "Delivering unsigned");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_eo_6_one_signature_per_reference_and_attempt(pool: PgPool) {
    let id = at(&pool, 1, "Delivering").await;
    // A second signing intent for attempt 1, over a second preparation of its order, is
    // recorded, but never signed (#24).
    let again = prepare_order(&pool, order_of(&pool, id, 1).await).await;
    let second: i64 = sqlx::query_scalar(
        "INSERT INTO intents (transfer_id, network, purpose, on_chain_reference, reference_scope, \
             attempt, payload) \
         SELECT id, 'issuing', 'mint', on_chain_reference, redeemer, 1, \
             jsonb_build_object('message', '0x01', 'amount', '1', 'prepared', $2::bigint) \
         FROM transfers WHERE id = $1 RETURNING id",
    )
    .bind(id)
    .bind(again)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_sqlstate(
        &pool,
        &format!(
            "INSERT INTO signatures (intent_id, key_id, scheme, public_key, signature) \
             VALUES ({second}, 'k', 'sr25519', '\\x01', '\\x03')"
        ),
        UNIQUE_VIOLATION,
    )
    .await;
    // Attempt 2 is signed once attempt 1 is proven, with the same reference.
    prove_not_executed(&pool, id).await;
    signed_attempt(&pool, id, 2).await;
    let lanes: (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(DISTINCT s.on_chain_reference) FROM signatures s \
         JOIN intents i ON i.id = s.intent_id WHERE i.transfer_id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(lanes, (2, 1));
    // A signature for an attempt whose predecessor is not proven is refused, even with no intent
    // of the core in its way: the store copies the attempt from the intent (INV-9).
    signing_intent(&pool, "other", "payout", "'\\x0909'", 1, "\"1\"", "01").await;
    let err = sqlx::query(
        "INSERT INTO intents (network, purpose, on_chain_reference, attempt, payload) \
         VALUES ('other', 'payout', '\\x0909', 2, '{\"message\": \"0x01\", \"amount\": \"1\"}')",
    )
    .execute(&pool)
    .await
    .expect_err("attempt 2 of an unproven lane");
    assert_refusal(&err, "REQ-EO-6", "attempt 2 of another lane");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_eo_6_refunding_needs_the_attempt_proven_at_finality(pool: PgPool) {
    // REQ-RF-1, INV-8: never on a timer, never on unknown.
    let id = at(&pool, 1, "Delivering").await;
    refund_of(&pool, id, "g-1-refund").await;
    let err = sqlx::query(
        "INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
             block_hash, block_time) \
         VALUES ($1, 'Delivering', 'Refunding', 'issuing', 10, '\\x0a', '2026-10-13T00:00:00Z')",
    )
    .bind(id)
    .execute(&pool)
    .await
    .expect_err("Refunding with no proof");
    assert_refusal(&err, "REQ-RF-1", "Refunding unproven");
    prove_not_executed(&pool, id).await;
    // Resting on another block than the proof's is refused too.
    let err = sqlx::query(
        "INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
             block_hash, block_time) \
         VALUES ($1, 'Delivering', 'Refunding', 'issuing', 11, '\\x0b', '2026-10-13T00:00:00Z')",
    )
    .bind(id)
    .execute(&pool)
    .await
    .expect_err("Refunding on another block");
    assert_refusal(&err, "REQ-RF-1", "Refunding elsewhere");
    transition(&pool, id, "Delivering", "Refunding")
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------------------------
// REQ-TR-4, NFR-3, F-001 NOTES S5: the transfer's milestones are those of the completing attempt.

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_tr_4_milestones_are_those_of_the_attempt_that_completed(pool: PgPool) {
    let id = at(&pool, 1, "Delivered").await;
    // Delivered no longer writes the transfer's own inclusion milestone; the attempt holds it.
    assert_eq!(
        text(
            &pool,
            &format!("SELECT COALESCE(destination_included_block_number::text, 'none') FROM transfers WHERE id = {id}")
        )
        .await,
        "none"
    );
    prove_not_executed(&pool, id).await;
    signed_attempt(&pool, id, 2).await;
    new_attempt(&pool, id, "Delivered", 2).await.unwrap();
    // Attempt 2 is included at block 30 and final at block 40.
    for (to, block) in [("Delivered", 30), ("Completed", 40)] {
        exec(
            &pool,
            &format!(
                "INSERT INTO transitions (transfer_id, from_state, to_state, network, \
                     block_number, block_hash, block_time) \
                 SELECT {id}, state, '{to}', 'issuing', {block}, '\\x{block:02x}', \
                     '2026-10-13T01:00:00Z' FROM transfers WHERE id = {id}"
            ),
        )
        .await;
    }
    let milestones: (i64, i64) = sqlx::query_as(
        "SELECT destination_included_block_number, destination_final_block_number FROM transfers \
         WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(milestones, (30, 40));
    let per_attempt: Vec<(i32, String, i64)> = sqlx::query_as(
        "SELECT attempt, milestone, block_number FROM attempt_milestones WHERE transfer_id = $1 \
         ORDER BY id",
    )
    .bind(id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        per_attempt,
        vec![
            (1, "included".to_owned(), 10),
            (2, "included".to_owned(), 30),
            (2, "final".to_owned(), 40)
        ]
    );
    // Only the transition writes them; they are append-only.
    assert_refused(
        &pool,
        &format!(
            "INSERT INTO attempt_milestones (transfer_id, attempt, milestone, network, block_number, \
                 block_hash, block_time, seen_at, transition_id) \
             VALUES ({id}, 3, 'included', 'issuing', 1, '\\x01', now(), now(), 999999)"
        ),
        "REQ-TR-2",
    )
    .await;
    assert_refused(
        &pool,
        "UPDATE attempt_milestones SET block_number = 1",
        "REQ-TR-2",
    )
    .await;
    assert_refused(&pool, "DELETE FROM attempt_milestones", "REQ-TR-2").await;
}

// ---------------------------------------------------------------------------------------------
// Amendment 0008, INV-2: a redemption's reference is (redeemer, reference), in its own namespace.

async fn withdrawal(
    pool: &PgPool,
    br: &str,
    block: i64,
    redeemer: &str,
    reference: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index, on_chain_reference, \
             redeemer) \
         VALUES ($1, 'req-' || $1, 'withdrawal', 'asset-a', 1, 'origin-a', '0xbeneficiary', \
             'Received', 'issuing', $2, '\\xcc', 'log', 0, decode($4, 'hex'), $3) RETURNING id",
    )
    .bind(br)
    .bind(block)
    .bind(redeemer)
    .bind(reference)
    .fetch_one(pool)
    .await
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_2_a_redemptions_reference_is_scoped_to_its_redeemer(pool: PgPool) {
    withdrawal(&pool, "w-1", 1, "0xalice", "0101")
        .await
        .unwrap();
    // Another redeemer may choose the same reference.
    withdrawal(&pool, "w-2", 2, "0xbob", "0101").await.unwrap();
    // The same redeemer may not.
    let err = withdrawal(&pool, "w-3", 3, "0xalice", "0101")
        .await
        .expect_err("the same redeemer and reference twice");
    assert_error_sqlstate(&err, UNIQUE_VIOLATION, "redeemer and reference");
    // A mint's reference is another namespace: a redemption naming a deposit's mint reference does
    // not block that mint (the attack of amendment 0008).
    let deposit = transfer(&pool, "d-1").await;
    exec(
        &pool,
        &format!("UPDATE transfers SET on_chain_reference = '\\x0101' WHERE id = {deposit}"),
    )
    .await;
    // The redeemer is part of the reference: it never changes.
    assert_refused(
        &pool,
        "UPDATE transfers SET redeemer = '0xmallory' WHERE bridge_reference = 'w-1'",
        "REQ-OT-2",
    )
    .await;
    // Only a redemption's transfers carry one.
    let err = sqlx::query("UPDATE transfers SET redeemer = '0xalice' WHERE id = $1")
        .bind(deposit)
        .execute(&pool)
        .await
        .expect_err("a deposit with a redeemer");
    assert!(err.as_database_error().is_some());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ob_2_one_final_head_per_height(pool: PgPool) {
    exec(
        &pool,
        "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
         VALUES ('origin-a', 30, '\\x30', now())",
    )
    .await;
    assert_sqlstate(
        &pool,
        "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
         VALUES ('origin-a', 30, '\\x31', now())",
        UNIQUE_VIOLATION,
    )
    .await;
    assert_refused(&pool, "UPDATE final_heads SET block_time = now()", "INV-16").await;
    assert_refused(&pool, "DELETE FROM final_heads", "INV-16").await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_cutoff_is_a_named_condition(pool: PgPool) {
    let id = at(&pool, 1, "Source-Final").await;
    exec(
        &pool,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state, condition) \
             VALUES ({id}, 'Source-Final', 'Source-Final', 'CND-CutOff')"
        ),
    )
    .await;
    assert_eq!(
        text(
            &pool,
            &format!("SELECT condition::text FROM transfers WHERE id = {id}")
        )
        .await,
        "CND-CutOff"
    );
}

// ---------------------------------------------------------------------------------------------
// REQ-RV-2, REQ-GV-5: a review ends by evidence, recorded with its actor, justification and, when
// value moves, a second approver; one ordinary edge from the state held before review.

/// The two test operators' pools, each logged in as the operator (review M3).
async fn ops(pool: &PgPool) -> [PgPool; 2] {
    [
        operator_login(pool, OPERATORS[0]).await,
        operator_login(pool, OPERATORS[1]).await,
    ]
}

/// The operator logged in on `ops` records a review resolution of transfer `id` into `to`,
/// resting on (`network`, 10, `\x0a`); returns its id. The store stamps the actor and the time.
async fn resolution(ops: &PgPool, id: i64, to: &str, network: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, \
             block_hash, block_time, justification) \
         VALUES ('review-resolution', $1, $2::transfer_state, $3, 10, '\\x0a', \
             '2026-10-13T00:00:00Z', 'the coordinate shows it') RETURNING id",
    )
    .bind(id)
    .bind(to)
    .bind(network)
    .fetch_one(ops)
    .await
    .expect("record the resolution")
}

/// The operator logged in on `ops` approves `action`.
async fn approve(ops: &PgPool, action: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO manual_approvals (action_id, justification) \
         VALUES ($1, 'checked the coordinate')",
    )
    .bind(action)
    .execute(ops)
    .await
    .map(|_| ())
}

async fn resolve(
    pool: &PgPool,
    id: i64,
    to: &str,
    network: Option<&str>,
    action: Option<i64>,
    attempt: Option<i32>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
             block_hash, block_time, manual_action_id, attempt) \
         VALUES ($1, 'Needs-Review', $2::transfer_state, $3, \
             CASE WHEN $3::text IS NULL THEN NULL ELSE 10 END, \
             CASE WHEN $3::text IS NULL THEN NULL ELSE '\\x0a'::bytea END, \
             CASE WHEN $3::text IS NULL THEN NULL ELSE '2026-10-13T00:00:00Z'::timestamptz END, \
             $4, $5)",
    )
    .bind(id)
    .bind(to)
    .bind(network)
    .bind(action)
    .bind(attempt)
    .execute(pool)
    .await
    .map(|_| ())
}

/// A transfer in review that held `before`.
async fn in_review(pool: &PgPool, n: i64, before: &str) -> i64 {
    let id = at(pool, n, before).await;
    bare_transition(pool, id, before, "Needs-Review")
        .await
        .unwrap();
    id
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rv_2_review_ends_only_by_evidence_run_as_a_recorded_action(pool: PgPool) {
    let ops = ops(&pool).await;
    let id = in_review(&pool, 1, "Delivered").await;
    // No evidence.
    let err = resolve(&pool, id, "Completed", None, None, None)
        .await
        .expect_err("a resolution with no coordinate");
    assert_refusal(&err, "REQ-RV-2", "no coordinate");
    // A coordinate, but no recorded action with its actor and justification.
    let err = resolve(&pool, id, "Completed", Some("issuing"), None, None)
        .await
        .expect_err("a resolution with no recorded action");
    assert_refusal(&err, "REQ-GV-5", "no action");
    // An action for another target, or other evidence, is not this one.
    let other = resolution(&ops[0], id, "Refunding", "issuing").await;
    let err = resolve(&pool, id, "Completed", Some("issuing"), Some(other), None)
        .await
        .expect_err("an action for another target");
    assert_refusal(&err, "REQ-RV-2", "other target");
    let elsewhere = resolution(&ops[0], id, "Completed", "origin-a").await;
    let err = resolve(
        &pool,
        id,
        "Completed",
        Some("issuing"),
        Some(elsewhere),
        None,
    )
    .await
    .expect_err("an action resting elsewhere");
    assert_refusal(&err, "REQ-RV-2", "other evidence");
    // The recorded action runs, once.
    let action = resolution(&ops[0], id, "Completed", "issuing").await;
    resolve(&pool, id, "Completed", Some("issuing"), Some(action), None)
        .await
        .unwrap();
    assert_eq!(
        text(
            &pool,
            &format!("SELECT state::text FROM transfers WHERE id = {id}")
        )
        .await,
        "Completed"
    );
    let again = in_review(&pool, 2, "Delivered").await;
    let err = resolve(
        &pool,
        again,
        "Completed",
        Some("issuing"),
        Some(action),
        None,
    )
    .await
    .expect_err("an action run twice");
    assert!(err.as_database_error().is_some());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rv_2_review_ends_one_ordinary_edge_from_the_state_held_before(pool: PgPool) {
    let ops = ops(&pool).await;
    // Before review: Accepted, Source-Final, Delivering, Delivered. Every target is tried; the
    // ordinary edges out of the state before review are the resolutions there are, never the
    // state itself (but a new attempt) and never review again.
    let cases: [(&str, &[&str]); 4] = [
        ("Accepted", &["Source-Final", "Expired", "Withdrawn"]),
        ("Source-Final", &["Delivering"]),
        ("Delivering", &["Delivered", "Refunding", "Delivering"]),
        ("Delivered", &["Completed", "Refunding", "Delivering"]),
    ];
    let mut n = 10;
    for (before, exits) in cases {
        // Refused carries a named error and is covered by the rows above.
        for to in STATES
            .into_iter()
            .filter(|s| *s != "Needs-Review" && *s != "Refused")
        {
            n += 1;
            let id = in_review(&pool, n, before).await;
            let network = if to == "Source-Final" {
                "origin-a"
            } else {
                "issuing"
            };
            // Whatever the edge needs besides the edge, as an ordinary step would have it.
            prepare(&pool, id, n, before, to).await;
            let mut attempt = None;
            if to == "Delivering" {
                if before != "Source-Final" && !proven(&pool, id).await {
                    prove_not_executed(&pool, id).await;
                }
                let next = current_attempt(&pool, id).await + 1;
                signed_attempt(&pool, id, next).await;
                attempt = Some(next);
            }
            let action = resolution(&ops[0], id, to, network).await;
            approve(&ops[1], action).await.unwrap();
            let got = resolve(&pool, id, to, Some(network), Some(action), attempt).await;
            if exits.contains(&to) {
                got.unwrap_or_else(|e| panic!("review from {before} -> {to} refused: {e}"));
            } else {
                let err = got.expect_err(&format!("review from {before} -> {to} applied"));
                assert_refusal(&err, "REQ-RV-2", &format!("review from {before} -> {to}"));
            }
        }
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_gv_5_a_resolution_that_moves_value_needs_a_second_approver(pool: PgPool) {
    let ops = ops(&pool).await;
    let id = in_review(&pool, 1, "Delivering").await;
    refund_of(&pool, id, "g-1-refund").await;
    prove_not_executed(&pool, id).await;
    let action = resolution(&ops[0], id, "Refunding", "issuing").await;
    let err = resolve(&pool, id, "Refunding", Some("issuing"), Some(action), None)
        .await
        .expect_err("value moved on one person's word");
    assert_refusal(&err, "REQ-GV-5", "no second approver");
    // The actor is not their own second approver.
    let err = approve(&ops[0], action).await.expect_err("self-approval");
    assert_refusal(&err, "REQ-GV-5", "self-approval");
    approve(&ops[1], action).await.unwrap();
    resolve(&pool, id, "Refunding", Some("issuing"), Some(action), None)
        .await
        .unwrap();
    // A resolution that moves no value needs none.
    let id = in_review(&pool, 2, "Delivered").await;
    let action = resolution(&ops[0], id, "Completed", "issuing").await;
    resolve(&pool, id, "Completed", Some("issuing"), Some(action), None)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_gv_5_manual_actions_are_append_only_and_the_operators_alone(pool: PgPool) {
    let ops = ops(&pool).await;
    let id = in_review(&pool, 1, "Delivered").await;
    let action = resolution(&ops[0], id, "Completed", "issuing").await;
    approve(&ops[1], action).await.unwrap();
    for table in ["manual_actions", "manual_approvals"] {
        assert_refused(
            &pool,
            &format!("UPDATE {table} SET justification = 'x'"),
            "REQ-GV-5",
        )
        .await;
        assert_refused(&pool, &format!("DELETE FROM {table}"), "REQ-GV-5").await;
        assert_refused(&pool, &format!("TRUNCATE {table}"), "REQ-GV-5").await;
    }
    // No service may record a manual action or approve one; the operator's role may, naming only
    // what it decides (review M3).
    let inserters: Vec<String> = sqlx::query_scalar(
        "SELECT rolname::text FROM pg_roles WHERE rolname LIKE 'bezent\\_%' \
           AND (has_any_column_privilege(oid, 'manual_actions', 'INSERT') \
             OR has_any_column_privilege(oid, 'manual_approvals', 'INSERT')) ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(inserters, vec!["bezent_operator".to_owned()]);
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql("SET LOCAL ROLE bezent_core")
        .execute(&mut *tx)
        .await
        .unwrap();
    let err = sqlx::raw_sql(&format!(
        "INSERT INTO manual_approvals (action_id, approver, justification) \
         VALUES ({action}, 'svc', 'x')"
    ))
    .execute(&mut *tx)
    .await
    .expect_err("the core's role approved a manual action");
    assert_error_sqlstate(&err, INSUFFICIENT_PRIVILEGE, "service approval");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m3_req_gv_5_the_actor_the_approver_and_their_times_are_the_stores(pool: PgPool) {
    // Review M3 of PR #32: the actor and the approver are the session's login, and the times are
    // the store's clock; the operator's role may name none of them, and one login cannot approve
    // its own action whatever name it gives.
    let ops = ops(&pool).await;
    let id = in_review(&pool, 1, "Delivered").await;
    for sql in [
        format!(
            "INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, \
                 block_hash, block_time, actor, justification) \
             VALUES ('review-resolution', {id}, 'Completed', 'issuing', 10, '\\x0a', \
                 '2026-10-13T00:00:00Z', 'alice', 'x')"
        ),
        format!(
            "INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, \
                 block_hash, block_time, recorded_at, justification) \
             VALUES ('review-resolution', {id}, 'Completed', 'issuing', 10, '\\x0a', \
                 '2026-10-13T00:00:00Z', '2000-01-01T00:00:00Z', 'x')"
        ),
    ] {
        let err = sqlx::raw_sql(&sql)
            .execute(&ops[0])
            .await
            .expect_err("the operator named who or when");
        assert_error_sqlstate(&err, INSUFFICIENT_PRIVILEGE, "named who or when");
    }
    let action = resolution(&ops[0], id, "Completed", "issuing").await;
    let err = sqlx::raw_sql(&format!(
        "INSERT INTO manual_approvals (action_id, approver, justification) \
         VALUES ({action}, 'bob', 'x')"
    ))
    .execute(&ops[0])
    .await
    .expect_err("the operator named the approver");
    assert_error_sqlstate(&err, INSUFFICIENT_PRIVILEGE, "named the approver");
    let err = approve(&ops[0], action)
        .await
        .expect_err("approved by the action's own login");
    assert_refusal(&err, "REQ-GV-5", "own login");
    approve(&ops[1], action).await.unwrap();
    let (actor, approver, fresh): (String, String, bool) = sqlx::query_as(
        "SELECT a.actor, p.approver, a.recorded_at > now() - interval '1 hour' \
         FROM manual_actions a JOIN manual_approvals p ON p.action_id = a.id WHERE a.id = $1",
    )
    .bind(action)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (actor.as_str(), approver.as_str(), fresh),
        (OPERATORS[0], OPERATORS[1], true)
    );
    // Even the owner cannot backdate a record or name another person: the store stamps both.
    let stamped: (String, bool) = sqlx::query_as(
        "INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, \
             block_hash, block_time, actor, recorded_at, justification) \
         VALUES ('review-resolution', $1, 'Completed', 'issuing', 10, '\\x0a', \
             '2026-10-13T00:00:00Z', 'alice', '2000-01-01T00:00:00Z', 'x') \
         RETURNING actor, recorded_at > now() - interval '1 hour'",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_ne!(stamped.0, "alice");
    assert!(stamped.1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_6_an_unassociated_arrival_is_released_only_with_a_second_approver(pool: PgPool) {
    let ops = ops(&pool).await;
    let refund: i64 = sqlx::query_scalar(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, token, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index) \
         VALUES ('u-1', 'req-u-1', 'refund', 'token-x', 5, 'origin-a', '0xback', 'Received', \
             'origin-a', 7, '\\xaa', 'log', 0) RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let release = |action: Option<i64>| {
        sqlx::query(
            "INSERT INTO transitions (transfer_id, from_state, to_state, manual_action_id) \
             VALUES ($1, 'Received', 'Accepted', $2)",
        )
        .bind(refund)
        .bind(action)
        .execute(&pool)
    };
    let err = release(None).await.expect_err("released with no decision");
    assert_refusal(&err, "REQ-IN-6", "no decision");
    let action: i64 = sqlx::query_scalar(
        "INSERT INTO manual_actions (kind, transfer_id, justification) \
         VALUES ('release-unassociated', $1, 'requester asked') RETURNING id",
    )
    .bind(refund)
    .fetch_one(&ops[0])
    .await
    .unwrap();
    let err = release(Some(action))
        .await
        .expect_err("released on one person's word");
    assert_refusal(&err, "REQ-IN-6", "no second approver");
    approve(&ops[1], action).await.unwrap();
    release(Some(action)).await.unwrap();
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rv_1_review_has_an_owner_and_a_one_business_day_service_level(pool: PgPool) {
    let id = in_review(&pool, 1, "Delivered").await;
    let (owner, before): (String, String) = sqlx::query_as(
        "SELECT owner, state_before_review::text FROM reviews WHERE transfer_id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (owner.as_str(), before.as_str()),
        ("operations", "Delivered")
    );
    // One business day: Tuesday -> Wednesday, Friday -> Monday, Saturday -> Monday.
    for (at, due) in [
        ("2026-10-13T10:00:00Z", "2026-10-14 10:00:00+00"),
        ("2026-10-16T10:00:00Z", "2026-10-19 10:00:00+00"),
        ("2026-10-17T10:00:00Z", "2026-10-19 10:00:00+00"),
    ] {
        let got: String = sqlx::query_scalar(&format!(
            "SELECT (one_business_day_after('{at}'::timestamptz) AT TIME ZONE 'UTC')::text || '+00'"
        ))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(got, due, "{at}");
    }
}

// ---------------------------------------------------------------------------------------------
// Verification review of PR #32 (migration 0017_lifecycle_verification).

/// Records the proof of non-execution of signed transaction `st`, as an endpoint would (F-007),
/// at the final block `block` of the issuing network, whose hash is the one byte `block`.
async fn prove_signed(pool: &PgPool, st: i64, block: i64) {
    exec(
        pool,
        &format!(
            "INSERT INTO non_execution_proofs (signed_transaction_id, network, signer, nonce, \
                 valid_from_block, valid_until_block, final_block_number, final_block_hash, \
                 nonce_at_final, kind) \
             VALUES ({st}, 'issuing', '\\x01', 0, 0, 5, {block}, \
                 set_byte('\\x00'::bytea, 0, {block}), 0, 'nonce-unconsumed')"
        ),
    )
    .await;
}

/// The unproven attempts of transfer `id`, as the store counts them.
async fn unproven(pool: &PgPool, id: i64) -> Vec<i32> {
    sqlx::query_scalar("SELECT attempts_unproven($1)")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("attempts_unproven")
}

/// Whether attempt `attempt` may be recorded or signed in transfer `id`'s mint lane.
async fn gate(pool: &PgPool, id: i64, attempt: i32) -> bool {
    sqlx::query_scalar(
        "SELECT attempt_gate_open('issuing', 'mint', redeemer, on_chain_reference, $2) \
         FROM transfers WHERE id = $1",
    )
    .bind(id)
    .bind(attempt)
    .fetch_one(pool)
    .await
    .expect("attempt_gate_open")
}

/// Appends `Delivering -> Refunding` resting on the issuing network's block `block`.
async fn refunding_at(pool: &PgPool, id: i64, block: i32) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
             block_hash, block_time) \
         VALUES ($1, 'Delivering', 'Refunding', 'issuing', $2, set_byte('\\x00'::bytea, 0, $2), \
             '2026-10-13T00:00:00Z')",
    )
    .bind(id)
    .bind(block)
    .execute(pool)
    .await
    .map(|_| ())
}

/// F2: the reviewer's case. Attempt 1's first prepared transaction expired unsigned, so the core
/// recorded a second signing intent over the re-prepared one, which was signed and is live. The
/// first is then signed late (dead, its era passed) and proven not executed. A proof of one of the
/// attempt's signed transactions says nothing of the other: neither a refund nor attempt 2 may
/// follow until every signed transaction of attempt 1 is proven (INV-8, REQ-EO-6, REQ-RF-1). The
/// store's one signature per (lane, attempt) is lifted here, as issue #24 may relax it, to show
/// that this rule does not lean on it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_8_an_attempt_re_prepared_after_expiry_is_proven_only_when_every_signed_transaction_is(
    pool: PgPool,
) {
    // The live one: attempt 1's signing intent over the re-prepared transaction, signed.
    let id = at(&pool, 1, "Delivering").await;
    let live: i64 = sqlx::query_scalar(
        "SELECT st.id FROM signed_transactions st JOIN intents i ON i.id = st.intent_id \
         WHERE i.transfer_id = $1 AND i.attempt = 1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    // The dead one: the first signing intent of attempt 1, signed late.
    exec(&pool, "DROP INDEX signatures_once_per_reference_attempt").await;
    let expired = prepare_order(&pool, order_of(&pool, id, 1).await).await;
    let late: i64 = sqlx::query_scalar(
        "INSERT INTO intents (transfer_id, network, purpose, on_chain_reference, reference_scope, \
             attempt, payload) \
         SELECT id, 'issuing', 'mint', on_chain_reference, redeemer, 1, \
             jsonb_build_object('message', '0x01', 'amount', '1', 'prepared', $2::bigint) \
         FROM transfers WHERE id = $1 RETURNING id",
    )
    .bind(id)
    .bind(expired)
    .fetch_one(&pool)
    .await
    .unwrap();
    exec(
        &pool,
        &format!(
            "INSERT INTO signatures (intent_id, key_id, scheme, public_key, signature) \
             VALUES ({late}, 'mint-key', 'sr25519', '\\x01', '\\x04')"
        ),
    )
    .await;
    let dead: i64 = sqlx::query_scalar(
        "INSERT INTO signed_transactions (intent_id, attempt, bytes) VALUES ($1, 1, '\\xdead') \
         RETURNING id",
    )
    .bind(late)
    .fetch_one(&pool)
    .await
    .unwrap();
    prove_signed(&pool, dead, 10).await;

    // Attempt 1 is not proven: the live signed transaction has no proof.
    assert!(
        !proven(&pool, id).await,
        "one proof of two proves the attempt"
    );
    assert_eq!(unproven(&pool, id).await, vec![1]);
    assert!(!gate(&pool, id, 2).await, "attempt 2's gate opened");
    let err = sqlx::query(
        "INSERT INTO intents (transfer_id, network, purpose, on_chain_reference, reference_scope, \
             attempt, payload) \
         SELECT id, 'issuing', 'mint', on_chain_reference, redeemer, 2, \
             '{\"message\": \"0x05\", \"amount\": \"1\"}' FROM transfers WHERE id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .expect_err("attempt 2 while attempt 1 has a live signed transaction");
    assert_refusal(&err, "REQ-EO-6", "attempt 2 on one proof of two");
    refund_of(&pool, id, "g-1-refund").await;
    let err = refunding_at(&pool, id, 10)
        .await
        .expect_err("Refunding on one proof of two");
    assert_refusal(&err, "INV-8", "Refunding on one proof of two");

    // Once the live one is proven too, the attempt stands proven at the later of the two final
    // blocks, and the refund rests there.
    prove_signed(&pool, live, 12).await;
    assert!(proven(&pool, id).await);
    assert!(unproven(&pool, id).await.is_empty());
    assert!(gate(&pool, id, 2).await);
    let point: (String, i64, Vec<u8>) = sqlx::query_as(
        "SELECT network, final_block_number, final_block_hash FROM attempt_proof($1, 1)",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(point, ("issuing".to_string(), 12, vec![12]));
    let err = refunding_at(&pool, id, 10)
        .await
        .expect_err("Refunding on the earlier proof's block");
    assert_refusal(&err, "INV-8", "Refunding on the earlier block");
    refunding_at(&pool, id, 12).await.unwrap();
}

/// F2, as reachable today: one signing intent signed into two transactions (its signed bytes
/// rebuilt); a proof of one leaves the attempt unproven (INV-8, REQ-EO-6).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_8_every_signed_transaction_of_one_signing_intent_must_be_proven(pool: PgPool) {
    let id = at(&pool, 2, "Delivering").await;
    let (intent, first): (i64, i64) = sqlx::query_as(
        "SELECT i.id, st.id FROM signed_transactions st JOIN intents i ON i.id = st.intent_id \
         WHERE i.transfer_id = $1 AND i.attempt = 1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let second: i64 = sqlx::query_scalar(
        "INSERT INTO signed_transactions (intent_id, attempt, bytes) VALUES ($1, 2, '\\xbeef02') \
         RETURNING id",
    )
    .bind(intent)
    .fetch_one(&pool)
    .await
    .unwrap();
    prove_signed(&pool, first, 10).await;
    assert!(
        !proven(&pool, id).await,
        "one proof of two proves the attempt"
    );
    assert_eq!(unproven(&pool, id).await, vec![1]);
    assert!(!gate(&pool, id, 2).await);
    prove_signed(&pool, second, 11).await;
    assert!(proven(&pool, id).await);
    assert!(gate(&pool, id, 2).await);
}

/// S-1 of review round 3: an attempt with two signatures, one of whose signed bytes no endpoint
/// recorded (a crash between the link's answer and the insert), is not proven by a proof of the
/// other. The store holds this by itself: the one signature per (lane, attempt) is lifted, as a lane
/// without a reference escapes it, and nothing of the core's code is in the way (INV-8, REQ-EO-6,
/// REQ-RF-1).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_8_an_attempt_is_proven_only_when_every_signature_recorded_for_it_is(pool: PgPool) {
    let id = at(&pool, 3, "Delivering").await;
    let proved: i64 = sqlx::query_scalar(
        "SELECT st.id FROM signed_transactions st JOIN intents i ON i.id = st.intent_id \
         WHERE i.transfer_id = $1 AND i.attempt = 1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    // A second signature of attempt 1, its signed bytes never recorded.
    exec(&pool, "DROP INDEX signatures_once_per_reference_attempt").await;
    let prepared = prepare_order(&pool, order_of(&pool, id, 1).await).await;
    let other: i64 = sqlx::query_scalar(
        "INSERT INTO intents (transfer_id, network, purpose, on_chain_reference, reference_scope, \
             attempt, payload) \
         SELECT id, 'issuing', 'mint', on_chain_reference, redeemer, 1, \
             jsonb_build_object('message', '0x01', 'amount', '1', 'prepared', $2::bigint) \
         FROM transfers WHERE id = $1 RETURNING id",
    )
    .bind(id)
    .bind(prepared)
    .fetch_one(&pool)
    .await
    .unwrap();
    exec(
        &pool,
        &format!(
            "INSERT INTO signatures (intent_id, key_id, scheme, public_key, signature) \
             VALUES ({other}, 'mint-key', 'sr25519', '\\x01', '\\x04')"
        ),
    )
    .await;
    let signatures: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM signatures s JOIN intents i ON i.id = s.intent_id \
         WHERE i.transfer_id = $1 AND i.attempt = 1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(signatures, 2);
    // Only the first signature's transaction is proven.
    prove_signed(&pool, proved, 10).await;

    let closed = |what: &'static str| {
        let pool = pool.clone();
        async move {
            assert!(
                !proven(&pool, id).await,
                "{what}: the attempt stands proven"
            );
            assert_eq!(unproven(&pool, id).await, vec![1], "{what}");
            assert!(!gate(&pool, id, 2).await, "{what}: attempt 2's gate opened");
            for block in [10, 11] {
                let err = refunding_at(&pool, id, block)
                    .await
                    .expect_err("Refunding while a signature of attempt 1 is unproven");
                assert_refusal(&err, "INV-8", what);
            }
        }
    };
    // Attempt 2 cannot be recorded; checked before the refund closes the attempt set.
    let err = sqlx::query(
        "INSERT INTO intents (transfer_id, network, purpose, on_chain_reference, reference_scope, \
             attempt, payload) \
         SELECT id, 'issuing', 'mint', on_chain_reference, redeemer, 2, \
             '{\"message\": \"0x05\", \"amount\": \"1\"}' FROM transfers WHERE id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .expect_err("attempt 2 while a signature of attempt 1 has no signed bytes");
    assert_refusal(&err, "REQ-EO-6", "attempt 2 on one signature of two proven");
    refund_of(&pool, id, "g-3-refund").await;
    closed("one signature of two has no signed bytes").await;

    // Its bytes recorded but not proven: still closed.
    let late: i64 = sqlx::query_scalar(
        "INSERT INTO signed_transactions (intent_id, attempt, bytes) VALUES ($1, 1, '\\xdead') \
         RETURNING id",
    )
    .bind(other)
    .fetch_one(&pool)
    .await
    .unwrap();
    closed("one signed transaction of two unproven").await;

    // Every signature's every signed transaction proven: the attempt stands proven at the later
    // final block, and the refund rests there.
    prove_signed(&pool, late, 11).await;
    assert!(proven(&pool, id).await);
    assert!(unproven(&pool, id).await.is_empty());
    assert!(gate(&pool, id, 2).await);
    refunding_at(&pool, id, 11).await.unwrap();
}

/// The functions 0014 and 0015 made, by signature.
const LIFECYCLE_FUNCTIONS: [&str; 19] = [
    "attempt_proof(bigint, integer)",
    "attempt_gate_open(text, text, text, bytea, integer)",
    "intents_guard_attempt()",
    "signatures_from_intent_attempt()",
    "transfers_redeemer_unchanged()",
    "manual_approvals_second_person()",
    "attempt_milestones_guard_insert()",
    "transfer_edge(transfer_state, transfer_state)",
    "resolution_moves_value(transfer_state)",
    "transitions_apply_lifecycle()",
    "one_business_day_after(timestamptz)",
    "transfers_destination_reference_unchanged()",
    "transfer_attempts_closed(bigint)",
    "attempts_unproven(bigint)",
    "intents_guard_attempt_set()",
    "signatures_attempt_set_open()",
    "transitions_refund_every_attempt_proven()",
    "manual_actions_by_session()",
    "manual_approvals_by_session()",
];

/// Whether `role` may execute function `f`.
async fn may_execute(pool: &PgPool, role: &str, f: &str) -> bool {
    sqlx::query_scalar("SELECT has_function_privilege($1, $2::regprocedure, 'EXECUTE')")
        .bind(role)
        .bind(f)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("{role} {f}: {e}"))
}

/// F1: only the roles that call them may run the lifecycle functions. Above all, a login of no
/// bezent role cannot run transfer_attempts_closed, which takes a share lock on a transfer row as
/// the owner and could otherwise stall every core step and every refund.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn f1_an_unrelated_login_cannot_call_the_lifecycle_functions(pool: PgPool) {
    let id = at(&pool, 3, "Delivering").await;
    const STRANGER: &str = "unrelated_login_for_tests";
    let stranger = unrelated_login(&pool, STRANGER).await;
    for sql in [
        format!("SELECT transfer_attempts_closed({id})"),
        format!("SELECT attempts_unproven({id})"),
        format!("SELECT * FROM attempt_proof({id}, 1)"),
        "SELECT attempt_gate_open('issuing', 'mint', NULL, '\\x01', 2)".to_string(),
        "SELECT transfer_edge('Received', 'Accepted')".to_string(),
        "SELECT resolution_moves_value('Delivering')".to_string(),
        "SELECT one_business_day_after(now())".to_string(),
    ] {
        let err = sqlx::raw_sql(&sql)
            .execute(&stranger)
            .await
            .expect_err(&format!("an unrelated login ran {sql}"));
        assert_error_sqlstate(&err, INSUFFICIENT_PRIVILEGE, &sql);
        let message = err.as_database_error().unwrap().message().to_string();
        assert!(
            message.contains("permission denied for function"),
            "{sql}: {message}"
        );
    }
    stranger.close().await;

    // Every function of 0014 and 0015 exists, and none is PUBLIC's.
    for f in LIFECYCLE_FUNCTIONS {
        assert!(
            !may_execute(&pool, STRANGER, f).await,
            "{f} is executable by PUBLIC"
        );
    }
    // Only the roles that call them hold them.
    for (role, f, held) in [
        ("bezent_core", "attempt_proof(bigint, integer)", true),
        ("bezent_core", "attempts_unproven(bigint)", true),
        (
            "bezent_signer",
            "attempt_gate_open(text, text, text, bytea, integer)",
            true,
        ),
        ("bezent_core", "one_business_day_after(timestamptz)", true),
        (
            "bezent_service",
            "one_business_day_after(timestamptz)",
            false,
        ),
        (
            "bezent_operator",
            "one_business_day_after(timestamptz)",
            true,
        ),
        ("bezent_signer", "attempt_proof(bigint, integer)", false),
        ("bezent_signer", "attempts_unproven(bigint)", false),
        ("bezent_endpoint", "attempt_proof(bigint, integer)", false),
        ("bezent_service", "attempts_unproven(bigint)", false),
        ("bezent_operator", "attempts_unproven(bigint)", false),
        ("bezent_service", "transfer_attempts_closed(bigint)", false),
        ("bezent_core", "transfer_attempts_closed(bigint)", false),
        ("bezent_signer", "transfer_attempts_closed(bigint)", false),
        ("bezent_endpoint", "transfer_attempts_closed(bigint)", false),
        ("bezent_operator", "transfer_attempts_closed(bigint)", false),
    ] {
        assert_eq!(may_execute(&pool, role, f).await, held, "{role} on {f}");
    }
}
