// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Store tests for T-001-04: one test per refusal, each named after the spec id it verifies, at
//! the store layer SPEC §9 names (`REQ-AR-5`). Each statement here is one the triggers or
//! constraints refuse; a positive control beside it shows the refusal is the rule, not a typo.

mod common;

use common::*;
use sqlx::PgPool;

const UNIQUE_VIOLATION: &str = "23505";
const CHECK_VIOLATION: &str = "23514";

// ---------------------------------------------------------------------------------------------
// INV-10: an observation coordinate is applied at most once.

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_10_observation_coordinate_is_recorded_once(pool: PgPool) {
    observation(&pool, 100, 3).await;
    // Same network, block number, block hash and position: refused.
    assert_sqlstate(
        &pool,
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, executed) \
         VALUES ('origin-a', 100, '\\xaa', '2026-10-13T00:00:00Z', 'log', 3, TRUE)",
        UNIQUE_VIOLATION,
    )
    .await;
    // A different position in the same block is a different coordinate.
    observation(&pool, 100, 4).await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_10_observation_update_is_refused(pool: PgPool) {
    let id = observation(&pool, 100, 3).await;
    assert_refused(
        &pool,
        &format!("UPDATE observations SET amount = 1 WHERE id = {id}"),
        "INV-10",
    )
    .await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_10_observation_delete_is_refused(pool: PgPool) {
    let id = observation(&pool, 100, 3).await;
    assert_refused(
        &pool,
        &format!("DELETE FROM observations WHERE id = {id}"),
        "INV-10",
    )
    .await;
    assert_refused(&pool, "TRUNCATE observations CASCADE", "INV-10").await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_10_observation_outcome_is_recorded_once_and_never_changed(pool: PgPool) {
    let id = observation(&pool, 100, 3).await;
    exec(
        &pool,
        &format!(
            "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
                 final_block_hash) VALUES ({id}, 'final', 120, '\\xbb')"
        ),
    )
    .await;
    assert_sqlstate(
        &pool,
        &format!(
            "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
                 final_block_hash) VALUES ({id}, 'withdrawn', 121, '\\xcc')"
        ),
        UNIQUE_VIOLATION,
    )
    .await;
    assert_refused(
        &pool,
        "UPDATE observation_outcomes SET outcome = 'withdrawn'",
        "INV-10",
    )
    .await;
    assert_refused(&pool, "DELETE FROM observation_outcomes", "INV-10").await;
}

// ---------------------------------------------------------------------------------------------
// INV-17, REQ-BK-7: the reserve book is append-only and double-entry, balanced per asset.

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_17_book_entry_update_is_refused(pool: PgPool) {
    let id = balanced_entry(&pool, "ref-1", 500).await;
    assert_refused(
        &pool,
        &format!("UPDATE book_entries SET memo = 'edited' WHERE id = {id}"),
        "INV-17",
    )
    .await;
    assert_refused(
        &pool,
        &format!("UPDATE book_postings SET amount = 1 WHERE entry_id = {id}"),
        "INV-17",
    )
    .await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_17_book_entry_delete_is_refused(pool: PgPool) {
    let id = balanced_entry(&pool, "ref-1", 500).await;
    assert_refused(
        &pool,
        &format!("DELETE FROM book_postings WHERE entry_id = {id}"),
        "INV-17",
    )
    .await;
    assert_refused(
        &pool,
        &format!("DELETE FROM book_entries WHERE id = {id}"),
        "INV-17",
    )
    .await;
    assert_refused(&pool, "TRUNCATE book_postings", "INV-17").await;
    assert_refused(&pool, "TRUNCATE book_entries CASCADE", "INV-17").await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_17_correction_is_a_new_entry(pool: PgPool) {
    let original = balanced_entry(&pool, "ref-1", 500).await;
    let mut tx = pool.begin().await.unwrap();
    let correction: i64 = sqlx::query_scalar(
        "INSERT INTO book_entries (reference, corrects) VALUES ('ref-1-correction', $1) RETURNING id",
    )
    .bind(original)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    exec_tx(
        &mut tx,
        &format!(
            "INSERT INTO book_postings (entry_id, asset, account, network, side, amount) VALUES \
               ({correction}, 'asset-a', 'owed-mint', NULL, 'debit', 500), \
               ({correction}, 'asset-a', 'reserve', 'origin-a', 'credit', 500)"
        ),
    )
    .await;
    tx.commit()
        .await
        .expect("a balanced correction is accepted");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_17_committed_entry_never_gains_a_posting(pool: PgPool) {
    // Review M1: entry 1 was committed with reserve +500 / owed-mint -500; a later transaction
    // adds a balanced pair to it. Refused: a committed entry is sealed.
    let id = balanced_entry(&pool, "ref-1", 500).await;
    let mut tx = pool.begin().await.unwrap();
    let err = sqlx::raw_sql(&format!(
        "INSERT INTO book_postings (entry_id, asset, account, network, side, amount) VALUES \
           ({id}, 'asset-a', 'reserve', 'origin-a', 'debit', 999), \
           ({id}, 'asset-a', 'surplus', NULL, 'credit', 999)"
    ))
    .execute(&mut *tx)
    .await
    .expect_err("a committed entry gained postings");
    assert_refusal(&err, "INV-17", "postings added to a committed entry");
    tx.rollback().await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM book_postings WHERE entry_id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 2);

    // The caller cannot claim the current transaction for an old entry: the store stamps it.
    let mut tx = pool.begin().await.unwrap();
    let (stamped, current): (i64, i64) = sqlx::query_as(
        "WITH e AS (INSERT INTO book_entries (reference, recorded_tx, recorded_at) \
                    VALUES ('ref-2', 1, '2000-01-01T00:00:00Z') RETURNING recorded_tx) \
         SELECT recorded_tx, txid_current() FROM e",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(stamped, current);
    tx.rollback().await.unwrap();

    // Within the entry's own transaction, postings added after a savepoint still join it.
    let mut tx = pool.begin().await.unwrap();
    let entry: i64 =
        sqlx::query_scalar("INSERT INTO book_entries (reference) VALUES ('ref-3') RETURNING id")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    exec_tx(
        &mut tx,
        &format!(
            "SAVEPOINT s; \
             INSERT INTO book_postings (entry_id, asset, account, network, side, amount) VALUES \
               ({entry}, 'asset-a', 'reserve', 'origin-a', 'debit', 7), \
               ({entry}, 'asset-a', 'owed-mint', NULL, 'credit', 7); \
             RELEASE SAVEPOINT s"
        ),
    )
    .await;
    tx.commit()
        .await
        .expect("postings in the entry's own transaction");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_17_unbalanced_entry_is_refused_at_commit(pool: PgPool) {
    let mut tx = pool.begin().await.unwrap();
    exec_tx(
        &mut tx,
        "WITH e AS (INSERT INTO book_entries (reference) VALUES ('ref-1') RETURNING id) \
         INSERT INTO book_postings (entry_id, asset, account, network, side, amount) \
         SELECT id, 'asset-a', 'reserve', 'origin-a', 'debit', 500 FROM e \
         UNION ALL SELECT id, 'asset-a', 'owed-mint', NULL, 'credit', 499 FROM e",
    )
    .await;
    let err = tx
        .commit()
        .await
        .expect_err("an unbalanced entry was committed");
    assert_refusal(&err, "INV-17", "unbalanced entry");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM book_entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_7_entry_balances_per_asset(pool: PgPool) {
    // Debits equal credits in total, but not per asset: refused.
    let mut tx = pool.begin().await.unwrap();
    exec_tx(
        &mut tx,
        "WITH e AS (INSERT INTO book_entries (reference) VALUES ('ref-1') RETURNING id) \
         INSERT INTO book_postings (entry_id, asset, account, network, side, amount) \
         SELECT id, 'asset-a', 'reserve', 'origin-a', 'debit', 500 FROM e \
         UNION ALL SELECT id, 'asset-b', 'owed-mint', NULL, 'credit', 500 FROM e",
    )
    .await;
    let err = tx
        .commit()
        .await
        .expect_err("a cross-asset entry was committed");
    assert_refusal(&err, "REQ-BK-7", "entry balanced only across assets");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_7_entry_needs_two_postings(pool: PgPool) {
    let mut tx = pool.begin().await.unwrap();
    exec_tx(
        &mut tx,
        "INSERT INTO book_entries (reference) VALUES ('ref-1')",
    )
    .await;
    let err = tx
        .commit()
        .await
        .expect_err("an entry without postings was committed");
    assert_refusal(&err, "REQ-BK-7", "entry without postings");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_7_reserve_posting_names_its_network(pool: PgPool) {
    let mut tx = pool.begin().await.unwrap();
    let err = sqlx::raw_sql(
        "WITH e AS (INSERT INTO book_entries (reference) VALUES ('ref-1') RETURNING id) \
         INSERT INTO book_postings (entry_id, asset, account, network, side, amount) \
         SELECT id, 'asset-a', 'reserve', NULL, 'debit', 500 FROM e",
    )
    .execute(&mut *tx)
    .await
    .expect_err("a reserve posting without a network was accepted");
    assert_eq!(
        err.as_database_error().unwrap().code().as_deref(),
        Some(CHECK_VIOLATION)
    );
}

// ---------------------------------------------------------------------------------------------
// INV-5: a terminal state never changes.

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_5_terminal_transfer_never_changes(pool: PgPool) {
    let id = completed(&pool, "br-1", 100).await;

    // No transition out of a terminal state.
    let err = transition(&pool, id, "Completed", "Needs-Review")
        .await
        .expect_err("a terminal transfer moved");
    assert_refusal(&err, "INV-5", "transition out of Completed");
    // No field of a terminal transfer changes, not even one that was empty.
    for set in [
        "state = 'Needs-Review'",
        "condition = 'CND-Paused'",
        "expires_at = now()",
        "refund_destination_network = 'origin-a', refund_destination = 'r'",
        "on_chain_reference = '\\x01'",
    ] {
        assert_refused(
            &pool,
            &format!("UPDATE transfers SET {set} WHERE id = {id}"),
            "INV-5",
        )
        .await;
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_5_every_terminal_state_is_frozen(pool: PgPool) {
    for (n, state) in ["Refused", "Completed", "Refunded", "Expired", "Withdrawn"]
        .into_iter()
        .enumerate()
    {
        let id = terminal(&pool, &format!("br-{n}"), state, 100 + n as i64).await;
        assert_eq!(
            text(
                &pool,
                &format!("SELECT state FROM transfers WHERE id = {id}")
            )
            .await,
            state
        );
        assert_refused(
            &pool,
            &format!("UPDATE transfers SET expires_at = now() WHERE id = {id}"),
            "INV-5",
        )
        .await;
        // A self-transition (a refusal names its error, or the CHECK refuses it first).
        let err = sqlx::query(
            "INSERT INTO transitions (transfer_id, from_state, to_state, error) \
             VALUES ($1, $2, $2, CASE WHEN $2::text = 'Refused' THEN 'ERR-BelowMinimum' END)",
        )
        .bind(id)
        .bind(state)
        .execute(&pool)
        .await
        .expect_err("a terminal transfer took a condition");
        assert_refusal(&err, "INV-5", "self-transition of a terminal transfer");
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_5_terminal_state_carries_no_condition(pool: PgPool) {
    // Review N3: a condition is held in a non-terminal state only (§5.4).
    let id = transfer(&pool, "br-1").await;
    to_source_final(&pool, id, 100).await;
    walk(&pool, id, &["Source-Final", "Delivering", "Delivered"]).await;
    assert_sqlstate(
        &pool,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state, condition, network, \
                 block_number, block_hash, block_time) \
             VALUES ({id}, 'Delivered', 'Completed', 'CND-AmountMismatch', 'issuing', 12, \
                 '\\x0c', '{BLOCK_TIME}')"
        ),
        CHECK_VIOLATION,
    )
    .await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_5_transfer_delete_is_refused(pool: PgPool) {
    let id = completed(&pool, "br-1", 100).await;
    assert_refused(
        &pool,
        &format!("DELETE FROM transfers WHERE id = {id}"),
        "INV-5",
    )
    .await;
    let id = transfer(&pool, "br-2").await;
    assert_refused(
        &pool,
        &format!("DELETE FROM transfers WHERE id = {id}"),
        "INV-5",
    )
    .await;
    // TRUNCATE ... CASCADE reaches every table that references transfers, each refusing with
    // its own spec id; whichever fires first, it is refused.
    assert_refused(&pool, "TRUNCATE transfers CASCADE", "").await;
}

// ---------------------------------------------------------------------------------------------
// REQ-TR-2: a transfer begins in an initial state and reaches every other one by a transition
// along §5.4, which records its coordinate and writes the milestone it requires.

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_tr_2_transfer_is_inserted_only_in_an_initial_state(pool: PgPool) {
    let insert = |n: usize, state: &str, condition: &str| {
        format!(
            "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                 destination_network, destination, state, condition) \
             VALUES ('br-{n}', 'req-{n}', 'deposit', 'asset-a', 100, 'issuing', 'd-{n}', \
                 '{state}', {condition})"
        )
    };
    // Review M3 (a): a transfer created already terminal, with a condition and no transition.
    assert_refused(
        &pool,
        &insert(0, "Completed", "'CND-AmountMismatch'"),
        "REQ-TR-2",
    )
    .await;
    for (n, state) in [
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
    ]
    .into_iter()
    .enumerate()
    {
        assert_refused(&pool, &insert(n + 1, state, "NULL"), "REQ-TR-2").await;
    }
    // Not even Received with a condition.
    assert_refused(&pool, &insert(20, "Received", "'CND-Paused'"), "REQ-TR-2").await;
    // Source-Final only for a deposit with its binding and its source-final milestone.
    assert_refused(
        &pool,
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index) \
         VALUES ('br-21', 'req-21', 'deposit', 'asset-a', 100, 'issuing', 'd', 'Source-Final', \
             'origin-a', 1, '\\xaa', 'log', 0)",
        "REQ-TR-2",
    )
    .await;
    // The two initial states are accepted.
    exec(&pool, &insert(30, "Received", "NULL")).await;
    bound_deposit(&pool, "br-31", 100).await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_tr_2_transition_off_the_state_graph_is_refused(pool: PgPool) {
    let id = transfer(&pool, "br-1").await;
    // Review M3 (b): a transition that skips states and carries no coordinate.
    let err = bare_transition(&pool, id, "Received", "Completed")
        .await
        .expect_err("Received -> Completed was applied");
    assert_refusal(&err, "REQ-TR-2", "Received -> Completed");
    // Not with a coordinate either.
    let err = transition(&pool, id, "Received", "Completed")
        .await
        .expect_err("Received -> Completed was applied");
    assert_refusal(&err, "REQ-TR-2", "Received -> Completed");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_tr_6_state_graph_is_the_transition_table_of_section_5_4(pool: PgPool) {
    // Review S2, then amendment 0009 (F-003, migration 0014): the ordinary edges of the §5.4 table,
    // and only those; a non-terminal state may also take or clear a condition in place. `true`
    // marks an edge that rests on chain data. Needs-Review's exits are not ordinary edges
    // (lifecycle_graph.rs).
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
    let edges: &[(&str, &str, bool)] = &[
        ("Received", "Refused", false),
        ("Received", "Accepted", false),
        ("Accepted", "Source-Final", true),
        ("Accepted", "Expired", true),
        ("Accepted", "Withdrawn", true),
        ("Accepted", "Needs-Review", false),
        ("Source-Final", "Delivering", false),
        ("Source-Final", "Needs-Review", false),
        ("Delivering", "Delivered", true),
        ("Delivered", "Delivering", true),
        ("Delivered", "Completed", true),
        ("Delivering", "Refunding", true),
        ("Delivered", "Refunding", true),
        ("Refunding", "Refunded", true),
        ("Delivering", "Needs-Review", false),
        ("Delivered", "Needs-Review", false),
    ];
    let terminal = ["Refused", "Completed", "Refunded", "Expired", "Withdrawn"];
    for from in STATES {
        for to in STATES {
            let got: Option<bool> = sqlx::query_scalar("SELECT transfer_edge($1, $2)")
                .bind(from)
                .bind(to)
                .fetch_one(&pool)
                .await
                .unwrap();
            let want = edges
                .iter()
                .find(|(f, t, _)| *f == from && *t == to)
                .map(|(_, _, c)| *c)
                .or((from == to && !terminal.contains(&from)).then_some(false));
            assert_eq!(got, want, "{from} -> {to}");
        }
    }
    // The review's examples: Completed only from Delivered, Refunded only from Refunding,
    // Refused only from Received, Expired only from Accepted.
    let id = transfer(&pool, "br-1").await;
    to_source_final(&pool, id, 100).await;
    let err = transition(&pool, id, "Source-Final", "Completed")
        .await
        .expect_err("Source-Final -> Completed was applied");
    assert_refusal(&err, "REQ-TR-2", "Source-Final -> Completed");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_tr_2_edge_resting_on_chain_data_records_its_coordinate(pool: PgPool) {
    let id = transfer(&pool, "br-1").await;
    to_source_final(&pool, id, 100).await;
    walk(&pool, id, &["Source-Final", "Delivering"]).await;
    let err = bare_transition(&pool, id, "Delivering", "Delivered")
        .await
        .expect_err("Delivered without a coordinate");
    assert_refusal(
        &err,
        "REQ-TR-2",
        "Delivering -> Delivered without a coordinate",
    );
    // A coordinate on a network other than the destination is refused too.
    let err = sqlx::raw_sql(&format!(
        "INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
             block_hash, block_time) \
         VALUES ({id}, 'Delivering', 'Delivered', 'origin-a', 7, '\\x07', '{BLOCK_TIME}')"
    ))
    .execute(&pool)
    .await
    .expect_err("Delivered on the source network");
    assert_refusal(&err, "REQ-TR-2", "Delivered on the wrong network");
    // Source-Final needs the source binding.
    let id = transfer(&pool, "br-2").await;
    bare_transition(&pool, id, "Received", "Accepted")
        .await
        .unwrap();
    let err = transition(&pool, id, "Accepted", "Source-Final")
        .await
        .expect_err("Source-Final without a source binding");
    assert_refusal(&err, "INV-2", "Source-Final without a source binding");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_tr_2_terminal_transition_writes_the_destination_final_milestone(pool: PgPool) {
    // Review M3 (c): the milestone was written by a separate UPDATE after the terminal transition,
    // and was then locked out. Now the transition into each state writes its own milestone.
    let id = transfer(&pool, "br-1").await;
    to_source_final(&pool, id, 100).await;
    walk(&pool, id, &["Source-Final", "Delivering", "Delivered"]).await;
    exec(
        &pool,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
                 block_hash, block_time, seen_at) \
             VALUES ({id}, 'Delivered', 'Completed', 'issuing', 12, '\\x0c', \
                 '2026-10-13T00:02:00Z', '2026-10-13T00:02:01Z')"
        ),
    )
    .await;
    let row: (String, i64, Vec<u8>, String, String) = sqlx::query_as(
        "SELECT destination_final_network, destination_final_block_number, \
             destination_final_block_hash, destination_final_block_time::text, \
             destination_final_seen_at::text FROM transfers WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.0, "issuing");
    assert_eq!(row.1, 12);
    assert_eq!(row.2, vec![0x0c]);
    let block_time = text(&pool, "SELECT '2026-10-13T00:02:00Z'::timestamptz::text").await;
    let seen_at = text(&pool, "SELECT '2026-10-13T00:02:01Z'::timestamptz::text").await;
    assert_eq!(row.3, block_time);
    assert_eq!(row.4, seen_at);
    // Every milestone of the path is there.
    let missing: i64 = sqlx::query_scalar(
        "SELECT num_nulls(source_final_seen_at, destination_included_seen_at, \
             destination_final_seen_at)::bigint FROM transfers WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(missing, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_tr_2_milestone_is_written_only_by_its_transition(pool: PgPool) {
    let id = transfer(&pool, "br-1").await;
    to_source_final(&pool, id, 100).await;
    walk(&pool, id, &["Source-Final", "Delivering", "Delivered"]).await;
    // Rewriting the same values is no change.
    exec(
        &pool,
        &format!(
            "UPDATE transfers SET source_final_block_time = source_final_block_time WHERE id = {id}"
        ),
    )
    .await;
    // A written milestone never changes; one not yet due is not written by hand.
    for set in [
        "source_final_block_time = '2026-10-13T00:00:01Z'",
        "source_final_seen_at = now()",
        "destination_included_block_hash = '\\x08'",
        "destination_final_network = 'issuing', destination_final_block_number = 10, \
         destination_final_block_hash = '\\x0a', destination_final_block_time = now(), \
         destination_final_seen_at = now()",
    ] {
        assert_refused(
            &pool,
            &format!("UPDATE transfers SET {set} WHERE id = {id}"),
            "REQ-TR-2",
        )
        .await;
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_tr_2_milestone_is_recorded_whole(pool: PgPool) {
    // A deposit that begins at Source-Final with a milestone missing its block time, bound to a
    // final arrival (review M1 of PR #56).
    finalise_coordinate(&pool, "origin-a", 1, "aa", "log", 0, "asset-a", "1000").await;
    assert_sqlstate(
        &pool,
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, \
             source_network, source_block_number, source_block_hash, source_position_kind, \
             source_position_index, \
             source_final_network, source_final_block_number, source_final_block_hash, \
             source_final_seen_at) \
         VALUES ('br-1', 'req-1', 'deposit', 'asset-a', 1000, 'issuing', 'dest-1', 'Source-Final', \
             'origin-a', 1, '\\xaa', 'log', 0, 'origin-a', 21, '\\xbb', now())",
        CHECK_VIOLATION,
    )
    .await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_tr_2_state_changes_only_by_transition(pool: PgPool) {
    let id = transfer(&pool, "br-1").await;
    bare_transition(&pool, id, "Received", "Accepted")
        .await
        .unwrap();
    bind_source(&pool, id, 100).await;
    assert_refused(
        &pool,
        &format!("UPDATE transfers SET state = 'Source-Final' WHERE id = {id}"),
        "REQ-TR-2",
    )
    .await;
    assert_refused(
        &pool,
        &format!("UPDATE transfers SET condition = 'CND-Paused' WHERE id = {id}"),
        "REQ-TR-2",
    )
    .await;
    // Through a transition, the same change is made.
    transition(&pool, id, "Accepted", "Source-Final")
        .await
        .unwrap();
    assert_eq!(
        text(
            &pool,
            &format!("SELECT state FROM transfers WHERE id = {id}")
        )
        .await,
        "Source-Final"
    );
    // A condition is taken in place by a transition too.
    exec(
        &pool,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state, condition) \
             VALUES ({id}, 'Source-Final', 'Source-Final', 'CND-MintWindow')"
        ),
    )
    .await;
    assert_eq!(
        text(
            &pool,
            &format!("SELECT condition FROM transfers WHERE id = {id}")
        )
        .await,
        "CND-MintWindow"
    );
    // After it, a direct change back is still refused.
    assert_refused(
        &pool,
        &format!("UPDATE transfers SET state = 'Accepted', condition = NULL WHERE id = {id}"),
        "REQ-TR-2",
    )
    .await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_tr_2_transition_starts_at_current_state(pool: PgPool) {
    let id = transfer(&pool, "br-1").await;
    let err = transition(&pool, id, "Delivering", "Delivered")
        .await
        .expect_err("a transition from a state the transfer is not in was applied");
    assert_refusal(&err, "REQ-TR-2", "transition from the wrong state");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_tr_2_transition_records_its_coordinate_whole(pool: PgPool) {
    let id = transfer(&pool, "br-1").await;
    // A coordinate without its block time is refused.
    assert_sqlstate(
        &pool,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
                 block_hash) VALUES ({id}, 'Received', 'Accepted', 'origin-a', 1, '\\x01')"
        ),
        CHECK_VIOLATION,
    )
    .await;
    // A refusal names its error, and only a refusal does.
    assert_sqlstate(
        &pool,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state) \
             VALUES ({id}, 'Received', 'Refused')"
        ),
        CHECK_VIOLATION,
    )
    .await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_tr_2_expired_rests_on_a_block_past_the_expiry(pool: PgPool) {
    let id = transfer(&pool, "br-1").await;
    bare_transition(&pool, id, "Received", "Accepted")
        .await
        .unwrap();
    // No expiry: nothing to expire on.
    let err = transition(&pool, id, "Accepted", "Expired")
        .await
        .expect_err("Expired without an expiry");
    assert_refusal(&err, "REQ-TR-2", "Expired without an expiry");
    // A block before the expiry is refused; one at or past it is accepted.
    exec(
        &pool,
        &format!("UPDATE transfers SET expires_at = '2026-10-14T00:00:00Z' WHERE id = {id}"),
    )
    .await;
    let err = transition(&pool, id, "Accepted", "Expired")
        .await
        .expect_err("Expired before the expiry");
    assert_refusal(&err, "REQ-TR-2", "Expired before the expiry");
    exec(
        &pool,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
                 block_hash, block_time) \
             VALUES ({id}, 'Accepted', 'Expired', 'origin-a', 50, '\\x32', '2026-10-14T00:00:00Z')"
        ),
    )
    .await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_tr_2_refund_states_rest_on_the_refund(pool: PgPool) {
    let id = transfer(&pool, "br-1").await;
    to_source_final(&pool, id, 100).await;
    walk(&pool, id, &["Source-Final", "Delivering"]).await;
    // Refunding needs a refund transfer (§5.4, REQ-RF-3).
    let err = transition(&pool, id, "Delivering", "Refunding")
        .await
        .expect_err("Refunding without a refund");
    assert_refusal(&err, "REQ-RF-3", "Refunding without a refund");
    let refund = refund_of(&pool, id, "br-1-refund").await;
    walk(&pool, id, &["Delivering", "Refunding"]).await;
    // Refunded needs the refund Completed.
    let err = transition(&pool, id, "Refunding", "Refunded")
        .await
        .expect_err("Refunded before the refund completed");
    assert_refusal(&err, "REQ-TR-2", "Refunded before the refund completed");
    to_source_final(&pool, refund, 100).await;
    walk(
        &pool,
        refund,
        &["Source-Final", "Delivering", "Delivered", "Completed"],
    )
    .await;
    // And rests on the refund's final coordinate, not another one.
    let err = sqlx::raw_sql(&format!(
        "INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
             block_hash, block_time) \
         VALUES ({id}, 'Refunding', 'Refunded', 'issuing', 11, '\\x0b', '{BLOCK_TIME}')"
    ))
    .execute(&pool)
    .await
    .expect_err("Refunded on another coordinate");
    assert_refusal(&err, "REQ-TR-2", "Refunded on another coordinate");
    walk(&pool, id, &["Refunding", "Refunded"]).await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_tr_2_transitions_are_append_only(pool: PgPool) {
    let id = transfer(&pool, "br-1").await;
    bare_transition(&pool, id, "Received", "Accepted")
        .await
        .unwrap();
    assert_refused(&pool, "UPDATE transitions SET seen_at = now()", "REQ-TR-2").await;
    assert_refused(&pool, "DELETE FROM transitions", "REQ-TR-2").await;
    assert_refused(&pool, "TRUNCATE transitions", "REQ-TR-2").await;
}

// ---------------------------------------------------------------------------------------------
// INV-2, INV-10: a source coordinate and an on-chain reference fund at most one transfer.

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_2_source_coordinate_funds_one_transfer(pool: PgPool) {
    // Review M2: two deposits bound to one source coordinate.
    bound_deposit(&pool, "b1", 10).await;
    let err = sqlx::query(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, \
             source_network, source_block_number, source_block_hash, source_position_kind, \
             source_position_index, \
             source_final_network, source_final_block_number, source_final_block_hash, \
             source_final_block_time, source_final_seen_at) \
         VALUES ('b2', 'r2', 'deposit', 'asset-a', 1000, 'issuing', 'd2', 'Source-Final', \
             'origin-a', 10, '\\xaa', 'log', 0, \
             'origin-a', 30, '\\xbb', '2026-10-13T00:00:00Z', '2026-10-13T00:00:05Z')",
    )
    .execute(&pool)
    .await
    .expect_err("one coordinate funded two transfers");
    assert_error_sqlstate(&err, UNIQUE_VIOLATION, "second binding at insert");
    // Nor by binding it later.
    let other = transfer(&pool, "b3").await;
    let err = sqlx::raw_sql(&format!(
        "UPDATE transfers SET source_network = 'origin-a', source_block_number = 10, \
           source_block_hash = '\\xaa', source_position_kind = 'log', \
           source_position_index = 0 WHERE id = {other}"
    ))
    .execute(&pool)
    .await
    .expect_err("one coordinate funded two transfers");
    assert_error_sqlstate(&err, UNIQUE_VIOLATION, "second binding by update");
    // A refund (REQ-RF-3) points at the coordinate of the transfer it refunds.
    let original = completed(&pool, "b4", 40).await;
    let refund = refund_of(&pool, original, "b4-refund").await;
    bind_source(&pool, refund, 40).await;
    // A transfer that ended with nothing moved (§5.4: Withdrawn) consumed nothing: the
    // coordinate it was bound to stays free for the transfer that consumes it.
    let withdrawn = transfer(&pool, "b5").await;
    bare_transition(&pool, withdrawn, "Received", "Accepted")
        .await
        .unwrap();
    bind_source(&pool, withdrawn, 50).await;
    walk(&pool, withdrawn, &["Accepted", "Withdrawn"]).await;
    bound_deposit(&pool, "b6", 50).await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_2_on_chain_reference_is_consumed_once(pool: PgPool) {
    let a = transfer(&pool, "b1").await;
    let b = transfer(&pool, "b2").await;
    exec(
        &pool,
        &format!("UPDATE transfers SET on_chain_reference = '\\x0101' WHERE id = {a}"),
    )
    .await;
    assert_sqlstate(
        &pool,
        &format!("UPDATE transfers SET on_chain_reference = '\\x0101' WHERE id = {b}"),
        UNIQUE_VIOLATION,
    )
    .await;
    // Write-once.
    exec(
        &pool,
        &format!("UPDATE transfers SET on_chain_reference = '\\x0101' WHERE id = {a}"),
    )
    .await;
    assert_refused(
        &pool,
        &format!("UPDATE transfers SET on_chain_reference = '\\x0202' WHERE id = {a}"),
        "INV-2",
    )
    .await;
}

// ---------------------------------------------------------------------------------------------
// REQ-ST-4: every transition and every new transfer appends its event in its own transaction.

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_4_every_transition_appends_its_event(pool: PgPool) {
    // Review S3: a transition committed with no event row.
    let id = completed(&pool, "br-1", 100).await;
    let transitions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM transitions WHERE transfer_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let events: Vec<String> = sqlx::query_scalar(
        "SELECT kind || ':' || COALESCE(payload->>'to_state', payload->>'state') \
         FROM event_log WHERE transfer_id = $1 ORDER BY seq",
    )
    .bind(id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(transitions, 5);
    assert_eq!(
        events,
        [
            "transfer:Received",
            "transition:Accepted",
            "transition:Source-Final",
            "transition:Delivering",
            "transition:Delivered",
            "transition:Completed",
        ]
    );
    // A rolled-back transition leaves neither its row nor its event.
    let open = transfer(&pool, "br-2").await;
    let head = text(&pool, "SELECT seq::text FROM event_log_head").await;
    let mut tx = pool.begin().await.unwrap();
    exec_tx(
        &mut tx,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state) \
             VALUES ({open}, 'Received', 'Accepted')"
        ),
    )
    .await;
    tx.rollback().await.unwrap();
    assert_eq!(
        text(&pool, "SELECT seq::text FROM event_log_head").await,
        head
    );
}

// ---------------------------------------------------------------------------------------------
// REQ-ST-4: the sequenced log is gapless and never rewritten.

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_4_event_log_is_gapless(pool: PgPool) {
    exec(
        &pool,
        "INSERT INTO event_log (kind, payload) VALUES ('test', '{}')",
    )
    .await;
    // A position that skips ahead, or repeats, is refused.
    assert_refused(
        &pool,
        "INSERT INTO event_log (seq, kind, payload) VALUES (3, 'test', '{}')",
        "REQ-ST-4",
    )
    .await;
    assert_refused(
        &pool,
        "INSERT INTO event_log (seq, kind, payload) VALUES (1, 'test', '{}')",
        "REQ-ST-4",
    )
    .await;
    // The exact next position is accepted.
    exec(
        &pool,
        "INSERT INTO event_log (seq, kind, payload) VALUES (2, 'test', '{}')",
    )
    .await;
    // A rolled-back append leaves no gap.
    let mut tx = pool.begin().await.unwrap();
    exec_tx(
        &mut tx,
        "INSERT INTO event_log (kind, payload) VALUES ('test', '{}')",
    )
    .await;
    tx.rollback().await.unwrap();
    exec(
        &pool,
        "INSERT INTO event_log (kind, payload) VALUES ('test', '{}')",
    )
    .await;

    let seqs: Vec<i64> = sqlx::query_scalar("SELECT seq FROM event_log ORDER BY seq")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(seqs, vec![1, 2, 3]);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_4_concurrent_appends_stay_gapless(pool: PgPool) {
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let pool = pool.clone();
        tasks.push(tokio::spawn(async move {
            for _ in 0..10 {
                sqlx::raw_sql("INSERT INTO event_log (kind, payload) VALUES ('test', '{}')")
                    .execute(&pool)
                    .await
                    .expect("append");
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let seqs: Vec<i64> = sqlx::query_scalar("SELECT seq FROM event_log ORDER BY seq")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(seqs, (1..=80).collect::<Vec<i64>>());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_4_event_log_is_append_only(pool: PgPool) {
    exec(
        &pool,
        "INSERT INTO event_log (kind, payload) VALUES ('test', '{}')",
    )
    .await;
    assert_refused(
        &pool,
        "UPDATE event_log SET payload = '{\"x\":1}'",
        "REQ-ST-4",
    )
    .await;
    assert_refused(&pool, "DELETE FROM event_log", "REQ-ST-4").await;
    assert_refused(&pool, "TRUNCATE event_log", "REQ-ST-4").await;
    // The head cannot be moved by hand, in either direction, nor replaced.
    assert_refused(&pool, "UPDATE event_log_head SET seq = seq + 1", "REQ-ST-4").await;
    assert_refused(&pool, "UPDATE event_log_head SET seq = 0", "REQ-ST-4").await;
    assert_refused(&pool, "DELETE FROM event_log_head", "REQ-ST-4").await;
    assert_refused(&pool, "TRUNCATE event_log_head", "REQ-ST-4").await;
}

// ---------------------------------------------------------------------------------------------
// INV-11: every amount is an exact integer count of minor units.

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_11_store_refuses_inexact_amounts(pool: PgPool) {
    for bad in [
        "1.5",
        "2.0",
        "-1",
        "'NaN'::numeric",
        "340282366920938463463374607431768211456",
        "1.5::float8",
    ] {
        assert_sqlstate(
            &pool,
            &format!(
                "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                     destination_network, destination, state) \
                 VALUES ('br-x', 'req-x', 'deposit', 'asset-a', {bad}, 'issuing', 'dest-1', 'Received')"
            ),
            CHECK_VIOLATION,
        )
        .await;
    }
    // u128::MAX is exact and accepted, and reads back unchanged.
    exec(
        &pool,
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state) \
         VALUES ('br-max', 'req-max', 'deposit', 'asset-a', \
             340282366920938463463374607431768211455, 'issuing', 'dest-1', 'Received')",
    )
    .await;
    let text: String =
        sqlx::query_scalar("SELECT amount::text FROM transfers WHERE bridge_reference = 'br-max'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(text.parse::<u128>(), Ok(u128::MAX));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_11_no_amount_column_is_floating_point(pool: PgPool) {
    let floats: Vec<String> = sqlx::query_scalar(
        "SELECT table_name || '.' || column_name FROM information_schema.columns \
         WHERE table_schema = 'public' AND data_type IN ('real', 'double precision')",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(floats.is_empty(), "floating-point columns: {floats:?}");
    let not_minor_units: Vec<String> = sqlx::query_scalar(
        "SELECT c.table_name || '.' || c.column_name FROM information_schema.columns c \
         JOIN information_schema.tables t USING (table_schema, table_name) \
         WHERE c.table_schema = 'public' AND t.table_type = 'BASE TABLE' \
           AND c.column_name = 'amount' AND c.domain_name IS DISTINCT FROM 'minor_units'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        not_minor_units.is_empty(),
        "amount columns outside the domain: {not_minor_units:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// REQ-ID-3: a bridge reference is distinct from the request reference.

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_id_3_bridge_reference_is_not_the_request_reference(pool: PgPool) {
    assert_sqlstate(
        &pool,
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state) \
         VALUES ('same', 'same', 'deposit', 'asset-a', 1, 'issuing', 'dest-1', 'Received')",
        CHECK_VIOLATION,
    )
    .await;
}

// ---------------------------------------------------------------------------------------------
// INV-2: the source binding is write-once. REQ-ST-7: what a transfer was asked never changes.

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_2_source_binding_is_write_once(pool: PgPool) {
    let id = transfer(&pool, "br-1").await;
    let bind = |n: i64| {
        format!(
            "UPDATE transfers SET source_network = 'origin-a', source_block_number = {n}, \
               source_block_hash = '\\xaa', source_position_kind = 'log', \
               source_position_index = 3 WHERE id = {id}"
        )
    };
    exec(&pool, &bind(100)).await;
    exec(&pool, &bind(100)).await;
    assert_refused(&pool, &bind(101), "INV-2").await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_7_what_a_transfer_was_asked_never_changes(pool: PgPool) {
    let id = transfer(&pool, "br-1").await;
    for set in [
        "amount = 999",
        "destination = 'dest-2'",
        "asset = 'asset-b'",
        "request_reference = 'req-2'",
        "bridge_reference = 'br-2'",
        "created_at = now() - interval '1 day'",
    ] {
        assert_refused(
            &pool,
            &format!("UPDATE transfers SET {set} WHERE id = {id}"),
            "REQ-ST-7",
        )
        .await;
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_2_refund_destination_is_write_once(pool: PgPool) {
    let id = transfer(&pool, "br-1").await;
    exec(
        &pool,
        &format!(
            "UPDATE transfers SET refund_destination_network = 'origin-a', \
               refund_destination = 'r-1' WHERE id = {id}"
        ),
    )
    .await;
    assert_refused(
        &pool,
        &format!("UPDATE transfers SET refund_destination = 'r-2' WHERE id = {id}"),
        "REQ-RF-2",
    )
    .await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_7_intake_is_append_only(pool: PgPool) {
    exec(
        &pool,
        "INSERT INTO intake (request_reference, requester, kind, body, body_digest) \
         VALUES ('req-1', 'requester-1', 'deposit', '{}', '\\x01')",
    )
    .await;
    // A repeat of the reference is never a second row (REQ-ID-1).
    assert_sqlstate(
        &pool,
        "INSERT INTO intake (request_reference, requester, kind, body, body_digest) \
         VALUES ('req-1', 'requester-1', 'deposit', '{\"x\":1}', '\\x02')",
        UNIQUE_VIOLATION,
    )
    .await;
    assert_refused(&pool, "UPDATE intake SET body = '{\"x\":1}'", "REQ-ST-7").await;
    assert_refused(&pool, "DELETE FROM intake", "REQ-ST-7").await;
    // CASCADE: since 0010 other tables reference intake, so a plain TRUNCATE is refused by the
    // foreign key before any trigger; with CASCADE the append-only triggers refuse it. The
    // message names the table, so it is intake's own trigger that refused, not intake_answers'.
    assert_refused(
        &pool,
        "TRUNCATE intake CASCADE",
        "TRUNCATE on intake is refused",
    )
    .await;
}

// ---------------------------------------------------------------------------------------------
// INV-9, REQ-EO-1: intents and the exact signed bytes are recorded once and never rewritten.

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_9_intents_and_signed_bytes_are_append_only(pool: PgPool) {
    exec(
        &pool,
        "INSERT INTO intents (network, purpose, payload) VALUES ('issuing', 'mint', '{}'); \
         INSERT INTO signed_transactions (intent_id, attempt, bytes) \
         SELECT id, 1, '\\xdeadbeef' FROM intents",
    )
    .await;
    assert_refused(&pool, "UPDATE intents SET payload = '{\"x\":1}'", "INV-9").await;
    assert_refused(&pool, "DELETE FROM intents", "INV-9").await;
    assert_refused(
        &pool,
        "UPDATE signed_transactions SET bytes = '\\x00'",
        "INV-9",
    )
    .await;
    assert_refused(&pool, "DELETE FROM signed_transactions", "INV-9").await;
    assert_refused(&pool, "TRUNCATE signed_transactions", "INV-9").await;
    // The same attempt is never recorded twice.
    assert_sqlstate(
        &pool,
        "INSERT INTO signed_transactions (intent_id, attempt, bytes) \
         SELECT id, 1, '\\x00' FROM intents",
        UNIQUE_VIOLATION,
    )
    .await;
}

// ---------------------------------------------------------------------------------------------
// REQ-ID-1, PLAN DEC-15: a request reference is scoped to its requester.

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_id_1_request_reference_is_scoped_per_requester(pool: PgPool) {
    let insert = |requester: &str| {
        format!(
            "INSERT INTO intake (request_reference, requester, kind, body, body_digest) \
             VALUES ('req-1', '{requester}', 'deposit', '{{}}', '\\x01')"
        )
    };
    exec(&pool, &insert("requester-a")).await;
    // Another requester's reference never collides with, or reveals, this one's (REQ-ID-4).
    exec(&pool, &insert("requester-b")).await;
    // The same requester repeating it is never a second row.
    assert_sqlstate(&pool, &insert("requester-a"), UNIQUE_VIOLATION).await;
}

// ---------------------------------------------------------------------------------------------
// REQ-OB-2: an outcome is decided at a final point at or past the observed block.

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ob_2_outcome_is_decided_at_or_past_the_observed_block(pool: PgPool) {
    // Review S4: a final point (40) below the observation's own block (50).
    let id = observation(&pool, 50, 0).await;
    assert_refused(
        &pool,
        &format!(
            "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
                 final_block_hash) VALUES ({id}, 'final', 40, '\\xbb')"
        ),
        "REQ-OB-2",
    )
    .await;
    exec(
        &pool,
        &format!(
            "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
                 final_block_hash) VALUES ({id}, 'final', 50, '\\xbb')"
        ),
    )
    .await;
}

// ---------------------------------------------------------------------------------------------
// REQ-AR-5 (SPEC §9, store layer): the roles the services use cannot switch the rules off. Each
// check runs under `SET LOCAL ROLE` inside its own transaction, so no pooled connection keeps a
// role it was switched to (#27).

const INSUFFICIENT_PRIVILEGE: &str = "42501";

/// Runs `sql` as `role` in a transaction that is committed when it succeeds.
async fn as_role(pool: &PgPool, role: &str, sql: &str) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(&format!("SET LOCAL ROLE {role}"))
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::raw_sql(sql).execute(&mut *tx).await?;
    tx.commit().await
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ar_5_service_role_cannot_switch_the_rules_off(pool: PgPool) {
    let id = transfer(&pool, "br-1").await;
    // Review S1: the owner could disable the append-only trigger and rewrite the book. No role of
    // bezent's is the owner.
    for role in bezent_store::grants::ROLES {
        for sql in [
            "ALTER TABLE book_postings DISABLE TRIGGER book_postings_append_only".to_owned(),
            "ALTER TABLE transfers DISABLE TRIGGER ALL".to_owned(),
            "DROP TRIGGER transfers_guard_update ON transfers".to_owned(),
            "DROP TABLE book_postings".to_owned(),
            "ALTER TABLE transfers DROP CONSTRAINT transfers_source_whole".to_owned(),
            "TRUNCATE event_log".to_owned(),
            "UPDATE book_postings SET amount = 1".to_owned(),
            "UPDATE event_log_head SET seq = 0".to_owned(),
            format!("UPDATE transfers SET state = 'Completed' WHERE id = {id}"),
            format!("UPDATE transfers SET destination_final_network = 'issuing' WHERE id = {id}"),
            "CREATE TEMP TABLE scratch (x int)".to_owned(),
            "CREATE FUNCTION public.f() RETURNS int LANGUAGE sql AS 'SELECT 1'".to_owned(),
            "SET session_replication_role = replica".to_owned(),
        ] {
            let err = as_role(&pool, role, &sql)
                .await
                .expect_err(&format!("{role} ran: {sql}"));
            assert_error_sqlstate(&err, INSUFFICIENT_PRIVILEGE, &format!("{role}: {sql}"));
        }
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ar_5_service_role_does_its_work_through_the_rules(pool: PgPool) {
    // The ingress: a request, its transfer, bound to its source at insert, and its first
    // transition; all by INSERT.
    as_role(
        &pool,
        bezent_store::INGRESS_ROLE,
        "INSERT INTO intake (request_reference, requester, kind, body, body_digest) \
           VALUES ('req-1', 'requester-1', 'deposit', '{}', '\\x01'); \
         INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index) \
           VALUES ('br-1', 'req-1', 'deposit', 'asset-a', 1000, 'issuing', 'dest-1', 'Received', \
             'origin-a', 1, '\\xaa', 'log', 0); \
         INSERT INTO transitions (transfer_id, from_state, to_state) \
           SELECT id, 'Received', 'Accepted' FROM transfers;",
    )
    .await
    .expect("the ingress does its work");
    // Since F-004 the book is the core's record (migration 0030): no other service posts to it.
    let entry = "WITH e AS (INSERT INTO book_entries (reference) VALUES ('br-1') RETURNING id) \
         INSERT INTO book_postings (entry_id, asset, account, network, side, amount) \
           SELECT id, 'asset-a', 'reserve', 'origin-a', 'debit', 1000 FROM e \
           UNION ALL SELECT id, 'asset-a', 'owed-mint', NULL, 'credit', 1000 FROM e";
    for role in [
        bezent_store::INGRESS_ROLE,
        bezent_store::KREIVO_ENDPOINT_ROLE,
        bezent_store::EVM_ENDPOINT_ROLE,
    ] {
        let err = as_role(&pool, role, entry)
            .await
            .expect_err(&format!("{role} posted to the book"));
        assert_error_sqlstate(&err, INSUFFICIENT_PRIVILEGE, role);
    }
    // The core: the on-chain reference, set once, a transition resting on a coordinate, an
    // event, and the book. The arrival is final first, as the endpoint records it (review M1 of
    // PR #56: no Source-Final before; INV-6).
    finalise_coordinate(&pool, "origin-a", 1, "aa", "log", 0, "asset-a", "1000").await;
    as_role(
        &pool,
        bezent_store::CORE_ROLE,
        &format!(
            "UPDATE transfers SET on_chain_reference = '\\x01'; \
             INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
                 block_hash, block_time) \
               SELECT id, 'Accepted', 'Source-Final', 'origin-a', 21, '\\xbb', now() FROM transfers; \
             INSERT INTO event_log (kind, payload) VALUES ('route', '{{}}'); {entry}"
        ),
    )
    .await
    .expect("the core does its work");
    let (state, events): (String, i64) =
        sqlx::query_as("SELECT (SELECT state FROM transfers), (SELECT count(*) FROM event_log)")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "Source-Final");
    assert_eq!(events, 4);
}

#[test]
fn req_ar_5_service_binaries_carry_no_down_migration() {
    // Review S1: `MIGRATOR.undo` could drop every table from a service binary. The migrator the
    // services embed has no down migration; the reversible one lives with the tests only.
    assert!(
        bezent_store::MIGRATOR
            .iter()
            .all(|m| !m.migration_type.is_down_migration()),
        "the service migrator carries a down migration"
    );
    assert!(
        REVERSIBLE_MIGRATOR
            .iter()
            .any(|m| m.migration_type.is_down_migration())
    );
    // Both carry the same up migrations, byte for byte.
    let ups = |m: &'static sqlx::migrate::Migrator| {
        m.iter()
            .filter(|m| !m.migration_type.is_down_migration())
            .map(|m| (m.version, m.checksum.to_vec()))
            .collect::<Vec<_>>()
    };
    assert_eq!(ups(&bezent_store::MIGRATOR), ups(&REVERSIBLE_MIGRATOR));
}
