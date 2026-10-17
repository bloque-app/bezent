// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reserve check's world (`F-004`): fake readings of the networks, a pager that remembers, a
//! clock the test moves, and helpers that make the observations the endpoints would.
//!
//! In these tests the observations table is the chain: [`sync_chain`] sets the fake readings to
//! what the recorded final observations add up to, as an endpoint would read them at a final
//! block, and a test seeds a divergence by changing a reading or by recording a movement no
//! transfer of bezent's explains.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bezent_core_service::reserve::FinalPoint;
use bezent_core_service::reserve::check::{Clock, Page, Pager, ReserveCheck};
use bezent_core_service::reserve::readings::{AttestedPosition, Holding, ReadError, Readings};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use sqlx::PgPool;

use super::*;

/// What the fake networks hold.
#[derive(Default, Debug)]
pub struct Chain {
    /// Supply per issuing asset, in the asset's minor units.
    pub supply: HashMap<String, u128>,
    /// Balance per (token, address), in the token's minor units.
    pub reserve: HashMap<(String, String), u128>,
    /// Convertible holdings per address.
    pub convertible: HashMap<String, Vec<Holding>>,
    /// Attested positions per (asset, network).
    pub attested: HashMap<(String, String), Vec<AttestedPosition>>,
    /// Every final point a reading was asked at.
    pub asked: Vec<FinalPoint>,
    /// When set, every reading fails.
    pub unreachable: bool,
}

/// Readings of the fake networks (`T-004-02`).
#[derive(Clone, Default, Debug)]
pub struct FakeReadings(pub Arc<Mutex<Chain>>);

impl FakeReadings {
    pub fn chain(&self) -> std::sync::MutexGuard<'_, Chain> {
        self.0.lock().unwrap()
    }
}

fn down<T>(c: &Chain, v: T) -> Result<T, ReadError> {
    if c.unreachable {
        Err(ReadError("the network is unreachable".into()))
    } else {
        Ok(v)
    }
}

impl Readings for FakeReadings {
    async fn supply(&self, at: &FinalPoint, issuing_asset: &str) -> Result<u128, ReadError> {
        let mut c = self.chain();
        c.asked.push(at.clone());
        let v = c.supply.get(issuing_asset).copied().unwrap_or(0);
        down(&c, v)
    }

    async fn reserve(
        &self,
        at: &FinalPoint,
        token: &str,
        address: &str,
    ) -> Result<u128, ReadError> {
        let mut c = self.chain();
        c.asked.push(at.clone());
        let v = c
            .reserve
            .get(&(token.to_owned(), address.to_owned()))
            .copied()
            .unwrap_or(0);
        down(&c, v)
    }

    async fn convertible(
        &self,
        _at: &FinalPoint,
        _asset: &str,
        address: &str,
    ) -> Result<Vec<Holding>, ReadError> {
        let c = self.chain();
        let v = c.convertible.get(address).cloned().unwrap_or_default();
        down(&c, v)
    }

    async fn attested(
        &self,
        asset: &str,
        network: &str,
    ) -> Result<Vec<AttestedPosition>, ReadError> {
        let c = self.chain();
        let v = c
            .attested
            .get(&(asset.to_owned(), network.to_owned()))
            .cloned()
            .unwrap_or_default();
        down(&c, v)
    }
}

/// A pager that remembers every page.
#[derive(Clone, Default, Debug)]
pub struct FakePager(pub Arc<Mutex<Vec<Page>>>);

impl FakePager {
    pub fn pages(&self) -> Vec<Page> {
        self.0.lock().unwrap().clone()
    }
    pub fn kinds(&self) -> Vec<String> {
        self.pages().into_iter().map(|p| p.kind).collect()
    }
}

impl Pager for FakePager {
    fn page(&self, page: &Page) {
        self.0.lock().unwrap().push(page.clone());
    }
}

/// A clock the test moves.
pub struct ManualClock(Mutex<DateTime<Utc>>);

impl ManualClock {
    pub fn at(t: DateTime<Utc>) -> Self {
        Self(Mutex::new(t))
    }
    pub fn advance(&self, by: Duration) {
        *self.0.lock().unwrap() += by;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

/// The check over the core's pool, with fake readings and pager.
pub fn checker(
    w: &World,
) -> (
    ReserveCheck<FakeReadings, FakePager>,
    FakeReadings,
    FakePager,
) {
    let readings = FakeReadings::default();
    let pager = FakePager::default();
    (
        ReserveCheck::new(w.core.pool().clone(), readings.clone(), pager.clone()),
        readings,
        pager,
    )
}

/// The endpoint read `network` up to `last` without a hole, under the final head `last`.
pub async fn read_through(pool: &PgPool, network: &str, last: i64) {
    head(pool, network, last).await;
    sqlx::query(
        "INSERT INTO observation_ranges (network, first_block, last_block, outcome, \
             final_block_number, final_block_hash) VALUES ($1, 0, $2, 'read', $2, $3)",
    )
    .bind(network)
    .bind(last)
    .bind(hash(network, last))
    .execute(pool)
    .await
    .unwrap();
}

/// Both networks read up to `last`.
pub async fn read_all(pool: &PgPool, last: i64) {
    read_through(pool, ISSUING, last).await;
    read_through(pool, ORIGIN, last).await;
}

/// Sets the fake readings to what the final observations add up to: supply per issuing asset,
/// and each reserve address's balance of its admitted token.
pub async fn sync_chain(pool: &PgPool, fake: &FakeReadings) {
    let supply: Vec<(String, String)> = sqlx::query_as(
        "SELECT a.issuing_asset, COALESCE(sum(CASE o.detail ->> 'event' \
             WHEN 'Redeemed' THEN -o.amount ELSE o.amount END), 0)::text \
         FROM assets a LEFT JOIN observations o ON o.token = a.issuing_asset AND o.executed \
           AND o.detail ->> 'event' IN ('Minted', 'RefundMinted', 'Redeemed') \
           AND EXISTS (SELECT 1 FROM observation_outcomes x \
                       WHERE x.observation_id = o.id AND x.outcome = 'final') \
         WHERE a.issuing_asset IS NOT NULL GROUP BY a.issuing_asset",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let reserve: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT k.token, r.address, \
             (COALESCE(sum(o.amount) FILTER (WHERE o.recipient = r.address), 0) \
              - COALESCE(sum(o.amount) FILTER (WHERE lower(o.sender) = lower(r.address)), 0))::text \
         FROM reserves r JOIN admitted_tokens k ON k.network = r.network AND k.asset = r.asset \
         LEFT JOIN observations o ON o.network = r.network AND o.token = k.token AND o.executed \
           AND EXISTS (SELECT 1 FROM observation_outcomes x \
                       WHERE x.observation_id = o.id AND x.outcome = 'final') \
         GROUP BY k.token, r.address",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let mut c = fake.chain();
    for (asset, s) in supply {
        c.supply.insert(asset, s.parse().unwrap());
    }
    for (token, address, b) in reserve {
        c.reserve.insert((token, address), b.parse().unwrap());
    }
}

/// An arrival at the reserve at (`ORIGIN`, `block`, log `index`), final two blocks later.
pub async fn final_arrival(pool: &PgPool, block: i64, index: i64, amount: u128) -> i64 {
    let id = observe(
        pool,
        ORIGIN,
        block,
        index,
        TOKEN,
        amount,
        Some("0xfunder"),
        Some(RESERVE),
        None,
        true,
        json!({}),
    )
    .await;
    decide(pool, id, "final", block + 2).await;
    id
}

/// The address the operator declares it funds the reserve from (`REQ-BK-5`).
pub const FUNDING: &str = "0x00000000000000000000000000000000000000f0";

/// Declares [`FUNDING`] as the operator's funding address of the reserve, as the owner does.
pub async fn declare_funding(pool: &PgPool) {
    exec(
        pool,
        &format!(
            "INSERT INTO surplus_funding_addresses (asset, network, address, reference) \
             VALUES ('{ASSET}', '{ORIGIN}', '{FUNDING}', 'funding-declaration-1') \
             ON CONFLICT DO NOTHING"
        ),
    )
    .await;
}

/// The operator funds the reserve with its own `amount` at `block`, from its declared funding
/// address, and the book posts it as surplus from the chain (`REQ-BK-5`, `REQ-BK-10`).
pub async fn fund(w: &World, block: i64, amount: u128) {
    declare_funding(&w.admin).await;
    let id = observe(
        &w.admin,
        ORIGIN,
        block,
        7,
        TOKEN,
        amount,
        Some(FUNDING),
        Some(RESERVE),
        None,
        true,
        json!({}),
    )
    .await;
    decide(&w.admin, id, "final", block + 2).await;
    bezent_core_service::reserve::book::post_pending(w.core.pool())
        .await
        .unwrap();
}

/// A deposit of `amount` minted in full: the arrival at `block`, the request, the delivery, and the
/// final `Minted` event at `block + 20`. Returns the transfer.
pub async fn minted_deposit(w: &World, block: i64, amount: u128) -> i64 {
    let a = arrival(&w.admin, block, amount).await;
    let id = deposit(&w.admin, block, amount).await;
    decide(&w.admin, a, "final", block + 2).await;
    let now = t0() + Duration::minutes(5);
    w.cycle(now).await;
    deliver(w, id, 1, now).await;
    let minted = observe(
        &w.admin,
        ISSUING,
        block + 20,
        0,
        ISSUING_ASSET,
        amount,
        None,
        Some(DESTINATION_ACCOUNT),
        Some(&reference_of(&w.admin, id).await),
        true,
        json!({"event": "Minted"}),
    )
    .await;
    decide(&w.admin, minted, "final", block + 22).await;
    w.settle(now).await;
    id
}

/// A redemption at `block` with `reference`, final two blocks later, and the withdrawal the core
/// makes of it.
pub async fn redeemed(w: &World, block: i64, reference: [u8; 32], amount: u128) -> i64 {
    let r = redemption(&w.admin, block, &reference, amount).await;
    decide(&w.admin, r, "final", block + 2).await;
    w.cycle(t0()).await;
    scalar(
        &w.admin,
        &format!(
            "SELECT id FROM transfers WHERE kind = 'withdrawal' AND on_chain_reference = '\\x{}'",
            hex::encode(reference)
        ),
    )
    .await
}

/// The payout leg of withdrawal `id`, out of the reserve, at (`ORIGIN`, `block`, log 3), final two
/// blocks later.
pub async fn paid_out(w: &World, id: i64, block: i64, amount: u128) -> i64 {
    let leg = observe(
        &w.admin,
        ORIGIN,
        block,
        3,
        TOKEN,
        amount,
        Some(RESERVE),
        Some(BENEFICIARY),
        Some(&payout_reference_of(&w.admin, id).await),
        true,
        json!({"event": "Payout"}),
    )
    .await;
    decide(&w.admin, leg, "final", block + 2).await;
    leg
}

/// The book's balance of (`account`, `item`) for `asset`, debits minus credits.
pub async fn balance(pool: &PgPool, asset: &str, account: &str, item: Option<&str>) -> i128 {
    let v: Option<String> = sqlx::query_scalar(
        "SELECT sum(CASE side WHEN 'debit' THEN amount ELSE -amount END)::text \
         FROM book_postings WHERE asset = $1 AND account = $2 \
           AND ($3::text IS NULL OR item = $3)",
    )
    .bind(asset)
    .bind(account)
    .bind(item)
    .fetch_one(pool)
    .await
    .unwrap();
    v.map_or(0, |v| v.parse().unwrap())
}
