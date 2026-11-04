// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #14, migration 0110: the delivery account's fee position is recorded by its network's
//! endpoint at a final head and read through `ops_observables()` (`REQ-ST-6`, `AC-H3.1`,
//! `REQ-OB-5`, `REQ-OB-6`, `REQ-NX-3`, `INV-8`, `INV-16`). A reading is exact in the store; an unknown
//! one is NULL with its reason, never zero.

mod common;

use common::{assert_refused, exec};
use sqlx::PgPool;

const REFUSED: Option<&str> = Some(bezent_store::REFUSAL_SQLSTATE);
/// Permission denied.
const DENIED: Option<&str> = Some("42501");

/// Runs `sql` as `role` in a transaction that is rolled back; returns the SQLSTATE of its error.
async fn as_role(pool: &PgPool, role: &str, sql: &str) -> Option<String> {
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(&format!("SET LOCAL ROLE {role}"))
        .execute(&mut *tx)
        .await
        .unwrap();
    let out = sqlx::raw_sql(sql).execute(&mut *tx).await;
    tx.rollback().await.unwrap();
    out.err().and_then(|e| {
        e.as_database_error()
            .and_then(|d| d.code().map(|c| c.into_owned()))
    })
}

async fn declare(pool: &PgPool) {
    exec(
        pool,
        "INSERT INTO networks (network, role, address_format) VALUES \
           ('kreivo', 'issuing', 'ss58'), ('polygon', 'origin', 'evm'); \
         INSERT INTO final_heads (network, block_number, block_hash, block_time) VALUES \
           ('kreivo', 1, '\\x01', now()), ('polygon', 1, '\\x01', now()), \
           ('polygon', 2, '\\x02', now())",
    )
    .await;
}

fn fee(network: &str, block: i64, amount: &str) -> String {
    format!(
        "INSERT INTO fee_position_readings (network, block_number, block_hash, holder, amount) \
         VALUES ('{network}', {block}, '\\x0{block}', '0xlane', {amount})"
    )
}

fn unknown_fee(network: &str, block: i64) -> String {
    format!(
        "INSERT INTO fee_position_readings (network, block_number, block_hash, holder, \
           unknown_reason) VALUES ('{network}', {block}, '\\x0{block}', '0xlane', 'the providers disagree')"
    )
}

/// `(metric, network, value as text)` of a fee position row; `None` for an absent row.
async fn row(pool: &PgPool, metric: &str, network: &str) -> Option<Option<String>> {
    let rows: Vec<(Option<String>,)> = sqlx::query_as(
        "SELECT value::text FROM ops_observables() WHERE metric = $1 AND network = $2",
    )
    .bind(metric)
    .bind(network)
    .fetch_all(pool)
    .await
    .unwrap();
    assert!(rows.len() <= 1, "one row per network: {rows:?}");
    rows.into_iter().next().map(|r| r.0)
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_h3_1_a_fee_position_is_exact_the_latest_counts_and_unknown_is_never_zero(pool: PgPool) {
    declare(&pool).await;
    // A declared network with no reading is a row with no value: unknown, not absent, not zero.
    for network in ["kreivo", "polygon"] {
        assert_eq!(row(&pool, "fee_position_amount", network).await, Some(None));
        assert_eq!(
            row(&pool, "fee_position_age_seconds", network).await,
            Some(None)
        );
    }
    // Exact above 2^53 (a float cannot hold this): 2^53 + 1 wei.
    exec(&pool, &fee("polygon", 1, "9007199254740993")).await;
    assert_eq!(
        row(&pool, "fee_position_amount", "polygon").await,
        Some(Some("9007199254740993".to_owned()))
    );
    let age: f64 = row(&pool, "fee_position_age_seconds", "polygon")
        .await
        .unwrap()
        .unwrap()
        .parse()
        .unwrap();
    assert!((0.0..60.0).contains(&age), "{age}");
    // The other network is untouched by it.
    assert_eq!(
        row(&pool, "fee_position_amount", "kreivo").await,
        Some(None)
    );
    // The latest reading counts, not the largest or the first.
    exec(&pool, &fee("polygon", 2, "5")).await;
    assert_eq!(
        row(&pool, "fee_position_amount", "polygon").await,
        Some(Some("5".to_owned()))
    );
    // A later reading that could not be made is unknown: the earlier amount is not carried
    // forward, and it is not zero.
    exec(
        &pool,
        "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
           VALUES ('polygon', 3, '\\x03', now())",
    )
    .await;
    exec(&pool, &unknown_fee("polygon", 3)).await;
    assert_eq!(
        row(&pool, "fee_position_amount", "polygon").await,
        Some(None)
    );
    assert!(
        row(&pool, "fee_position_age_seconds", "polygon")
            .await
            .unwrap()
            .is_some(),
        "the age of the last attempt is still known"
    );
    // No address reaches the observables (they carry no column for one).
    let leaked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ops_observables() o WHERE o::text LIKE '%0xlane%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(leaked, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ob_6_inv_8_only_the_evm_endpoint_records_an_evm_networks_fee_position(pool: PgPool) {
    declare(&pool).await;
    let evm = bezent_store::EVM_ENDPOINT_ROLE;
    // The EVM endpoint records its own network's; never the issuing network's.
    assert_eq!(as_role(&pool, evm, &fee("polygon", 1, "7")).await, None);
    assert_eq!(
        as_role(&pool, evm, &fee("kreivo", 1, "7")).await.as_deref(),
        REFUSED
    );
    // Nobody else may insert: the Kreivo endpoint records none (the amount is not decided), and
    // the marks, the core, the signer and the operators never write it.
    for role in [
        bezent_store::KREIVO_ENDPOINT_ROLE,
        bezent_store::ENDPOINT_ROLE,
        bezent_store::INGRESS_ROLE,
        bezent_store::SERVICE_ROLE,
        bezent_store::CORE_ROLE,
        bezent_store::SIGNER_ROLE,
        bezent_store::OPERATOR_ROLE,
    ] {
        assert_eq!(
            as_role(&pool, role, &fee("polygon", 1, "7"))
                .await
                .as_deref(),
            DENIED,
            "{role}"
        );
    }
    // The core and the operators read it through ops_observables() alone.
    for role in [bezent_store::CORE_ROLE, bezent_store::OPERATOR_ROLE] {
        assert_eq!(
            as_role(&pool, role, "SELECT * FROM fee_position_readings")
                .await
                .as_deref(),
            DENIED,
            "{role}"
        );
        assert_eq!(
            as_role(&pool, role, "SELECT * FROM ops_observables()").await,
            None,
            "{role}"
        );
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ob_5_a_fee_position_is_a_record_of_a_recorded_final_head(pool: PgPool) {
    declare(&pool).await;
    // A block that is not a recorded final head, or with another hash, has no reading.
    assert_refused(&pool, &fee("polygon", 9, "7"), "REQ-OB-5").await;
    assert_refused(
        &pool,
        "INSERT INTO fee_position_readings (network, block_number, block_hash, holder, amount) \
         VALUES ('polygon', 1, '\\x99', '0xlane', 7)",
        "REQ-OB-5",
    )
    .await;
    // Known or why, never both and never neither (REQ-NX-3).
    for bad in [
        "INSERT INTO fee_position_readings (network, block_number, block_hash, holder) \
         VALUES ('polygon', 1, '\\x01', '0xlane')",
        "INSERT INTO fee_position_readings (network, block_number, block_hash, holder, amount, \
           unknown_reason) VALUES ('polygon', 1, '\\x01', '0xlane', 1, 'why')",
    ] {
        assert!(
            sqlx::raw_sql(bad).execute(&pool).await.is_err(),
            "{bad} must be refused"
        );
    }
    // Append-only (INV-16).
    exec(&pool, &fee("polygon", 1, "7")).await;
    assert_refused(
        &pool,
        "UPDATE fee_position_readings SET amount = 8",
        "INV-16",
    )
    .await;
    assert_refused(&pool, "DELETE FROM fee_position_readings", "INV-16").await;
    assert_refused(&pool, "TRUNCATE fee_position_readings", "INV-16").await;
}
