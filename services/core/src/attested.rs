// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! An attesting party's reports become observations (`F-011`, T-011-04; `REQ-BE-4`, `REQ-BE-5`; PLAN
//! `DEC-14`; F-011 NOTES 2, 5).
//!
//! The ingress takes a report in; the core records it. Before it does, the core checks again what
//! the ingress checked: the signature over the exact bytes, by a key declared for the backend, and
//! that the row says what those bytes say. So a row a compromised ingress wrote without the party's
//! key never becomes an observation, and a report is what the party signed.
//!
//! Reports are recorded in their network's order. One the core refuses holds that network's queue
//! and pages, once: nothing later on that network is read past it, so no later report can reach
//! the book around it. The stale head then makes the reserve check pause the asset (`REQ-IC-2`).
//! Other networks go on.

use std::collections::BTreeSet;

use bezent_core::attested::{report_digest, report_message};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::store;

/// A report not yet recorded.
#[derive(sqlx::FromRow)]
struct Pending {
    id: i64,
    network: String,
    seq: i64,
    party: String,
    report_id: String,
    kind: String,
    body: sqlx::types::Json<Value>,
    signed_bytes: Vec<u8>,
    signature: Vec<u8>,
    public_key: Vec<u8>,
    digest: Vec<u8>,
    declared: bool,
}

/// Why a report is not recorded, if it is not: what the core checks itself, whatever the row says.
fn refusal(r: &Pending) -> Option<&'static str> {
    if !r.declared {
        return Some("its key is not declared for this backend");
    }
    let (Ok(signature), Ok(key)) = (
        <[u8; 64]>::try_from(r.signature.as_slice()),
        <[u8; 32]>::try_from(r.public_key.as_slice()),
    ) else {
        return Some("its signature or key has the wrong length");
    };
    let Ok(key) = VerifyingKey::from_bytes(&key) else {
        return Some("its key is not a key");
    };
    let message = report_message(&r.signed_bytes);
    if key
        .verify(&message, &Signature::from_bytes(&signature))
        .is_err()
    {
        return Some("its signature does not verify");
    }
    if report_digest(&r.signed_bytes).as_slice() != r.digest.as_slice() {
        return Some("its digest is not of the bytes signed");
    }
    match serde_json::from_slice::<Value>(&r.signed_bytes) {
        Ok(signed) if signed == r.body.0 => None,
        _ => Some("its body is not the document signed"),
    }
}

/// Pages that report `r` was refused, once (`REQ-IC-2`), with the party's identifiers.
async fn page_refused(pool: &PgPool, r: &Pending, why: &str) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    let name = format!("{}:{}", r.network, r.seq);
    if store::has_event(&mut tx, "attested-report-refused", None, "report", &name).await? {
        return Ok(());
    }
    let detail = json!({"spec": "REQ-BE-5, REQ-IC-2", "report": name, "network": r.network,
                        "party": r.party, "report_id": r.report_id, "kind": r.kind,
                        "reason": why});
    store::event(&mut tx, "attested-report-refused", None, &detail).await?;
    tx.commit().await?;
    tracing::error!(kind = %"attested-report-refused", %detail,
        "page: a report was not recorded; its network's queue is held behind it");
    Ok(())
}

/// Records every report not yet an observation, in its network's order, and returns how many.
pub async fn record_pending(pool: &PgPool) -> sqlx::Result<usize> {
    let found: Vec<Pending> = sqlx::query_as(
        "SELECT r.id, r.network, r.seq, r.party, r.report_id, r.kind, r.body, r.signed_bytes, \
                r.signature, r.public_key, r.digest, \
                EXISTS (SELECT 1 FROM attesting_keys k \
                        WHERE k.network = r.network AND k.public_key = r.public_key) AS declared \
         FROM attested_reports r \
         WHERE NOT EXISTS (SELECT 1 FROM observations o \
                           WHERE o.network = r.network AND o.block_number = r.seq) \
         ORDER BY r.network, r.seq",
    )
    .fetch_all(pool)
    .await?;
    let mut held = BTreeSet::new();
    let mut recorded = 0;
    for r in &found {
        if held.contains(&r.network) {
            continue;
        }
        if let Some(why) = refusal(r) {
            held.insert(r.network.clone());
            page_refused(pool, r, why).await?;
            continue;
        }
        let mut tx = pool.begin().await?;
        let observation: sqlx::Result<Option<i64>> =
            sqlx::query_scalar("SELECT bezent_record_attested_observation($1)")
                .bind(r.id)
                .fetch_one(&mut *tx)
                .await;
        match observation {
            Ok(Some(observation)) => {
                store::event(
                    &mut tx,
                    "attested-report-recorded",
                    None,
                    &json!({"spec": "REQ-BE-4, REQ-BE-5", "network": r.network, "party": r.party,
                            "report_id": r.report_id, "kind": r.kind,
                            "observation": observation.to_string(),
                            "block_number": r.seq.to_string()}),
                )
                .await?;
                tx.commit().await?;
                recorded += 1;
            }
            Ok(None) => tx.rollback().await?,
            Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("BZ001") => {
                tx.rollback().await?;
                held.insert(r.network.clone());
                page_refused(pool, r, e.message()).await?;
            }
            Err(e) => {
                // A passing failure: tried again next cycle, and nothing later goes before it.
                held.insert(r.network.clone());
                tracing::warn!(network = %r.network, seq = r.seq, error = %e, "a report was not recorded; tried again next cycle");
            }
        }
    }
    Ok(recorded)
}
