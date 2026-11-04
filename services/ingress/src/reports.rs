// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! An attested backend's reports (`F-011`; `REQ-BE-4`, `REQ-BE-5`, `REQ-BE-6`, `REQ-OB-7`; PLAN
//! `DEC-14`; `features/F-012-swaps-module/attesting-contract.md`).
//!
//! A report is a signed envelope: the bytes of a JSON document, an ed25519 signature over them and
//! the key, one declared for the backend. Taking it in is a request like any other: durable before
//! it is answered (`REQ-RQ-1`), the party's own identifier as its request reference (`REQ-ID-1`).
//! It moves nothing here. The core checks the signature again and records it as the observation.
//!
//! Who: a requester holding the attesting grant (`REQ-PN-2`) that is declared as the attesting
//! party of the backend the report names (`REQ-RQ-4`). The rest is refused with nothing recorded:
//! not this requester's to make. A report that fits the surface but not what it names is recorded
//! as refused, with its named error (§10), as a deposit is.
//!
//! `REQ-RQ-7` does not bind here (`REQ-BE-5`): a report's amount is the observation. What is checked
//! is that a report pointing at something points at something real: nothing is matched by amount
//! or time (`REQ-OT-2`).

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use bezent_core::attested::{REPORT_POSITION_INDEX, report_digest, report_message};
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Postgres, Transaction};
use utoipa::ToSchema;

use crate::api::{CoordinateOut, MAX_REFERENCE_LEN, PositionBody, PositionKind, parse_hex, to_hex};
use crate::app::{App, Body};
use crate::auth::{Grant, Requester};
use crate::error::{ApiError, ErrorBody};
use crate::intake::{Recorded, record};
use crate::refs;

const SOURCE_NOT_FOUND: &str = "ERR-SourceNotFound";
const SOURCE_MISMATCH: &str = "ERR-SourceMismatch";
const SOURCE_CONSUMED: &str = "ERR-SourceConsumed";
const UNKNOWN_ROUTE: &str = "ERR-UnknownRoute";

/// The most of a party's other identifiers one report carries.
const MAX_IDS: usize = 16;

/// A signed report (`DEC-14`).
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportEnvelope {
    /// The report, a JSON document, as a string. Its UTF-8 bytes are what is signed, exactly as
    /// sent. See `features/F-012-swaps-module/attesting-contract.md`.
    pub report: String,
    /// The ed25519 signature over `bezent-attested-report-v1`, one zero byte, and those bytes,
    /// `0x`-prefixed hexadecimal.
    pub signature: String,
    /// The key that signed, `0x`-prefixed hexadecimal: one declared for the backend.
    pub public_key: String,
}

/// What became of a report (`REQ-RQ-1`).
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct ReportStatus {
    /// The party's own identifier for it (`REQ-BE-4`).
    pub report_id: String,
    /// `Recorded`, or `Refused` with its named error.
    pub state: String,
    /// The named error of a refusal (§10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The coordinate the report's observation has: what a deposit names (`REQ-RQ-3`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinate: Option<CoordinateOut>,
}

/// A position's balance, as the party reports it (`REQ-AS-8`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Line {
    position: String,
    amount: String,
}

/// The kinds of report.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Arrival,
    Payout,
    /// WORKING ASSUMPTION, amendment 0016 A1: the party's report is the proof of non-execution.
    #[serde(rename = "payout-not-executed")]
    PayoutNotExecuted,
    Positions,
    Reversal,
    Restoration,
}

/// The report document. Which of its optional fields a kind needs is [`Document::fits`].
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    report_id: String,
    network: String,
    kind: Kind,
    asset: String,
    token: String,
    #[serde(default)]
    ids: BTreeMap<String, String>,
    /// When it happened, by the party's clock: checked for its form, recorded in the body, and
    /// never used to decide anything (`REQ-RQ-7`'s spirit: nothing rests on a party's clock).
    #[serde(default)]
    #[allow(dead_code)]
    occurred_at: Option<DateTime<Utc>>,
    #[serde(default)]
    amount: Option<String>,
    #[serde(default)]
    position: Option<String>,
    #[serde(default)]
    funder: Option<String>,
    #[serde(default)]
    bridge_reference: Option<String>,
    #[serde(default)]
    attempt: Option<i32>,
    #[serde(default)]
    positions: Option<Vec<Line>>,
    #[serde(default)]
    through: Option<String>,
    #[serde(default)]
    reverses: Option<String>,
    #[serde(default)]
    restores: Option<String>,
}

fn malformed<T>(why: impl Into<String>) -> Result<T, ApiError> {
    Err(ApiError::Malformed(why.into()))
}

/// An exact count of minor units, in the one spelling the store and the book use (`INV-11`).
fn minor_units(field: &str, text: &str, at_least: u128) -> Result<u128, ApiError> {
    let canonical = !text.is_empty()
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    // The store holds up to `u128::MAX`, and the book is signed: an amount above `i128::MAX`
    // would be recorded and then be one the reserve check cannot add (INV-11), stalling the
    // backend's check until someone repaired an append-only table. Refused here instead.
    match text.parse::<u128>() {
        Ok(n) if canonical && n >= at_least && i128::try_from(n).is_ok() => Ok(n),
        _ => malformed(format!(
            "{field} is a decimal string of exact minor units, from {at_least} to {} (INV-11)",
            i128::MAX
        )),
    }
}

impl Document {
    /// Whether the document has exactly what its kind needs (`ERR-Malformed`, §10).
    fn fits(&self) -> Result<(), ApiError> {
        if self.version != 1 {
            return malformed("version is 1");
        }
        for (field, value) in [
            ("report_id", &self.report_id),
            ("network", &self.network),
            ("asset", &self.asset),
            ("token", &self.token),
        ] {
            if value.is_empty() || value.len() > MAX_REFERENCE_LEN {
                return malformed(format!("{field} is 1 to {MAX_REFERENCE_LEN} bytes"));
            }
        }
        if self.ids.len() > MAX_IDS
            || self
                .ids
                .iter()
                .any(|(k, v)| k.len() > MAX_REFERENCE_LEN || v.len() > MAX_REFERENCE_LEN)
        {
            return malformed(format!(
                "ids holds at most {MAX_IDS} short identifiers (REQ-BE-4)"
            ));
        }
        let moved = matches!(
            self.kind,
            Kind::Arrival | Kind::Payout | Kind::Reversal | Kind::Restoration
        );
        let given = |present: bool, field: &str, wanted: bool| -> Result<(), ApiError> {
            if present == wanted {
                Ok(())
            } else if wanted {
                malformed(format!("{field} is required for this kind of report"))
            } else {
                malformed(format!("{field} is not part of this kind of report"))
            }
        };
        given(self.amount.is_some(), "amount", moved)?;
        given(self.position.is_some(), "position", moved)?;
        if self.funder.is_some() && self.kind != Kind::Arrival {
            return malformed("funder is part of an arrival only");
        }
        if self.through.is_some() && self.kind != Kind::Positions {
            return malformed("through is part of a positions report only");
        }
        given(
            self.bridge_reference.is_some(),
            "bridge_reference",
            matches!(self.kind, Kind::Payout | Kind::PayoutNotExecuted),
        )?;
        given(
            self.attempt.is_some(),
            "attempt",
            self.kind == Kind::PayoutNotExecuted,
        )?;
        if self.attempt.is_some_and(|a| a < 1) {
            return malformed("attempt counts from 1");
        }
        given(
            self.positions.is_some(),
            "positions",
            self.kind == Kind::Positions,
        )?;
        given(
            self.reverses.is_some(),
            "reverses",
            self.kind == Kind::Reversal,
        )?;
        given(
            self.restores.is_some(),
            "restores",
            self.kind == Kind::Restoration,
        )?;
        if let Some(amount) = &self.amount {
            minor_units("amount", amount, 1)?;
        }
        if self.position.as_deref().is_some_and(str::is_empty) {
            return malformed("position is not empty");
        }
        if let Some(lines) = &self.positions {
            let mut seen = std::collections::BTreeSet::new();
            for line in lines {
                minor_units("positions.amount", &line.amount, 0)?;
                if line.position.is_empty() || !seen.insert(line.position.as_str()) {
                    return malformed("each position is named, once");
                }
            }
        }
        Ok(())
    }
}

/// Whether `signature` is `key`'s over `document`. The ingress checks it to refuse at once; the
/// core checks it again before recording anything (F-011 NOTES 2).
fn signed_by(document: &[u8], signature: &[u8], key: &[u8]) -> bool {
    let (Ok(signature), Ok(key)) = (<[u8; 64]>::try_from(signature), <[u8; 32]>::try_from(key))
    else {
        return false;
    };
    let Ok(key) = VerifyingKey::from_bytes(&key) else {
        return false;
    };
    key.verify(
        &report_message(document),
        &Signature::from_bytes(&signature),
    )
    .is_ok()
}

/// An earlier report of the party on this network, by the party's own identifier, of `asset`.
async fn earlier(
    conn: &mut PgConnection,
    network: &str,
    asset: &str,
    kind: &str,
    report_id: &str,
) -> Result<Option<u128>, ApiError> {
    let found: Option<Option<String>> = sqlx::query_scalar(
        "SELECT body ->> 'amount' FROM attested_reports \
         WHERE network = $1 AND asset = $2 AND kind = $3 AND report_id = $4",
    )
    .bind(network)
    .bind(asset)
    .bind(kind)
    .bind(report_id)
    .fetch_optional(conn)
    .await?;
    Ok(found.map(|a| a.and_then(|a| a.parse().ok()).unwrap_or(0)))
}

/// What a report that fits the surface comes to.
enum Decision {
    Take,
    Refuse(&'static str),
}

/// The transfer a payout report names: a withdrawal, or the refund of an unassociated arrival.
#[derive(sqlx::FromRow)]
struct Payable {
    amount: String,
    state: String,
    /// Its source is final (`REQ-OT-3`), and it has the reference its leg will carry.
    ready: bool,
    /// Its instructions were accepted, or it names its destination in its request (`REQ-RQ-9`).
    instructed: bool,
    /// The attempt it is at, zero before its first.
    attempt: i32,
}

async fn payable(
    conn: &mut PgConnection,
    d: &Document,
    bridge_reference: &str,
) -> Result<Option<Payable>, sqlx::Error> {
    sqlx::query_as(
        "SELECT t.amount::text AS amount, t.state::text AS state, \
                t.source_final_network IS NOT NULL \
                  AND COALESCE(t.destination_reference, t.on_chain_reference) IS NOT NULL AS ready, \
                t.kind = 'refund' \
                  OR EXISTS (SELECT 1 FROM relayed_instructions r WHERE r.transfer_id = t.id) \
                  AS instructed, \
                COALESCE((SELECT max(x.attempt) FROM transitions x WHERE x.transfer_id = t.id), 0) \
                  AS attempt \
         FROM transfers t \
         WHERE t.bridge_reference = $1 \
           AND (t.kind = 'withdrawal' OR (t.kind = 'refund' AND t.refund_of IS NULL)) \
           AND t.destination_network = $2 AND t.asset = $3",
    )
    .bind(bridge_reference)
    .bind(&d.network)
    .bind(&d.asset)
    .fetch_optional(conn)
    .await
}

/// Whether the party already reported this transfer paid.
async fn paid(
    conn: &mut PgConnection,
    network: &str,
    bridge_reference: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM attested_reports \
           WHERE network = $1 AND kind = 'payout' AND body ->> 'bridge_reference' = $2)",
    )
    .bind(network)
    .bind(bridge_reference)
    .fetch_one(conn)
    .await
}

/// Whether the party already reported attempt `attempt` of this transfer not executed.
async fn not_executed(
    conn: &mut PgConnection,
    network: &str,
    bridge_reference: &str,
    attempt: i32,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM attested_reports \
           WHERE network = $1 AND kind = 'payout-not-executed' \
             AND body ->> 'bridge_reference' = $2 AND body ->> 'attempt' = $3::text)",
    )
    .bind(network)
    .bind(bridge_reference)
    .bind(attempt)
    .fetch_one(conn)
    .await
}

/// A `payout`: of a transfer of this asset on this backend, whose instructions were accepted and
/// whose source is final (`REQ-RQ-9`, `REQ-OT-2`, `REQ-OT-3`), for its amount (`REQ-TR-5`), once.
/// Never of one already refunding, nor of an attempt the party reported not executed (`INV-8`): a
/// double payment is refused here, before it is a divergence.
async fn decide_payout(
    conn: &mut PgConnection,
    d: &Document,
    bridge_reference: &str,
    amount: &str,
) -> Result<Decision, ApiError> {
    let Some(w) = payable(conn, d, bridge_reference)
        .await?
        .filter(|w| w.instructed)
    else {
        return Ok(Decision::Refuse(SOURCE_NOT_FOUND));
    };
    if !w.ready {
        return Err(ApiError::NotFinal);
    }
    if w.amount != amount {
        return Ok(Decision::Refuse(SOURCE_MISMATCH));
    }
    let open = matches!(
        w.state.as_str(),
        "Source-Final" | "Delivering" | "Delivered" | "Needs-Review"
    );
    if !open || paid(conn, &d.network, bridge_reference).await? {
        return Ok(Decision::Refuse(SOURCE_CONSUMED));
    }
    // The attempt it is at was reported not executed, and the core has not moved on yet: a new
    // attempt or the refund mint. Whichever it is, not now: nothing recorded, and the same bytes are
    // taken once a new attempt is there, or refused once it is refunding.
    if not_executed(conn, &d.network, bridge_reference, w.attempt).await? {
        return Err(ApiError::NotFinal);
    }
    Ok(Decision::Take)
}

/// A `payout-not-executed`: of the attempt the transfer is at, handed over and not reported paid,
/// once (`REQ-BE-5`; amendment 0016 A1, a working assumption). A payout reported paid stays paid: a
/// later failure of it is a reversal's to report, never this.
async fn decide_not_executed(
    conn: &mut PgConnection,
    d: &Document,
    bridge_reference: &str,
) -> Result<Decision, ApiError> {
    let Some(w) = payable(conn, d, bridge_reference)
        .await?
        .filter(|w| w.instructed)
    else {
        return Ok(Decision::Refuse(SOURCE_NOT_FOUND));
    };
    match w.state.as_str() {
        "Delivering" | "Needs-Review" => {}
        "Accepted" | "Source-Final" => return Err(ApiError::NotFinal),
        _ => return Ok(Decision::Refuse(SOURCE_CONSUMED)),
    }
    if d.attempt != Some(w.attempt) {
        return Ok(Decision::Refuse(SOURCE_MISMATCH));
    }
    if paid(conn, &d.network, bridge_reference).await?
        || not_executed(conn, &d.network, bridge_reference, w.attempt).await?
    {
        return Ok(Decision::Refuse(SOURCE_CONSUMED));
    }
    Ok(Decision::Take)
}

/// A report fits what it names: the asset and token on this backend, and any earlier report or
/// transfer it points at. Runs with the backend's reports locked, so what it reads stays true.
async fn decide(conn: &mut PgConnection, d: &Document) -> Result<Decision, ApiError> {
    let declared: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM assets a \
           JOIN reserves r ON r.asset = a.asset AND r.network = $1 \
           JOIN admitted_tokens k ON k.asset = a.asset AND k.network = $1 AND k.token = $3 \
           WHERE a.asset = $2 AND a.assurance_class = 'attested')",
    )
    .bind(&d.network)
    .bind(&d.asset)
    .bind(&d.token)
    .fetch_one(&mut *conn)
    .await?;
    if !declared {
        return Ok(Decision::Refuse(UNKNOWN_ROUTE));
    }
    let amount = d.amount.as_deref().unwrap_or("0");
    let reported: u128 = amount.parse().unwrap_or(0);
    match d.kind {
        Kind::Arrival => Ok(Decision::Take),
        Kind::Payout => {
            decide_payout(conn, d, d.bridge_reference.as_deref().unwrap_or(""), amount).await
        }
        Kind::PayoutNotExecuted => {
            decide_not_executed(conn, d, d.bridge_reference.as_deref().unwrap_or("")).await
        }
        Kind::Positions => {
            if let Some(through) = &d.through {
                let known: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM attested_reports \
                       WHERE network = $1 AND report_id = $2)",
                )
                .bind(&d.network)
                .bind(through)
                .fetch_one(conn)
                .await?;
                if !known {
                    return Ok(Decision::Refuse(SOURCE_NOT_FOUND));
                }
            }
            Ok(Decision::Take)
        }
        // REQ-BE-6: of a payment reported final, named by its report; never more than it. A reversal
        // that was restored is no longer reversed, so it does not count against the arrival: a
        // second real chargeback after a restoration is as good as the first.
        Kind::Reversal => {
            let reverses = d.reverses.as_deref().unwrap_or("");
            let Some(arrival) = earlier(conn, &d.network, &d.asset, "arrival", reverses).await?
            else {
                return Ok(Decision::Refuse(SOURCE_NOT_FOUND));
            };
            let before: Option<String> = sqlx::query_scalar(
                "SELECT sum((v.body ->> 'amount')::numeric)::text FROM attested_reports v \
                 WHERE v.network = $1 AND v.asset = $2 AND v.kind = 'reversal' \
                   AND v.body ->> 'reverses' = $3 \
                   AND NOT EXISTS (SELECT 1 FROM attested_reports r \
                                   WHERE r.network = v.network AND r.kind = 'restoration' \
                                     AND r.body ->> 'restores' = v.report_id)",
            )
            .bind(&d.network)
            .bind(&d.asset)
            .bind(reverses)
            .fetch_one(conn)
            .await?;
            let before: u128 = before.and_then(|b| b.parse().ok()).unwrap_or(0);
            Ok(match before.checked_add(reported) {
                Some(total) if total <= arrival => Decision::Take,
                _ => Decision::Refuse(SOURCE_MISMATCH),
            })
        }
        // REQ-BE-6: the reserve a reversal took, in full, once. Only the party that reported it
        // reports here: a backend has one.
        Kind::Restoration => {
            let restores = d.restores.as_deref().unwrap_or("");
            let Some(reversal) = earlier(conn, &d.network, &d.asset, "reversal", restores).await?
            else {
                return Ok(Decision::Refuse(SOURCE_NOT_FOUND));
            };
            if reversal != reported {
                return Ok(Decision::Refuse(SOURCE_MISMATCH));
            }
            let restored: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM attested_reports \
                   WHERE network = $1 AND kind = 'restoration' AND body ->> 'restores' = $2)",
            )
            .bind(&d.network)
            .bind(restores)
            .fetch_one(conn)
            .await?;
            Ok(if restored {
                Decision::Refuse(SOURCE_CONSUMED)
            } else {
                Decision::Take
            })
        }
    }
}

fn coordinate(network: String, seq: i64, digest: &[u8]) -> CoordinateOut {
    CoordinateOut {
        network,
        block_number: seq,
        block_hash: to_hex(digest),
        position: PositionBody {
            kind: PositionKind::Log,
            index: REPORT_POSITION_INDEX,
        },
    }
}

/// The answer already recorded for a request (`REQ-ID-1`).
async fn answered(
    conn: &mut PgConnection,
    intake: i64,
    reference: &str,
) -> Result<ReportStatus, ApiError> {
    let row: Option<(String, i64, Vec<u8>)> =
        sqlx::query_as("SELECT network, seq, digest FROM attested_reports WHERE intake_id = $1")
            .bind(intake)
            .fetch_optional(&mut *conn)
            .await?;
    if let Some((network, seq, digest)) = row {
        return Ok(ReportStatus {
            report_id: reference.to_owned(),
            state: "Recorded".into(),
            error: None,
            coordinate: Some(coordinate(network, seq, &digest)),
        });
    }
    let error: Option<Option<String>> =
        sqlx::query_scalar("SELECT error::text FROM intake_answers WHERE intake_id = $1")
            .bind(intake)
            .fetch_optional(conn)
            .await?;
    let Some(error) = error else {
        tracing::error!(
            intake,
            "a report's request has neither a report nor an answer"
        );
        return Err(ApiError::Internal);
    };
    Ok(ReportStatus {
        report_id: reference.to_owned(),
        state: "Refused".into(),
        error,
        coordinate: None,
    })
}

/// Commits, telling a refused commit from one whose outcome is unknown (as a deposit's).
async fn commit(tx: Transaction<'static, Postgres>, requester: &str) -> Result<(), ApiError> {
    match tx.commit().await {
        Ok(()) => Ok(()),
        Err(e @ sqlx::Error::Database(_)) => Err(e.into()),
        Err(e) => {
            tracing::error!(error = %e, requester, "the commit of a report has an unknown outcome");
            Err(ApiError::OutcomeUnknown)
        }
    }
}

/// Take in a report of an attested backend (`REQ-BE-5`).
#[utoipa::path(
    post, path = "/v0/reports", tag = "requests",
    request_body = ReportEnvelope,
    security(("bearer" = [])),
    responses(
        (status = 201, description = "Taken in now: `Recorded`, with the coordinate its observation has; or `Refused` with its named error", body = ReportStatus),
        (status = 200, description = "A repeat of the same bytes: the original's answer (REQ-ID-1)", body = ReportStatus),
        (status = 400, body = ErrorBody),
        (status = 401, body = ErrorBody),
        (status = 403, description = "ERR-NotAuthorised: no attesting grant, not this backend's party, an undeclared key or a signature that does not verify. Nothing recorded", body = ErrorBody),
        (status = 409, body = ErrorBody),
        (status = 503, description = "ERR-Overloaded: nothing recorded; retry. not-final: a payout of a redemption not final yet; nothing recorded; send the same bytes again. outcome-unknown: repeat the same request", body = ErrorBody),
    )
)]
pub(crate) async fn report(
    State(app): State<App>,
    who: Requester,
    Body(envelope): Body<ReportEnvelope>,
) -> Result<(StatusCode, Json<ReportStatus>), ApiError> {
    who.require(Grant::Attest)?;
    let bytes = envelope.report.as_bytes();
    let document: Document =
        serde_json::from_slice(bytes).map_err(|e| ApiError::Malformed(format!("report: {e}")))?;
    document.fits()?;
    let body: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| ApiError::Internal)?;
    let signature = parse_hex("signature", &envelope.signature)?;
    let public_key = parse_hex("public_key", &envelope.public_key)?;
    if signature.len() != 64 {
        return malformed("signature is 64 bytes");
    }
    if public_key.len() != 32 {
        return malformed("public_key is 32 bytes");
    }

    let _permit = app.permit()?;
    let mut tx = app.pool().begin().await?;
    // REQ-RQ-4, REQ-BE-5: this backend's party, with a key declared for it, over these bytes.
    let party: Option<String> = sqlx::query_scalar(
        "SELECT party FROM attesting_parties WHERE network = $1 AND requester = $2",
    )
    .bind(&document.network)
    .bind(&who.name)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(party) = party else {
        return Err(ApiError::NotAuthorised);
    };
    let declared: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM attesting_keys WHERE network = $1 AND public_key = $2)",
    )
    .bind(&document.network)
    .bind(&public_key)
    .fetch_one(&mut *tx)
    .await?;
    if !declared || !signed_by(bytes, &signature, &public_key) {
        return Err(ApiError::NotAuthorised);
    }

    let reference = document.report_id.as_str();
    let intake = match record(&mut tx, &who.name, "report", reference, &envelope).await? {
        Recorded::New(id) => id,
        Recorded::Repeat(id) => {
            let status = answered(&mut tx, id, reference).await?;
            tx.rollback().await?;
            return Ok((StatusCode::OK, Json(status)));
        }
    };
    // One report at a time per backend, to the commit: what `decide` reads stays true, and the
    // store numbers them in commit order under the same lock.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("bezent/attested-report/{}", document.network))
        .execute(&mut *tx)
        .await?;
    let status = match decide(&mut tx, &document).await? {
        Decision::Take => {
            let digest = report_digest(bytes);
            let seq: i64 = sqlx::query_scalar(
                "INSERT INTO attested_reports (network, seq, intake_id, requester, party, \
                     report_id, kind, asset, body, signed_bytes, signature, public_key, digest) \
                 VALUES ($1, 1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING seq",
            )
            .bind(&document.network)
            .bind(intake)
            .bind(&who.name)
            .bind(&party)
            .bind(reference)
            .bind(body["kind"].as_str())
            .bind(&document.asset)
            .bind(sqlx::types::Json(&body))
            .bind(bytes)
            .bind(&signature)
            .bind(&public_key)
            .bind(&digest[..])
            .fetch_one(&mut *tx)
            .await?;
            ReportStatus {
                report_id: reference.to_owned(),
                state: "Recorded".into(),
                error: None,
                coordinate: Some(coordinate(document.network.clone(), seq, &digest)),
            }
        }
        Decision::Refuse(error) => {
            // As any refusal before a transfer can be named: the answer, and the log (REQ-ST-4).
            let bridge_reference = refs::refused(&who.name, reference);
            sqlx::query(
                "INSERT INTO intake_answers (intake_id, bridge_reference, error) \
                 VALUES ($1, $2, $3::refusal_error)",
            )
            .bind(intake)
            .bind(&bridge_reference)
            .bind(error)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO event_log (kind, payload) VALUES ('refusal', jsonb_build_object( \
                   'bridge_reference', $1::text, 'request_reference', $2::text, \
                   'requester', $3::text, 'kind', 'report', 'state', 'Refused', \
                   'error', $4::text))",
            )
            .bind(&bridge_reference)
            .bind(reference)
            .bind(&who.name)
            .bind(error)
            .execute(&mut *tx)
            .await?;
            ReportStatus {
                report_id: reference.to_owned(),
                state: "Refused".into(),
                error: Some(error.to_owned()),
                coordinate: None,
            }
        }
    };
    commit(tx, &who.name).await?;
    tracing::info!(
        requester = %who.name,
        network = %document.network,
        report = %status.report_id,
        state = %status.state,
        "report taken in"
    );
    Ok((StatusCode::CREATED, Json(status)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inv_11_minor_units_have_one_spelling() {
        assert!(minor_units("a", "1", 1).is_ok());
        assert!(minor_units("a", "0", 0).is_ok());
        for bad in ["", "0", "01", "1.5", "-1", " 1", "abc"] {
            assert!(minor_units("a", bad, 1).is_err(), "{bad}");
        }
    }
}
