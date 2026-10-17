// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Migration 0044: who may write what in the webhook tables (issue #2; PR #55 re-check S-a, S-c).
//! Every check logs in as a login of its own, so the session's login (`session_user`) is the
//! role under test, as in production.

mod common;

use sqlx::PgPool;

const LOGIN_PASSWORD: &str = "test-only-webhook-roles-login";

/// A login on `pool`'s database named `name` holding `roles`, with a test-only password.
async fn login(pool: &PgPool, name: &str, roles: &[&str]) -> PgPool {
    let grants: String = roles
        .iter()
        .map(|r| format!("GRANT {r} TO {name}; "))
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

const SECRET: &str = "INSERT INTO webhook_secrets (requester, nonce, sealed) \
    VALUES ('alpha', decode(repeat('00', 12), 'hex'), decode(repeat('00', 48), 'hex'))";
const CONDITION_URL: &str = "INSERT INTO webhook_condition_urls (requester, url_nonce, url_sealed) \
    VALUES ('alpha', decode(repeat('00', 12), 'hex'), decode(repeat('00', 20), 'hex'))";

/// PR #55 re-check S-c (`REQ-ID-4`): only the ingress API's login makes a secret or registers a
/// URL for a requester. The core's and an endpoint's logins, which hold `bezent_service` too, are
/// refused, with or without an INSERT grant, so they cannot add a row that becomes the requester's
/// latest and silently stops its webhooks.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_id_4_only_the_ingress_login_registers_secrets_and_urls(pool: PgPool) {
    common::exec(&pool, "INSERT INTO requesters (requester) VALUES ('alpha')").await;
    let core = login(
        &pool,
        "wr_core_for_tests",
        &[bezent_store::SERVICE_ROLE, bezent_store::CORE_ROLE],
    )
    .await;
    let endpoint = login(
        &pool,
        "wr_endpoint_for_tests",
        &[bezent_store::ENDPOINT_ROLE],
    )
    .await;
    let ingress = login(&pool, "wr_ingress_for_tests", &[bezent_store::INGRESS_ROLE]).await;

    // Not granted: refused for privilege.
    for (who, p) in [("core", &core), ("endpoint", &endpoint)] {
        for sql in [SECRET, CONDITION_URL] {
            assert!(sqlx::query(sql).execute(p).await.is_err(), "{who}: {sql}");
        }
    }
    // Granted by mistake: the store still refuses, naming the rule.
    common::exec(
        &pool,
        "GRANT INSERT ON webhook_secrets, webhook_condition_urls, webhook_transfer_urls \
         TO bezent_service",
    )
    .await;
    for (who, p) in [("core", &core), ("endpoint", &endpoint)] {
        for sql in [SECRET, CONDITION_URL] {
            let err = sqlx::query(sql).execute(p).await.unwrap_err();
            assert!(
                err.to_string().contains("only the ingress API"),
                "{who}: {err}"
            );
        }
    }
    common::exec(
        &pool,
        "REVOKE INSERT ON webhook_secrets, webhook_condition_urls, webhook_transfer_urls \
         FROM bezent_service",
    )
    .await;

    // The ingress's login registers both.
    for sql in [SECRET, CONDITION_URL] {
        sqlx::query(sql)
            .execute(&ingress)
            .await
            .unwrap_or_else(|e| panic!("ingress: {sql}: {e}"));
    }
}

/// PR #55 re-check S-a (`REQ-ST-5`): the dispatcher-only rule asks the session's login, not the
/// role a statement runs as. A `SECURITY DEFINER` function owned by the migration owner runs as
/// the owner; called by a service login it must still be refused a delivery write.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_5_a_definer_function_does_not_lend_the_owner_s_standing_to_a_service_login(
    pool: PgPool,
) {
    common::exec(
        &pool,
        "CREATE FUNCTION wr_forge() RETURNS void LANGUAGE sql SECURITY DEFINER AS $$ \
           INSERT INTO webhook_deliveries (requester, seq, dedupe_key, target, transfer_id) \
           VALUES ('alpha', 1, 'log-1', 'transfer', 1) $$; \
         GRANT EXECUTE ON FUNCTION wr_forge() TO bezent_service",
    )
    .await;
    let service = login(&pool, "wr_service_for_tests", &[bezent_store::SERVICE_ROLE]).await;
    // And a plain insert, even with the INSERT granted.
    common::exec(
        &pool,
        "GRANT INSERT ON webhook_deliveries TO bezent_service",
    )
    .await;
    let err = sqlx::query(
        "INSERT INTO webhook_deliveries (requester, seq, dedupe_key, target, transfer_id) \
         VALUES ('alpha', 1, 'log-1', 'transfer', 1)",
    )
    .execute(&service)
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("only the webhook dispatcher"),
        "{err}"
    );
    let err = sqlx::query("SELECT wr_forge()")
        .execute(&service)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("only the webhook dispatcher"),
        "{err}"
    );
}
