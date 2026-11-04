// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! A provider that accepts the connection and never answers is an error, not a stall: alloy's
//! client sets no timeout of its own, so `rpc::Http` sets one (`rpc::REQUEST_TIMEOUT`). A hung
//! call would otherwise hold the follower's head and the reserve path for good.

use std::net::TcpListener;
use std::time::{Duration, Instant};

use bezent_evm_endpoint::rpc::{Http, Rpc};

/// An address that accepts every connection and reads nothing, answers nothing, closes nothing.
fn silent_provider() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            held.push(stream);
        }
    });
    url
}

#[tokio::test]
async fn a_provider_that_never_answers_is_an_error_within_the_timeout() {
    let provider = Http::with_timeout("silent", &silent_provider(), Duration::from_millis(300))
        .expect("a valid URL");
    let started = Instant::now();
    // The outer bound is the test's own: without a timeout in the client, this is what fails.
    let answer = tokio::time::timeout(Duration::from_secs(10), provider.chain_id())
        .await
        .expect("the client's own timeout ended the request: it is not left waiting");
    let error = answer.expect_err("a silent provider gives no chain id");
    assert!(
        error.to_string().starts_with("silent: eth_chainId:"),
        "{error}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}
