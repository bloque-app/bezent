// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The services reach PostgreSQL over TLS, and fail closed (`bezent_store::tls`).
//!
//! These need two servers that the CI `tls` job (and `tools/ci/tls-postgres.sh`, locally) starts: one
//! that forces TLS with a throwaway CA, and one that offers none. They are `#[ignore]`d so that
//! `cargo test --workspace` does not need them; that job runs them with `--ignored`, and a missing
//! variable then fails the test rather than skipping it.
//!
//! - `BEZENT_TLS_TEST_HOSTPORT`: the TLS server, `localhost:<port>` (its certificate is for `localhost`).
//! - `BEZENT_TLS_TEST_CA`: the PEM file of the CA that signed it.
//! - `BEZENT_PLAIN_TEST_HOSTPORT`: a server with TLS off.

use bezent_store::tls;
use sqlx::postgres::PgPoolOptions;

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set; see this file's header"))
}

fn url(hostport: &str, query: &str) -> String {
    format!("postgres://postgres:postgres@{hostport}/postgres?{query}")
}

async fn connect(url: &str) -> Result<sqlx::PgPool, String> {
    tls::connect(PgPoolOptions::new().max_connections(1), url).await
}

async fn is_tls(pool: &sqlx::PgPool) -> bool {
    sqlx::query_scalar("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
        .fetch_one(pool)
        .await
        .expect("pg_stat_ssl")
}

#[tokio::test]
#[ignore = "needs the TLS servers of tools/ci/tls-postgres.sh"]
async fn require_connects_over_tls() {
    let pool = connect(&url(&var("BEZENT_TLS_TEST_HOSTPORT"), "sslmode=require"))
        .await
        .expect("sslmode=require");
    assert!(is_tls(&pool).await, "the session is not encrypted");
}

#[tokio::test]
#[ignore = "needs the TLS servers of tools/ci/tls-postgres.sh"]
async fn verify_full_with_the_ca_bundle_connects() {
    let ca = var("BEZENT_TLS_TEST_CA");
    let pool = connect(&url(
        &var("BEZENT_TLS_TEST_HOSTPORT"),
        &format!("sslmode=verify-full&sslrootcert={ca}"),
    ))
    .await
    .expect("sslmode=verify-full with the CA");
    assert!(is_tls(&pool).await, "the session is not encrypted");
}

#[tokio::test]
#[ignore = "needs the TLS servers of tools/ci/tls-postgres.sh"]
async fn verify_full_without_the_ca_is_refused() {
    // The throwaway CA is in no trust store.
    let e = connect(&url(
        &var("BEZENT_TLS_TEST_HOSTPORT"),
        "sslmode=verify-full",
    ))
    .await
    .expect_err("an unknown CA must not verify");
    assert!(!e.is_empty());
}

#[tokio::test]
#[ignore = "needs the TLS servers of tools/ci/tls-postgres.sh"]
async fn verify_full_checks_the_host_name() {
    // Same server, same CA, reached by an address the certificate does not name.
    let ca = var("BEZENT_TLS_TEST_CA");
    let port = var("BEZENT_TLS_TEST_HOSTPORT")
        .rsplit(':')
        .next()
        .expect("a port")
        .to_owned();
    connect(&url(
        &format!("127.0.0.1:{port}"),
        &format!("sslmode=verify-full&sslrootcert={ca}"),
    ))
    .await
    .expect_err("a certificate for localhost must not verify for 127.0.0.1");
}

#[tokio::test]
#[ignore = "needs the TLS servers of tools/ci/tls-postgres.sh"]
async fn a_server_without_tls_is_refused_when_tls_is_asked_for() {
    let plain = var("BEZENT_PLAIN_TEST_HOSTPORT");
    let ca = var("BEZENT_TLS_TEST_CA");
    // Sanity: the server is up and does not offer TLS.
    let pool = connect(&url(&plain, "sslmode=disable"))
        .await
        .expect("sslmode=disable on the plain server");
    assert!(!is_tls(&pool).await);
    for query in [
        "sslmode=require".to_owned(),
        format!("sslmode=verify-ca&sslrootcert={ca}"),
        format!("sslmode=verify-full&sslrootcert={ca}"),
    ] {
        connect(&url(&plain, &query)).await.expect_err(&format!(
            "{query} must fail closed against a server with no TLS"
        ));
    }
}
