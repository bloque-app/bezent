// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! A signer that accepts the connection and never answers the hello is an error, not a stall: the
//! caller's dial ends within its bound, before any `Sign` is written. Without the bound, a signer
//! that holds the socket and says nothing would hold the endpoint's pass for good. The TLS end of
//! the same bound is in `tls_link.rs`.

use std::time::{Duration, Instant};

use bezent_signer::link::{Client, DIAL_TIMEOUT, LinkError};
use tokio::net::UnixListener;

#[tokio::test]
async fn a_signer_that_never_answers_the_hello_is_an_error_within_the_bound() {
    let dir = tempfile_dir();
    let socket = dir.join("silent.sock");
    // Accepts every connection and holds it: reads nothing, answers nothing, closes nothing.
    let listener = UnixListener::bind(&socket).unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    let uid = rustix::process::geteuid().as_raw();

    let started = Instant::now();
    // The outer bound is the test's own: without a bound in the dial, this is what fails.
    let answer = tokio::time::timeout(
        Duration::from_secs(10),
        Client::connect_within(&socket, "token", uid, Duration::from_millis(300)),
    )
    .await
    .expect("the dial's own bound ended it: it is not left waiting");
    let error = answer.expect_err("a silent signer does not welcome");
    assert!(
        matches!(&error, LinkError::Io(e) if e.kind() == std::io::ErrorKind::TimedOut),
        "{error:?}"
    );
    // Not before the bound: a connection that simply died would also be an error.
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(280), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn the_dial_is_bound_to_ten_seconds() {
    assert_eq!(DIAL_TIMEOUT, Duration::from_secs(10));
}

fn tempfile_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("bezent-dial-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
