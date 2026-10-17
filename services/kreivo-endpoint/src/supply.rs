// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Each bridged asset's supply at each final head the endpoint records (issue #42; `REQ-BK-8`,
//! `NFR-5`, `INV-1`, `REQ-OB-5`; PLAN `DEC-16`).
//!
//! The reserve check reads supply at a final point of the issuing network, and the core holds no
//! network connection (`DEC-3`): the endpoint reads it. At the finalized head, through the light
//! client (every storage read and runtime call it serves is proven, `DEC-5`), it reads two things
//! per bridged asset:
//!
//! - **the on-chain component's own count**, its `supply(asset)` message, by a dry run;
//! - **the asset's issuance**, `pallet-assets`' `Asset::supply` of `Here(index)`.
//!
//! Both are recorded. The issuance is the reading (§5.3: Supply is the total of the asset on the
//! issuing network); when the two differ the reading is flagged `mismatch`: value was minted or
//! destroyed outside the component, a breach of `INV-4` (PLAN `DEC-16`, 2026-10-15). The flag is
//! committed with the head, and the core raises `key-compromise` and pauses the asset from it
//! (PR #50 review M1). A read that fails is recorded as unknown with its reason, never
//! a guess, and the check takes the evaluation as incomplete.
//!
//! The head is recorded in `final_heads` in the same transaction as its readings, so a final point
//! the check picks has its supply readings with it.

use serde_json::{Value, json};
use sqlx::PgPool;

use crate::chain::{BlockRef, Chain};
use crate::kreivo::{self, AccountId32, AssetId};

/// Records supply at final heads. See the module documentation.
pub struct Supplies<'a, C> {
    /// The chain.
    pub chain: &'a C,
    /// The store, as the Kreivo endpoint's role.
    pub pool: &'a PgPool,
    /// The network id.
    pub network: &'a str,
    /// The on-chain component.
    pub contract: AccountId32,
}

/// What one recording did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// The head and its readings were recorded.
    At {
        /// The head's number.
        block: u64,
        /// How many readings, known or not.
        readings: usize,
    },
    /// The head was already recorded, with this hash: nothing to do.
    Already(u64),
    /// The store holds another hash at the head's height (`REQ-OB-2`): nothing was recorded.
    Conflict(u64),
    /// The chain could not be read: nothing was recorded.
    Unavailable(String),
}

/// One supply reading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The bridged asset.
    pub asset: String,
    /// Its issuing-network identity, as the store spells it (`Here(101)`).
    pub subject: String,
    /// The issuance, or `None` when unknown.
    pub amount: Option<u128>,
    /// Why it is unknown.
    pub unknown_reason: Option<String>,
    /// Whether the component's count differs from the issuance.
    pub mismatch: bool,
    /// What each source answered.
    pub detail: Value,
}

impl<C: Chain> Supplies<'_, C> {
    /// Records the supply of every bridged asset at the finalized head.
    pub async fn record(&self) -> Result<Recorded, sqlx::Error> {
        match self.chain.finalized().await {
            Ok(head) => self.record_at(&head).await,
            Err(e) => Ok(Recorded::Unavailable(e.to_string())),
        }
    }

    /// Records `head`, a final block, and the supply of every bridged asset at it, in one
    /// transaction.
    pub async fn record_at(&self, head: &BlockRef) -> Result<Recorded, sqlx::Error> {
        let number = i64_of(head.number)?;
        let stored: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT block_hash FROM final_heads WHERE network = $1 AND block_number = $2",
        )
        .bind(self.network)
        .bind(number)
        .fetch_optional(self.pool)
        .await?;
        match stored {
            Some(h) if h == head.hash => return Ok(Recorded::Already(head.number)),
            Some(_) => return Ok(Recorded::Conflict(head.number)),
            None => {}
        }
        let time_ms = match self.chain.events(head, &self.contract).await {
            Ok(e) => e.time_ms,
            Err(e) => return Ok(Recorded::Unavailable(e.to_string())),
        };
        let assets: Vec<(String, String)> = sqlx::query_as(
            "SELECT asset, issuing_asset FROM assets WHERE issuing_asset IS NOT NULL ORDER BY asset",
        )
        .fetch_all(self.pool)
        .await?;
        let mut readings = Vec::with_capacity(assets.len());
        for (asset, subject) in assets {
            readings.push(self.read(head, asset, subject).await);
        }

        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
             VALUES ($1, $2, $3, to_timestamp(0) + $4 * interval '1 millisecond') \
             ON CONFLICT ON CONSTRAINT final_heads_one_hash_per_height DO NOTHING",
        )
        .bind(self.network)
        .bind(number)
        .bind(head.hash.as_slice())
        .bind(i64_of(time_ms)?)
        .execute(&mut *tx)
        .await?;
        let stored: Vec<u8> = sqlx::query_scalar(
            "SELECT block_hash FROM final_heads WHERE network = $1 AND block_number = $2",
        )
        .bind(self.network)
        .bind(number)
        .fetch_one(&mut *tx)
        .await?;
        if stored != head.hash {
            return Ok(Recorded::Conflict(head.number));
        }
        for r in &readings {
            sqlx::query(
                "INSERT INTO reserve_readings (network, block_number, block_hash, kind, subject, \
                     holder, amount, unknown_reason, mismatch, detail) \
                 VALUES ($1, $2, $3, 'supply', $4, '', $5::numeric, $6, $7, $8::text::jsonb) \
                 ON CONFLICT ON CONSTRAINT reserve_readings_once DO NOTHING",
            )
            .bind(self.network)
            .bind(number)
            .bind(head.hash.as_slice())
            .bind(&r.subject)
            .bind(r.amount.map(|a| a.to_string()))
            .bind(&r.unknown_reason)
            .bind(r.mismatch)
            .bind(r.detail.to_string())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;

        // INV-4 (PLAN DEC-16, 2026-10-15; PR #50 review M1): the flag is durable in the store,
        // committed with the head, and the core raises key-compromise and pauses the asset from
        // it, once per mismatch, whether or not this process lives to say anything more.
        for r in readings.iter().filter(|r| r.mismatch) {
            tracing::error!(
                network = self.network,
                asset = %r.asset,
                block = head.number,
                detail = %r.detail,
                "INV-4: the on-chain component's supply count and the asset's issuance differ: value was minted or destroyed outside the component; recorded for the core's key-compromise path"
            );
        }
        Ok(Recorded::At {
            block: head.number,
            readings: readings.len(),
        })
    }

    /// `asset`'s supply at `head`: the component's count and the issuance.
    pub async fn read(&self, head: &BlockRef, asset: String, subject: String) -> Reading {
        let unknown = |why: String, detail: Value| Reading {
            asset: asset.clone(),
            subject: subject.clone(),
            amount: None,
            unknown_reason: Some(why),
            mismatch: false,
            detail,
        };
        let Some(id) = AssetId::parse(&subject) else {
            return unknown(
                format!("{subject} is not a local asset of the issuing network"),
                json!({}),
            );
        };
        let AssetId::Here(index) = id;
        let args = kreivo::contracts_api_call_args(
            &self.contract,
            &self.contract,
            &kreivo::supply_data(id),
        );
        let counted = match self.chain.contracts_api_call(head, &args).await {
            Ok(answer) => kreivo::decode_getter(&answer).and_then(|v| {
                kreivo::decode_exact::<u128>(&v)
                    .ok_or_else(|| "the component's supply does not decode".to_owned())
            }),
            Err(e) => Err(e.to_string()),
        };
        let issuance = self
            .chain
            .asset_issuance(head, index)
            .await
            .map_err(|e| e.to_string());
        match (counted, issuance) {
            (Ok(c), Ok(Some(i))) => Reading {
                asset: asset.clone(),
                subject: subject.clone(),
                amount: Some(i),
                unknown_reason: None,
                mismatch: c != i,
                detail: json!({"contract_supply": c.to_string(), "issuance": i.to_string()}),
            },
            (Ok(c), Ok(None)) => unknown(
                format!("{subject} does not exist on the issuing network"),
                json!({"contract_supply": c.to_string()}),
            ),
            (Err(e), _) => unknown(format!("the component's supply count: {e}"), json!({})),
            (_, Err(e)) => unknown(format!("the asset's issuance: {e}"), json!({})),
        }
    }
}

fn i64_of(n: u64) -> Result<i64, sqlx::Error> {
    i64::try_from(n).map_err(|e| sqlx::Error::Encode(Box::new(e)))
}
