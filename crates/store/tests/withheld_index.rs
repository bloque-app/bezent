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

async fn plan(pool: &PgPool, sql: &str) -> String {
    // A sequential scan is the cheapest way to read an empty table; the question is whether the
    // query can use the index, so the scan is turned off for the plan.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL enable_seqscan = off")
        .execute(&mut *tx)
        .await
        .unwrap();
    let rows: Vec<String> = sqlx::query_scalar(&format!("EXPLAIN {sql}"))
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    rows.join("\n")
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn the_last_page_of_a_hold_is_read_by_index(pool: PgPool) {
    let used = plan(&pool, LAST_PAGE).await;
    assert!(used.contains("event_log_blocked_by_inclusion"), "{used}");
    // Another kind of event does not take the index: it holds one kind only.
    let other = plan(
        &pool,
        "SELECT 1 FROM event_log WHERE kind = 'late-mint-prover-silent' AND transfer_id = 1",
    )
    .await;
    assert!(!other.contains("event_log_blocked_by_inclusion"), "{other}");
}
