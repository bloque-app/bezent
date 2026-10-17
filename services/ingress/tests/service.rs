// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! T-002-01: the service starts against the store, answers, and shuts down gracefully; the
//! committed OpenAPI document is the one the code generates.

mod common;

use bezent_ingress::{App, Limits, run};
use common::*;
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn t_002_01_the_service_starts_against_the_store_and_shuts_down_gracefully(pool: PgPool) {
    seed(&pool).await;
    let app = App::new(
        service_pool(&pool).await,
        Limits {
            max_in_flight: 4,
            log_page: 10,
        },
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(run(listener, app, async {
        let _ = stopped.await;
    }));

    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: ingress\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("\"status\":\"ok\""), "{response}");

    stop.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .expect("the service stops")
        .unwrap()
        .expect("it stops cleanly");
}

/// #46: the ingress connects only as its own login, a member of `bezent_ingress` and of nothing
/// else; the webhook dispatcher's login, or a login holding a privilege of its own, is refused
/// before it serves.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_46_the_ingress_starts_only_on_its_own_login(pool: PgPool) {
    seed(&pool).await;
    ingress_pool(&pool).await.close().await;
    dispatcher_pool(&pool).await.close().await;
    let o = pool.connect_options();
    let config = |role: &str| -> bezent_ingress::Config {
        let url = format!(
            "postgres://ingress_tests_{role}:{LOGIN_PASSWORD}@{}:{}/{}",
            o.get_host(),
            o.get_port(),
            o.get_database().unwrap()
        );
        toml::from_str(&format!(
            "listen = \"127.0.0.1:0\"\ndatabase_url = \"{url}\"\nmax_connections = 2\n"
        ))
        .unwrap()
    };
    let ingress = bezent_ingress::connect(&config(bezent_store::INGRESS_ROLE))
        .await
        .expect("the ingress's own login");
    ingress.close().await;
    let err = bezent_ingress::connect(&config(bezent_store::WEBHOOKS_ROLE))
        .await
        .expect_err("the dispatcher's login was accepted");
    assert!(
        err.contains("#46") && err.contains("bezent_ingress"),
        "{err}"
    );
    // A grant of its own, on this database only.
    exec(
        &pool,
        "GRANT SELECT ON signatures TO ingress_tests_bezent_ingress",
    )
    .await;
    let err = bezent_ingress::connect(&config(bezent_store::INGRESS_ROLE))
        .await
        .expect_err("a login with a grant of its own was accepted");
    assert!(err.contains("signatures"), "{err}");
}

#[test]
fn t_002_01_the_committed_openapi_document_is_generated_from_the_code() {
    let committed = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/openapi.json"))
        .expect("services/ingress/openapi.json is committed");
    assert!(
        committed == bezent_ingress::openapi::document(),
        "services/ingress/openapi.json is stale: regenerate it with \
         `cargo run -p bezent-ingress -- openapi > services/ingress/openapi.json`"
    );
}
