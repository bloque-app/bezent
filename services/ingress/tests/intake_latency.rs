// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! T-002-06: intake latency, measured inside bezent and reported against `NFR-1` (250 ms at p99).
//! Reported, not gated: the figure depends on the machine and its store. Run with
//! `cargo test -p bezent-ingress --test intake_latency -- --nocapture` to see it.
//!
//! Each sample is one deposit request, from the router receiving it to its answer, through
//! authentication, the durable intake, the decision and the commit: the whole of "take in durably
//! and answer" (`REQ-RQ-1`). No network hop is in it.

mod common;

use common::*;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::{Duration, Instant};

const SEQUENTIAL: i64 = 300;
const CONCURRENT_TASKS: i64 = 8;
const PER_TASK: i64 = 50;

fn percentile(sorted: &[Duration], p: usize) -> Duration {
    let rank = (sorted.len() * p).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn report(label: &str, mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    let p99 = percentile(&samples, 99);
    println!(
        "NFR-1 intake latency, {label}: n={} p50={:?} p95={:?} p99={:?} max={:?} (target: p99 <= 250ms)",
        samples.len(),
        percentile(&samples, 50),
        percentile(&samples, 95),
        p99,
        samples[samples.len() - 1],
    );
    p99
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn nfr_1_intake_latency_is_measured_and_reported(pool: PgPool) {
    let ingress = Arc::new(Ingress::new(&pool).await);
    exec(&pool, "UPDATE routes SET capacity = 100000, window_limit = 340282366920938463463374607431768211455").await;
    let total = SEQUENTIAL + CONCURRENT_TASKS * PER_TASK;
    for block in 1..=total {
        arrival(&pool, block, 1_000).await;
    }

    let mut sequential = Vec::new();
    for block in 1..=SEQUENTIAL {
        let body = deposit_body(&format!("seq-{block}"), coordinate(block));
        let start = Instant::now();
        let (status, answer) = ingress.post("/v0/deposits", ALPHA, &body).await;
        sequential.push(start.elapsed());
        assert_eq!(answer["state"], "Accepted", "{status} {answer}");
    }
    report("sequential", sequential);

    let mut tasks = Vec::new();
    for task in 0..CONCURRENT_TASKS {
        let ingress = Arc::clone(&ingress);
        tasks.push(tokio::spawn(async move {
            let mut samples = Vec::new();
            for i in 0..PER_TASK {
                let block = SEQUENTIAL + task * PER_TASK + i + 1;
                let body = deposit_body(&format!("con-{block}"), coordinate(block));
                let start = Instant::now();
                let (_, answer) = ingress.post("/v0/deposits", ALPHA, &body).await;
                samples.push(start.elapsed());
                assert_eq!(answer["state"], "Accepted", "{answer}");
            }
            samples
        }));
    }
    let mut concurrent = Vec::new();
    for t in tasks {
        concurrent.extend(t.await.unwrap());
    }
    report(
        &format!("{CONCURRENT_TASKS} concurrent requesters"),
        concurrent,
    );
    assert_eq!(count(&pool, "transfers").await, total);
}
