// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #84: the `statement_timeout_ms` the ingress is configured with reaches its store session, so
//! an operator can raise (or lower) the bound without a release. Real PostgreSQL, through
//! [`Config::pool_options`], the function the service's `main` makes its pool with.

use bezent_ingress::Config;
use sqlx::PgPool;
use sqlx::error::DatabaseError;

/// SQLSTATE `query_canceled`: what `statement_timeout` raises.
const QUERY_CANCELED: &str = "57014";

/// The statement bound a session of `config`'s pool reports, on the test's database.
async fn session_bound(config: &Config, base: &PgPool) -> String {
    let pool = config
        .pool_options()
        .connect_with((*base.connect_options()).clone())
        .await
        .unwrap();
    sqlx::query_scalar("SELECT current_setting('statement_timeout')")
        .fetch_one(&pool)
        .await
        .unwrap()
}

fn config(ms: Option<u64>) -> Config {
    let key = ms.map_or(String::new(), |ms| format!("statement_timeout_ms = {ms}\n"));
    toml::from_str(&format!(
        "listen = \"127.0.0.1:8080\"\ndatabase_url = \"postgres://svc@db/bezent\"\n{key}"
    ))
    .unwrap()
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_84_the_ingress_statement_bound_is_the_configured_one(base: PgPool) {
    // Left out, the default: thirty seconds, shorter than the others' because a request waits on it.
    assert_eq!(session_bound(&config(None), &base).await, "30s");
    // Configured, the session has exactly that.
    assert_eq!(session_bound(&config(Some(7_321)), &base).await, "7321ms");
    // And it does what a bound does: a statement that outlasts it ends with 57014.
    let pool = config(Some(400))
        .pool_options()
        .connect_with((*base.connect_options()).clone())
        .await
        .unwrap();
    let e = sqlx::query("SELECT pg_sleep(30)")
        .execute(&pool)
        .await
        .unwrap_err();
    let code = e.as_database_error().and_then(DatabaseError::code);
    assert_eq!(code.as_deref(), Some(QUERY_CANCELED), "{e}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_84_the_webhook_dispatcher_pool_has_the_configured_statement_bound(base: PgPool) {
    // `main` makes the dispatcher's pool with this function; it used to take the store's fixed 60 s.
    let pool = config(Some(400))
        .dispatcher_pool_options()
        .connect_with((*base.connect_options()).clone())
        .await
        .unwrap();
    let bound: String = sqlx::query_scalar("SELECT current_setting('statement_timeout')")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(bound, "400ms");
    assert_eq!(pool.options().get_max_connections(), 2);
    let e = sqlx::query("SELECT pg_sleep(30)")
        .execute(&pool)
        .await
        .unwrap_err();
    let code = e.as_database_error().and_then(DatabaseError::code);
    assert_eq!(code.as_deref(), Some(QUERY_CANCELED), "{e}");
}
