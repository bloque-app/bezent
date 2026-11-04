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
    "observation_walk_anchors",
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
    "reserve_statement_objects",
    "ledger_accounts",
    "webhook_secrets",
    "webhook_transfer_urls",
    "webhook_condition_urls",
    "webhook_cursor",
    "webhook_condition_holds",
    "webhook_deliveries",
    "unsigned_attempt_cancellations",
    "attesting_parties",
    "attesting_keys",
    "attested_reports",
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

/// F-011 (S7): the attested backend's three migrations revert one step at a time and apply again.
/// 0061 changes functions 0060 made, so reverting it alone must leave 0060's, and 0060 must revert
/// whole with 0061 gone.
#[sqlx::test(migrations = false)]
async fn nfr_7_the_attested_migrations_revert_stepwise_and_apply_again(pool: PgPool) {
    REVERSIBLE_MIGRATOR
        .run(&pool)
        .await
        .expect("migrations apply on an empty database");
    let objects = bezent_objects(&pool).await;
    for target in [61, 60, 59] {
        REVERSIBLE_MIGRATOR
            .undo(&pool, target)
            .await
            .unwrap_or_else(|e| panic!("revert to {target}: {e}"));
        REVERSIBLE_MIGRATOR
            .run(&pool)
            .await
            .unwrap_or_else(|e| panic!("apply again after {target}: {e}"));
        assert_eq!(
            bezent_objects(&pool).await,
            objects,
            "up, down to {target}, up"
        );
    }
}

/// F-011 (S7): the attested migrations also revert with rows they made: a report taken in by the
/// intake, and a report of non-execution. Both tables are append-only, so the narrower checks of the
/// older versions are added `NOT VALID` and the rows stay.
#[sqlx::test(migrations = false)]
async fn nfr_7_the_attested_migrations_revert_with_the_rows_they_made(pool: PgPool) {
    REVERSIBLE_MIGRATOR
        .run(&pool)
        .await
        .expect("migrations apply on an empty database");
    let objects = bezent_objects(&pool).await;
    sqlx::raw_sql(
        "INSERT INTO networks (network, role, address_format, backend) VALUES \
           ('kreivo', 'issuing', 'ss58', NULL), ('rail', 'origin', 'opaque', 'attested:1'); \
         INSERT INTO assets (asset, precision, assurance_class, issuing_asset) \
           VALUES ('cop', 2, 'attested', 'Here(100)'); \
         INSERT INTO requesters (requester) VALUES ('swaps'); \
         INSERT INTO attesting_parties (network, requester, party) VALUES ('rail', 'swaps', 'Swaps'); \
         INSERT INTO attesting_keys (network, public_key) VALUES ('rail', decode(repeat('11', 32), 'hex')); \
         INSERT INTO intake (request_reference, requester, kind, body, body_digest) \
           VALUES ('r-1', 'swaps', 'report', '{}', '\\x01'); \
         INSERT INTO attested_reports (network, seq, intake_id, requester, party, report_id, kind, \
                 asset, body, signed_bytes, signature, public_key, digest) \
           SELECT 'rail', 1, i.id, 'swaps', 'Swaps', 'r-1', 'payout-not-executed', 'cop', \
                  '{\"report_id\": \"r-1\", \"kind\": \"payout-not-executed\", \"network\": \"rail\", \"asset\": \"cop\"}', \
                  '\\x01', decode(repeat('00', 64), 'hex'), decode(repeat('11', 32), 'hex'), \
                  decode(repeat('22', 32), 'hex') \
           FROM intake i WHERE i.request_reference = 'r-1';",
    )
    .execute(&pool)
    .await
    .expect("rows the attested migrations made");
    for target in [61, 60, 59] {
        REVERSIBLE_MIGRATOR
            .undo(&pool, target)
            .await
            .unwrap_or_else(|e| panic!("revert to {target} with rows: {e}"));
    }
    let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM intake WHERE kind = 'report'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kept, 1, "the append-only intake keeps its row");
    REVERSIBLE_MIGRATOR
        .run(&pool)
        .await
        .expect("apply again with the intake row there");
    assert_eq!(bezent_objects(&pool).await, objects);
}
