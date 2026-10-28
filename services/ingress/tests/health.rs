// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `bezent-ingress health` (issue #61): healthy on a 200, unhealthy on anything else, on nothing
//! listening, and on an instance that never answers, within the bound; and it prints no secret.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bezent_ingress::health::{check, probe, target};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A stand-in for the ingress that answers every request with `reply`.
async fn answering(reply: &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0u8; 256];
            let _ = s.read(&mut buf).await;
            let _ = s.write_all(reply.as_bytes()).await;
        }
    });
    addr
}

#[tokio::test]
async fn a_200_is_healthy() {
    let addr = answering("HTTP/1.1 200 OK\r\ncontent-length: 15\r\n\r\n{\"status\":\"ok\"}").await;
    assert_eq!(probe(addr, Duration::from_secs(2)).await, Ok(()));
}

#[tokio::test]
async fn a_503_is_not_healthy() {
    let addr = answering("HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n").await;
    assert!(probe(addr, Duration::from_secs(2)).await.is_err());
}

#[tokio::test]
async fn nothing_listening_is_not_healthy() {
    let free = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap()
    };
    assert!(probe(free, Duration::from_secs(2)).await.is_err());
}

#[tokio::test]
async fn an_instance_that_never_answers_times_out_within_the_bound() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((s, _)) = listener.accept().await {
            held.push(s);
        }
    });
    let started = Instant::now();
    assert!(probe(addr, Duration::from_millis(300)).await.is_err());
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn an_unspecified_listen_address_is_reached_on_the_loopback() {
    let t = |s: &str| target(s.parse().unwrap()).to_string();
    assert_eq!(t("0.0.0.0:8080"), "127.0.0.1:8080");
    assert_eq!(t("[::]:8080"), "[::1]:8080");
    assert_eq!(t("10.0.0.5:9000"), "10.0.0.5:9000");
}

#[tokio::test]
async fn the_check_reads_the_port_from_the_configuration_and_prints_none_of_it() {
    let addr = answering("HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n").await;
    let dir = std::env::temp_dir().join(format!("bezent-ingress-health-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("ingress.toml");
    std::fs::write(
        &file,
        format!("listen = \"{addr}\"\ndatabase_url = \"postgres://svc:hunter2@db/bezent\"\n"),
    )
    .unwrap();
    assert_eq!(check(&file).await, Ok(()));

    // A file that does not parse: the line it quotes would carry the password.
    std::fs::write(
        &file,
        "listen = \"nope\"\ndatabase_url = \"postgres://svc:hunter2@db/bezent\"\n",
    )
    .unwrap();
    let err = check(&file).await.unwrap_err();
    assert!(
        !err.contains("hunter2") && !err.contains("postgres"),
        "{err}"
    );
    let err = check(&dir.join("missing.toml")).await.unwrap_err();
    assert!(!err.contains("missing"), "{err}");
}
