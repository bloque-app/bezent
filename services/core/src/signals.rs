// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Signals that only reach the event log, put in front of a human (`F-014`; `REQ-IC-2`,
//! `REQ-IC-4`).
//!
//! The signer and the endpoints raise `key-compromise` (an unrecorded signature, `AC-I1.2`) and its
//! `pause-requested` by appending to the event log, because the store lets them write nothing
//! else (`F-005` NOTES 5). Nothing wrote a log line for them, so no alarm could match one, and
//! the store applies the pause itself, in the transaction that records the alert (migration 0095,
//! issue #77). This step puts the alert in front of a human: each such alert is logged once as a
//! page, `kind=key-compromise`, which says the pause is applied and what a person still has to do
//! (`deploy/runbooks/pages/key-compromise.md`).
//!
//! The core's own detection of a supply mismatch (`reserve::check`, spec `INV-4`) pages and pauses
//! by itself, through the reserve check's pager, and is not repeated here. Every other alert is,
//! whichever process raised it: the core's lines for its own detections carry a message but no
//! `kind`, which no filter can match.

use serde_json::json;
use sqlx::PgPool;

use crate::store;

/// The event log kind that records an alert has been put in front of a human.
pub const SURFACED: &str = "signal-surfaced";

/// Surfaces every key-compromise alert that has not been surfaced yet and was not paged by the
/// reserve check; returns how many. At least once: an alert is logged before it is marked, so a
/// failure between the two repeats the page rather than losing it.
pub async fn surface(pool: &PgPool) -> sqlx::Result<usize> {
    let pending: Vec<(i64, serde_json::Value)> = sqlx::query_as(
        "SELECT a.seq, a.payload FROM event_log a \
         WHERE a.kind = 'key-compromise' \
           AND coalesce(a.payload ->> 'spec', '') NOT LIKE '%INV-4%' \
           AND NOT EXISTS (SELECT 1 FROM event_log s WHERE s.kind = $1 \
                             AND s.payload ->> 'alert_seq' = a.seq::text) \
         ORDER BY a.seq LIMIT 50",
    )
    .bind(SURFACED)
    .fetch_all(pool)
    .await?;
    let mut surfaced = 0;
    for (seq, alert) in pending {
        let detector = alert
            .get("detector")
            .and_then(|d| d.as_str())
            .unwrap_or("unknown");
        tracing::error!(
            kind = %"key-compromise", alert_seq = seq, detector = %detector,
            "page: a signature bezent did not record was seen, or a key was used where no intent explains it; \
             the store paused the bridge when it recorded the alert (check pauses_in_force); follow the key-compromise runbook: the chain half first, then rotate"
        );
        let mut conn = pool.acquire().await?;
        store::event(
            &mut conn,
            SURFACED,
            None,
            &json!({"spec": "REQ-IC-2, REQ-IC-4", "signal": "key-compromise", "alert_seq": seq}),
        )
        .await?;
        surfaced += 1;
    }
    Ok(surfaced)
}
