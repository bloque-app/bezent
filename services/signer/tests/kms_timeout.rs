// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! A KMS that accepts the request and never answers is an error, not a stall: the AWS SDK sets no
//! read, attempt or operation timeout of its own (only a 3.1 s connect timeout), so `AwsKms` sets
//! them (`kms::timeouts`). A hung `Sign` would otherwise hold the signer's one signing lock, and
//! every intent with it. The error is `Unavailable`: no answer, nothing signed, nothing recorded.

use std::net::TcpListener;
use std::time::{Duration, Instant};

use aws_config::timeout::TimeoutConfig;
use bezent_signer::keys::KeyError;
use bezent_signer::kms::{ATTEMPT_TIMEOUT, AwsKms, KmsApi, OPERATION_TIMEOUT, timeouts};

/// An address that accepts every connection and reads nothing, answers nothing, closes nothing.
fn silent_endpoint() -> String {
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
async fn a_kms_that_never_answers_is_unavailable_within_the_timeout() {
    let bound = TimeoutConfig::builder()
        .operation_timeout(Duration::from_millis(900))
        .operation_attempt_timeout(Duration::from_millis(300))
        .build();
    let credentials = aws_sdk_kms::config::Credentials::new("id", "secret", None, None, "test");
    let kms = AwsKms::with_credentials("us-east-2", &silent_endpoint(), credentials, bound).await;

    let started = Instant::now();
    // The outer bound is the test's own: without a timeout in the client, this is what fails.
    let answer = tokio::time::timeout(
        Duration::from_secs(20),
        kms.sign_digest("arn:aws:kms:us-east-2:000000000000:key/test", [7; 32]),
    )
    .await
    .expect("the client's own timeout ended the request: it is not left waiting");
    match answer {
        Err(KeyError::Unavailable(text)) => {
            assert!(text.contains("KMS Sign failed"), "{text}");
        }
        other => panic!("a silent KMS is unavailable, not {other:?}"),
    }
    // Not before one attempt's bound: a connection that simply died would also be an error. Only
    // the upper bound is tight: the retries' backoff is jittered.
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(280), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
}

#[test]
fn the_default_client_is_bound_to_fifteen_seconds_and_five_an_attempt() {
    assert_eq!(OPERATION_TIMEOUT, Duration::from_secs(15));
    assert_eq!(ATTEMPT_TIMEOUT, Duration::from_secs(5));
    let t = timeouts();
    assert_eq!(t.operation_timeout(), Some(OPERATION_TIMEOUT));
    assert_eq!(t.operation_attempt_timeout(), Some(ATTEMPT_TIMEOUT));
}
