// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Migration 0100: the read of the last page of a hold (`withheld` in the core's engine) is served
//! by a partial index on `resolution-blocked-by-inclusion`, not by a scan of the whole event log.

use sqlx::PgPool;

/// The core's query, as `Core::withheld` writes it.
const LAST_PAGE: &str = "SELECT (payload ->> 'at')::timestamptz, \
        COALESCE((payload ->> 'wall')::timestamptz, appended_at) FROM event_log \
     WHERE kind = 'resolution-blocked-by-inclusion' AND transfer_id = 1 \
       AND payload ->> 'hold' = 'Refunding-attempt-1' ORDER BY seq DESC LIMIT 1";

/// The plan of `sql` as the planner chooses it, with no setting turned off.
async fn plan(pool: &PgPool, sql: &str) -> String {
    let rows: Vec<String> = sqlx::query_scalar(&format!("EXPLAIN {sql}"))
        .fetch_all(pool)
        .await
        .unwrap();
    rows.join("\n")
}

/// A log as it is in production: many events of other kinds (so a scan of the log, or a walk of
/// its primary key from the newest end, has a lot to pass), and a few pages of the kind. Statistics
/// are gathered, so the planner chooses on what it sees, not on an empty table.
async fn a_log_with_history(pool: &PgPool) {
    sqlx::raw_sql(
        "INSERT INTO event_log (kind, payload) \
           SELECT 'noise', '{}'::jsonb FROM generate_series(1, 10000); \
         INSERT INTO event_log (kind, payload) \
           SELECT 'resolution-blocked-by-inclusion', '{\"hold\": \"Refunding-attempt-2\"}'::jsonb \
           FROM generate_series(1, 5); \
         INSERT INTO event_log (kind, payload) \
           SELECT 'noise', '{}'::jsonb FROM generate_series(1, 10000); \
         ANALYZE event_log",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn the_last_page_of_a_hold_is_read_by_index(pool: PgPool) {
    a_log_with_history(&pool).await;
    // No planner setting is turned off: with 20 000 events of other kinds and none that match
    // (the pages are of another hold), neither a scan of the log nor a walk of its primary key
    // from the newest end is cheaper than the partial index.
    let used = plan(&pool, LAST_PAGE).await;
    assert!(used.contains("event_log_blocked_by_inclusion"), "{used}");
    assert!(!used.contains("Seq Scan"), "{used}");
    assert!(!used.contains("event_log_pkey"), "{used}");
    // Another kind of event does not take the index: it holds one kind only.
    let other = plan(
        &pool,
        "SELECT 1 FROM event_log WHERE kind = 'late-mint-prover-silent' AND transfer_id = 1",
    )
    .await;
    assert!(!other.contains("event_log_blocked_by_inclusion"), "{other}");
}
