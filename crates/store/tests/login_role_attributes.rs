// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Review S1 of PR #56: the start-up check looked at the login's own attributes only, and let the
//! login `SET ROLE` into its role. With `ALTER ROLE bezent_core CREATEROLE`, the core's login passed
//! its check and then ran `SET ROLE bezent_core; CREATE ROLE …`.
//!
//! This test alters a store role, which is server-wide, so it is the only test of its binary
//! (`cargo test` runs one test binary at a time), and it puts the role back before it asserts.

mod common;

use bezent_store::login::{as_login, check_login, ensure_login, pool_options};
use sqlx::PgPool;

const PASSWORD: &str = "test-only-role-attributes-password";
const LOGIN: &str = "logins_s1_core_for_tests";

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s1_a_login_that_can_act_as_or_reaches_a_role_with_createrole_is_refused(pool: PgPool) {
    ensure_login(&pool, LOGIN, bezent_store::CORE_ROLE, PASSWORD)
        .await
        .unwrap();
    let core = pool_options()
        .max_connections(1)
        .connect_with(as_login(&pool.connect_options(), LOGIN, PASSWORD))
        .await
        .unwrap();
    check_login(&core, bezent_store::CORE_ROLE).await.unwrap();

    // Held WITH SET FALSE: the login cannot act as its role at all.
    let mut tx = core.begin().await.unwrap();
    let err = sqlx::raw_sql("SET LOCAL ROLE bezent_core")
        .execute(&mut *tx)
        .await
        .expect_err("the login switched to its role");
    assert_eq!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("42501"),
        "{err}"
    );
    tx.rollback().await.unwrap();

    // A membership with SET is refused.
    common::exec(
        &pool,
        &format!("GRANT bezent_core TO {LOGIN} WITH INHERIT TRUE, SET TRUE"),
    )
    .await;
    let with_set = check_login(&core, bezent_store::CORE_ROLE).await;
    ensure_login(&pool, LOGIN, bezent_store::CORE_ROLE, PASSWORD)
        .await
        .unwrap();
    let err = with_set.expect_err("a login that can switch to its role was accepted");
    assert!(err.contains("can switch to"), "{err}");
    check_login(&core, bezent_store::CORE_ROLE).await.unwrap();

    // The reviewer's case: the role itself may create roles. Put back before asserting, so a
    // failure never leaves the server's role widened.
    common::exec(&pool, "ALTER ROLE bezent_core CREATEROLE").await;
    let verdict = check_login(&core, bezent_store::CORE_ROLE).await;
    common::exec(&pool, "ALTER ROLE bezent_core NOCREATEROLE").await;
    let err = verdict.expect_err("a login reaching a CREATEROLE role was accepted");
    assert!(
        err.contains("bezent_core") && err.contains("create roles"),
        "{err}"
    );
    check_login(&core, bezent_store::CORE_ROLE).await.unwrap();
    core.close().await;
}
