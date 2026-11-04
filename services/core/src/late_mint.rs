// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! A mint of a reference after its proof of non-execution (issue #26; `AC-I1.2`, `REQ-IC-4`,
//! `INV-8`, `INV-16`).
//!
//! The Kreivo endpoint reads the contract's single-use state at the final block that proves a
//! delivery not executed, and records no proof if the reference was used by then (`INV-8`). That
//! read covers the chain only up to the proof's final block. A reference minted **after** it, by a
//! leaked delivery key, by governance, or by an attempt bezent did not record, is seen by nothing
//! else, and a refund may already be ordered. The observer already records every `Minted` and
//! `RefundMinted` of the contract with its reference, final, so the check is the core's, over what
//! is recorded; this module is the whole of it.
//!
//! # What raises the alert
//!
//! A final, executed `Minted` (or `RefundMinted`) observation on the issuing network, at block `b`,
//! for reference `R`, is a **late mint** when one of the two holds for the deliveries of `R`
//! (the intents of the same network, purpose and reference; a refund mint's also in its
//! redeemer's scope, amendment 0008, so a `RefundMinted` is matched only to the redemption of its
//! recipient):
//!
//! 1. **after a proof**: a signed transaction of `R` has a proof of non-execution whose final block
//!    is below `b`. At or above `b`, the endpoint's own read already covers the mint (it records an
//!    anomaly, not a proof), so nothing is matched;
//! 2. **a refund ordered, nothing signed**: for a `RefundMinted` only, a refund mint is ordered
//!    for `R` and no transaction of `R` was ever signed. Nothing may be submitted before its exact
//!    bytes are stored (`INV-9`), so a refund mint of `R` cannot be bezent's. A deposit's mint
//!    order with nothing signed yet is not matched (the issue names a proof or a refund order):
//!    that case is left to the owner, see the pull request.
//!
//! and it is **not explained** by a later attempt of the same reference, which is legitimate
//! (`REQ-RF-5`: after a proof, a new attempt carries the same reference):
//!
//! - *explained*: a signed transaction of `R` was included, and executed, in the observation's own
//!   block (the endpoint records the inclusion as an observation, `REQ-OB-6`);
//! - *undecided*: a signed transaction of `R` has no proof, no inclusion and no anomaly yet, and
//!   its era covers `b`. The endpoint decides it when it next looks; the check waits, and decides
//!   when it has. The era bounds the wait: a transaction that has expired is settled by the
//!   endpoint, and then no longer holds anything back.
//!
//! Otherwise the alert is raised through the compromise path of F-005: `key-compromise` and the
//! global `pause-requested`, once per observation (the store keeps one alert per detector and
//! detection key, migration 0049, so a restart or a replay of the observer raises nothing twice).
//!
//! # Finality
//!
//! Only observations with a `final` outcome are read (`INV-16`, `REQ-OB-5`): the alert is a safety
//! signal, not a disposition, but it rests on the same data and acts only on what is final; the
//! observer writes nothing else. A `withdrawn` or undecided observation raises nothing.
//!
//! # A mint with no reference of bezent's at all
//!
//! Not matched here, and not meant to be: with no order recorded the reference is not one of
//! bezent's, and a backfilled history may hold mints of earlier deployments. `REQ-IC-2` makes the
//! unexplained supply a divergence of the reserve equation, which the reserve check pauses the
//! asset for (`REQ-BK-1`, `INV-4`).

use serde_json::json;
use sqlx::{PgConnection, PgPool};

use crate::refs;
use bezent_signer::compromise;

/// A final mint observation to look at.
#[derive(Debug, sqlx::FromRow)]
struct Candidate {
    network: String,
    block_number: i64,
    block_hash: Vec<u8>,
    position_index: i64,
    recipient: Option<String>,
    reference: Vec<u8>,
    event: String,
}

/// One delivery (an intent) of the reference.
#[derive(Debug, sqlx::FromRow)]
struct Delivery {
    id: i64,
    scope: Option<String>,
}

/// One signed transaction of the reference, and what the store knows of it.
#[derive(Debug, sqlx::FromRow)]
struct Signed {
    /// The final block of its proof of non-execution, if it has one.
    proof: Option<i64>,
    /// Whether it was included, executed, in the candidate's block.
    executed_here: bool,
    /// Whether it was included at all (executed or refused), or an anomaly names it.
    decided: bool,
    /// Its era, if the prepared transaction is on record.
    valid_from: Option<i64>,
    valid_until: Option<i64>,
}

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

/// The dedupe key of the alert for the observation at (`network`, `block_hash`, `index`).
fn dedupe(c: &Candidate) -> String {
    format!(
        "late-mint:{}:{}:{}",
        c.network,
        hex0x(&c.block_hash),
        c.position_index
    )
}

/// Looks at the final mint observations whose reference may have been minted late, and raises the
/// key-compromise alert and the global pause request for each that was. Returns how many alerts it
/// raised. `pool` connects as the core's login. Safe to repeat: an observation raises at most one.
pub async fn watch(pool: &PgPool) -> sqlx::Result<usize> {
    let mut conn = pool.acquire().await?;
    // Only a reference that has a proof, or an order and no signature, can be late: a few rows.
    let candidates: Vec<Candidate> = sqlx::query_as(
        "SELECT o.network, o.block_number, o.block_hash, o.position_index, o.recipient, \
                o.on_chain_reference AS reference, o.detail ->> 'event' AS event \
         FROM observations o \
         JOIN observation_outcomes x ON x.observation_id = o.id AND x.outcome = 'final' \
         JOIN networks n ON n.network = o.network AND n.role = 'issuing' \
         WHERE o.executed AND o.on_chain_reference IS NOT NULL \
           AND o.detail ->> 'event' IN ('Minted', 'RefundMinted') \
           AND EXISTS ( \
             SELECT 1 FROM intents i \
             WHERE i.network = o.network AND i.on_chain_reference = o.on_chain_reference \
               AND i.purpose = CASE o.detail ->> 'event' \
                                 WHEN 'Minted' THEN 'mint' ELSE 'refund-mint' END \
               AND (EXISTS (SELECT 1 FROM signed_transactions st \
                            JOIN non_execution_proofs p ON p.signed_transaction_id = st.id \
                            JOIN intents si ON si.id = st.intent_id \
                            WHERE si.network = i.network AND si.purpose = i.purpose \
                              AND si.on_chain_reference = i.on_chain_reference \
                              AND p.final_block_number < o.block_number) \
                    OR (i.purpose = 'refund-mint' \
                        AND NOT EXISTS (SELECT 1 FROM signed_transactions st \
                                        JOIN intents si ON si.id = st.intent_id \
                                        WHERE si.network = i.network AND si.purpose = i.purpose \
                                          AND si.on_chain_reference = i.on_chain_reference)))) \
         ORDER BY o.id",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut raised = 0;
    for c in candidates {
        if compromise::raised(
            &mut *conn,
            compromise::DETECTOR_CORE,
            &format!("dedupe:{}", dedupe(&c)),
        )
        .await?
        {
            continue;
        }
        let Some(why) = late(&mut conn, &c).await? else {
            continue;
        };
        let alert = json!({
            "spec": "AC-I1.2, REQ-IC-4, INV-8",
            "source": "late-mint",
            "dedupe": dedupe(&c),
            "network": c.network,
            "reason": why.text(),
            "event": c.event,
            "on_chain_reference": hex0x(&c.reference),
            "recipient": c.recipient,
            "coordinate": {
                "network": c.network,
                "block_number": c.block_number,
                "block_hash": hex0x(&c.block_hash),
                "position_kind": "log",
                "position_index": c.position_index,
            },
            "proof": why.proof(),
            "actions": ["pause", "rotate", "report"],
        });
        let (alert_seq, pause_seq) = compromise::raise(pool, &alert).await?;
        tracing::error!(
            network = c.network,
            block = c.block_number,
            alert_seq,
            pause_seq,
            reason = why.text(),
            "AC-I1.2: a reference was minted that bezent did not deliver; key-compromise and a global pause request are in the event log"
        );
        raised += 1;
    }
    Ok(raised)
}

/// Why a mint is late.
enum Late {
    /// A transaction of the reference was proven not executed at final block `proof`, below the mint.
    AfterProof { proof: i64 },
    /// An order is recorded and nothing was ever signed for it.
    Unsigned,
}

impl Late {
    fn text(&self) -> &'static str {
        match self {
            Late::AfterProof { .. } => {
                "the reference was minted after its proof of non-execution, by no transaction bezent signed"
            }
            Late::Unsigned => {
                "the refund mint was made though bezent signed nothing for its reference"
            }
        }
    }

    fn proof(&self) -> serde_json::Value {
        match self {
            Late::AfterProof { proof } => json!({"final_block_number": proof}),
            Late::Unsigned => serde_json::Value::Null,
        }
    }
}

/// Whether `c` is a late mint that no attempt of bezent's explains, and not one still undecided.
async fn late(conn: &mut PgConnection, c: &Candidate) -> sqlx::Result<Option<Late>> {
    let purpose = if c.event == "Minted" {
        "mint"
    } else {
        "refund-mint"
    };
    let deliveries: Vec<Delivery> = sqlx::query_as(
        "SELECT id, reference_scope AS scope FROM intents \
         WHERE network = $1 AND purpose = $2 AND on_chain_reference = $3",
    )
    .bind(&c.network)
    .bind(purpose)
    .bind(&c.reference)
    .fetch_all(&mut *conn)
    .await?;
    // A mint is of the reference's own scope: none for a mint, the redeemer's for a refund mint
    // (amendment 0008). A refund mint to another account is another redemption's.
    let ids: Vec<i64> = deliveries
        .iter()
        .filter(|d| match (&d.scope, &c.recipient, c.event.as_str()) {
            (None, _, "Minted") => true,
            (Some(scope), Some(to), "RefundMinted") => refs::same_account(scope, to),
            _ => false,
        })
        .map(|d| d.id)
        .collect();
    if ids.is_empty() {
        return Ok(None);
    }
    let signed: Vec<Signed> = sqlx::query_as(
        "SELECT (SELECT min(p.final_block_number) FROM non_execution_proofs p \
                 WHERE p.signed_transaction_id = st.id) AS proof, \
                EXISTS (SELECT 1 FROM observations t \
                        WHERE t.network = $2 AND t.block_hash = $3 AND t.position_kind = 'transaction' \
                          AND t.executed AND t.detail ->> 'signed_transaction' = st.id::text) \
                  AS executed_here, \
                (EXISTS (SELECT 1 FROM observations t \
                         WHERE t.network = $2 AND t.position_kind = 'transaction' \
                           AND t.detail ->> 'signed_transaction' = st.id::text) \
                 OR EXISTS (SELECT 1 FROM event_log e WHERE e.kind = 'delivery-anomaly' \
                            AND e.payload ->> 'network' = $2 \
                            AND e.payload ->> 'signed_transaction' = st.id::text)) AS decided, \
                (SELECT p.valid_from_block FROM prepared_transactions p \
                 WHERE p.id::text = i.payload ->> 'prepared') AS valid_from, \
                (SELECT p.valid_until_block FROM prepared_transactions p \
                 WHERE p.id::text = i.payload ->> 'prepared') AS valid_until \
         FROM signed_transactions st JOIN intents i ON i.id = st.intent_id \
         WHERE st.intent_id = ANY($1) ORDER BY st.id",
    )
    .bind(&ids)
    .bind(&c.network)
    .bind(&c.block_hash)
    .fetch_all(&mut *conn)
    .await?;

    // The late-mint rule: after a proof, or before any signature.
    let after_proof = signed
        .iter()
        .filter_map(|s| s.proof)
        .filter(|p| *p < c.block_number)
        .min();
    let matched = match after_proof {
        Some(proof) => Late::AfterProof { proof },
        None if signed.is_empty() && c.event == "RefundMinted" => Late::Unsigned,
        None => return Ok(None),
    };
    // A transaction of the reference executed in this very block explains it.
    if signed.iter().any(|s| s.executed_here) {
        return Ok(None);
    }
    // A transaction still to be decided, whose era covers the block, may yet be the one.
    let undecided = signed.iter().any(|s| {
        s.proof.is_none()
            && !s.decided
            && matches!((s.valid_from, s.valid_until),
                        (Some(from), Some(until)) if from <= c.block_number && c.block_number <= until)
    });
    if undecided {
        return Ok(None);
    }
    Ok(Some(matched))
}
