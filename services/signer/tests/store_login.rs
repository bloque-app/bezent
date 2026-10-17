// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Review S3, #46: the signer's start-up check (the store's shared one, which every service runs)
//! proves that the store login it actually uses is a member of `bezent_signer` and of nothing
//! else, and can act as nothing else (`REQ-AR-3`). A login that owns the schema, can become
//! another role, or holds a privilege of its own, is refused.

use std::sync::atomic::{AtomicU32, Ordering};

use bezent_signer::signer::{check_login, pool_options};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

const INSUFFICIENT_PRIVILEGE: &str = "42501";

/// A test-only password for the login roles these tests create and drop.
const PASSWORD: &str = "bezent-signer-login-test";

/// Login roles are shared by every database on the server: each name is unique to the test run.
fn unique(tag: &str) -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!(
        "bzs_{tag}_{}_{}_{}",
        std::process::id(),
        nanos,
        N.fetch_add(1, Ordering::Relaxed)
    )
}

async fn exec(pool: &PgPool, sql: &str) {
    sqlx::raw_sql(sql)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// Creates login `tag` with `attributes`, and runs `setup` (with `{name}` replaced) as the owner.
async fn login(admin: &PgPool, tag: &str, attributes: &str, setup: &str) -> String {
    let name = unique(tag);
    exec(
        admin,
        &format!("CREATE ROLE {name} LOGIN PASSWORD '{PASSWORD}' {attributes}"),
    )
    .await;
    // Held without SET (review S1 of PR #56): the login never acts as its role.
    exec(
        admin,
        &format!("GRANT bezent_signer TO {name} WITH INHERIT TRUE, SET FALSE"),
    )
    .await;
    if !setup.is_empty() {
        exec(admin, &setup.replace("{name}", &name)).await;
    }
    name
}

async fn signer_pool(connect: &PgConnectOptions, name: &str) -> PgPool {
    pool_options()
        .connect_with(connect.clone().username(name).password(PASSWORD))
        .await
        .unwrap_or_else(|e| panic!("connect as {name}: {e}"))
}

async fn verdict(connect: &PgConnectOptions, name: &str) -> Result<(), String> {
    let pool = signer_pool(connect, name).await;
    let verdict = check_login(&pool).await;
    pool.close().await;
    verdict
}

async fn drop_login(admin: &PgPool, name: &str) {
    exec(admin, &format!("DROP OWNED BY {name}; DROP ROLE {name}")).await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ar_3_signer_login_acts_as_the_signer_role_and_cannot_switch(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let admin = pool.connect_with(connect.clone()).await.unwrap();
    let name = login(&admin, "signer", "", "").await;
    verdict(&connect, &name)
        .await
        .expect("a login that is a member of bezent_signer only is accepted");

    // And it cannot reach anything with more: resetting its role leaves the login as it was, and
    // no switch leads anywhere else. The refusals under test run on a pool of their own, closed
    // below (#27).
    let signer = signer_pool(&connect, &name).await;
    let mut conn = signer.acquire().await.unwrap();
    for sql in [
        // role-switch-ok
        "RESET ROLE; INSERT INTO intents (network, purpose, payload) VALUES ('n', 'mint', '{}')",
        // role-switch-ok
        "RESET ROLE; ALTER TABLE signatures DISABLE TRIGGER ALL",
        // role-switch-ok
        "RESET ROLE; UPDATE intents SET payload = '{}'",
        // role-switch-ok
        "SET ROLE bezent_service",
        // role-switch-ok
        "SET ROLE bezent_core",
    ] {
        let err = sqlx::raw_sql(sql)
            .execute(&mut *conn)
            .await
            .expect_err(&format!("the signer login ran: {sql}"));
        let code = err
            .as_database_error()
            .and_then(|e| e.code())
            .map(|c| c.into_owned());
        assert_eq!(
            code.as_deref(),
            Some(INSUFFICIENT_PRIVILEGE),
            "{sql}: {err}"
        );
    }
    drop(conn);
    signer.close().await;
    drop_login(&admin, &name).await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ar_3_signer_refuses_a_login_that_can_act_as_more_than_the_signer(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let admin = pool.connect_with(connect.clone()).await.unwrap();
    for (tag, attributes, setup, expected) in [
        // The reviewer's case: the migration owner, no superuser, granted bezent_signer.
        (
            "owner",
            "",
            "CREATE TABLE {name}_t (x int); ALTER TABLE {name}_t OWNER TO {name}",
            "owns objects",
        ),
        // A login that is also the coordinator.
        (
            "service",
            "",
            "GRANT bezent_service TO {name}",
            "is a member of",
        ),
        ("core", "", "GRANT bezent_core TO {name}", "is a member of"),
        // Its own role with SET, or with ADMIN OPTION (review S1, M2 of PR #56).
        (
            "set",
            "",
            "GRANT bezent_signer TO {name} WITH INHERIT TRUE, SET TRUE",
            "can switch to",
        ),
        (
            "admin",
            "",
            "GRANT bezent_signer TO {name} WITH ADMIN OPTION",
            "ADMIN OPTION",
        ),
        // A login that may create roles, and one that may create databases.
        ("createrole", "CREATEROLE", "", "create roles"),
        ("createdb", "CREATEDB", "", "create roles or databases"),
        // A login with a privilege of its own.
        ("grant", "", "GRANT UPDATE ON intents TO {name}", "holds"),
    ] {
        let name = login(&admin, tag, attributes, setup).await;
        let err = verdict(&connect, &name)
            .await
            .expect_err(&format!("the {tag} login was accepted"));
        assert!(
            err.contains("REQ-AR-3") && err.contains(expected),
            "{tag}: {err}"
        );
        drop_login(&admin, &name).await;
    }
}
