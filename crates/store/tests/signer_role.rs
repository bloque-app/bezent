// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! F-005 T-005-01: the signer's role and its signature table (`REQ-AR-3`, `INV-9`, `INV-2`,
//! `REQ-EO-1`). The signer reads intents and records signatures, append-only, and nothing else.

mod common;

use common::*;
use sqlx::postgres::Postgres;
use sqlx::{PgPool, Transaction};

const UNIQUE_VIOLATION: &str = "23505";
const INSUFFICIENT_PRIVILEGE: &str = "42501";

/// Records a signing intent on `issuing` with `reference` (hex, or none) and `amount` (a JSON
/// value), over its order's prepared bytes `0x01` (0037). An amount that is not a JSON string is
/// one the store refuses on the intent itself, so it is recorded with that check off, to reach the
/// signature's own check (INV-11).
async fn intent(pool: &PgPool, reference: Option<&str>, amount: &str) -> i64 {
    let reference = reference.map_or("NULL".to_owned(), |r| format!("'\\x{r}'"));
    if amount.starts_with('"') {
        return signing_intent(pool, "issuing", "mint", &reference, 1, amount, "01").await;
    }
    without_intent_check(
        pool,
        &format!(
            "INSERT INTO intents (network, purpose, on_chain_reference, payload) \
             VALUES ('issuing', 'mint', {reference}, \
                     jsonb_build_object('message', '0x01', 'amount', '{amount}'::jsonb)) \
             RETURNING id"
        ),
    )
    .await
}

fn insert_signature(intent_id: i64) -> String {
    format!(
        "INSERT INTO signatures (intent_id, key_id, scheme, public_key, signature) \
         VALUES ({intent_id}, 'mint-key', 'sr25519', '\\x01', '\\x02')"
    )
}

/// A transaction acting as `role` (`SET LOCAL ROLE`): the pooled connection never keeps the role
/// past it (#27). Dropped, it rolls back.
async fn as_role(pool: &PgPool, role: &str) -> Transaction<'static, Postgres> {
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(&format!("SET LOCAL ROLE {role}"))
        .execute(&mut *tx)
        .await
        .unwrap();
    tx
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_9_signer_role_reads_intents_and_inserts_signatures(pool: PgPool) {
    let id = intent(&pool, Some("01"), "\"1000\"").await;
    let mut conn = as_role(&pool, bezent_store::SIGNER_ROLE).await;
    let message: String =
        sqlx::query_scalar("SELECT payload ->> 'message' FROM intents WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *conn)
            .await
            .expect("the signer reads the intent");
    assert_eq!(message, "0x01");
    sqlx::raw_sql(&insert_signature(id))
        .execute(&mut *conn)
        .await
        .expect("the signer records its signature");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM signatures WHERE intent_id = $1")
        .bind(id)
        .fetch_one(&mut *conn)
        .await
        .expect("the signer reads its own signatures");
    assert_eq!(n, 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ar_3_signer_role_holds_no_privilege_but_its_own(pool: PgPool) {
    // Every table and view of the schema, present and future: the signer may SELECT intents and
    // prepared transactions (0037, MF-A), and SELECT and INSERT signatures. Nothing else, of any kind.
    let granted: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.relname::text, p.privilege \
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         CROSS JOIN unnest(ARRAY['SELECT', 'INSERT', 'UPDATE', 'DELETE', 'TRUNCATE', \
                                 'REFERENCES', 'TRIGGER']) AS p(privilege) \
         WHERE n.nspname = 'public' AND c.relkind IN ('r', 'v', 'm', 'p') \
           AND has_table_privilege($1, c.oid, p.privilege) \
         ORDER BY 1, 2",
    )
    .bind(bezent_store::SIGNER_ROLE)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        granted,
        vec![
            // The alert of bytes that do not answer the core's order, and its pause request
            // (0049, audit must-fix 2): the store admits no other kind from it.
            // Which token an order's asset is paid in (0049, must-fix 2).
            ("admitted_tokens".to_owned(), "SELECT".to_owned()),
            ("event_log".to_owned(), "INSERT".to_owned()),
            ("intents".to_owned(), "SELECT".to_owned()),
            ("prepared_transactions".to_owned(), "SELECT".to_owned()),
            ("signatures".to_owned(), "INSERT".to_owned()),
            ("signatures".to_owned(), "SELECT".to_owned()),
        ]
    );
    // No column-level grant elsewhere either.
    let columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.column_privileges \
         WHERE grantee = $1 \
           AND table_name NOT IN ('admitted_tokens', 'event_log', 'intents', \
                                  'prepared_transactions', 'signatures', 'signed_transactions')",
    )
    .bind(bezent_store::SIGNER_ROLE)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(columns, 0);

    // And what it tries is refused.
    let id = intent(&pool, None, "\"1\"").await;
    let transfer = transfer(&pool, "br-1").await;
    for sql in [
        "INSERT INTO intents (network, purpose, payload) VALUES ('issuing', 'mint', '{}')"
            .to_owned(),
        "INSERT INTO signed_transactions (intent_id, attempt, bytes) SELECT id, 1, '\\x01' FROM intents"
            .to_owned(),
        format!("UPDATE transfers SET on_chain_reference = '\\x09' WHERE id = {transfer}"),
        "SELECT * FROM transfers".to_owned(),
        "SELECT * FROM book_postings".to_owned(),
        "SELECT * FROM intake".to_owned(),
        "ALTER TABLE signatures DISABLE TRIGGER signatures_append_only".to_owned(),
        "DROP TRIGGER signatures_from_intent ON signatures".to_owned(),
        "CREATE TEMP TABLE scratch (x int)".to_owned(),
        "SET session_replication_role = replica".to_owned(),
        format!("SELECT 1 FROM intents WHERE id = {id} FOR UPDATE"),
    ] {
        let mut conn = as_role(&pool, bezent_store::SIGNER_ROLE).await;
        let err = sqlx::raw_sql(&sql)
            .execute(&mut *conn)
            .await
            .expect_err(&format!("the signer role ran: {sql}"));
        assert_error_sqlstate(&err, INSUFFICIENT_PRIVILEGE, &sql);
    }
    // Of the log, the store admits from it an alert and its pause request only (0049).
    for kind in ["x", "delivery-anomaly", "condition-raised", "refusal"] {
        let mut conn = as_role(&pool, bezent_store::SIGNER_ROLE).await;
        let sql = format!(
            "INSERT INTO event_log (kind, payload) VALUES ('{kind}', '{{\"requested_intent\": 1}}')"
        );
        let err = sqlx::raw_sql(&sql)
            .execute(&mut *conn)
            .await
            .expect_err(&format!("the signer role ran: {sql}"));
        assert_error_sqlstate(&err, bezent_store::REFUSAL_SQLSTATE, &sql);
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_9_signature_needs_a_recorded_intent(pool: PgPool) {
    assert_refused(&pool, &insert_signature(4242), "INV-9").await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_11_signature_needs_the_intents_amount_in_minor_units(pool: PgPool) {
    for amount in ["1000", "\"1.5\"", "\"-1\"", "null"] {
        let id = intent(&pool, None, amount).await;
        assert_refused(&pool, &insert_signature(id), "INV-11").await;
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_9_signature_takes_reference_and_amount_from_the_intent(pool: PgPool) {
    let id = intent(
        &pool,
        Some("0a0b"),
        "\"340282366920938463463374607431768211455\"",
    )
    .await;
    exec(
        &pool,
        &format!(
            "INSERT INTO signatures (intent_id, key_id, scheme, public_key, signature, \
                 on_chain_reference, amount) \
             VALUES ({id}, 'mint-key', 'sr25519', '\\x01', '\\x02', '\\xff', 1)"
        ),
    )
    .await;
    let (reference, amount) = (
        text(
            &pool,
            "SELECT encode(on_chain_reference, 'hex') FROM signatures",
        )
        .await,
        text(&pool, "SELECT amount::text FROM signatures").await,
    );
    assert_eq!(reference, "0a0b");
    assert_eq!(amount, "340282366920938463463374607431768211455");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_d2_1_one_signature_per_intent(pool: PgPool) {
    let id = intent(&pool, None, "\"1\"").await;
    exec(&pool, &insert_signature(id)).await;
    assert_sqlstate(&pool, &insert_signature(id), UNIQUE_VIOLATION).await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_2_one_signature_per_on_chain_reference(pool: PgPool) {
    let first = intent(&pool, Some("01"), "\"1\"").await;
    let second = intent(&pool, Some("01"), "\"1\"").await;
    exec(&pool, &insert_signature(first)).await;
    assert_sqlstate(&pool, &insert_signature(second), UNIQUE_VIOLATION).await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_9_signatures_are_append_only(pool: PgPool) {
    let id = intent(&pool, None, "\"1\"").await;
    exec(&pool, &insert_signature(id)).await;
    assert_refused(&pool, "UPDATE signatures SET signature = '\\x03'", "INV-9").await;
    assert_refused(&pool, "DELETE FROM signatures", "INV-9").await;
    assert_refused(&pool, "TRUNCATE signatures", "INV-9").await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_9_only_the_core_role_records_intents(pool: PgPool) {
    // Review S5: every service holds the service role; only the core's login holds the core role.
    // A signing intent names its order's prepared transaction (0037): the order and its
    // preparation first, as the core and an endpoint would.
    let order: i64 = sqlx::query_scalar(
        "INSERT INTO intents (network, purpose, payload) \
         VALUES ('issuing', 'mint', '{\"order\": {}, \"amount\": \"1\"}') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let prepared = prepare_order(&pool, order).await;
    let record = format!(
        "INSERT INTO intents (network, purpose, payload) \
         VALUES ('issuing', 'mint', \
                 '{{\"message\": \"0x01\", \"amount\": \"1\", \"prepared\": {prepared}}}') \
         RETURNING id"
    );
    let record = record.as_str();
    let mut core = as_role(&pool, bezent_store::CORE_ROLE).await;
    sqlx::raw_sql(record)
        .execute(&mut *core)
        .await
        .expect("the core records an intent");
    for role in [
        bezent_store::INGRESS_ROLE,
        bezent_store::KREIVO_ENDPOINT_ROLE,
        bezent_store::EVM_ENDPOINT_ROLE,
        bezent_store::SERVICE_ROLE,
        bezent_store::SIGNER_ROLE,
    ] {
        let mut conn = as_role(&pool, role).await;
        let err = sqlx::raw_sql(record)
            .execute(&mut *conn)
            .await
            .expect_err(&format!("{role} recorded an intent"));
        assert_error_sqlstate(&err, INSUFFICIENT_PRIVILEGE, role);
    }
    // No role of bezent's but the core's may insert an intent, by any grant.
    let inserters: Vec<String> = sqlx::query_scalar(
        "SELECT rolname::text FROM pg_roles \
         WHERE rolname LIKE 'bezent\\_%' AND has_table_privilege(oid, 'intents', 'INSERT') \
         ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(inserters, vec![bezent_store::CORE_ROLE.to_owned()]);
    // What else the core's role holds is its rows of the grant matrix (tests/grant_matrix.rs).
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        bezent_store::grants::held(&mut conn, bezent_store::CORE_ROLE)
            .await
            .unwrap(),
        bezent_store::grants::expected(bezent_store::CORE_ROLE)
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ar_3_the_core_and_the_endpoints_read_signatures_and_never_write_one(pool: PgPool) {
    let id = intent(&pool, None, "\"1\"").await;
    exec(&pool, &insert_signature(id)).await;
    // The ingress does not even read them (0045, #35).
    let mut conn = as_role(&pool, bezent_store::INGRESS_ROLE).await;
    let err = sqlx::raw_sql("SELECT count(*) FROM signatures")
        .execute(&mut *conn)
        .await
        .expect_err("the ingress read a signature");
    assert_error_sqlstate(&err, INSUFFICIENT_PRIVILEGE, "ingress read");
    drop(conn);
    for role in [
        bezent_store::KREIVO_ENDPOINT_ROLE,
        bezent_store::EVM_ENDPOINT_ROLE,
    ] {
        let mut conn = as_role(&pool, role).await;
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM signatures")
            .fetch_one(&mut *conn)
            .await
            .expect("an endpoint reads signatures");
        assert_eq!(n, 1, "{role}");
    }
    let mut conn = as_role(&pool, bezent_store::CORE_ROLE).await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM signatures")
        .fetch_one(&mut *conn)
        .await
        .expect("the core reads signatures");
    assert_eq!(n, 1);
    let second = intent(&pool, None, "\"1\"").await;
    let err = sqlx::raw_sql(&insert_signature(second))
        .execute(&mut *conn)
        .await
        .expect_err("the core's role recorded a signature");
    assert_error_sqlstate(&err, INSUFFICIENT_PRIVILEGE, "core insert");
}
