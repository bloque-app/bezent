// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The compromise path (`T-005-05`; `AC-I1.2`, `REQ-IC-4`, `REQ-IC-2`).
//!
//! An endpoint that observes a transaction signed by one of bezent's keys hands the signature to
//! [`check`], which runs in the core, as the service role, never in the signer (the signer writes
//! nothing but signatures). A signature that is bezent's own answers a recorded intent. One that
//! answers none was made outside the signer, or by a signer that no longer signs only what the
//! core recorded: a key compromise. [`check`] then appends, in one transaction, an alert record
//! (`key-compromise`, with the key, the signature and where it was seen) and a pause request
//! (`pause-requested`, global) to the event log, for the operator and the pause path to act on:
//! pause, rotate, report (`REQ-IC-4`).
//!
//! A signature is bezent's own only if it **verifies**, with the key's public key, over the exact
//! bytes (`message`) of an intent the core recorded (`INV-9`; review M1). The `signatures` row the
//! signer wrote only points at the candidate intent: the signer is what this path must catch, so a
//! row it wrote proves nothing by itself. Candidates are found by the public key and the first 64
//! bytes (`r ‖ s` for secp256k1, low-`s` normalised; the whole signature for sr25519), since an EVM
//! transaction writes the recovery byte differently (`27`/`28`, or an EIP-155 or typed-transaction
//! parity).
//!
//! The core makes the same check when the signer returns a signature, before it treats it as
//! usable ([`returned`], called by [`crate::link::Client::sign`]): a signature that is not exactly
//! the named key's over the requested intent's bytes starts the same compromise path.

use serde_json::json;
use sqlx::PgPool;

use crate::verify::{canonical_prefix, verifies, verifies_exactly};

/// A signature seen on chain, by a key bezent holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedSignature {
    /// The network it was seen on.
    pub network: String,
    /// The signing key's public key, as the signer records it.
    pub public_key: Vec<u8>,
    /// The signature as it appears on chain.
    pub signature: Vec<u8>,
    /// Where it was seen: the block number.
    pub block_number: i64,
    /// Where it was seen: the block hash.
    pub block_hash: Vec<u8>,
    /// Where it was seen: the transaction's index in the block.
    pub position_index: i64,
}

/// What a signature seen on chain is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// bezent's own: it verifies over this recorded intent's bytes.
    Recorded {
        /// The intent.
        intent_id: i64,
    },
    /// Made outside the signer, or over bytes no intent recorded. The alert and the pause request
    /// are recorded.
    Compromise {
        /// The event log position of the alert record.
        alert_seq: i64,
        /// The event log position of the pause request.
        pause_seq: i64,
    },
}

/// Matches `observed` against the recorded intents and, if it verifies over none, starts the
/// compromise path. `pool` connects as the service role.
pub async fn check(pool: &PgPool, observed: &ObservedSignature) -> Result<Verdict, sqlx::Error> {
    // Every signature bezent makes is at least 64 bytes; a shorter one is not bezent's.
    let candidates: Vec<(i64, String, Option<String>)> =
        match canonical_prefix(&observed.public_key, &observed.signature) {
            Some(prefix) => {
                sqlx::query_as(
                    "SELECT s.intent_id, s.scheme, i.payload ->> 'message' \
                     FROM signatures s JOIN intents i ON i.id = s.intent_id \
                     WHERE s.public_key = $1 AND substring(s.signature FROM 1 FOR 64) = $2 \
                     ORDER BY s.intent_id",
                )
                .bind(&observed.public_key)
                .bind(prefix)
                .fetch_all(pool)
                .await?
            }
            None => Vec::new(),
        };
    for (intent_id, scheme, message) in &candidates {
        let message = message.as_deref().and_then(decode_hex);
        if message.is_some_and(|m| verifies(scheme, &observed.public_key, &m, &observed.signature))
        {
            return Ok(Verdict::Recorded {
                intent_id: *intent_id,
            });
        }
    }

    let alert = json!({
        "spec": "AC-I1.2, REQ-IC-4",
        "network": observed.network,
        "public_key": format!("0x{}", hex::encode(&observed.public_key)),
        "signature": format!("0x{}", hex::encode(&observed.signature)),
        "coordinate": {
            "network": observed.network,
            "block_number": observed.block_number,
            "block_hash": format!("0x{}", hex::encode(&observed.block_hash)),
            "position_kind": "transaction",
            "position_index": observed.position_index,
        },
        // Recorded signatures that name these bytes, over intents whose bytes they do not sign.
        "unverified_records": candidates.iter().map(|c| c.0).collect::<Vec<_>>(),
        "actions": ["pause", "rotate", "report"],
    });
    let (alert_seq, pause_seq) = raise(pool, &alert).await?;
    Ok(Verdict::Compromise {
        alert_seq,
        pause_seq,
    })
}

/// What a signature the signer returned is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Returned {
    /// It is exactly the named key's signature over the requested intent's recorded bytes: the
    /// core may use it.
    Verified,
    /// It is not. The alert and the pause request are recorded; the core must not use it.
    Compromise {
        /// The event log position of the alert record.
        alert_seq: i64,
        /// The event log position of the pause request.
        pause_seq: i64,
    },
}

/// A signature the signer returned, as the link carries it.
#[derive(Debug, Clone, Copy)]
pub struct Answer<'a> {
    /// The intent the signer says it signed.
    pub intent: i64,
    /// The key id.
    pub key: &'a str,
    /// The scheme.
    pub scheme: &'a str,
    /// The public key, `0x` hex.
    pub public_key: &'a str,
    /// The signature, `0x` hex.
    pub signature: &'a str,
}

/// Checks a signature the signer returned for intent `requested` against that intent's recorded
/// bytes (`INV-9`; review M1). Anything but an exact signature, by the named public key, over those
/// bytes starts the compromise path. `pool` connects as the service role.
pub async fn returned(
    pool: &PgPool,
    requested: i64,
    answer: Answer<'_>,
) -> Result<Returned, sqlx::Error> {
    let message: Option<Option<String>> =
        sqlx::query_scalar("SELECT payload ->> 'message' FROM intents WHERE id = $1")
            .bind(requested)
            .fetch_optional(pool)
            .await?;
    let message = message.flatten().as_deref().and_then(decode_hex);
    let verified = answer.intent == requested
        && match (
            message,
            decode_hex(answer.public_key),
            decode_hex(answer.signature),
        ) {
            (Some(message), Some(public_key), Some(signature)) => {
                verifies_exactly(answer.scheme, &public_key, &message, &signature)
            }
            _ => false,
        };
    if verified {
        return Ok(Returned::Verified);
    }
    let alert = json!({
        "spec": "AC-I1.2, REQ-IC-4, INV-9",
        "source": "signer-link",
        "reason": "the signer returned a signature that is not over the recorded intent's bytes",
        "requested_intent": requested,
        "answered_intent": answer.intent,
        "key": answer.key,
        "scheme": answer.scheme,
        "public_key": answer.public_key,
        "signature": answer.signature,
        "actions": ["pause", "rotate", "report"],
    });
    let (alert_seq, pause_seq) = raise(pool, &alert).await?;
    Ok(Returned::Compromise {
        alert_seq,
        pause_seq,
    })
}

/// `0x` hex, at least one byte.
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let digits = text.strip_prefix("0x")?;
    hex::decode(digits).ok().filter(|b| !b.is_empty())
}

/// The detector the store stamps on an alert the core writes (migration 0049; audit of PR #56).
pub const DETECTOR_CORE: &str = "core";
/// The detector the store stamps on an alert the Kreivo endpoint writes.
pub const DETECTOR_KREIVO_ENDPOINT: &str = "kreivo-endpoint";
/// The detector the store stamps on an alert the EVM endpoint writes.
pub const DETECTOR_EVM_ENDPOINT: &str = "evm-endpoint";
/// The detector the store stamps on an alert the signer writes (audit must-fix 2).
pub const DETECTOR_SIGNER: &str = "signer";

/// The detection key the store derives for `alert` (migration 0049), the same way: its `dedupe`,
/// else its `requested_intent`, else the coordinate it was seen at. `None` when it names none: the
/// store refuses such an alert from any service.
pub fn detection_key(alert: &serde_json::Value) -> Option<String> {
    let text = |v: &serde_json::Value| match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if let Some(d) = alert.get("dedupe").and_then(serde_json::Value::as_str) {
        return Some(format!("dedupe:{d}"));
    }
    if let Some(i) = alert.get("requested_intent").filter(|v| v.is_number()) {
        return Some(format!("intent:{i}"));
    }
    let c = alert.get("coordinate").filter(|c| c.is_object())?;
    let (Some(network), Some(hash), Some(index)) = (
        c.get("network").and_then(serde_json::Value::as_str),
        c.get("block_hash").and_then(serde_json::Value::as_str),
        c.get("position_index").filter(|v| !v.is_null()),
    ) else {
        return None;
    };
    Some(format!("coordinate:{network}:{hash}:{}", text(index)))
}

/// Whether `detector` already raised the alert of detection key `key` (migration 0049): only its
/// own alerts count, so a row another role wrote, or one of a free-form shape, never stands in for
/// it (audit of PR #56, should-fix).
pub async fn raised<'e, E: sqlx::PgExecutor<'e>>(
    executor: E,
    detector: &str,
    key: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM event_log WHERE kind = 'key-compromise' \
           AND payload ->> 'detector' = $1 AND payload ->> 'detection_key' = $2)",
    )
    .bind(detector)
    .bind(key)
    .fetch_one(executor)
    .await
}

/// Whether `e` is the store's refusal of a second alert for one detection (the unique index of
/// migration 0049).
pub fn is_repeat(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .is_some_and(|d| d.code().as_deref() == Some("23505"))
}

/// Appends the alert record (`key-compromise`) and a global pause request (`pause-requested`) to
/// the event log, in one transaction, for the operator and the pause path (`REQ-IC-4`, `REQ-IC-2`).
/// Public for an endpoint that sees the key used where no signature can be matched (a lane nonce
/// consumed by an extrinsic it cannot decode, F-007 review S3). `pool` connects as the service role.
///
/// The store keeps one alert per detector and detection key (migration 0049): when this detector
/// already raised this one, nothing is written and the positions of the alert on record are
/// returned (or 0 where they cannot be read).
pub async fn raise(pool: &PgPool, alert: &serde_json::Value) -> Result<(i64, i64), sqlx::Error> {
    match raise_new(pool, alert).await {
        Err(e) if is_repeat(&e) => {
            let key = detection_key(alert).unwrap_or_default();
            let on_record: Option<(i64, i64)> = sqlx::query_as(
                "SELECT a.seq, COALESCE((SELECT p.seq FROM event_log p \
                   WHERE p.kind = 'pause-requested' AND p.payload ->> 'alert_seq' = a.seq::text \
                   ORDER BY p.seq LIMIT 1), 0) \
                 FROM event_log a WHERE a.kind = 'key-compromise' \
                   AND a.payload ->> 'detection_key' = $1 ORDER BY a.seq DESC LIMIT 1",
            )
            .bind(key)
            .fetch_optional(pool)
            .await
            .unwrap_or_default();
            Ok(on_record.unwrap_or((0, 0)))
        }
        other => other,
    }
}

async fn raise_new(pool: &PgPool, alert: &serde_json::Value) -> Result<(i64, i64), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let alert_seq: i64 = sqlx::query_scalar(
        "INSERT INTO event_log (kind, payload) VALUES ('key-compromise', $1::jsonb) RETURNING seq",
    )
    .bind(alert.to_string())
    .fetch_one(&mut *tx)
    .await?;
    let pause = json!({
        "spec": "REQ-IC-2, REQ-IC-4",
        "scope": "global",
        "reason": "key-compromise",
        "alert_seq": alert_seq,
    });
    let pause_seq: i64 = sqlx::query_scalar(
        "INSERT INTO event_log (kind, payload) VALUES ('pause-requested', $1::jsonb) RETURNING seq",
    )
    .bind(pause.to_string())
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((alert_seq, pause_seq))
}
