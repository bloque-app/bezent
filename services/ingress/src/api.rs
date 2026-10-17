// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The JSON shapes of the request and query surfaces (§7.A, §7.B).
//!
//! Every request type refuses a field it does not define (`deny_unknown_fields`), so a request
//! cannot carry a claimed outcome, a claimed finality or an amount bezent would take as observed
//! (`REQ-RQ-7`): a coordinate is the only thing a request says about a chain.
//!
//! Amounts are decimal strings of exact integer minor units (`INV-11`); hashes and references on
//! chain are `0x`-prefixed hexadecimal.

use crate::error::ApiError;
use bezent_core::{BlockHash, Coordinate, NetworkId, Position};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// The longest request reference taken, in bytes.
pub const MAX_REFERENCE_LEN: usize = 256;

/// Where in a block a movement is.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PositionKind {
    /// A transaction (extrinsic) index.
    Transaction,
    /// A log (event) index.
    Log,
}

impl PositionKind {
    /// The store's spelling (`position_kind`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transaction => "transaction",
            Self::Log => "log",
        }
    }
}

/// A movement's position in its block.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PositionBody {
    /// Transaction or log.
    pub kind: PositionKind,
    /// Its index in the block.
    pub index: u32,
}

/// A coordinate: network, block number, block hash and position (SPEC §2). A pointer bezent
/// verifies, never a claim (`REQ-RQ-7`).
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CoordinateBody {
    /// The network, as bezent names it.
    pub network: String,
    /// The block's number.
    pub block_number: u64,
    /// The block's hash, `0x`-prefixed hexadecimal.
    pub block_hash: String,
    /// The movement's position in the block.
    pub position: PositionBody,
}

/// A destination: a network and an address on it.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DestinationBody {
    /// The network, as bezent names it.
    pub network: String,
    /// The address, in the network's own format.
    pub address: String,
}

/// Request a deposit (`REQ-RQ-3`): bind the arrival at a coordinate, and name where it is minted
/// and where it is refunded.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DepositRequest {
    /// The requester's reference, unique for this requester forever (`REQ-RQ-1`, `REQ-ID-1`).
    pub request_reference: String,
    /// The coordinate of the arrival at a reserve address: an observation its endpoint recorded
    /// as kind `arrival`, or `conversion_output` (the output of a conversion into the reserve,
    /// `REQ-CV-2`), final and not yet consumed; never a convertible arrival (`REQ-CV-1`), a payout
    /// leg or any other kind (`REQ-RQ-3`, `REQ-IN-3`, `INV-2`). Otherwise `ERR-SourceNotFound`,
    /// recorded. An arrival of one of those kinds that is observed but not final yet is answered
    /// `not-final` (HTTP 503) and nothing is recorded: the same request, sent again once it is
    /// final, is taken in.
    pub coordinate: CoordinateBody,
    /// Where the bridged asset is minted: an address on the issuing network (`REQ-IN-5`).
    pub destination: DestinationBody,
    /// Where the value is refunded if the mint is proven never to execute: an address on the
    /// arrival's network (`REQ-RF-2`, `REQ-OT-4`).
    pub refund_destination: DestinationBody,
}

/// Add, by a redemption's on-chain reference, what a withdrawal may carry beyond the redemption
/// (`REQ-RQ-4`): an expiry, and for an attested backend the payout instructions relayed for the
/// redemption's commitment (`REQ-RQ-9`). At least one is given. A withdrawal's refund always goes
/// to its redeemer (`REQ-RF-4`, amendment 0011), so no refund destination is taken.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WithdrawalExtrasRequest {
    /// The requester's reference, unique for this requester forever.
    pub request_reference: String,
    /// The redemption's on-chain reference, `0x`-prefixed hexadecimal (`REQ-OT-2`).
    pub redemption_reference: String,
    /// The withdrawal's expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// The payout instructions, `0x`-prefixed hexadecimal of their encoded bytes, whose BLAKE2b-256
    /// must equal the redemption's commitment (`REQ-RQ-9`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

/// Request the refund of an unassociated arrival (`REQ-RQ-6`). It waits in `Received` for the
/// operator's second approver (`REQ-IN-6`).
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RefundRequest {
    /// The requester's reference, unique for this requester forever.
    pub request_reference: String,
    /// The coordinate of the unassociated arrival: an observation at a reserve its endpoint
    /// recorded as kind `arrival` (whatever its amount against the route's bounds, whoever sent
    /// it), `other_arrival` (a token not admitted), `convertible_arrival` or `conversion_output`,
    /// final and not yet consumed (`REQ-RQ-6`, `REQ-IN-6`, `INV-2`). Otherwise
    /// `ERR-SourceNotFound`, recorded. One observed but not final yet is answered `not-final`
    /// (HTTP 503), nothing recorded: send the same request again once it is final.
    pub coordinate: CoordinateBody,
    /// Where the value is returned: an address on the arrival's network (`REQ-OT-4`).
    pub destination: DestinationBody,
}

/// A milestone: the coordinate a state change rests on, and when bezent saw it (`REQ-TR-2`,
/// `REQ-TR-4`, `NFR-3`).
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct Milestone {
    /// The network.
    pub network: String,
    /// The block's number.
    pub block_number: i64,
    /// The block's hash.
    pub block_hash: String,
    /// The block's time.
    pub block_time: DateTime<Utc>,
    /// When bezent saw it.
    pub seen_at: DateTime<Utc>,
}

/// A coordinate in an answer.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct CoordinateOut {
    /// The network.
    pub network: String,
    /// The block's number.
    pub block_number: i64,
    /// The block's hash.
    pub block_hash: String,
    /// The movement's position in the block.
    pub position: PositionBody,
}

/// Where a transfer stands (`REQ-RQ-1`, `REQ-RQ-8`, `REQ-ST-2`). Observations, not verdicts
/// (`REQ-ST-3`): included, final and completed are separate milestones (`REQ-TR-4`).
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct TransferStatus {
    /// The request reference the transfer carries.
    pub request_reference: String,
    /// bezent's reference (`REQ-ID-3`).
    pub bridge_reference: String,
    /// `deposit`, `withdrawal` or `refund`.
    pub kind: String,
    /// The state (§5.4).
    pub state: String,
    /// The named condition held, if any (§10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// The named error of a refusal (§10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// What the named error means for this request, when it is worth saying
    /// (`ERR-InvalidDestination`: a destination that is malformed, or one the bridge does not
    /// deliver to). Derived from `error`, so a repeat of the request answers alike.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The bridged asset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
    /// The origin token, only on the refund of an unassociated arrival whose token is not
    /// admitted: such a refund names no bridged asset (`REQ-RQ-6`, `REQ-IN-6`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// The amount, as a decimal string (`INV-11`): in the asset's book unit, or in the origin
    /// token's minor units when the transfer names a token instead of an asset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,
    /// The coordinate of the leg that funds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CoordinateOut>,
    /// Where it is delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<DestinationBody>,
    /// Where it is refunded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refund_destination: Option<DestinationBody>,
    /// Its expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// The source leg is final.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_final: Option<Milestone>,
    /// The destination leg is included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_included: Option<Milestone>,
    /// The destination leg is final.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_final: Option<Milestone>,
}

/// The transfers bound to a coordinate that this requester may read.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct TransferList {
    /// The transfers.
    pub transfers: Vec<TransferStatus>,
}

/// A coordinate in a query string.
#[derive(Clone, Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct CoordinateQuery {
    /// The network.
    pub network: String,
    /// The block's number.
    pub block_number: u64,
    /// The block's hash, `0x`-prefixed hexadecimal.
    pub block_hash: String,
    /// `transaction` or `log`.
    pub position_kind: PositionKind,
    /// The index in the block.
    pub position_index: u32,
}

/// A route's window limit and how much of it is used (`REQ-LM-1`).
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct WindowOut {
    /// The most the route carries in one window, in the origin token's minor units.
    pub limit: String,
    /// The window's length, in seconds.
    pub seconds: i64,
    /// What the trailing window has used.
    pub used: String,
    /// What is left of it.
    pub remaining: String,
}

/// A route's capacity: transfers it carries at once, not yet terminal.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct CapacityOut {
    /// The declared capacity.
    pub declared: i64,
    /// Transfers in flight now.
    pub in_flight: i64,
    /// What is left: the current capacity.
    pub available: i64,
}

/// One route of the route table (`REQ-AS-5`, `REQ-LM-1`).
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct RouteOut {
    /// The bridged asset.
    pub asset: String,
    /// The origin network.
    pub network: String,
    /// `deposit` or `withdrawal`.
    pub direction: String,
    /// The minimum per transfer, in the origin token's minor units.
    pub minimum: String,
    /// The maximum per transfer.
    pub maximum: String,
    /// Any minimum a venue imposes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub venue_minimum: Option<String>,
    /// The window limit.
    pub window: WindowOut,
    /// The capacity.
    pub capacity: CapacityOut,
    /// The cut-offs, as declared.
    #[schema(value_type = Object)]
    pub cutoffs: serde_json::Value,
    /// The venue addresses, for venue routes.
    #[schema(value_type = Object)]
    pub venue_addresses: serde_json::Value,
    /// The completion budget, in seconds (`NFR-2`).
    pub completion_budget_seconds: i64,
    /// Whether the route, its asset, its direction or the bridge is paused (`REQ-GV-1`).
    pub paused: bool,
}

/// The route table.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct RouteTable {
    /// Every declared route; a route not here does not exist (`REQ-AS-5`).
    pub routes: Vec<RouteOut>,
}

/// Reserve per (asset, network), as the reserve book holds it (`REQ-RQ-8`).
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct ReserveOut {
    /// The bridged asset.
    pub asset: String,
    /// The origin network.
    pub network: String,
    /// The reserve address (`REQ-BK-5`).
    pub address: String,
    /// The reserve book's balance, in the asset's book unit.
    pub balance: String,
}

/// Reserves.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct Reserves {
    /// One per declared reserve.
    pub reserves: Vec<ReserveOut>,
}

/// The reserve equation's latest evaluation, as stored (`REQ-RQ-8`). None is stored until the
/// reserve check exists (`F-004`).
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct ReserveEquation {
    /// The latest evaluation, or null.
    #[schema(value_type = Option<Object>)]
    pub latest: Option<serde_json::Value>,
}

/// A read of the sequenced log.
#[derive(Clone, Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct LogQuery {
    /// Read positions after this one (0 reads from the start).
    #[serde(default)]
    pub after: i64,
    /// The most positions to cover; at most the configured page.
    #[serde(default)]
    pub limit: Option<i64>,
}

/// One entry of the sequenced log. Its position is its stable key (`REQ-ST-4`, `REQ-ST-5`).
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct LogEntry {
    /// The position, gapless across the log.
    pub seq: i64,
    /// `transfer`, `transition` or `refusal`.
    pub kind: String,
    /// What changed.
    #[schema(value_type = Object)]
    pub payload: serde_json::Value,
    /// When it was appended.
    pub appended_at: DateTime<Utc>,
}

/// A page of the sequenced log: this requester's entries in positions (`after`, `next_after`].
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct LogPage {
    /// The entries, in order.
    pub entries: Vec<LogEntry>,
    /// The position to read after next time.
    pub next_after: i64,
    /// The log's last position now.
    pub head: i64,
}

/// The health answer.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct Health {
    /// `ok` when the store answers.
    pub status: String,
}

/// Checks a request reference: non-empty and bounded.
pub fn check_reference(reference: &str) -> Result<(), ApiError> {
    if reference.is_empty() || reference.len() > MAX_REFERENCE_LEN {
        return Err(ApiError::Malformed(format!(
            "a request reference has 1 to {MAX_REFERENCE_LEN} bytes (REQ-RQ-1)"
        )));
    }
    Ok(())
}

/// Parses `0x`-prefixed, non-empty hexadecimal.
pub fn parse_hex(field: &str, text: &str) -> Result<Vec<u8>, ApiError> {
    let digits = text
        .strip_prefix("0x")
        .ok_or_else(|| ApiError::Malformed(format!("{field} is 0x-prefixed hexadecimal")))?;
    let bytes = hex::decode(digits)
        .map_err(|_| ApiError::Malformed(format!("{field} is 0x-prefixed hexadecimal")))?;
    if bytes.is_empty() {
        return Err(ApiError::Malformed(format!("{field} is empty")));
    }
    Ok(bytes)
}

/// `0x`-prefixed lowercase hexadecimal.
pub fn to_hex(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

fn check_destination(field: &str, d: &DestinationBody) -> Result<(), ApiError> {
    if d.network.is_empty() || d.address.is_empty() {
        return Err(ApiError::Malformed(format!(
            "{field} names a network and an address"
        )));
    }
    Ok(())
}

impl CoordinateBody {
    /// The coordinate, parsed (block number within the store's range; hash non-empty).
    pub fn parse(&self) -> Result<Coordinate, ApiError> {
        let network = NetworkId::new(self.network.clone())
            .map_err(|_| ApiError::Malformed("coordinate.network is empty".into()))?;
        if i64::try_from(self.block_number).is_err() {
            return Err(ApiError::Malformed(
                "coordinate.block_number is out of range".into(),
            ));
        }
        let hash = parse_hex("coordinate.block_hash", &self.block_hash)?;
        Ok(Coordinate {
            network,
            block_number: self.block_number,
            block_hash: BlockHash::new(hash),
            position: match self.position.kind {
                PositionKind::Transaction => Position::Transaction(self.position.index),
                PositionKind::Log => Position::Log(self.position.index),
            },
        })
    }

    fn canonical(&self) -> Result<Self, ApiError> {
        let c = self.parse()?;
        Ok(Self {
            block_hash: to_hex(c.block_hash.as_bytes()),
            ..self.clone()
        })
    }
}

impl CoordinateQuery {
    /// The same coordinate as a body.
    pub fn body(&self) -> CoordinateBody {
        CoordinateBody {
            network: self.network.clone(),
            block_number: self.block_number,
            block_hash: self.block_hash.clone(),
            position: PositionBody {
                kind: self.position_kind,
                index: self.position_index,
            },
        }
    }
}

impl DepositRequest {
    /// The request checked for form and put in canonical form: what is recorded and compared on
    /// a repeat (`REQ-ID-1`).
    pub fn canonical(&self) -> Result<Self, ApiError> {
        check_reference(&self.request_reference)?;
        check_destination("destination", &self.destination)?;
        check_destination("refund_destination", &self.refund_destination)?;
        Ok(Self {
            coordinate: self.coordinate.canonical()?,
            ..self.clone()
        })
    }
}

impl RefundRequest {
    /// The request checked for form and put in canonical form.
    pub fn canonical(&self) -> Result<Self, ApiError> {
        check_reference(&self.request_reference)?;
        check_destination("destination", &self.destination)?;
        Ok(Self {
            coordinate: self.coordinate.canonical()?,
            ..self.clone()
        })
    }
}

impl WithdrawalExtrasRequest {
    /// The request checked for form and put in canonical form.
    pub fn canonical(&self) -> Result<Self, ApiError> {
        check_reference(&self.request_reference)?;
        if self.expires_at.is_none() && self.instructions.is_none() {
            return Err(ApiError::Malformed(
                "a withdrawal's extras name an expiry or instructions (REQ-RQ-4)".into(),
            ));
        }
        let reference = parse_hex("redemption_reference", &self.redemption_reference)?;
        let instructions = match &self.instructions {
            Some(i) => Some(to_hex(&parse_hex("instructions", i)?)),
            None => None,
        };
        Ok(Self {
            redemption_reference: to_hex(&reference),
            instructions,
            ..self.clone()
        })
    }
}

/// The explanation that goes with a named refusal (`REQ-IN-5`). It names no vendor and no ledger.
pub fn refusal_detail(error: Option<&str>) -> Option<String> {
    match error {
        Some("ERR-InvalidDestination") => Some(
            "the destination or refund destination is malformed for its network, or is a contract \
             account that holds a ledger's value, which bezent does not deliver to: name an \
             account of your own on the network, and deliver into the ledger from there"
                .to_owned(),
        ),
        _ => None,
    }
}
