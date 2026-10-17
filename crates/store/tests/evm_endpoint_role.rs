// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! F-009, migration 0035: the EVM endpoint's role writes the records of EVM networks only
//! (`REQ-OB-6`, `INV-8`). Each check runs under `SET LOCAL ROLE` inside its own transaction, so no
//! pooled connection keeps a role it was switched to (#27).

mod common;

use sqlx::PgPool;

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

const REFUSED: Option<&str> = Some(bezent_store::REFUSAL_SQLSTATE);

async fn declare(pool: &PgPool) {
    common::exec(
        pool,
        "INSERT INTO networks (network, role, address_format) VALUES \
           ('kreivo', 'issuing', 'ss58'), ('polygon', 'origin', 'evm')",
    )
    .await;
}

fn observation(network: &str) -> String {
    format!(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
           position_index, executed) VALUES ('{network}', 1, '\\x01', now(), 'log', 0, true)"
    )
}

fn final_head(network: &str) -> String {
    format!(
        "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
         VALUES ('{network}', 1, '\\x01', now())"
    )
}

fn range(network: &str) -> String {
    format!(
        "INSERT INTO observation_ranges (network, first_block, last_block, outcome, \
           final_block_number, final_block_hash) VALUES ('{network}', 1, 1, 'read', 1, '\\x01')"
    )
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_8_the_evm_endpoint_cannot_write_the_issuing_networks_records(pool: PgPool) {
    declare(&pool).await;
    let role = bezent_store::EVM_ENDPOINT_ROLE;
    for sql in [observation("kreivo"), final_head("kreivo"), range("kreivo")] {
        assert_eq!(
            as_role(&pool, role, &sql).await.as_deref(),
            REFUSED,
            "{sql}"
        );
    }
    // A network nobody declared is not an EVM network either.
    assert_eq!(
        as_role(&pool, role, &observation("nowhere"))
            .await
            .as_deref(),
        REFUSED
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ob_6_the_evm_endpoint_writes_its_networks_records(pool: PgPool) {
    declare(&pool).await;
    let role = bezent_store::EVM_ENDPOINT_ROLE;
    for sql in [
        observation("polygon"),
        final_head("polygon"),
        range("polygon"),
    ] {
        assert_eq!(as_role(&pool, role, &sql).await, None, "{sql}");
    }
    // An outcome is guarded by its observation's network.
    // `origin-a` (common::observation) is not declared, so not an EVM network.
    let undeclared = common::observation(&pool, 1, 0).await;
    let sql = format!(
        "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
           final_block_hash) VALUES ({undeclared}, 'final', 1, '\\x01')"
    );
    assert_eq!(as_role(&pool, role, &sql).await.as_deref(), REFUSED);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ob_6_the_other_endpoint_role_and_the_owner_are_unchanged(pool: PgPool) {
    declare(&pool).await;
    // Since 0034 the Kreivo endpoint's own role records the issuing network only, and a role
    // holding bezent_endpoint with no registered scope records nothing (since 0045 it holds no
    // privilege at all).
    assert_eq!(
        as_role(
            &pool,
            bezent_store::KREIVO_ENDPOINT_ROLE,
            &observation("kreivo")
        )
        .await,
        None
    );
    assert_eq!(
        as_role(
            &pool,
            bezent_store::KREIVO_ENDPOINT_ROLE,
            &observation("polygon")
        )
        .await
        .as_deref(),
        REFUSED
    );
    for network in ["polygon", "kreivo"] {
        assert_eq!(
            as_role(&pool, bezent_store::ENDPOINT_ROLE, &observation(network))
                .await
                .as_deref(),
            Some("42501"),
            "{network}"
        );
    }
    common::exec(&pool, &observation("kreivo")).await;
    // The services' role still cannot write observations at all (0021).
    assert!(
        as_role(&pool, bezent_store::SERVICE_ROLE, &observation("polygon"))
            .await
            .is_some()
    );
}

const LOGIN_PASSWORD: &str = "test-only-evm-endpoint-login";

/// A login on `pool`'s database named `name` holding `roles`, created once per server, with a
/// test-only password: one pool, one login (#27).
async fn login(pool: &PgPool, name: &str, roles: &[&str]) -> PgPool {
    let grants: String = roles
        .iter()
        .map(|r| format!("BEGIN GRANT {r} TO {name}; EXCEPTION WHEN OTHERS THEN NULL; END; "))
        .collect();
    common::exec(
        pool,
        &format!(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{name}') THEN \
                 BEGIN CREATE ROLE {name} LOGIN PASSWORD '{LOGIN_PASSWORD}'; \
                 EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END; \
               END IF; {grants} \
             END $$;"
        ),
    )
    .await;
    let options = (*pool.connect_options())
        .clone()
        .username(name)
        .password(LOGIN_PASSWORD);
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap_or_else(|e| panic!("log in as {name}: {e}"))
}

/// Runs `SET LOCAL ROLE bezent_endpoint` and then `sql` in one transaction that is rolled back;
/// returns the SQLSTATE and message of the first error.
async fn switched(pool: &PgPool, sql: &str) -> Option<(String, String)> {
    let mut tx = pool.begin().await.unwrap();
    let mut out = sqlx::raw_sql("SET LOCAL ROLE bezent_endpoint")
        .execute(&mut *tx)
        .await
        .map(|_| ());
    if out.is_ok() {
        out = sqlx::raw_sql(sql).execute(&mut *tx).await.map(|_| ());
    }
    tx.rollback().await.unwrap();
    out.err().and_then(|e| {
        e.as_database_error().map(|d| {
            (
                d.code().map(|c| c.into_owned()).unwrap_or_default(),
                d.message().to_owned(),
            )
        })
    })
}

/// A Kreivo signed transaction, recorded as the core, the endpoint and the signer would; returns
/// the insert of a proof of its non-execution.
async fn kreivo_proof(pool: &PgPool) -> String {
    let order: i64 = sqlx::query_scalar(
        "INSERT INTO intents (network, purpose, on_chain_reference, payload) \
         VALUES ('kreivo', 'mint', '\\x01', '{\"order\": {}, \"amount\": \"1\"}') RETURNING id",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let prepared: i64 = sqlx::query_scalar(
        "INSERT INTO prepared_transactions (order_intent_id, attempt, network, lane, key_id, \
           nonce, valid_from_block, valid_until_block, checkpoint_hash, call, signing_payload, \
           message, dry_run_block_number, dry_run_block_hash, dry_run_passed, dry_run) \
         VALUES ($1, 1, 'kreivo', '\\x01', 'k', 0, 0, 5, '\\x01', '\\x01', '\\x01', '\\x01', 0, \
           '\\x01', true, '{}') RETURNING id",
    )
    .bind(order)
    .fetch_one(pool)
    .await
    .unwrap();
    let signing: i64 = sqlx::query_scalar(&format!(
        "INSERT INTO intents (network, purpose, on_chain_reference, payload) \
         VALUES ('kreivo', 'mint', '\\x01', \
           '{{\"message\": \"0x01\", \"amount\": \"1\", \"prepared\": {prepared}}}') RETURNING id"
    ))
    .fetch_one(pool)
    .await
    .unwrap();
    let signed: i64 = sqlx::query_scalar(
        "INSERT INTO signed_transactions (intent_id, attempt, bytes, tx_hash) \
         VALUES ($1, 1, '\\x01', '\\x01') RETURNING id",
    )
    .bind(signing)
    .fetch_one(pool)
    .await
    .unwrap();
    format!(
        "INSERT INTO non_execution_proofs (signed_transaction_id, network, signer, nonce, \
           valid_from_block, valid_until_block, final_block_number, final_block_hash, \
           nonce_at_final, kind) \
         VALUES ({signed}, 'kreivo', '\\x01', 0, 0, 5, 10, '\\x0a', 0, 'nonce-unconsumed')"
    )
}

/// PR #39 review M1 (`INV-8`): a login holding `bezent_evm_endpoint` that runs
/// `SET ROLE bezent_endpoint` still cannot write a Kreivo observation or a Kreivo non-execution
/// proof. On PostgreSQL 16 the switch itself is refused (the membership has SET FALSE); a login
/// that reached `bezent_endpoint` by another grant switches, and the guard, which asks of the
/// session's login, refuses the writes.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m1_inv_8_set_role_bezent_endpoint_from_an_evm_endpoint_login_is_refused(pool: PgPool) {
    declare(&pool).await;
    let proof = kreivo_proof(&pool).await;
    let writes = [observation("kreivo"), proof.clone()];
    // The proof is well formed: the Kreivo endpoint's role records it.
    assert_eq!(
        as_role(&pool, bezent_store::KREIVO_ENDPOINT_ROLE, &proof).await,
        None
    );

    let version: i32 = sqlx::query_scalar("SELECT current_setting('server_version_num')::int")
        .fetch_one(&pool)
        .await
        .unwrap();
    let evm = login(
        &pool,
        "evm_endpoint_login_for_tests",
        &[bezent_store::EVM_ENDPOINT_ROLE],
    )
    .await;
    for w in &writes {
        let (code, message) = switched(&evm, w).await.expect("refused");
        if version >= 160000 {
            assert_eq!(code, "42501", "the switch itself: {message}");
        } else {
            assert_eq!(code, bezent_store::REFUSAL_SQLSTATE, "{w}: {message}");
            assert!(message.contains("INV-8"), "{message}");
        }
    }

    // Another grant of bezent_endpoint, with SET: the switch works, and the write is refused:
    // bezent_endpoint holds no privilege since 0045 (#35), and the guard would refuse it anyway.
    let misgranted = login(
        &pool,
        "evm_endpoint_misgranted_for_tests",
        &[bezent_store::EVM_ENDPOINT_ROLE, bezent_store::ENDPOINT_ROLE],
    )
    .await;
    for w in &writes {
        let (code, message) = switched(&misgranted, w).await.expect("refused");
        assert_eq!(code, "42501", "{w}: {message}");
    }
    // Switched to bezent_endpoint, it records no observation at all, of any network.
    let (code, _) = switched(&misgranted, &observation("polygon"))
        .await
        .expect("refused");
    assert_eq!(code, "42501");
}
