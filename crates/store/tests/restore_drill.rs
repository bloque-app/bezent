// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! T-001-05: the restore drill (`NFR-7`).
//!
//! A populated database is dumped with `pg_dump`, restored with `psql` into a fresh database, and
//! every derived view (transfer state, book balances per asset, the log head) is compared. The
//! restored store keeps enforcing its rules: the next log position follows the restored head.
//!
//! `pg_dump` and `psql` come from `PATH`, or from `BEZENT_PG_DUMP` and `BEZENT_PSQL`; their major
//! version must be at least the server's.

mod common;

use common::*;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, PgConnection, PgPool};
use std::process::Command;

/// `DATABASE_URL` with its database replaced by `database`.
fn url_for(database: &str) -> String {
    let base = std::env::var("DATABASE_URL").expect("DATABASE_URL is set for the store tests");
    let (before_query, query) = match base.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (base.as_str(), None),
    };
    let scheme_end = before_query.find("://").map_or(0, |i| i + 3);
    let server = match before_query[scheme_end..].find('/') {
        Some(i) => &before_query[..scheme_end + i],
        None => before_query,
    };
    match query {
        Some(q) => format!("{server}/{database}?{q}"),
        None => format!("{server}/{database}"),
    }
}

fn tool(var: &str, default: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| default.to_owned())
}

async fn populate(pool: &PgPool) {
    exec(
        pool,
        "INSERT INTO intake (request_reference, requester, kind, body, body_digest) VALUES \
           ('req-br-1', 'requester-1', 'deposit', '{\"amount\":\"1000\"}', '\\x01'), \
           ('req-br-2', 'requester-1', 'withdrawal', '{\"amount\":\"1000\"}', '\\x02')",
    )
    .await;

    // A completed deposit, with its milestones written by its transitions.
    completed(pool, "br-1", 100).await;
    // One in flight, and one that began at Source-Final holding a condition.
    let open = transfer(pool, "br-2").await;
    to_source_final(pool, open, 101).await;
    let held = bound_deposit(pool, "br-3", 102).await;
    exec(
        pool,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state, condition) \
             VALUES ({held}, 'Source-Final', 'Source-Final', 'CND-MintWindow')"
        ),
    )
    .await;
    // A refusal.
    terminal(pool, "br-4", "Refused", 0).await;

    // The sources above are observed and final already (review M1 of PR #56); two more.
    for (block, index) in [(100, 1), (101, 1)] {
        observation(pool, block, index).await;
    }
    exec(
        pool,
        "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
             final_block_hash) SELECT id, 'final', 130, '\\xbb' FROM observations \
         WHERE block_number = 100 \
           AND id NOT IN (SELECT observation_id FROM observation_outcomes)",
    )
    .await;

    // The book: two arrivals, a mint, and a second asset; values beyond u64.
    balanced_entry(pool, "br-1", 1000).await;
    balanced_entry(pool, "br-2", 2500).await;
    let mut tx = pool.begin().await.unwrap();
    exec_tx(
        &mut tx,
        "WITH e AS (INSERT INTO book_entries (reference) VALUES ('mint-br-1') RETURNING id) \
         INSERT INTO book_postings (entry_id, asset, account, network, side, amount) \
         SELECT id, 'asset-a', 'owed-mint', NULL, 'debit', 1000 FROM e \
         UNION ALL SELECT id, 'asset-a', 'supply', NULL, 'credit', 1000 FROM e; \
         WITH e AS (INSERT INTO book_entries (reference) VALUES ('surplus-b') RETURNING id) \
         INSERT INTO book_postings (entry_id, asset, account, network, side, amount) \
         SELECT id, 'asset-b', 'reserve', 'origin-b', 'debit', 340282366920938463463374607431768211455 FROM e \
         UNION ALL SELECT id, 'asset-b', 'surplus', NULL, 'credit', 340282366920938463463374607431768211455 FROM e",
    )
    .await;
    tx.commit().await.unwrap();

    exec(
        pool,
        "INSERT INTO intents (transfer_id, network, purpose, payload) \
           SELECT id, 'issuing', 'mint', '{}' FROM transfers WHERE bridge_reference = 'br-1'; \
         INSERT INTO signed_transactions (intent_id, attempt, bytes) SELECT id, 1, '\\xdeadbeef' FROM intents WHERE payload = '{}'::jsonb",
    )
    .await;

    for n in 0..5 {
        exec(
            pool,
            &format!("INSERT INTO event_log (kind, payload) VALUES ('test', '{{\"n\":{n}}}')"),
        )
        .await;
    }
}

/// Every derived view, as text rows in a fixed order.
async fn derived_views(conn: &mut PgConnection) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    for (name, sql) in [
        (
            "transfer_status",
            "SELECT row_to_json(v)::text FROM transfer_status v ORDER BY bridge_reference",
        ),
        (
            "book_balances",
            "SELECT row_to_json(v)::text FROM book_balances v ORDER BY asset, account, network",
        ),
        (
            "event_log_position",
            "SELECT row_to_json(v)::text FROM event_log_position v",
        ),
        (
            "event_log",
            "SELECT row_to_json(v)::text FROM event_log v ORDER BY seq",
        ),
        (
            "observations",
            "SELECT row_to_json(v)::text FROM ( \
               SELECT o.*, x.outcome FROM observations o \
               LEFT JOIN observation_outcomes x ON x.observation_id = o.id) v ORDER BY id",
        ),
        (
            "transitions",
            "SELECT row_to_json(v)::text FROM transitions v ORDER BY id",
        ),
    ] {
        let rows: Vec<String> = sqlx::query_scalar(sql)
            .fetch_all(&mut *conn)
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        out.push((name.to_owned(), rows));
    }
    out
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn nfr_7_restore_drill_rederives_every_view(pool: PgPool) {
    populate(&pool).await;

    let source_db: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .unwrap();
    let mut source = pool.acquire().await.unwrap();
    let before = derived_views(&mut source).await;
    for (name, rows) in &before {
        assert!(
            !rows.is_empty(),
            "derived view {name} is empty before the dump"
        );
    }
    // Every transfer's recorded state is the state its transitions derive.
    let mismatched: i64 =
        sqlx::query_scalar("SELECT count(*) FROM transfer_status WHERE state <> derived_state")
            .fetch_one(&mut *source)
            .await
            .unwrap();
    assert_eq!(mismatched, 0);
    let head: i64 = sqlx::query_scalar("SELECT seq FROM event_log_head")
        .fetch_one(&mut *source)
        .await
        .unwrap();
    drop(source);

    // Dump.
    let dump = std::env::temp_dir().join(format!("bezent-restore-drill-{source_db}.sql"));
    let status = Command::new(tool("BEZENT_PG_DUMP", "pg_dump"))
        .args(["--no-owner", "--no-privileges", "--format=plain", "--file"])
        .arg(&dump)
        .arg(url_for(&source_db))
        .status()
        .expect("run pg_dump");
    assert!(status.success(), "pg_dump failed");

    // Restore into a fresh database.
    // A short name of its own: PostgreSQL truncates names at 63 bytes, and the test database's
    // name already fills them.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let restored_db = format!("bezent_restore_{}_{nanos}", std::process::id());
    sqlx::raw_sql(&format!("CREATE DATABASE \"{restored_db}\""))
        .execute(&pool)
        .await
        .expect("create the restore target");
    let status = Command::new(tool("BEZENT_PSQL", "psql"))
        .args(["--quiet", "--no-psqlrc", "-v", "ON_ERROR_STOP=1", "--file"])
        .arg(&dump)
        .arg(url_for(&restored_db))
        .status()
        .expect("run psql");
    let _ = std::fs::remove_file(&dump);
    assert!(status.success(), "psql restore failed");

    let restored_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url_for(&restored_db))
        .await
        .expect("connect to the restored database");
    let mut restored = restored_pool.acquire().await.unwrap();
    let after = derived_views(&mut restored).await;
    for ((name, b), (_, a)) in before.iter().zip(after.iter()) {
        assert_eq!(b, a, "derived view {name} differs after restore");
    }

    // The restored store still enforces its rules, from where the original left off.
    let next: i64 = sqlx::query_scalar(
        "INSERT INTO event_log (kind, payload) VALUES ('after-restore', '{}') RETURNING seq",
    )
    .fetch_one(&mut *restored)
    .await
    .unwrap();
    assert_eq!(next, head + 1);
    let err = sqlx::raw_sql("UPDATE book_postings SET amount = 1")
        .execute(&mut *restored)
        .await
        .expect_err("the restored book accepted an update");
    assert_refusal(&err, "INV-17", "update after restore");
    // A restored entry is as sealed as it was before the dump (INV-17).
    let err = sqlx::raw_sql(
        "INSERT INTO book_postings (entry_id, asset, account, network, side, amount) \
         SELECT min(id), 'asset-a', 'reserve', 'origin-a', 'debit', 1 FROM book_entries \
         UNION ALL SELECT min(id), 'asset-a', 'surplus', NULL, 'credit', 1 FROM book_entries",
    )
    .execute(&mut *restored)
    .await
    .expect_err("a restored entry gained postings");
    assert_refusal(&err, "INV-17", "postings added after restore");

    // SF-A (PR #39 third review): signing intents naming their prepared transactions were
    // restored with no session exempt from 0037's check, since pg_dump creates the trigger after
    // the data; and the restored store holds the check, to the restoring login too.
    let restored_signing: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM intents i JOIN prepared_transactions p \
           ON p.id::text = i.payload ->> 'prepared'",
    )
    .fetch_one(&mut *restored)
    .await
    .unwrap();
    assert!(
        restored_signing > 0,
        "the drill restores no signing intent naming its prepared transaction"
    );
    let err = sqlx::raw_sql(
        "INSERT INTO intents (transfer_id, network, purpose, on_chain_reference, reference_scope, \
             attempt, payload) \
         SELECT transfer_id, network, purpose, on_chain_reference, reference_scope, attempt, \
             payload || '{\"message\": \"0x99\"}' \
         FROM intents WHERE payload ? 'prepared' ORDER BY id LIMIT 1",
    )
    .execute(&mut *restored)
    .await
    .expect_err("the restored store accepted a signing intent over other bytes");
    assert_refusal(
        &err,
        "INV-9",
        "a signing intent over other bytes after restore",
    );

    drop(restored);
    restored_pool.close().await;
    let mut admin = PgConnection::connect(&url_for(&source_db)).await.unwrap();
    sqlx::raw_sql(&format!("DROP DATABASE \"{restored_db}\" WITH (FORCE)"))
        .execute(&mut admin)
        .await
        .expect("drop the restore target");
    admin.close().await.unwrap();
}
