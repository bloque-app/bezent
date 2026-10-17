// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! T-001-03: the migrations apply on an empty database and are reversible, in tests only (the
//! down migrations live under `tests/migrations/`; no service binary carries them).
//!
//! A restore re-creates the schema on an empty server before it loads the records (`NFR-7`), so
//! the migrations must apply from nothing; reverting them leaves nothing of bezent behind.

mod common;

use common::REVERSIBLE_MIGRATOR;
use sqlx::PgPool;

const TABLES: &[&str] = &[
    "intake",
    "transfers",
    "transitions",
    "observations",
    "observation_outcomes",
    "book_entries",
    "book_postings",
    "intents",
    "signed_transactions",
    "signatures",
    "event_log",
    "event_log_head",
    "networks",
    "assets",
    "admitted_tokens",
    "reserves",
    "routes",
    "pauses",
    "requesters",
    "requester_grants",
    "requester_credentials",
    "intake_answers",
    "relayed_instructions",
    "final_heads",
    "attempt_milestones",
    "manual_actions",
    "manual_approvals",
    "observation_ranges",
    "observation_watermarks",
    "observation_gaps",
    "prepared_transactions",
    "non_execution_proofs",
    "reserve_evaluations",
    "reserve_statements",
    "book_quarantine",
    "surplus_funding_addresses",
    "reserve_readings",
    "reserve_statement_abandoned",
    "ledger_accounts",
    "webhook_secrets",
    "webhook_transfer_urls",
    "webhook_condition_urls",
    "webhook_cursor",
    "webhook_condition_holds",
    "webhook_deliveries",
];

async fn bezent_objects(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT kind || ' ' || name FROM ( \
           SELECT 'table' AS kind, table_name::text AS name FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_name <> '_sqlx_migrations' \
           UNION ALL \
           SELECT 'domain', domain_name::text FROM information_schema.domains \
             WHERE domain_schema = 'public' \
           UNION ALL \
           SELECT 'function', p.proname::text FROM pg_proc p \
             JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = 'public' \
         ) o ORDER BY 1",
    )
    .fetch_all(pool)
    .await
    .expect("list objects")
}

#[sqlx::test(migrations = false)]
async fn nfr_7_migrations_apply_on_an_empty_database_and_revert(pool: PgPool) {
    assert!(
        bezent_objects(&pool).await.is_empty(),
        "the database is not empty"
    );

    REVERSIBLE_MIGRATOR
        .run(&pool)
        .await
        .expect("migrations apply on an empty database");
    let objects = bezent_objects(&pool).await;
    for table in TABLES {
        assert!(
            objects.contains(&format!("table {table}")),
            "missing table {table}"
        );
    }

    REVERSIBLE_MIGRATOR
        .undo(&pool, 0)
        .await
        .expect("migrations revert");
    assert_eq!(
        bezent_objects(&pool).await,
        Vec::<String>::new(),
        "objects left after reverting"
    );

    // And they apply again on the emptied database.
    REVERSIBLE_MIGRATOR
        .run(&pool)
        .await
        .expect("migrations re-apply");
    assert_eq!(bezent_objects(&pool).await, objects);

    // The services' migrator sees the same migrations, already applied.
    bezent_store::MIGRATOR
        .run(&pool)
        .await
        .expect("the service migrator agrees with the reversible one");
    assert_eq!(bezent_objects(&pool).await, objects);
}
