// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Signals the endpoint must never lose (F-007 review S3, kept for F-009; `AC-I1.2`, `REQ-IC-4`).
//!
//! A compromise signal (the delivery key used for something bezent did not record) and an anomaly
//! (bytes that do not answer what the store says they answer) are written to the event log, in
//! the store, before anything that depends on them is recorded, and never only to a log line or a
//! channel. A failed write fails the step, which records nothing and is repeated: the signal is
//! written on a later look, never dropped. Each is written once: a repeated look finds it there.

use bezent_signer::compromise::{self, ObservedSignature, Verdict};
use serde_json::{Value, json};
use sqlx::PgPool;

/// The event log kind of an anomaly: bytes or rows that do not answer what they should. It holds
/// the delivery for review; it is not a pause.
pub const ANOMALY: &str = "delivery-anomaly";

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

/// What a signature of the delivery key seen on chain is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seen {
    /// It verifies over this recorded intent's bytes.
    Recorded {
        /// The intent.
        intent_id: i64,
    },
    /// It answers no recorded intent: the compromise path is in the event log.
    Compromise,
}

/// Matches a signature seen on chain against the recorded intents (`compromise::check`); one
/// that answers none starts the compromise path, once per coordinate.
pub async fn check_signature(
    pool: &PgPool,
    observed: &ObservedSignature,
) -> Result<Seen, sqlx::Error> {
    // This endpoint's own alert for the coordinate, by its detection key (0049): a row another
    // role wrote never stands in for it.
    let raised = compromise::raised(
        pool,
        compromise::DETECTOR_EVM_ENDPOINT,
        &format!(
            "coordinate:{}:{}:{}",
            observed.network,
            hex0x(&observed.block_hash),
            observed.position_index
        ),
    )
    .await?;
    if raised {
        return Ok(Seen::Compromise);
    }
    Ok(match compromise::check(pool, observed).await? {
        Verdict::Recorded { intent_id } => Seen::Recorded { intent_id },
        Verdict::Compromise {
            alert_seq,
            pause_seq,
        } => {
            tracing::error!(
                network = observed.network,
                block = observed.block_number,
                index = observed.position_index,
                alert_seq,
                pause_seq,
                "AC-I1.2: the delivery key signed a transaction bezent did not record; key-compromise and a global pause request are in the event log"
            );
            Seen::Compromise
        }
    })
}

/// Starts the compromise path for a use of the key no signature can be matched for (a transaction
/// of the delivery key that is not an EIP-1559 one, or a nonce consumed with no transaction of the
/// key in its block), once per `dedupe`.
pub async fn raise_once(pool: &PgPool, dedupe: &str, mut alert: Value) -> Result<(), sqlx::Error> {
    if compromise::raised(
        pool,
        compromise::DETECTOR_EVM_ENDPOINT,
        &format!("dedupe:{dedupe}"),
    )
    .await?
    {
        return Ok(());
    }
    alert["dedupe"] = json!(dedupe);
    alert["spec"] = json!("AC-I1.2, REQ-IC-4");
    alert["actions"] = json!(["pause", "rotate", "report"]);
    let (alert_seq, pause_seq) = compromise::raise(pool, &alert).await?;
    tracing::error!(dedupe, alert_seq, pause_seq, %alert, "AC-I1.2: the delivery key was used where bezent recorded nothing; key-compromise and a global pause request are in the event log");
    Ok(())
}

/// Records an anomaly ([`ANOMALY`]), once per `dedupe`.
pub async fn anomaly_once(
    pool: &PgPool,
    dedupe: &str,
    mut detail: Value,
) -> Result<(), sqlx::Error> {
    detail["dedupe"] = json!(dedupe);
    let seq: Option<i64> = sqlx::query_scalar(
        "INSERT INTO event_log (kind, payload) SELECT $1, $2::text::jsonb \
         WHERE NOT EXISTS (SELECT 1 FROM event_log WHERE kind = $1 AND payload ->> 'dedupe' = $3) \
         RETURNING seq",
    )
    .bind(ANOMALY)
    .bind(detail.to_string())
    .bind(dedupe)
    .fetch_optional(pool)
    .await?;
    if let Some(seq) = seq {
        tracing::error!(dedupe, seq, %detail, "a delivery anomaly is in the event log");
    }
    Ok(())
}
