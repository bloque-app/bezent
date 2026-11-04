// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-014 · `REQ-IC-4`, `REQ-IC-2`: a key-compromise alert the signer or an endpoint wrote to the
//! event log is put in front of a human, once, as a page an alarm can match. Nothing here pauses
//! (the pause request has no consumer yet; `features/F-014-operations/NOTES.md`).

mod common;

use bezent_core_service::signals;
use common::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ic_4_an_alert_in_the_log_becomes_one_page_with_kind_unquoted(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let log = PageLog::start();
    // As an endpoint raises it: the alert, then the request to pause. And one the reserve check
    // raised for a supply mismatch, which it pages itself.
    exec(
        &w.admin,
        "INSERT INTO event_log (kind, payload) VALUES \
           ('key-compromise', '{\"detector\": \"evm-endpoint\", \"detection_key\": \"dedupe:k1\", \"dedupe\": \"k1\", \"spec\": \"AC-I1.2, REQ-IC-4\"}'), \
           ('pause-requested', '{\"scope\": \"global\", \"alert_seq\": 1}'), \
           ('key-compromise', '{\"detector\": \"core\", \"detection_key\": \"dedupe:m1\", \"dedupe\": \"m1\", \"spec\": \"INV-4, REQ-IC-4, REQ-IC-2\"}')",
    )
    .await;
    assert_eq!(signals::surface(w.core.pool()).await.unwrap(), 1);
    // Once: marked in the log, so the next cycle repeats nothing.
    assert_eq!(signals::surface(w.core.pool()).await.unwrap(), 0);
    let out = log.text();
    assert_page(&log, "key-compromise", 1);
    assert!(out.contains("ERROR"), "{out}");
    assert!(out.contains("detector=evm-endpoint"), "{out}");
    assert!(out.contains("nothing has paused yet"), "{out}");
    assert_eq!(
        w.count("SELECT count(*) FROM event_log WHERE kind = 'signal-surfaced'")
            .await,
        1
    );
    assert_eq!(
        w.count("SELECT count(*) FROM pauses").await,
        0,
        "this step pauses nothing"
    );
}
