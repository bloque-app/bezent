// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-007 review S1, S2, M1: only an endpoint's role records what an endpoint proves
//! (`REQ-NX-1`, `REQ-NX-2`, `INV-8`). Each check runs under `SET LOCAL ROLE` inside its own
//! transaction, so no pooled connection keeps a role it was switched to (#27).

mod common;

use sqlx::PgPool;

const INSUFFICIENT_PRIVILEGE: &str = "42501";
const UNIQUE_VIOLATION: &str = "23505";

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

const PROOF: &str = "INSERT INTO non_execution_proofs (signed_transaction_id, network, signer, nonce, \
       valid_from_block, valid_until_block, final_block_number, final_block_hash, nonce_at_final, kind) \
     VALUES (999, 'issuing', '\\x01', 0, 0, 1, 1, '\\x01', 0, 'nonce-unconsumed')";

const PREPARED: &str = "INSERT INTO prepared_transactions (order_intent_id, attempt, network, lane, key_id, \
       nonce, valid_from_block, valid_until_block, checkpoint_hash, call, signing_payload, message, \
       dry_run_block_number, dry_run_block_hash, dry_run_passed, dry_run) \
     VALUES (999, 1, 'issuing', '\\x01', 'k', 0, 0, 63, '\\x01', '\\x01', '\\x01', '\\x01', 0, '\\x01', true, '{}')";

const RANGE: &str = "INSERT INTO observation_ranges (network, first_block, last_block, outcome, \
       final_block_number, final_block_hash) VALUES ('issuing', 0, 0, 'read', 0, '\\x01')";

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_nx_1_only_the_endpoint_role_records_proofs(pool: PgPool) {
    // The issuing network the records are of (0049: an endpoint writes only its declared
    // networks' records).
    common::exec(
        &pool,
        "INSERT INTO networks (network, role, address_format) VALUES ('issuing', 'issuing', 'ss58')",
    )
    .await;
    // The ingress and the core: refused for lack of privilege, and so is the retired service role.
    for role in [
        bezent_store::INGRESS_ROLE,
        bezent_store::CORE_ROLE,
        bezent_store::SERVICE_ROLE,
    ] {
        for sql in [PROOF, PREPARED, RANGE] {
            assert_eq!(
                as_role(&pool, role, sql).await.as_deref(),
                Some(INSUFFICIENT_PRIVILEGE),
                "{role}: {sql}"
            );
        }
    }
    // An endpoint's role has the privilege: the proof reaches its trigger, which refuses a proof
    // of a signed transaction that does not exist; the range is accepted.
    assert_eq!(
        as_role(&pool, bezent_store::KREIVO_ENDPOINT_ROLE, PROOF)
            .await
            .as_deref(),
        Some(bezent_store::REFUSAL_SQLSTATE)
    );
    assert_eq!(
        as_role(&pool, bezent_store::KREIVO_ENDPOINT_ROLE, RANGE).await,
        None
    );
    // The endpoints' mark alone holds nothing since 0045 (#35).
    for sql in [PROOF, RANGE] {
        assert_eq!(
            as_role(&pool, bezent_store::ENDPOINT_ROLE, sql)
                .await
                .as_deref(),
            Some(INSUFFICIENT_PRIVILEGE),
            "{sql}"
        );
    }
    // And it reads what it delivers against.
    assert_eq!(
        as_role(
            &pool,
            bezent_store::KREIVO_ENDPOINT_ROLE,
            "SELECT count(*) FROM signed_transactions; SELECT count(*) FROM event_log"
        )
        .await,
        None
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_nx_2_a_proof_names_its_transactions_signer_nonce_and_expiry(pool: PgPool) {
    // The issuing network the records are of (0049: an endpoint writes only its declared
    // networks' records).
    common::exec(
        &pool,
        "INSERT INTO networks (network, role, address_format) VALUES ('issuing', 'issuing', 'ss58')",
    )
    .await;
    let order: i64 = sqlx::query_scalar(
        "INSERT INTO intents (network, purpose, on_chain_reference, payload) \
         VALUES ('issuing', 'mint', '\\x01', '{\"order\": {}, \"amount\": \"1\"}') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let prepared: i64 = sqlx::query_scalar(&format!(
        "{} RETURNING id",
        PREPARED.replace("999", &order.to_string())
    ))
    .fetch_one(&pool)
    .await
    .unwrap();
    let signing: i64 = sqlx::query_scalar(&format!(
        "INSERT INTO intents (network, purpose, on_chain_reference, payload) \
         VALUES ('issuing', 'mint', '\\x01', \
           '{{\"message\": \"0x01\", \"amount\": \"1\", \"prepared\": {prepared}}}') RETURNING id"
    ))
    .fetch_one(&pool)
    .await
    .unwrap();
    let signed: i64 = sqlx::query_scalar(
        "INSERT INTO signed_transactions (intent_id, attempt, bytes, tx_hash) \
         VALUES ($1, 1, '\\x01', '\\x01') RETURNING id",
    )
    .bind(signing)
    .fetch_one(&pool)
    .await
    .unwrap();
    let proof = |signer: &str, nonce: i64, until: i64| {
        format!(
            "INSERT INTO non_execution_proofs (signed_transaction_id, network, signer, nonce, \
               valid_from_block, valid_until_block, final_block_number, final_block_hash, \
               nonce_at_final, kind) \
             VALUES ({signed}, 'issuing', '\\x{signer}', {nonce}, 0, {until}, 100, '\\x01', {nonce}, \
               'nonce-unconsumed')"
        )
    };
    // Another signer, nonce or expiry than the prepared transaction's: refused (S2).
    for sql in [proof("02", 0, 63), proof("01", 1, 63), proof("01", 0, 62)] {
        common::assert_refused(&pool, &sql, "REQ-NX-2").await;
    }
    // Its own: accepted, as the endpoint's role.
    assert_eq!(
        as_role(
            &pool,
            bezent_store::KREIVO_ENDPOINT_ROLE,
            &proof("01", 0, 63)
        )
        .await,
        None
    );

    // M1: a second signing intent for the same prepared transaction is refused.
    let err = sqlx::raw_sql(&format!(
        "INSERT INTO intents (network, purpose, on_chain_reference, payload) \
         VALUES ('issuing', 'mint', '\\x01', \
           '{{\"message\": \"0x01\", \"amount\": \"1\", \"prepared\": {prepared}}}')"
    ))
    .execute(&pool)
    .await
    .unwrap_err();
    assert_eq!(
        err.as_database_error().and_then(|d| d.code()).as_deref(),
        Some(UNIQUE_VIOLATION)
    );
}

const SIGNED: &str =
    "INSERT INTO signed_transactions (intent_id, attempt, bytes) VALUES (999, 1, '\\x01')";

const OBSERVATION: &str = "INSERT INTO observations (network, block_number, block_hash, block_time, \
       position_kind, position_index, token, amount, executed) \
     VALUES ('origin', 1, '\\x01', now(), 'log', 0, 'usdc', 1, true)";

/// Issues #33, #35 (migration 0021): the signed bytes and every network's observations are an
/// endpoint's record. The ingress and the core may not insert them; an endpoint's role may (since
/// 0045, its own role, not the endpoints' mark).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_eo_1_only_the_endpoint_role_records_signed_bytes_and_observations(pool: PgPool) {
    for role in [
        bezent_store::INGRESS_ROLE,
        bezent_store::CORE_ROLE,
        bezent_store::SERVICE_ROLE,
        bezent_store::ENDPOINT_ROLE,
    ] {
        for sql in [SIGNED, OBSERVATION] {
            assert_eq!(
                as_role(&pool, role, sql).await.as_deref(),
                Some(INSUFFICIENT_PRIVILEGE),
                "{role}: {sql}"
            );
        }
    }
    // An endpoint's role has the privilege: the signed bytes reach the store's rules, which refuse
    // signed bytes of no intent (0049: they are of no network the endpoint covers), and the
    // observation reaches them too. Since 0034 (#30) an endpoint records only the networks its own
    // registered role covers: the Kreivo endpoint's the issuing network.
    assert_eq!(
        as_role(&pool, bezent_store::KREIVO_ENDPOINT_ROLE, SIGNED)
            .await
            .as_deref(),
        Some(bezent_store::REFUSAL_SQLSTATE)
    );
    assert_eq!(
        as_role(&pool, bezent_store::KREIVO_ENDPOINT_ROLE, OBSERVATION)
            .await
            .as_deref(),
        Some(bezent_store::REFUSAL_SQLSTATE)
    );
    sqlx::raw_sql(
        "INSERT INTO networks (network, role, address_format) VALUES ('issuing', 'issuing', 'ss58')",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        as_role(
            &pool,
            bezent_store::KREIVO_ENDPOINT_ROLE,
            &OBSERVATION.replace("'origin'", "'issuing'")
        )
        .await,
        None
    );
    // The core and the endpoints read them; the ingress reads observations, never signed bytes.
    for role in [
        bezent_store::CORE_ROLE,
        bezent_store::KREIVO_ENDPOINT_ROLE,
        bezent_store::EVM_ENDPOINT_ROLE,
    ] {
        assert_eq!(
            as_role(
                &pool,
                role,
                "SELECT count(*) FROM signed_transactions; SELECT count(*) FROM observations"
            )
            .await,
            None,
            "{role}"
        );
    }
    assert_eq!(
        as_role(
            &pool,
            bezent_store::INGRESS_ROLE,
            "SELECT count(*) FROM observations"
        )
        .await,
        None
    );
    assert_eq!(
        as_role(
            &pool,
            bezent_store::INGRESS_ROLE,
            "SELECT count(*) FROM signed_transactions"
        )
        .await
        .as_deref(),
        Some(INSUFFICIENT_PRIVILEGE)
    );
}

/// Whether `role` may execute function `f`.
async fn may_execute(pool: &PgPool, role: &str, f: &str) -> bool {
    sqlx::query_scalar("SELECT has_function_privilege($1, $2::regprocedure, 'EXECUTE')")
        .bind(role)
        .bind(f)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("{role} {f}: {e}"))
}

/// F1 of 0017, for F-007's functions (migration 0021): none is PUBLIC's. The trigger functions go
/// to no role; the roles that insert into a guarded table keep the role check their triggers run as
/// them, and the shared roles hold nothing (0045).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ar_3_no_endpoint_function_is_public(pool: PgPool) {
    for f in [
        "observation_ranges_guard_insert()",
        "non_execution_proofs_guard_insert()",
        "event_log_guard_signal()",
        "observations_guard_writer()",
        "observation_outcomes_guard_writer()",
    ] {
        for role in [
            bezent_store::SERVICE_ROLE,
            bezent_store::ENDPOINT_ROLE,
            bezent_store::SIGNER_ROLE,
        ] {
            assert!(!may_execute(&pool, role, f).await, "{role} may run {f}");
        }
    }
    let holds = "bezent_writer_holds(text, oid)";
    for role in [
        bezent_store::CORE_ROLE,
        bezent_store::INGRESS_ROLE,
        bezent_store::KREIVO_ENDPOINT_ROLE,
        bezent_store::EVM_ENDPOINT_ROLE,
    ] {
        assert!(may_execute(&pool, role, holds).await, "{role}");
    }
    assert!(!may_execute(&pool, bezent_store::SERVICE_ROLE, holds).await);
    assert!(!may_execute(&pool, bezent_store::ENDPOINT_ROLE, holds).await);
    assert!(!may_execute(&pool, bezent_store::SIGNER_ROLE, holds).await);
    assert!(!may_execute(&pool, bezent_store::OPERATOR_ROLE, holds).await);
}
