// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The reserve book, the reserve check and the statement (`F-004`; SPEC §5.3, §4.6; PLAN `DEC-16`).
//!
//! For a bridged asset *A*, at one final point per network:
//!
//! ```text
//! Σ Reserve(A) + InTransit(A) = Supply(A) + OwedMint(A) + OwedPayout(A) + Surplus(A)
//! ```
//!
//! - [`book`] (`T-004-01`): the double-entry, append-only book per asset (`REQ-BK-7`, `INV-17`).
//!   Every movement the core records is posted once, from the final observation that proves it,
//!   resting on that observation's block, so the book can be read as of any set of final points.
//!   Reserve and value in transit are debit-normal; supply, owed mints, owed payouts and surplus
//!   are credit-normal, so a balanced entry keeps the equation.
//! - [`readings`] (`T-004-02`): what the check reads from the networks and reports at a final
//!   point: supply, reserve balances, convertible holdings, an attesting party's positions. The
//!   endpoints implement it; tests use a fake (`REQ-BK-8`, `REQ-BE-4`).
//! - [`check`] (`T-004-03`, `T-004-04`): the evaluation, once a minute per asset at zero tolerance,
//!   re-deriving every term from the networks and comparing it with the book (`REQ-BK-1`,
//!   `REQ-BK-8`, `NFR-5`, `INV-1`). A divergence pauses the asset's routes and pages a human
//!   (`REQ-IC-2`); a surplus near or under its floor alerts (`REQ-BK-10`).
//! - [`statement`] (`T-004-05`): the daily statement per asset, signed with a key used for nothing
//!   else, chained by hash and kept in an append-only store (`REQ-PR-1`, `REQ-PR-2`, `REQ-BE-3`,
//!   `REQ-BK-9`, `REQ-CV-4`).
//!
//! # Units (`INV-11`, `REQ-AS-4`; F-001 NOTES S6)
//!
//! Every book amount is in the asset's **book unit**, the minor unit of its finest origin token.
//! Issuing-network amounts (supply, mints, redemptions, refund mints) are multiplied by the declared
//! ratio before they are posted or compared.

pub mod book;
pub mod check;
pub mod readings;
pub mod s3;
pub mod statement;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sqlx::PgConnection;

use crate::refs::Coordinate;

/// The key an item of owed mint is known by: its arrival's coordinate, as text anyone can look up
/// on the network (`REQ-BK-2`).
pub fn coordinate_key(c: &Coordinate) -> String {
    format!(
        "{}:{}:0x{}:{}:{}",
        c.network,
        c.block_number,
        hex::encode(&c.block_hash),
        c.position_kind,
        c.position_index
    )
}

/// `10^exp`, if it fits.
pub(crate) fn pow10(exp: i16) -> Option<u128> {
    10u128.checked_pow(u32::try_from(exp).ok()?)
}

/// A final point on a network: what a reading and a book cut rest on (`REQ-OB-5`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalPoint {
    /// The network.
    pub network: String,
    /// The block number.
    pub block_number: i64,
    /// The block hash.
    #[serde(with = "hex0x")]
    pub block_hash: Vec<u8>,
}

/// `0x` hex for bytes in JSON.
pub(crate) mod hex0x {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(b: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("0x{}", hex::encode(b)))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        hex::decode(text.strip_prefix("0x").unwrap_or(&text)).map_err(serde::de::Error::custom)
    }
}

/// A reserve position of an asset: its network, the address that holds it, how the network is
/// backed, and the origin token admitted there (`REQ-AS-2`, `REQ-AS-8`, `REQ-BK-5`).
#[derive(Clone, Debug)]
pub struct Position {
    /// The origin network, or the attested backend's network.
    pub network: String,
    /// The reserve address, as declared and published.
    pub address: String,
    /// `evm:<chain id>` or `attested:<id>` (`networks.backend`).
    pub backend: Option<String>,
    /// The origin token admitted for the asset on this network, with its precision.
    pub token: Option<(String, i16)>,
    /// The addresses the operator declared it funds this reserve from (`REQ-BK-5`): a final
    /// arrival from one of them, at or after the declaration's block, is a surplus contribution;
    /// any other arrival is owed mint.
    pub funding: Vec<Funding>,
    /// Whether the network's addresses are EVM addresses, compared without regard to case; any
    /// other format is compared exactly (SS58 is case-significant).
    pub evm: bool,
}

/// An operator funding address of a reserve (`REQ-BK-5`; migrations 0031, 0032).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Funding {
    /// The address.
    pub address: String,
    /// The first block the declaration applies to. `None` (declared before migration 0032)
    /// applies to nothing: a declaration is never retroactive (review SF3).
    pub from_block: Option<i64>,
}

impl Position {
    /// Whether an arrival from `sender` at `block` is from one of the operator's funding addresses
    /// here, declared at or before `block`: the rule of the store's `funding_arrival()`, which
    /// also keeps any deposit from binding such an arrival (review MF1).
    pub fn funded_by(&self, sender: Option<&str>, block: i64) -> bool {
        sender.is_some_and(|s| {
            self.funding.iter().any(|f| {
                f.from_block.is_some_and(|b| block >= b)
                    && (f.address == s || (self.evm && f.address.eq_ignore_ascii_case(s)))
            })
        })
    }

    /// Whether this position's facts are an attesting party's reports (`REQ-BE-2`).
    pub fn attested(&self) -> bool {
        self.backend
            .as_deref()
            .is_some_and(|b| b.starts_with("attested:"))
    }
}

/// An asset as the reserve check reads its declarations.
#[derive(Clone, Debug)]
pub struct Asset {
    /// The bridged asset.
    pub asset: String,
    /// Its identity on the issuing network.
    pub issuing_asset: Option<String>,
    /// Its precision on the issuing network.
    pub precision: i16,
    /// `public` or `attested` (`REQ-BE-2`).
    pub assurance_class: String,
    /// The precision of the book unit: its finest admitted origin token's.
    pub book_precision: i16,
    /// Its reserve positions.
    pub positions: Vec<Position>,
    /// The surplus floor and the alert level, in book units (`REQ-BK-10`).
    pub surplus_floor: u128,
    /// The level below which the surplus alert fires; at least the floor.
    pub surplus_alert: u128,
    /// Whether the operator declared both the floor and the alert level (`REQ-BK-10`: "a floor it
    /// declares"). A missing one alerts (review S6).
    pub levels_declared: bool,
    /// The issuing network.
    pub issuing_network: Option<String>,
}

impl Asset {
    /// The factor an issuing-network amount is multiplied by to reach the book unit.
    pub fn issuing_ratio(&self) -> Option<u128> {
        pow10(self.book_precision - self.precision)
    }

    /// The factor an amount of an origin token of `precision` is multiplied by.
    pub fn token_ratio(&self, precision: i16) -> Option<u128> {
        pow10(self.book_precision - precision)
    }

    /// The position on `network`.
    pub fn position(&self, network: &str) -> Option<&Position> {
        self.positions.iter().find(|p| p.network == network)
    }

    /// Reads the declarations of `asset`.
    pub async fn load(conn: &mut PgConnection, asset: &str) -> sqlx::Result<Option<Self>> {
        #[allow(clippy::type_complexity)]
        let row: Option<(
            String,
            Option<String>,
            i16,
            String,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT asset, issuing_asset, precision, assurance_class, \
                    surplus_floor::text, surplus_alert::text FROM assets WHERE asset = $1",
        )
        .bind(asset)
        .fetch_optional(&mut *conn)
        .await?;
        let Some((asset, issuing_asset, precision, assurance_class, floor, alert)) = row else {
            return Ok(None);
        };
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            String,
            String,
            Option<String>,
            Option<String>,
            Option<i16>,
            bool,
        )> = sqlx::query_as(
            "SELECT r.network, r.address, n.backend, k.token, k.precision, \
                    n.address_format = 'evm' \
             FROM reserves r JOIN networks n ON n.network = r.network \
             LEFT JOIN admitted_tokens k ON k.network = r.network AND k.asset = r.asset \
             WHERE r.asset = $1 ORDER BY r.network, k.token",
        )
        .bind(&asset)
        .fetch_all(&mut *conn)
        .await?;
        let book_precision: Option<i16> =
            sqlx::query_scalar("SELECT max(precision) FROM admitted_tokens WHERE asset = $1")
                .bind(&asset)
                .fetch_one(&mut *conn)
                .await?;
        let issuing_network: Option<String> =
            sqlx::query_scalar("SELECT network FROM networks WHERE role = 'issuing'")
                .fetch_optional(&mut *conn)
                .await?;
        let funding: Vec<(String, String, Option<i64>)> = sqlx::query_as(
            "SELECT network, address, from_block FROM surplus_funding_addresses WHERE asset = $1 \
             ORDER BY network, address",
        )
        .bind(&asset)
        .fetch_all(&mut *conn)
        .await?;
        let positions = rows
            .into_iter()
            .map(|(network, address, backend, token, p, evm)| Position {
                funding: funding
                    .iter()
                    .filter(|(n, _, _)| *n == network)
                    .map(|(_, a, b)| Funding {
                        address: a.clone(),
                        from_block: *b,
                    })
                    .collect(),
                evm,
                network,
                address,
                backend,
                token: token.zip(p),
            })
            .collect();
        let levels_declared = floor.is_some() && alert.is_some();
        let floor: u128 = floor.and_then(|f| f.parse().ok()).unwrap_or(0);
        let alert: u128 = alert.and_then(|a| a.parse().ok()).unwrap_or(floor);
        Ok(Some(Self {
            asset,
            issuing_asset,
            precision,
            assurance_class,
            book_precision: book_precision.unwrap_or(precision).max(precision),
            positions,
            surplus_floor: floor,
            surplus_alert: alert.max(floor),
            levels_declared,
            issuing_network,
        }))
    }
}

/// An item of an itemised term (`REQ-BK-2`): what it is, its amount in book units, and the
/// coordinate anyone can check it at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// The item: an arrival's coordinate (owed mint) or a withdrawal's bridge reference (owed
    /// payout).
    pub item: String,
    /// Its amount, in book units, as decimal text (`INV-11`).
    pub amount: String,
    /// The coordinate it rests on, when it is a movement on chain.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub coordinate: Option<String>,
}

/// The terms of the reserve equation for one asset (`REQ-BK-1`, `REQ-BK-2`): reserve per position,
/// the itemised terms, supply and surplus, in book units. Signed, so a book that went wrong shows
/// as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Terms {
    /// Reserve per network.
    pub reserve: BTreeMap<String, i128>,
    /// Value in transit, per venue leg.
    pub in_transit: BTreeMap<String, i128>,
    /// Supply on the issuing network.
    pub supply: i128,
    /// Owed mints, per arrival.
    pub owed_mint: BTreeMap<String, i128>,
    /// Owed payouts, per withdrawal.
    pub owed_payout: BTreeMap<String, i128>,
    /// Surplus.
    pub surplus: i128,
}

impl Terms {
    /// `Σ Reserve + InTransit − Supply − OwedMint − OwedPayout`: the surplus the other terms imply.
    pub fn implied_surplus(&self) -> i128 {
        self.reserve.values().sum::<i128>() + self.in_transit.values().sum::<i128>()
            - self.supply
            - self.owed_mint.values().sum::<i128>()
            - self.owed_payout.values().sum::<i128>()
    }
}

/// An amount as decimal text.
pub(crate) fn text(n: i128) -> String {
    n.to_string()
}

/// A map of amounts as JSON, decimal text.
pub(crate) fn map_json(m: &BTreeMap<String, i128>) -> serde_json::Value {
    serde_json::Value::Object(
        m.iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(text(*v))))
            .collect(),
    )
}
