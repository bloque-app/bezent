// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The chain-event driver: **TEST only** (PLAN `DEC-19`, `pl-pr-e2e` ruling, 2026-10-15).
//!
//! The sidecar has no chain behind it. The driver is an HTTP surface, on a port of its own, through
//! which another repository's harness makes the networks do things: an arrival at the reserve, a
//! redemption on the issuing network, their finality, the inclusion or the proven non-execution of
//! a delivery the core ordered, and the balance and supply readings the reserve check reads. It
//! **plays the endpoints, never the requester** (guardrail 3, `REQ-RQ-7`): it writes what an
//! endpoint would have seen, as that endpoint's login, through the store's own rules, and the core
//! decides everything from there as it would on a real network. It asserts no transfer's outcome,
//! writes no transfer state, and records no intent.
//!
//! # Test only, three times over
//!
//! 1. It exists only inside the sidecar, which refuses to start unless
//!    `BEZENT_SIDECAR_ENVIRONMENT=test` and every network in the store is one of its two test
//!    networks (`REQ-KY-6`), and stops when any other appears.
//! 2. It is bound only when `BEZENT_SIDECAR_DRIVER_TOKEN` is set, and [`start`] refuses to bind it on
//!    a store that declares a network outside the sidecar's test networks, by the store's own rule
//!    ([`bezent_store::environment`], shared with the core).
//! 3. Every call checks that rule again before it writes anything, and answers `404 absent` when a
//!    production network is declared: the driver then does not exist.
//!
//! Every call needs `Authorization: Bearer <BEZENT_SIDECAR_DRIVER_TOKEN>`.
//!
//! # Writes, and whose
//!
//! The issuing network's rows go through a login holding `bezent_kreivo_endpoint`, the origin
//! network's through one holding `bezent_evm_endpoint` (migrations 0034, 0035): one pool per login,
//! never a role switched on a pooled connection. Every observation rests on a block of the
//! driver's chain ([`chain`]) and every outcome, proof and reading on a final head recorded first.

pub mod chain;
pub mod standin;

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use bezent_signer::config::Config as SignerConfig;
use bezent_signer::keys::{FileBackend, KeyBackend};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use self::chain::{Block, Observed, hex0x};
use self::standin::Lane;
use crate::setup::{self, ASSET, ISSUING, ORIGIN, SERVED};

/// The shortest driver token taken.
pub const MIN_TOKEN: usize = 16;

/// Why a call was not done.
#[derive(Debug)]
pub enum Error {
    /// The store declares a production network: the driver does not exist (`REQ-KY-6`).
    Absent(String),
    /// No valid driver token.
    Unauthorised,
    /// The body is not what the call takes.
    Malformed(String),
    /// Not yet: what the call needs is not there (a signed transaction, a final head).
    NotReady(String),
    /// It would contradict what is recorded.
    Conflict(String),
    /// The store refused the write by one of its rules (SQLSTATE `BZ001`).
    Refused(String),
    /// The store failed.
    Store(String),
}

impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        match &e {
            sqlx::Error::Database(d)
                if d.code().as_deref() == Some(bezent_store::REFUSAL_SQLSTATE) =>
            {
                Error::Refused(d.message().to_owned())
            }
            _ => Error::Store(e.to_string()),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (code, detail) = self.parts();
        write!(f, "{code}: {detail}")
    }
}

impl Error {
    fn parts(&self) -> (&'static str, &str) {
        match self {
            Error::Absent(d) => ("absent", d),
            Error::Unauthorised => ("unauthorised", "Authorization: Bearer <driver token>"),
            Error::Malformed(d) => ("malformed", d),
            Error::NotReady(d) => ("not-ready", d),
            Error::Conflict(d) => ("conflict", d),
            Error::Refused(d) => ("refused", d),
            Error::Store(d) => ("store", d),
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            Error::Absent(_) => StatusCode::NOT_FOUND,
            Error::Unauthorised => StatusCode::UNAUTHORIZED,
            Error::Malformed(_) => StatusCode::BAD_REQUEST,
            Error::NotReady(_) | Error::Conflict(_) | Error::Refused(_) => StatusCode::CONFLICT,
            Error::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (code, detail) = self.parts();
        (
            self.status(),
            axum::Json(json!({"error": code, "detail": detail})),
        )
            .into_response()
    }
}

/// The test signer's link, as the driver's stand-in endpoints reach it.
#[derive(Clone)]
pub struct SignerLink {
    /// Its socket.
    pub socket: PathBuf,
    /// The caller token.
    pub token: String,
    /// The signer's user id.
    pub uid: u32,
}

/// The driver.
pub struct Driver {
    kreivo: PgPool,
    evm: PgPool,
    lookup: PgPool,
    token_sha256: [u8; 32],
    link: SignerLink,
    lanes: HashMap<(String, String), Lane>,
    dry_run_failures: StdMutex<HashMap<String, VecDeque<String>>>,
    lock: Mutex<()>,
}

impl std::fmt::Debug for Driver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Driver").finish_non_exhaustive()
    }
}

impl Driver {
    /// The driver over `kreivo` (a login holding `bezent_kreivo_endpoint`) and `evm` (a login
    /// holding `bezent_evm_endpoint`), taking calls with `token`, its stand-in endpoints signing
    /// through `link`. A call names a transfer by its bridge reference, which no endpoint reads
    /// (#35: the endpoints' roles hold no SELECT on `transfers`); `lookup`, the core's login,
    /// resolves it, read only. Every write is the endpoints' own.
    pub fn new(
        kreivo: PgPool,
        evm: PgPool,
        lookup: PgPool,
        token: &str,
        link: SignerLink,
    ) -> Result<Self, String> {
        if token.len() < MIN_TOKEN {
            return Err(format!(
                "the driver token is at least {MIN_TOKEN} characters"
            ));
        }
        let config = SignerConfig::parse(setup::SIGNER_CONFIG).map_err(|e| e.to_string())?;
        let keys = FileBackend::parse(setup::TEST_KEYS).map_err(|e| e.to_string())?;
        let mut lanes = HashMap::new();
        for b in &config.bindings {
            let public = keys.public_key(&b.key).map_err(|e| e.to_string())?;
            let sender = if b.network == ORIGIN {
                bezent_evm_endpoint::evm::address_of(&public)
                    .ok_or("the payout key is not a secp256k1 key")?
                    .to_vec()
            } else {
                public
            };
            for p in &b.purposes {
                lanes.insert(
                    (b.network.clone(), p.clone()),
                    Lane {
                        key: b.key.clone(),
                        sender: sender.clone(),
                    },
                );
            }
        }
        Ok(Self {
            kreivo,
            evm,
            lookup,
            token_sha256: Sha256::digest(token.as_bytes()).into(),
            link,
            lanes,
            dry_run_failures: StdMutex::default(),
            lock: Mutex::new(()),
        })
    }

    /// The pool of `network`'s endpoint login.
    fn pool(&self, network: &str) -> Result<&PgPool, Error> {
        match network {
            ISSUING => Ok(&self.kreivo),
            ORIGIN => Ok(&self.evm),
            other => Err(Error::Malformed(format!(
                "network {other:?} is not one of the sidecar's ({})",
                SERVED.join(", ")
            ))),
        }
    }

    /// The test-network rule, read from the store before anything is written: any network
    /// outside the sidecar's makes the driver absent (`REQ-KY-6`).
    pub async fn guard(&self) -> Result<(), Error> {
        setup::only_test_networks(&self.kreivo)
            .await
            .map_err(Error::Absent)
    }

    fn authorised(&self, header: Option<&str>) -> bool {
        let Some(token) = header.and_then(|h| h.strip_prefix("Bearer ")) else {
            return false;
        };
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        // Compared in full, whatever differs first.
        digest
            .iter()
            .zip(self.token_sha256.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }

    /// Makes the next preparation on `network` fail its dry run with `reason`, once.
    pub fn fail_next_dry_run(&self, network: &str, reason: &str) -> Result<(), Error> {
        self.pool(network)?;
        if let Ok(mut q) = self.dry_run_failures.lock() {
            q.entry(network.to_owned())
                .or_default()
                .push_back(reason.to_owned());
        }
        Ok(())
    }

    fn next_dry_run_failure(&self, network: &str) -> Option<String> {
        self.dry_run_failures
            .lock()
            .ok()
            .and_then(|mut q| q.get_mut(network).and_then(VecDeque::pop_front))
    }

    // ---------------------------------------------------------------------------------------
    // The calls.

    /// An arrival at the reserve on the origin network, observed and not yet final.
    pub async fn arrival(&self, body: ArrivalBody) -> Result<Value, Error> {
        let amount = chain::amount(&body.amount)?;
        let _one = self.lock.lock().await;
        self.guard().await?;
        let mut tx = self.evm.begin().await?;
        let token = match body.token {
            Some(t) => t,
            None => admitted_token(&mut tx, ASSET).await?,
        };
        let admitted: Option<String> = sqlx::query_scalar(
            "SELECT asset FROM admitted_tokens WHERE network = $1 AND token = $2",
        )
        .bind(ORIGIN)
        .bind(&token)
        .fetch_optional(&mut *tx)
        .await?;
        let reserve = reserve(&mut tx, admitted.as_deref().unwrap_or(ASSET)).await?;
        let (event, kind) = if admitted.is_some() {
            ("Arrival", "arrival")
        } else {
            ("OtherArrival", "other_arrival")
        };
        let sender = body
            .sender
            .unwrap_or_else(|| "0x00000000000000000000000000000000000000de".to_owned());
        let block = chain::new_block(&mut tx, ORIGIN, 1).await?;
        chain::observe(
            &mut tx,
            &block,
            &Observed {
                kind,
                token: Some(token.clone()),
                amount: Some(amount),
                sender: Some(sender.clone()),
                recipient: Some(reserve.clone()),
                reference: None,
                detail: json!({"event": event}),
            },
        )
        .await?;
        tx.commit().await?;
        Ok(json!({
            "coordinate": block.coordinate(0),
            "kind": kind,
            "token": token,
            "amount": amount.to_string(),
            "sender": sender,
            "recipient": reserve,
            "final": false,
        }))
    }

    /// A redemption on the issuing network, observed and not yet final.
    pub async fn redemption(&self, body: RedemptionBody) -> Result<Value, Error> {
        let amount = chain::amount(&body.amount)?;
        let redeemer = bezent_core_service::refs::account32(&body.redeemer)
            .map(|a| hex0x(&a))
            .ok_or_else(|| {
                Error::Malformed(format!(
                    "redeemer {:?} is not an SS58 address or 0x and 64 hex digits",
                    body.redeemer
                ))
            })?;
        let payout = chain::hex_bytes(&body.payout_address, "payout_address")?;
        if payout.len() != 20 {
            return Err(Error::Malformed(
                "payout_address is not an EVM address".to_owned(),
            ));
        }
        let reference = match &body.reference {
            Some(r) => chain::hex_bytes(r, "reference")?,
            None => setup::random_secret()
                .ok()
                .and_then(|h| hex::decode(h).ok())
                .ok_or_else(|| Error::Store("no randomness".to_owned()))?,
        };
        let _one = self.lock.lock().await;
        self.guard().await?;
        let mut tx = self.kreivo.begin().await?;
        let issuing_asset: Option<String> =
            sqlx::query_scalar("SELECT issuing_asset FROM assets WHERE asset = $1")
                .bind(ASSET)
                .fetch_optional(&mut *tx)
                .await?
                .flatten();
        let issuing_asset = issuing_asset
            .ok_or_else(|| Error::NotReady(format!("{ASSET} has no issuing identity declared")))?;
        let backend: Option<String> =
            sqlx::query_scalar("SELECT backend FROM networks WHERE network = $1")
                .bind(ORIGIN)
                .fetch_optional(&mut *tx)
                .await?
                .flatten();
        let backend = match backend.as_deref().and_then(|b| b.strip_prefix("evm:")) {
            Some(id) => json!({"kind": "evm", "chain_id": id.parse::<u64>().unwrap_or_default()}),
            None => {
                return Err(Error::NotReady(format!(
                    "{ORIGIN} has no EVM backend declared"
                )));
            }
        };
        let block = chain::new_block(&mut tx, ISSUING, 1).await?;
        chain::observe(
            &mut tx,
            &block,
            &Observed {
                kind: "redemption",
                token: Some(issuing_asset.clone()),
                amount: Some(amount),
                sender: Some(redeemer.clone()),
                recipient: None,
                reference: Some(reference.clone()),
                detail: json!({
                    "event": "Redeemed",
                    "backend": backend,
                    "payout": {"address": hex0x(&payout)},
                }),
            },
        )
        .await?;
        tx.commit().await?;
        Ok(json!({
            "coordinate": block.coordinate(0),
            "kind": "redemption",
            "asset": issuing_asset,
            "amount": amount.to_string(),
            "redeemer": redeemer,
            "reference": hex0x(&reference),
            "payout_address": hex0x(&payout),
            "bridge_reference": bezent_core_service::refs::withdrawal(&redeemer, &reference),
            "final": false,
        }))
    }

    /// Finality on `network`: its tip becomes a final head, every block up to it read, and the
    /// observation at `coordinate` (or every one still open) decided at it.
    pub async fn finality(&self, body: FinalityBody) -> Result<Value, Error> {
        let pool = self.pool(&body.network)?;
        let outcome = body.outcome.as_deref().unwrap_or("final");
        if !matches!(outcome, "final" | "withdrawn") {
            return Err(Error::Malformed("outcome is final or withdrawn".to_owned()));
        }
        if outcome == "withdrawn" && body.coordinate.is_none() {
            return Err(Error::Malformed(
                "a withdrawn outcome names its coordinate".to_owned(),
            ));
        }
        let _one = self.lock.lock().await;
        self.guard().await?;
        let mut tx = pool.begin().await?;
        let head = chain::tip_or_new(&mut tx, &body.network).await?;
        chain::finalise(&mut tx, &head).await?;
        let open: Vec<(i64, i64, Vec<u8>, i64)> = match &body.coordinate {
            Some(c) => {
                if c.network != body.network {
                    return Err(Error::Malformed(
                        "the coordinate is on another network".to_owned(),
                    ));
                }
                let hash = chain::hex_bytes(&c.block_hash, "block_hash")?;
                let row: Option<(i64, bool)> = sqlx::query_as(
                    "SELECT o.id, EXISTS (SELECT 1 FROM observation_outcomes x \
                                          WHERE x.observation_id = o.id) \
                     FROM observations o WHERE o.network = $1 AND o.block_number = $2 \
                       AND o.block_hash = $3 AND o.position_kind = $4::position_kind \
                       AND o.position_index = $5",
                )
                .bind(&c.network)
                .bind(c.block_number)
                .bind(&hash)
                .bind(&c.position.kind)
                .bind(c.position.index)
                .fetch_optional(&mut *tx)
                .await?;
                match row {
                    None => {
                        return Err(Error::NotReady(
                            "no observation at that coordinate".to_owned(),
                        ));
                    }
                    Some((_, true)) => {
                        return Err(Error::Conflict(
                            "that observation is already decided".to_owned(),
                        ));
                    }
                    Some((id, false)) => {
                        vec![(id, c.block_number, hash, c.position.index)]
                    }
                }
            }
            None => {
                sqlx::query_as(
                    "SELECT o.id, o.block_number, o.block_hash, o.position_index \
                     FROM observations o WHERE o.network = $1 AND o.block_number <= $2 \
                       AND NOT EXISTS (SELECT 1 FROM observation_outcomes x \
                                       WHERE x.observation_id = o.id) \
                     ORDER BY o.id",
                )
                .bind(&body.network)
                .bind(head.number)
                .fetch_all(&mut *tx)
                .await?
            }
        };
        let mut decided = Vec::new();
        for (id, number, hash, index) in open {
            chain::decide(&mut tx, id, outcome, &head).await?;
            decided.push(json!({
                "coordinate": {
                    "network": body.network,
                    "block_number": number,
                    "block_hash": hex0x(&hash),
                    "position": {"kind": "log", "index": index},
                },
                "outcome": outcome,
            }));
        }
        tx.commit().await?;
        Ok(json!({"final_head": head.json(), "decided": decided}))
    }

    /// The signed transaction of the transfer's current delivery attempt that is not proven
    /// not executed: (signed id, purpose, network, reference, amount, order, transfer asset,
    /// transfer destination, transfer redeemer).
    async fn signed_delivery(&self, bridge_reference: &str) -> Result<SignedDelivery, Error> {
        let row: Option<SignedDelivery> = sqlx::query_as(
            "SELECT st.id, st.tx_hash, i.purpose, i.network, i.on_chain_reference, \
                    i.payload ->> 'amount', o.payload -> 'order', t.asset, t.destination, \
                    t.redeemer \
             FROM transfers t \
             JOIN intents i ON i.transfer_id = t.id AND i.payload ? 'prepared' \
             JOIN signed_transactions st ON st.intent_id = i.id \
             JOIN prepared_transactions p ON p.id::text = i.payload ->> 'prepared' \
             JOIN intents o ON o.id = p.order_intent_id \
             WHERE t.bridge_reference = $1 \
               AND NOT EXISTS (SELECT 1 FROM non_execution_proofs n \
                               WHERE n.signed_transaction_id = st.id) \
             ORDER BY i.attempt DESC, st.id DESC LIMIT 1",
        )
        .bind(bridge_reference)
        .fetch_optional(&self.lookup)
        .await?;
        row.ok_or_else(|| {
            Error::NotReady(format!(
                "transfer {bridge_reference:?} has no signed delivery awaiting inclusion: \
                 wait until it is Delivering"
            ))
        })
    }

    /// The delivery the core ordered for `bridge_reference`, signed and recorded, included on its
    /// network: the leg the core matches by the reference the delivery writes (`REQ-OT-2`).
    pub async fn include(&self, body: IncludeBody) -> Result<Value, Error> {
        let override_amount = body.amount.as_deref().map(chain::amount).transpose()?;
        let _one = self.lock.lock().await;
        self.guard().await?;
        let (
            signed,
            tx_hash,
            purpose,
            network,
            reference,
            amount,
            order,
            asset,
            destination,
            redeemer,
        ) = self.signed_delivery(&body.bridge_reference).await?;
        let Some(reference) = reference else {
            return Err(Error::NotReady(
                "the delivery carries no reference".to_owned(),
            ));
        };
        let pool = self.pool(&network)?;
        let mut tx = pool.begin().await?;
        let (event, kind) = match purpose.as_str() {
            "mint" => ("Minted", "mint"),
            "refund-mint" => ("RefundMinted", "refund_mint"),
            _ => ("Payout", "payout_leg"),
        };
        let included: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM observations o \
               LEFT JOIN observation_outcomes x ON x.observation_id = o.id \
               WHERE o.network = $1 AND o.on_chain_reference = $2 AND o.kind = $3 \
                 AND x.outcome IS DISTINCT FROM 'withdrawn')",
        )
        .bind(&network)
        .bind(&reference)
        .bind(kind)
        .fetch_one(&mut *tx)
        .await?;
        if included {
            return Err(Error::Conflict(format!(
                "a {kind} carrying this delivery's reference is already observed"
            )));
        }
        let order = order.unwrap_or_default();
        let amount = match override_amount {
            Some(a) => a,
            None => chain::amount(amount.as_deref().unwrap_or_default())?,
        };
        let asset = asset.unwrap_or_else(|| ASSET.to_owned());
        let (token, sender, recipient) = match purpose.as_str() {
            "mint" => (
                order["asset"].as_str().map(str::to_owned),
                None,
                order["to"].as_str().map(str::to_owned),
            ),
            "refund-mint" => {
                let issuing: Option<String> =
                    sqlx::query_scalar("SELECT issuing_asset FROM assets WHERE asset = $1")
                        .bind(&asset)
                        .fetch_optional(&mut *tx)
                        .await?
                        .flatten();
                let to = order["redeemer"]
                    .as_str()
                    .map(str::to_owned)
                    .or(redeemer)
                    .and_then(|r| bezent_core_service::refs::account32(&r))
                    .map(|a| hex0x(&a));
                (issuing, None, to)
            }
            _ => (
                Some(admitted_token(&mut tx, &asset).await?),
                Some(reserve(&mut tx, &asset).await?),
                order["to"]
                    .as_str()
                    .map(str::to_owned)
                    .or(Some(destination)),
            ),
        };
        let recipient = body.recipient.or(recipient);
        let block = chain::new_block(&mut tx, &network, 1).await?;
        chain::observe(
            &mut tx,
            &block,
            &Observed {
                kind,
                token,
                amount: Some(amount),
                sender,
                recipient: recipient.clone(),
                reference: Some(reference.clone()),
                detail: json!({
                    "event": event,
                    "signed_transaction_id": signed,
                    "tx_hash": tx_hash.as_deref().map(hex0x),
                }),
            },
        )
        .await?;
        tx.commit().await?;
        Ok(json!({
            "coordinate": block.coordinate(0),
            "kind": kind,
            "purpose": purpose,
            "amount": amount.to_string(),
            "recipient": recipient,
            "reference": hex0x(&reference),
            "signed_transaction_id": signed,
            "final": false,
        }))
    }

    /// Every signed transaction of the transfer's current attempt proven not executed: its nonce
    /// not consumed when its window ended, at a final head past the window (`REQ-NX-1`,
    /// `REQ-NX-2`). The core then makes the next attempt, or the refund.
    pub async fn not_executed(&self, body: NotExecutedBody) -> Result<Value, Error> {
        let _one = self.lock.lock().await;
        self.guard().await?;
        type Unproven = (i64, String, Vec<u8>, i64, i64, i64, Option<Vec<u8>>);
        let rows: Vec<Unproven> = sqlx::query_as(
            "SELECT st.id, i.network, p.lane, p.nonce, p.valid_from_block, p.valid_until_block, \
                    i.on_chain_reference \
             FROM transfers t \
             JOIN intents i ON i.transfer_id = t.id AND i.payload ? 'prepared' \
             JOIN signed_transactions st ON st.intent_id = i.id \
             JOIN prepared_transactions p ON p.id::text = i.payload ->> 'prepared' \
             WHERE t.bridge_reference = $1 \
               AND i.attempt = (SELECT max(i2.attempt) FROM intents i2 \
                                JOIN signed_transactions s2 ON s2.intent_id = i2.id \
                                WHERE i2.transfer_id = t.id) \
               AND NOT EXISTS (SELECT 1 FROM non_execution_proofs n \
                               WHERE n.signed_transaction_id = st.id) \
             ORDER BY st.id",
        )
        .bind(&body.bridge_reference)
        .fetch_all(&self.lookup)
        .await?;
        let Some(network) = rows.first().map(|r| r.1.clone()) else {
            return Err(Error::NotReady(format!(
                "transfer {:?} has no signed delivery left to prove not executed",
                body.bridge_reference
            )));
        };
        let pool = self.pool(&network)?;
        let mut tx = pool.begin().await?;
        // An attempt `include` confirmed executed: the test chain never shows it both executed
        // and provably not executed, so a later attempt could not deliver twice.
        for reference in rows.iter().filter_map(|r| r.6.as_ref()) {
            let included: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM observations o \
                   LEFT JOIN observation_outcomes x ON x.observation_id = o.id \
                   WHERE o.network = $1 AND o.on_chain_reference = $2 \
                     AND o.kind IN ('mint', 'refund_mint', 'payout_leg') \
                     AND x.outcome IS DISTINCT FROM 'withdrawn')",
            )
            .bind(&network)
            .bind(reference)
            .fetch_one(&mut *tx)
            .await?;
            if included {
                return Err(Error::Conflict(
                    "this attempt's delivery is already included; it cannot be proven not executed"
                        .to_owned(),
                ));
            }
        }
        let until = rows.iter().map(|r| r.5).max().unwrap_or_default();
        let tip = chain::tip_or_new(&mut tx, &network).await?;
        let head = if tip.number >= until {
            tip
        } else {
            chain::new_block(&mut tx, &network, until).await?
        };
        chain::finalise(&mut tx, &head).await?;
        let mut proofs = Vec::new();
        for (signed, _, lane, nonce, from, until, _) in rows {
            sqlx::query(
                "INSERT INTO non_execution_proofs (signed_transaction_id, network, signer, nonce, \
                   valid_from_block, valid_until_block, final_block_number, final_block_hash, \
                   nonce_at_final, kind, detail) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $4, 'nonce-unconsumed', $9::text::jsonb)",
            )
            .bind(signed)
            .bind(&network)
            .bind(&lane)
            .bind(nonce)
            .bind(from)
            .bind(until)
            .bind(head.number)
            .bind(&head.hash)
            .bind(json!({"driver": chain::DRIVER}).to_string())
            .execute(&mut *tx)
            .await?;
            proofs.push(json!({
                "signed_transaction_id": signed,
                "kind": "nonce-unconsumed",
                "nonce": nonce,
                "valid_until_block": until,
            }));
        }
        tx.commit().await?;
        Ok(json!({"final_head": head.json(), "proofs": proofs}))
    }

    /// Supply on the issuing network and the reserve's balance on the origin network, at the
    /// point the reserve check reads each at: the latest final head at or below the read
    /// watermark and below every observation still open. Each is what the driver's own final
    /// observations add up to there, unless `set` names it.
    pub async fn readings(&self, body: ReadingsBody) -> Result<Value, Error> {
        let networks: Vec<&str> = match body.network.as_deref() {
            Some(n) => {
                self.pool(n)?;
                vec![if n == ISSUING { ISSUING } else { ORIGIN }]
            }
            None => vec![ISSUING, ORIGIN],
        };
        let mut set: HashMap<(String, String), u128> = HashMap::new();
        for s in body.set.unwrap_or_default() {
            set.insert(
                (s.subject, s.holder.unwrap_or_default()),
                chain::amount(&s.amount)?,
            );
        }
        let _one = self.lock.lock().await;
        self.guard().await?;
        let mut out = Vec::new();
        for network in networks {
            let pool = self.pool(network)?;
            let mut tx = pool.begin().await?;
            let point = check_point(&mut tx, network).await?;
            let (kind, derived) = if network == ISSUING {
                ("supply", supply_at(&mut tx, point.number).await?)
            } else {
                ("reserve", reserve_at(&mut tx, point.number).await?)
            };
            for (subject, holder, amount) in derived {
                let amount = set
                    .get(&(subject.clone(), holder.clone()))
                    .copied()
                    .map_or_else(
                        || {
                            u128::try_from(amount).map_err(|_| {
                                Error::Conflict(format!(
                                    "{network}'s {kind} of {subject} adds up below zero at block {}",
                                    point.number
                                ))
                            })
                        },
                        Ok,
                    )?;
                sqlx::query(
                    "INSERT INTO reserve_readings (network, block_number, block_hash, kind, \
                       subject, holder, amount, detail) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7::numeric, $8::text::jsonb) \
                     ON CONFLICT ON CONSTRAINT reserve_readings_once DO NOTHING",
                )
                .bind(network)
                .bind(point.number)
                .bind(&point.hash)
                .bind(kind)
                .bind(&subject)
                .bind(&holder)
                .bind(amount.to_string())
                .bind(json!({"driver": chain::DRIVER}).to_string())
                .execute(&mut *tx)
                .await?;
                let stored: Option<String> = sqlx::query_scalar(
                    "SELECT amount::text FROM reserve_readings WHERE network = $1 \
                       AND block_number = $2 AND kind = $3 AND subject = $4 AND holder = $5",
                )
                .bind(network)
                .bind(point.number)
                .bind(kind)
                .bind(&subject)
                .bind(&holder)
                .fetch_one(&mut *tx)
                .await?;
                out.push(json!({
                    "network": network,
                    "block_number": point.number,
                    "block_hash": hex0x(&point.hash),
                    "kind": kind,
                    "subject": subject,
                    "holder": holder,
                    "amount": stored,
                }));
            }
            tx.commit().await?;
        }
        Ok(json!({"readings": out}))
    }

    /// The router: every call behind the token.
    pub fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route("/driver/v0/arrivals", post(arrival))
            .route("/driver/v0/redemptions", post(redemption))
            .route("/driver/v0/finality", post(finality))
            .route("/driver/v0/deliveries/include", post(include))
            .route("/driver/v0/deliveries/not-executed", post(not_executed))
            .route("/driver/v0/dry-run-failures", post(dry_run_failure))
            .route("/driver/v0/readings", post(readings))
            .layer(middleware::from_fn_with_state(self.clone(), authenticate))
            .with_state(self)
    }
}

type SignedDelivery = (
    i64,
    Option<Vec<u8>>,
    String,
    String,
    Option<Vec<u8>>,
    Option<String>,
    Option<Value>,
    Option<String>,
    String,
    Option<String>,
);

/// The token `asset` is admitted as on the origin network.
async fn admitted_token(conn: &mut PgConnection, asset: &str) -> Result<String, Error> {
    sqlx::query_scalar(
        "SELECT token FROM admitted_tokens WHERE network = $1 AND asset = $2 ORDER BY token LIMIT 1",
    )
    .bind(ORIGIN)
    .bind(asset)
    .fetch_optional(conn)
    .await?
    .ok_or_else(|| Error::NotReady(format!("{asset} has no token admitted on {ORIGIN}")))
}

/// `asset`'s reserve address on the origin network.
async fn reserve(conn: &mut PgConnection, asset: &str) -> Result<String, Error> {
    sqlx::query_scalar("SELECT address FROM reserves WHERE network = $1 AND asset = $2")
        .bind(ORIGIN)
        .bind(asset)
        .fetch_optional(conn)
        .await?
        .ok_or_else(|| Error::NotReady(format!("{asset} has no reserve on {ORIGIN}")))
}

/// The final head the reserve check reads `network` at (`reserve::check`'s rule).
async fn check_point(conn: &mut PgConnection, network: &str) -> Result<Block, Error> {
    let row: Option<(i64, Vec<u8>, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT h.block_number, h.block_hash, h.block_time FROM final_heads h \
         WHERE h.network = $1 \
           AND h.block_number <= (SELECT watermark FROM observation_watermarks WHERE network = $1) \
           AND h.block_number < COALESCE((SELECT min(o.block_number) FROM observations o \
                 WHERE o.network = $1 AND NOT EXISTS (SELECT 1 FROM observation_outcomes x \
                                                      WHERE x.observation_id = o.id)), \
                 9223372036854775807) \
         ORDER BY h.block_number DESC LIMIT 1",
    )
    .bind(network)
    .fetch_optional(conn)
    .await?;
    let (number, hash, time) = row.ok_or_else(|| {
        Error::NotReady(format!(
            "{network} has no final head below its open observations: call finality first"
        ))
    })?;
    Ok(Block {
        network: network.to_owned(),
        number,
        hash,
        time,
    })
}

/// Each issuing asset's supply at block `at`: minted and refund-minted less redeemed, final.
async fn supply_at(conn: &mut PgConnection, at: i64) -> Result<Vec<(String, String, i128)>, Error> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT a.issuing_asset, COALESCE(sum(CASE o.kind WHEN 'redemption' THEN -o.amount \
                                                ELSE o.amount END), 0)::text \
         FROM assets a LEFT JOIN observations o ON o.network = $1 AND o.token = a.issuing_asset \
           AND o.executed AND o.block_number <= $2 \
           AND o.kind IN ('mint', 'refund_mint', 'redemption') \
           AND EXISTS (SELECT 1 FROM observation_outcomes x \
                       WHERE x.observation_id = o.id AND x.outcome = 'final') \
         WHERE a.issuing_asset IS NOT NULL GROUP BY a.issuing_asset ORDER BY 1",
    )
    .bind(ISSUING)
    .bind(at)
    .fetch_all(conn)
    .await?;
    parsed(rows.into_iter().map(|(s, a)| (s, String::new(), a)))
}

/// Each reserve address's balance of its admitted token at block `at`: what arrived less what
/// left, final.
async fn reserve_at(
    conn: &mut PgConnection,
    at: i64,
) -> Result<Vec<(String, String, i128)>, Error> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT k.token, r.address, \
             (COALESCE(sum(o.amount) FILTER (WHERE o.recipient = r.address), 0) \
              - COALESCE(sum(o.amount) FILTER (WHERE lower(o.sender) = lower(r.address)), 0))::text \
         FROM reserves r JOIN admitted_tokens k ON k.network = r.network AND k.asset = r.asset \
         LEFT JOIN observations o ON o.network = r.network AND o.token = k.token AND o.executed \
           AND o.block_number <= $2 \
           AND EXISTS (SELECT 1 FROM observation_outcomes x \
                       WHERE x.observation_id = o.id AND x.outcome = 'final') \
         WHERE r.network = $1 GROUP BY k.token, r.address ORDER BY 1, 2",
    )
    .bind(ORIGIN)
    .bind(at)
    .fetch_all(conn)
    .await?;
    parsed(rows.into_iter())
}

fn parsed(
    rows: impl Iterator<Item = (String, String, String)>,
) -> Result<Vec<(String, String, i128)>, Error> {
    rows.map(|(s, h, a)| {
        a.parse::<i128>()
            .map(|a| (s, h, a))
            .map_err(|_| Error::Store(format!("{a} is not an amount")))
    })
    .collect()
}

// -------------------------------------------------------------------------------------------
// HTTP.

/// `POST /driver/v0/arrivals`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArrivalBody {
    /// In the token's minor units, decimal.
    pub amount: String,
    /// From; a fixed test address if omitted.
    pub sender: Option<String>,
    /// The token as the store spells it; the asset's admitted token if omitted. A token that is
    /// not admitted arrives as `other_arrival`.
    pub token: Option<String>,
}

/// `POST /driver/v0/redemptions`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedemptionBody {
    /// The redeemer on the issuing network: SS58, or `0x` and 64 hex digits.
    pub redeemer: String,
    /// In the bridged asset's minor units on the issuing network, decimal.
    pub amount: String,
    /// The beneficiary on the origin network.
    pub payout_address: String,
    /// The redemption's on-chain reference, `0x` hex; 32 random bytes if omitted.
    pub reference: Option<String>,
}

/// A coordinate, as the ingress takes it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoordinateBody {
    /// The network.
    pub network: String,
    /// The block's number.
    pub block_number: i64,
    /// The block's hash, `0x` hex.
    pub block_hash: String,
    /// The position.
    pub position: PositionBody,
}

/// A position in a block.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PositionBody {
    /// `log` or `transaction`.
    pub kind: String,
    /// Its index.
    pub index: i64,
}

/// `POST /driver/v0/finality`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinalityBody {
    /// The network.
    pub network: String,
    /// The one observation to decide; every open one on the network if omitted.
    pub coordinate: Option<CoordinateBody>,
    /// `final` (the default) or `withdrawn` (with a coordinate).
    pub outcome: Option<String>,
}

/// `POST /driver/v0/deliveries/include`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncludeBody {
    /// The transfer's bridge reference.
    pub bridge_reference: String,
    /// What the leg moved, if not what was ordered (the core then sends it to review).
    pub amount: Option<String>,
    /// To whom, if not the ordered recipient (the core then sends it to review).
    pub recipient: Option<String>,
}

/// `POST /driver/v0/deliveries/not-executed`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotExecutedBody {
    /// The transfer's bridge reference.
    pub bridge_reference: String,
}

/// `POST /driver/v0/dry-run-failures`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DryRunFailureBody {
    /// The network whose next preparation fails.
    pub network: String,
    /// The refusal, as the network would word it.
    pub reason: String,
}

/// `POST /driver/v0/readings`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadingsBody {
    /// One network; both if omitted.
    pub network: Option<String>,
    /// Readings to record as given instead of as derived.
    pub set: Option<Vec<SetReading>>,
}

/// A reading given.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetReading {
    /// The issuing asset (supply) or the token (reserve).
    pub subject: String,
    /// The reserve address (reserve); empty for supply.
    pub holder: Option<String>,
    /// The amount, decimal minor units.
    pub amount: String,
}

type Shared = State<Arc<Driver>>;

/// The body of a call, its refusal a [`Error::Malformed`].
pub struct Body<T>(pub T);

impl<S: Send + Sync, T: serde::de::DeserializeOwned> axum::extract::FromRequest<S> for Body<T> {
    type Rejection = Error;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        axum::Json::<T>::from_request(req, state)
            .await
            .map(|axum::Json(v)| Self(v))
            .map_err(|r| Error::Malformed(r.body_text()))
    }
}

async fn authenticate(State(d): Shared, req: Request, next: Next) -> Response {
    let header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok());
    if !d.authorised(header) {
        return Error::Unauthorised.into_response();
    }
    next.run(req).await
}

fn answer(r: Result<Value, Error>) -> Response {
    match r {
        Ok(v) => (StatusCode::OK, axum::Json(v)).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "driver call not done");
            e.into_response()
        }
    }
}

async fn arrival(State(d): Shared, Body(b): Body<ArrivalBody>) -> Response {
    answer(d.arrival(b).await)
}

async fn redemption(State(d): Shared, Body(b): Body<RedemptionBody>) -> Response {
    answer(d.redemption(b).await)
}

async fn finality(State(d): Shared, Body(b): Body<FinalityBody>) -> Response {
    answer(d.finality(b).await)
}

async fn include(State(d): Shared, Body(b): Body<IncludeBody>) -> Response {
    answer(d.include(b).await)
}

async fn not_executed(State(d): Shared, Body(b): Body<NotExecutedBody>) -> Response {
    answer(d.not_executed(b).await)
}

async fn readings(State(d): Shared, Body(b): Body<ReadingsBody>) -> Response {
    answer(d.readings(b).await)
}

async fn dry_run_failure(State(d): Shared, Body(b): Body<DryRunFailureBody>) -> Response {
    let r = async {
        let _one = d.lock.lock().await;
        d.guard().await?;
        d.fail_next_dry_run(&b.network, &b.reason)?;
        Ok(json!({"network": b.network, "reason": b.reason, "next_preparation": "fails"}))
    }
    .await;
    answer(r)
}

/// A bound driver.
#[derive(Debug)]
pub struct Bound {
    /// Where it listens.
    pub local_addr: SocketAddr,
    listener: TcpListener,
    driver: Arc<Driver>,
}

/// Binds the driver at `listen`, only on a store whose every network is one of the sidecar's test
/// networks (`REQ-KY-6`); on any other it refuses, and nothing listens.
pub async fn start(driver: Arc<Driver>, listen: SocketAddr) -> Result<Bound, String> {
    driver
        .guard()
        .await
        .map_err(|e| format!("the chain-event driver is not bound: {e}"))?;
    let listener = TcpListener::bind(listen)
        .await
        .map_err(|e| format!("the driver cannot listen on {listen}: {e}"))?;
    let local_addr = listener
        .local_addr()
        .map_err(|e| format!("the driver's address: {e}"))?;
    Ok(Bound {
        local_addr,
        listener,
        driver,
    })
}

impl Bound {
    /// Serves until `shutdown`.
    pub async fn serve(
        self,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        axum::serve(self.listener, self.driver.router())
            .with_graceful_shutdown(shutdown)
            .await
    }
}
