// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #84: the `statement_timeout_ms` the signer is configured with reaches its store session, so
//! an operator can raise (or lower) the bound without a release. Real PostgreSQL, through
//! [`Config::pool_options`], the function the service's `main` makes its pool with.

use bezent_signer::config::Config;
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
    let mut json: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/signer.json")).unwrap();
    if let Some(ms) = ms {
        json["statement_timeout_ms"] = ms.into();
    }
    Config::parse(json.to_string().as_bytes()).unwrap()
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_84_the_signers_statement_bound_is_the_configured_one(base: PgPool) {
    // Left out, the default: one minute.
    assert_eq!(session_bound(&config(None), &base).await, "1min");
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
