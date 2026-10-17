// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! What the reserve check reads from the networks and from an attesting party's reports
//! (`T-004-02`; `REQ-BK-8`, `REQ-BE-4`, `REQ-CV-4`).
//!
//! The core holds no network connection (PLAN `DEC-3`): the endpoints read. [`Readings`] is the
//! interface the check uses, at a final point it names, so the network and the book are compared
//! at the same point:
//!
//! - **supply** of a bridged asset on the issuing network (the Kreivo endpoint, F-007);
//! - **reserve**: the balance of the origin token at a reserve address on an origin network (the
//!   EVM endpoint, F-009);
//! - **convertible holdings** at a reserve address, itemised per token, outside the equation
//!   (`REQ-CV-4`; F-009 observes them);
//! - an attested backend's **positions**, as its attesting party last reported them, with the
//!   party's own identifiers (`REQ-BE-4`, `REQ-AS-8`; F-011).
//!
//! Amounts are each network's own minor units, unscaled; the check scales them to the book unit.
//!
//! [`Recorded`] is the implementation the running core uses (issue #42): it reads what the
//! endpoints recorded in `reserve_readings` at the very final point the check names (network, block
//! number and hash), so every term rests on one point per network. Nothing recorded there, or a
//! reading recorded as unknown (the providers disagreed, a read failed), is a [`ReadError`]: the
//! evaluation is incomplete, and one that stays so past the network's bound pauses the asset
//! (`REQ-IC-2`). Attested backends' reports (F-011) are not wired: an attested position is a
//! [`ReadError`], "no attested assets configured".
//!
//! # A supply mismatch is a breach of `INV-4` (PLAN `DEC-16`, 2026-10-15; PR #50 review M1)
//!
//! The issuing network's endpoint records both the on-chain component's own supply count and the
//! asset's issuance, and flags the reading when they differ (`reserve_readings.mismatch`): value
//! was minted or destroyed outside the component. The issuance is the supply the equation uses;
//! the flag is read from the store by the check itself ([`supply_mismatches`],
//! [`supply_mismatch_at`]), whatever [`Readings`] it is given, so it is never lost with a process
//! that stopped between recording it and signalling it.

use std::fmt;
use std::future::Future;

use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};

use super::FinalPoint;

/// Why a reading could not be made. The evaluation is then incomplete, never a guess.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadError(pub String);

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ReadError {}

/// A convertible holding at a reserve address (`REQ-CV-4`): backs nothing until converted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holding {
    /// The token, as the network names it.
    pub token: String,
    /// Its amount, in the token's own minor units, as decimal text.
    pub amount: String,
}

/// An attested backend's position, as its attesting party last reported it (`REQ-BE-4`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestedPosition {
    /// The position (an account at the backend), as the party names it.
    pub position: String,
    /// Its balance of the origin token, in the token's own minor units, as decimal text.
    pub amount: String,
    /// The attesting party.
    pub party: String,
    /// The report the balance comes from, by the party's own identifier.
    pub report: String,
}

/// The readings the reserve check makes (`REQ-BK-8`). Each is at the final point named, or fails.
pub trait Readings: Send + Sync {
    /// Total supply of `issuing_asset` on the issuing network at `at`, in the asset's minor units.
    fn supply(
        &self,
        at: &FinalPoint,
        issuing_asset: &str,
    ) -> impl Future<Output = Result<u128, ReadError>> + Send;

    /// The balance of `token` held by `address` on `at`'s network at `at`, in the token's minor
    /// units.
    fn reserve(
        &self,
        at: &FinalPoint,
        token: &str,
        address: &str,
    ) -> impl Future<Output = Result<u128, ReadError>> + Send;

    /// The convertible tokens `address` holds at `at` that back `asset`'s deposits once converted
    /// (`REQ-CV-4`, `REQ-AS-7`).
    fn convertible(
        &self,
        at: &FinalPoint,
        asset: &str,
        address: &str,
    ) -> impl Future<Output = Result<Vec<Holding>, ReadError>> + Send;

    /// The positions an attesting party last reported for `asset` on the attested backend
    /// `network` (`REQ-BE-4`).
    fn attested(
        &self,
        asset: &str,
        network: &str,
    ) -> impl Future<Output = Result<Vec<AttestedPosition>, ReadError>> + Send;
}

/// The readings the endpoints recorded at final heads (`reserve_readings`, migration 0039).
#[derive(Clone, Debug)]
pub struct Recorded {
    pool: PgPool,
}

impl Recorded {
    /// The readings over `pool` (the core's login).
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The reading of `kind` of `subject` held by `holder` at `at`.
    async fn one(
        &self,
        at: &FinalPoint,
        kind: &str,
        subject: &str,
        holder: &str,
    ) -> Result<u128, ReadError> {
        let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT amount::text, unknown_reason FROM reserve_readings \
             WHERE network = $1 AND block_number = $2 AND block_hash = $3 \
               AND kind = $4 AND subject = $5 AND holder = $6",
        )
        .bind(&at.network)
        .bind(at.block_number)
        .bind(&at.block_hash)
        .bind(kind)
        .bind(subject)
        .bind(holder)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| ReadError(format!("store: {e}")))?;
        let at_text = || {
            format!(
                "{} block {} (0x{})",
                at.network,
                at.block_number,
                hex::encode(&at.block_hash)
            )
        };
        match row {
            None => Err(ReadError(format!(
                "no {kind} reading of {subject} recorded at {}",
                at_text()
            ))),
            Some((Some(amount), _)) => amount
                .parse()
                .map_err(|_| ReadError(format!("the {kind} reading of {subject} is out of range"))),
            Some((None, why)) => Err(ReadError(format!(
                "the {kind} reading of {subject} at {} is unknown: {}",
                at_text(),
                why.unwrap_or_default()
            ))),
        }
    }
}

impl Readings for Recorded {
    async fn supply(&self, at: &FinalPoint, issuing_asset: &str) -> Result<u128, ReadError> {
        self.one(at, "supply", issuing_asset, "").await
    }

    async fn reserve(
        &self,
        at: &FinalPoint,
        token: &str,
        address: &str,
    ) -> Result<u128, ReadError> {
        self.one(at, "reserve", token, address).await
    }

    async fn convertible(
        &self,
        at: &FinalPoint,
        _asset: &str,
        address: &str,
    ) -> Result<Vec<Holding>, ReadError> {
        let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT subject, amount::text, unknown_reason FROM reserve_readings \
             WHERE network = $1 AND block_number = $2 AND block_hash = $3 \
               AND kind = 'convertible' AND holder = $4 ORDER BY subject",
        )
        .bind(&at.network)
        .bind(at.block_number)
        .bind(&at.block_hash)
        .bind(address)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ReadError(format!("store: {e}")))?;
        rows.into_iter()
            .map(|(token, amount, why)| match amount {
                Some(amount) => Ok(Holding { token, amount }),
                None => Err(ReadError(format!(
                    "the convertible reading of {token} at {} block {} is unknown: {}",
                    at.network,
                    at.block_number,
                    why.unwrap_or_default()
                ))),
            })
            .collect()
    }

    async fn attested(
        &self,
        asset: &str,
        network: &str,
    ) -> Result<Vec<AttestedPosition>, ReadError> {
        // F-011 (issue #11): the attesting party's reports are not wired yet.
        Err(ReadError(format!(
            "no attested assets configured: {asset} on {network} rests on an attesting party's \
             reports, which bezent does not read yet (F-011)"
        )))
    }
}

/// A supply reading the issuing network's endpoint flagged: the on-chain component's own count and
/// the asset's issuance differ (`INV-4`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SupplyMismatch {
    /// The reading's row in `reserve_readings`.
    pub reading: i64,
    /// The issuing network.
    pub network: String,
    /// The final head it was read at.
    pub block_number: i64,
    /// Its hash.
    pub block_hash: Vec<u8>,
    /// The component's count, as recorded.
    pub contract_supply: Option<String>,
    /// The issuance, as recorded: the supply (§5.3).
    pub issuance: Option<String>,
    /// Issuance minus the component's count, as decimal text, or `unknown`: what tells one
    /// mismatch from another (a mint of X outside the component stays a difference of X while
    /// ordinary mints go on).
    pub difference: String,
}

/// The difference a supply reading `r` records: issuance minus the component's count, or
/// `unknown`.
macro_rules! difference {
    () => {
        "CASE WHEN r.detail ->> 'issuance' ~ '^[0-9]{1,78}$' \
              AND r.detail ->> 'contract_supply' ~ '^[0-9]{1,78}$' \
             THEN ((r.detail ->> 'issuance')::numeric \
                   - (r.detail ->> 'contract_supply')::numeric)::text \
             ELSE 'unknown' END"
    };
}

/// Flagged supply readings on the issuing network, with the difference each records.
const MISMATCH: &str = concat!(
    "SELECT r.id, r.network, r.block_number, r.block_hash, \
        r.detail ->> 'contract_supply', r.detail ->> 'issuance', ",
    difference!(),
    " FROM reserve_readings r \
    JOIN networks n ON n.network = r.network AND n.role = 'issuing' \
    WHERE r.kind = 'supply' AND r.mismatch"
);

/// Every known supply reading of the issuing asset `$1` names on the issuing network, in block
/// order, with its difference and whether the reading before it was flagged and with what
/// difference. An unknown reading that is not flagged says nothing of the mismatch and is left out.
const SUPPLY_RUN: &str = concat!(
    "SELECT r.id, r.network, r.block_number, r.block_hash, \
        r.detail ->> 'contract_supply' AS contract_supply, r.detail ->> 'issuance' AS issuance, ",
    difference!(),
    " AS difference, r.mismatch, \
        lag(r.mismatch) OVER w AS before_flagged, \
        lag(",
    difference!(),
    ") OVER w AS before_difference \
    FROM reserve_readings r \
    JOIN networks n ON n.network = r.network AND n.role = 'issuing' \
    WHERE r.kind = 'supply' AND (r.mismatch OR r.amount IS NOT NULL) \
      AND r.subject = (SELECT a.issuing_asset FROM assets a WHERE a.asset = $1) \
    WINDOW w AS (PARTITION BY r.network ORDER BY r.block_number, r.id)"
);

type MismatchRow = (
    i64,
    String,
    i64,
    Vec<u8>,
    Option<String>,
    Option<String>,
    String,
);

fn mismatch(
    (reading, network, block_number, block_hash, contract_supply, issuance, difference): MismatchRow,
) -> SupplyMismatch {
    SupplyMismatch {
        reading,
        network,
        block_number,
        block_hash,
        contract_supply,
        issuance,
        difference,
    }
}

/// Every supply mismatch event recorded for `asset` on the issuing network, at any final head,
/// oldest first: what the check raises `INV-4` on, once each, even when the mismatch is gone by the
/// point it reads at. An event starts at a flagged reading whose known predecessor, in block order,
/// was not flagged, or was flagged with another difference; it is named by that reading (PR #50
/// re-check S-2). So a mismatch that clears and comes back with the same difference is a new
/// event, and one that stands while ordinary mints go on (its difference unchanged) is one.
pub async fn supply_mismatches(
    conn: &mut PgConnection,
    asset: &str,
) -> sqlx::Result<Vec<SupplyMismatch>> {
    let rows: Vec<MismatchRow> = sqlx::query_as(&format!(
        "SELECT e.id, e.network, e.block_number, e.block_hash, e.contract_supply, e.issuance, \
                e.difference \
         FROM ({SUPPLY_RUN}) AS e \
         WHERE e.mismatch \
           AND (e.before_flagged IS NOT TRUE OR e.before_difference IS DISTINCT FROM e.difference) \
         ORDER BY e.id"
    ))
    .bind(asset)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(mismatch).collect())
}

/// The supply mismatch recorded for `issuing_asset` at exactly the final point `at`, if any.
pub async fn supply_mismatch_at(
    conn: &mut PgConnection,
    at: &FinalPoint,
    issuing_asset: &str,
) -> sqlx::Result<Option<SupplyMismatch>> {
    let row: Option<MismatchRow> = sqlx::query_as(&format!(
        "{MISMATCH} AND r.network = $1 AND r.block_number = $2 AND r.block_hash = $3 \
           AND r.subject = $4"
    ))
    .bind(&at.network)
    .bind(at.block_number)
    .bind(&at.block_hash)
    .bind(issuing_asset)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(mismatch))
}
