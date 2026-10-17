// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Mints and refund mints: from the core's order to submitted bytes (`T-007-03`; `REQ-EO-1`,
//! `REQ-EO-4`, `REQ-HN-3`, `AC-I1.2`, `INV-9`; PLAN `DEC-3`, `DEC-5`).
//!
//! Only the core records intents (`INV-9`; F-005 review S5), and the signer signs an intent's
//! `message` as it is (F-005 NOTES, decision 2). The endpoint builds that message, since only it
//! reads the chain; so a delivery goes through two intents, both the core's:
//!
//! 1. **The order.** The core records an intent of purpose `mint` or `refund-mint` on this network,
//!    with the on-chain reference, and a payload `{"order": {…}, "amount": "<minor units>"}`:
//!    `{"asset": "Here(101)", "to": "0x<32 bytes>"}` for a mint, `{"redeemer": "0x<32 bytes>"}` for
//!    a refund mint. It has no `message`: the signer refuses to sign it.
//! 2. **Preparation** ([`Submitter::prepare`]). At the finalized head, the endpoint builds the call,
//!    takes the lane's next nonce, builds a mortal era born at that head (`MAX_ERA`, `DEC-5`), dry-runs
//!    the call through the light client as the delivery account, and records all of it, append-only,
//!    in `prepared_transactions`, the dry run's outcome included.
//! 3. **The signing intent** (the core's, `F-003`). For a prepared transaction whose dry run
//!    passed, the core records an intent of the same network, purpose and reference with the
//!    payload [`signing_intent_payload`]: the exact `message`, the `amount`, and the `prepared` id.
//! 4. **Signing and submission** ([`Submitter::sign_and_submit`]). The endpoint asks for a
//!    signature only for a signing intent whose message is a prepared transaction's, whose dry run
//!    passed, whose era is still live, and whose call moves what the order says. Before that, it
//!    decodes the whole signing payload and compares every part with the order, the lane and the
//!    chain (review M2, `Submitter::check_payload`): call, era, nonce, tip, extensions, versions,
//!    genesis and birth block; a mismatch is refused and recorded as an anomaly. It gets the
//!    signature over the link, which verifies it against the recorded bytes and starts the
//!    compromise path if it does not (`AC-I1.2`, the core's check, `Client::sign`), checks it is the
//!    lane's key, persists the exact signed extrinsic in `signed_transactions` (`REQ-EO-1`,
//!    `INV-9`), and only then submits it.
//! 5. **After a restart** ([`Submitter::resend`]): every signed transaction whose era is live and
//!    whose outcome is not known yet is re-sent as the same bytes; nothing is signed again for an
//!    intent that has a signed transaction (`REQ-EO-1`, `AC-D2.1`).
//!
//! **Exactly once** (`REQ-EO-1`): a new attempt for an order is prepared only while every earlier
//! one for the same reference is dead without a signature (its era passed unsigned). A signed one
//! blocks every new attempt until its non-execution is proven ([`crate::proof`]); a retry after that
//! is the core's to order, as a new order. The contract refuses a reference used twice either way
//! (`INV-2`, `AC-D2.2`).
//!
//! **Signing lane** (`REQ-EO-4`), declared by [`Lane::declaration`]: one lane per delivery key, the
//! key's account's nonce sequence. Every mint and refund mint of the network shares it, in nonce
//! order: each waits for the one before. A transaction whose outcome is unknown holds the lane for
//! at most its era ([`MAX_ERA`] blocks), after which its outcome is proven from chain data and the
//! nonce is taken again from the chain.

use std::path::PathBuf;

use serde_json::{Value, json};
use sqlx::PgPool;

use crate::chain::{BlockRef, Chain, ancestry};
use crate::kreivo::{
    self, AccountId32, AssetId, MAX_ERA, Message, Mortality, Reference, SPEC_VERSIONS, Scheme,
    TxParams, blake2_256,
};
use crate::signal;

/// The purposes the endpoint delivers.
pub const PURPOSES: &[&str] = &["mint", "refund-mint"];

/// The margin on a dry run's gas and deposit, in percent.
pub const LIMIT_MARGIN: u64 = 120;

/// A signing lane: one delivery key's account and its nonce sequence (`REQ-EO-4`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lane {
    /// The signer's id for the key.
    pub key: String,
    /// Its scheme.
    pub scheme: Scheme,
    /// Its public key.
    pub public_key: Vec<u8>,
    /// Its account: the lane.
    pub account: AccountId32,
}

impl Lane {
    /// The lane's declaration (`REQ-EO-4`): which transfers share it and whether order is kept.
    pub fn declaration(&self, network: &str) -> String {
        format!(
            "REQ-EO-4: on {network}, every mint and refund mint shares one signing lane, the nonce \
             sequence of the delivery key {} (account 0x{}); order is kept within it, by nonce; a \
             transaction whose outcome is unknown holds the lane for at most its era of {MAX_ERA} \
             blocks",
            self.key,
            hex::encode(self.account)
        )
    }
}

/// The signer link's coordinates. The token is never printed.
#[derive(Clone)]
pub struct SignerLink {
    /// The signer's socket.
    pub socket: PathBuf,
    /// The signer's user id.
    pub uid: u32,
    /// The endpoint's token.
    pub token: String,
}

impl std::fmt::Debug for SignerLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignerLink")
            .field("socket", &self.socket)
            .field("uid", &self.uid)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// An order, as the core recorded it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Order {
    /// The order intent's id.
    pub intent_id: i64,
    /// Its purpose.
    pub purpose: String,
    /// Its on-chain reference.
    pub reference: Reference,
    /// What it moves, in minor units.
    pub amount: u128,
    /// The contract message it asks for.
    pub message: Message,
}

fn account_field(order: &Value, field: &str) -> Result<AccountId32, String> {
    let s = order
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("order.{field} is missing"))?;
    crate::config::hex_bytes(s, field)
}

fn asset_field(order: &Value) -> Result<AssetId, String> {
    let s = order
        .get("asset")
        .and_then(Value::as_str)
        .ok_or("order.asset is missing")?;
    s.strip_prefix("Here(")
        .and_then(|r| r.strip_suffix(')'))
        .and_then(|n| n.parse::<u32>().ok())
        .map(AssetId::Here)
        .ok_or_else(|| format!("order.asset {s} is not Here(<u32>)"))
}

/// The amount field of an intent's payload: a decimal string of minor units (`INV-11`).
pub fn amount_field(payload: &Value) -> Result<u128, String> {
    payload
        .get("amount")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<u128>().ok())
        .ok_or_else(|| "amount is not a decimal string of minor units".into())
}

impl Order {
    /// Parses an order intent.
    pub fn parse(
        intent_id: i64,
        purpose: &str,
        reference: Option<&[u8]>,
        payload: &Value,
    ) -> Result<Order, String> {
        let reference: Reference = reference
            .and_then(|r| r.try_into().ok())
            .ok_or("the order has no 32-byte on-chain reference")?;
        let amount = amount_field(payload)?;
        let order = payload.get("order").ok_or("the intent is not an order")?;
        let message = match purpose {
            "mint" => Message::Mint {
                asset: asset_field(order)?,
                to: account_field(order, "to")?,
                amount,
                reference,
            },
            "refund-mint" => Message::RefundMint {
                redeemer: account_field(order, "redeemer")?,
                reference,
            },
            other => return Err(format!("purpose {other} is not delivered here")),
        };
        Ok(Order {
            intent_id,
            purpose: purpose.to_owned(),
            reference,
            amount,
            message,
        })
    }
}

/// The payload of the signing intent the core records for prepared transaction `prepared`
/// (`INV-9`: only the core records it; the signer signs `message` as it is).
pub fn signing_intent_payload(prepared: i64, message: &[u8], amount: u128) -> Value {
    json!({
        "message": format!("0x{}", hex::encode(message)),
        "amount": amount.to_string(),
        "prepared": prepared,
    })
}

/// What preparing an order did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prepared {
    /// A transaction was prepared and its dry run passed: the core may record its signing intent.
    Ready {
        /// The prepared transaction's id.
        id: i64,
        /// Its attempt.
        attempt: i32,
        /// Its nonce in the lane.
        nonce: u64,
        /// What the key will sign.
        message: Vec<u8>,
    },
    /// The dry run failed: recorded, and nothing will be signed for it (`DEC-5`).
    DryRunFailed {
        /// The prepared transaction's id.
        id: i64,
        /// Why.
        reason: String,
    },
    /// An earlier attempt for the same reference is still open: nothing new (`REQ-EO-1`).
    Open,
    /// The order cannot be prepared: malformed, or a runtime the endpoint has no format for.
    Refused(String),
    /// The chain did not answer; try again.
    Unavailable(String),
}

/// What signing and submitting a signing intent did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Submitted {
    /// The signed bytes were persisted, then submitted.
    Submitted {
        /// The `signed_transactions` row.
        id: i64,
        /// The extrinsic's hash.
        hash: [u8; 32],
    },
    /// The signed bytes were persisted; the submission failed and will be repeated.
    Persisted {
        /// The `signed_transactions` row.
        id: i64,
        /// Why the submission failed.
        reason: String,
    },
    /// Nothing was signed: the intent does not answer a passing dry run, its era passed, or its
    /// bytes do not move what the order says.
    Refused(String),
    /// The signer refused or was unavailable, or the link failed; nothing was signed or used.
    NotSigned(String),
}

/// Builds, signs and submits the network's deliveries. See the module documentation.
pub struct Submitter<'a, C> {
    /// The chain.
    pub chain: &'a C,
    /// The store, as the service role.
    pub pool: &'a PgPool,
    /// The network id.
    pub network: &'a str,
    /// The network's genesis hash, as declared and checked at start-up (`AC-I4.1`).
    pub genesis: [u8; 32],
    /// The contract.
    pub contract: AccountId32,
    /// The lane.
    pub lane: &'a Lane,
    /// The signer link.
    pub signer: &'a SignerLink,
}

fn i64_of(n: u64) -> Result<i64, sqlx::Error> {
    i64::try_from(n).map_err(|e| sqlx::Error::Encode(Box::new(e)))
}

fn u64_of(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

/// Whether a prepared transaction has a signed transaction, through a signing intent.
const SIGNED: &str = "EXISTS (SELECT 1 FROM intents si JOIN signed_transactions st ON st.intent_id = si.id \
     WHERE si.payload ->> 'prepared' = p.id::text)";

impl<C: Chain> Submitter<'_, C> {
    /// The orders on this network that may be prepared now: no attempt yet, or only attempts
    /// whose era passed unsigned. One whose dry run failed is not prepared again.
    pub async fn orders_to_prepare(&self, finalized: &BlockRef) -> Result<Vec<i64>, sqlx::Error> {
        sqlx::query_scalar(&format!(
            "SELECT o.id FROM intents o \
             WHERE o.network = $1 AND o.purpose = ANY($2) AND o.payload -> 'order' IS NOT NULL \
               AND NOT EXISTS (SELECT 1 FROM prepared_transactions p WHERE p.order_intent_id = o.id \
                 AND p.network = o.network \
                 AND (NOT p.dry_run_passed OR p.valid_until_block > $3 OR {SIGNED})) \
             ORDER BY o.id"
        ))
        .bind(self.network)
        .bind(PURPOSES)
        .bind(i64_of(finalized.number)?)
        .fetch_all(self.pool)
        .await
    }

    /// Prepares order `order_intent_id` at the finalized head. See the module documentation.
    pub async fn prepare(&self, order_intent_id: i64) -> Result<Prepared, sqlx::Error> {
        let row: Option<(String, String, Option<Vec<u8>>, String)> = sqlx::query_as(
            "SELECT network, purpose, on_chain_reference, payload::text FROM intents WHERE id = $1",
        )
        .bind(order_intent_id)
        .fetch_optional(self.pool)
        .await?;
        let Some((network, purpose, reference, payload)) = row else {
            return Ok(Prepared::Refused(format!(
                "no intent {order_intent_id} (INV-9)"
            )));
        };
        if network != self.network {
            return Ok(Prepared::Refused(format!(
                "intent {order_intent_id} is for {network}"
            )));
        }
        let payload: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
        let order = match Order::parse(order_intent_id, &purpose, reference.as_deref(), &payload) {
            Ok(o) => o,
            Err(e) => return Ok(Prepared::Refused(e)),
        };

        let finalized = match self.chain.finalized().await {
            Ok(f) => f,
            Err(e) => return Ok(Prepared::Unavailable(e.to_string())),
        };
        let version = match self.chain.runtime_version(&finalized).await {
            Ok(v) => v,
            Err(e) => return Ok(Prepared::Unavailable(e.to_string())),
        };
        if !SPEC_VERSIONS.contains(&version.spec_version) {
            return Ok(Prepared::Refused(format!(
                "Kreivo spec_version {} has no transaction format here (known: {SPEC_VERSIONS:?}); nothing is built for it",
                version.spec_version
            )));
        }

        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!(
                "kreivo-lane:{}:{}",
                self.network,
                hex::encode(self.lane.account)
            ))
            .execute(&mut *tx)
            .await?;

        // REQ-EO-1: no new attempt for the reference while an earlier one may still execute.
        let open: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS (SELECT 1 FROM prepared_transactions p \
               JOIN intents o ON o.id = p.order_intent_id \
               WHERE o.network = $1 AND o.purpose = $2 AND o.on_chain_reference = $3 \
                 AND p.network = o.network AND p.dry_run_passed \
                 AND ((NOT {SIGNED} AND p.valid_until_block > $4) \
                   OR ({SIGNED} AND NOT EXISTS (SELECT 1 FROM non_execution_proofs n \
                     JOIN signed_transactions st ON st.id = n.signed_transaction_id \
                     JOIN intents si ON si.id = st.intent_id \
                     WHERE si.payload ->> 'prepared' = p.id::text))))"
        ))
        .bind(self.network)
        .bind(&order.purpose)
        .bind(order.reference.as_slice())
        .bind(i64_of(finalized.number)?)
        .fetch_one(&mut *tx)
        .await?;
        if open {
            return Ok(Prepared::Open);
        }

        // The lane's next nonce: the chain's at the finalized head, or past every prepared
        // transaction of the lane that may still be included.
        let chain_nonce = match self
            .chain
            .account_nonce(&finalized, &self.lane.account)
            .await
        {
            Ok(n) => n,
            Err(e) => return Ok(Prepared::Unavailable(e.to_string())),
        };
        // Review S4: a nonce past everything bezent signed is the key used while the lane was idle.
        self.check_idle_use(chain_nonce).await?;
        let live: Option<i64> = sqlx::query_scalar(
            "SELECT max(nonce) FROM prepared_transactions \
             WHERE network = $1 AND lane = $2 AND dry_run_passed AND valid_until_block > $3",
        )
        .bind(self.network)
        .bind(self.lane.account.as_slice())
        .bind(i64_of(finalized.number)?)
        .fetch_one(&mut *tx)
        .await?;
        let nonce = live.map_or(chain_nonce, |n| chain_nonce.max(u64_of(n) + 1));
        let Ok(nonce32) = u32::try_from(nonce) else {
            return Ok(Prepared::Refused(
                "the lane's nonce is past Kreivo's u32".into(),
            ));
        };

        // The dry run, at the finalized head, as the delivery account (DEC-5).
        let data = order.message.data();
        let args = kreivo::contracts_api_call_args(&self.lane.account, &self.contract, &data);
        let dry_run = match self.chain.contracts_api_call(&finalized, &args).await {
            Ok(bytes) => match kreivo::decode_dry_run(&bytes) {
                Ok(d) => d,
                Err(e) => return Ok(Prepared::Unavailable(e)),
            },
            Err(e) => return Ok(Prepared::Unavailable(e.to_string())),
        };
        let call = kreivo::contracts_call(&self.contract, &dry_run.limits(LIMIT_MARGIN), &data);
        let era = Mortality::born_at(finalized.number);
        let params = TxParams {
            genesis: self.genesis,
            spec_version: version.spec_version,
            transaction_version: version.transaction_version,
            era,
            birth_hash: finalized.hash,
            nonce: nonce32,
            tip: 0,
        };
        let payload = kreivo::signing_payload(&call, &params);
        let message = kreivo::message_to_sign(&payload);

        let attempt: i32 = sqlx::query_scalar(
            "SELECT coalesce(max(attempt), 0) + 1 FROM prepared_transactions WHERE order_intent_id = $1",
        )
        .bind(order_intent_id)
        .fetch_one(&mut *tx)
        .await?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO prepared_transactions (order_intent_id, attempt, network, lane, key_id, \
               nonce, valid_from_block, valid_until_block, checkpoint_hash, call, signing_payload, \
               message, dry_run_block_number, dry_run_block_hash, dry_run_passed, dry_run) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $9, $14, $15::text::jsonb) \
             RETURNING id",
        )
        .bind(order_intent_id)
        .bind(attempt)
        .bind(self.network)
        .bind(self.lane.account.as_slice())
        .bind(&self.lane.key)
        .bind(i64_of(nonce)?)
        .bind(i64_of(era.birth)?)
        .bind(i64_of(era.last_valid())?)
        .bind(finalized.hash.as_slice())
        .bind(&call)
        .bind(&payload)
        .bind(&message)
        .bind(i64_of(finalized.number)?)
        .bind(dry_run.passed())
        .bind(dry_run.to_json().to_string())
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;

        if dry_run.passed() {
            tracing::info!(
                network = self.network,
                order = order_intent_id,
                prepared = id,
                nonce,
                valid_until = era.last_valid(),
                "prepared: the dry run passed; the core may record the signing intent"
            );
            Ok(Prepared::Ready {
                id,
                attempt,
                nonce,
                message,
            })
        } else {
            let reason = dry_run.to_json()["reason"]
                .as_str()
                .unwrap_or("the dry run failed")
                .to_owned();
            tracing::warn!(network = self.network, order = order_intent_id, prepared = id, reason = %reason, "DEC-5: the dry run failed; nothing will be signed for it");
            Ok(Prepared::DryRunFailed { id, reason })
        }
    }

    /// The signing intents on this network with no signed transaction yet, whose prepared
    /// transaction is still live at `finalized`.
    pub async fn intents_to_sign(&self, finalized: &BlockRef) -> Result<Vec<i64>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT si.id FROM intents si \
             JOIN prepared_transactions p ON p.id::text = si.payload ->> 'prepared' \
             WHERE si.network = $1 AND si.purpose = ANY($2) AND p.network = si.network \
               AND p.valid_until_block > $3 \
               AND NOT EXISTS (SELECT 1 FROM signed_transactions st WHERE st.intent_id = si.id) \
             ORDER BY p.nonce, si.id",
        )
        .bind(self.network)
        .bind(PURPOSES)
        .bind(i64_of(finalized.number)?)
        .fetch_all(self.pool)
        .await
    }

    /// Signs signing intent `intent_id`, persists the signed bytes, then submits them. See the
    /// module documentation.
    pub async fn sign_and_submit(&self, intent_id: i64) -> Result<Submitted, sqlx::Error> {
        type Row = (
            String,
            String,
            Option<Vec<u8>>,
            String,
            i64,
            i64,
            Vec<u8>,
            Vec<u8>,
            Vec<u8>,
            bool,
            i64,
            Vec<u8>,
            String,
            String,
            Option<Vec<u8>>,
            String,
        );
        let row: Option<Row> = sqlx::query_as(
            "SELECT si.network, si.purpose, si.on_chain_reference, si.payload::text, \
               p.id, p.valid_until_block, p.call, p.signing_payload, p.message, p.dry_run_passed, \
               o.id, p.lane, o.network, o.purpose, o.on_chain_reference, o.payload::text \
             FROM intents si \
             JOIN prepared_transactions p ON p.id::text = si.payload ->> 'prepared' \
             JOIN intents o ON o.id = p.order_intent_id \
             WHERE si.id = $1",
        )
        .bind(intent_id)
        .fetch_optional(self.pool)
        .await?;
        let Some((
            network,
            purpose,
            reference,
            payload,
            prepared_id,
            valid_until,
            call,
            signing_payload,
            prepared_message,
            passed,
            order_id,
            lane,
            order_network,
            order_purpose,
            order_reference,
            order_payload,
        )) = row
        else {
            return Ok(Submitted::Refused(format!(
                "intent {intent_id} names no prepared transaction"
            )));
        };
        // Nothing is signed without a passing dry run (DEC-5).
        if !passed {
            return Ok(Submitted::Refused(format!(
                "DEC-5: prepared transaction {prepared_id} did not pass its dry run"
            )));
        }
        let payload: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
        let message = payload
            .get("message")
            .and_then(Value::as_str)
            .and_then(|m| m.strip_prefix("0x"))
            .and_then(|m| hex::decode(m).ok());
        let order_payload: Value = serde_json::from_str(&order_payload).unwrap_or(Value::Null);
        let order = match Order::parse(
            order_id,
            &order_purpose,
            order_reference.as_deref(),
            &order_payload,
        ) {
            Ok(o) => o,
            Err(e) => return Ok(Submitted::Refused(e)),
        };
        // The signing intent answers its order and its prepared bytes, and nothing else.
        let mismatch = if network != self.network || order_network != self.network {
            Some("another network")
        } else if purpose != order_purpose || reference != order_reference {
            Some("another purpose or reference than its order's")
        } else if amount_field(&payload).ok() != Some(order.amount) {
            Some("another amount than its order's")
        } else if message.as_deref() != Some(prepared_message.as_slice()) {
            Some("another message than the prepared transaction's")
        } else if lane != self.lane.account {
            Some("another lane")
        } else if kreivo::message_to_sign(&signing_payload) != prepared_message
            || !signing_payload.starts_with(&call)
        {
            Some("a message that is not its payload's")
        } else if kreivo::decode_contracts_call(&call)
            .filter(|(dest, value, ..)| *dest == self.contract && *value == 0)
            .and_then(|(.., data)| Message::decode(&data))
            .as_ref()
            != Some(&order.message)
        {
            Some("a call that does not move what its order says")
        } else {
            None
        };
        if let Some(why) = mismatch {
            self.anomaly(intent_id, "intent", why).await?;
            return Ok(Submitted::Refused(format!(
                "INV-9: intent {intent_id} carries {why}; nothing is signed"
            )));
        }
        // REQ-EO-1: an intent with a signed transaction is re-sent, never signed again.
        let signed: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM signed_transactions WHERE intent_id = $1)",
        )
        .bind(intent_id)
        .fetch_one(self.pool)
        .await?;
        if signed {
            return Ok(Submitted::Refused(format!(
                "REQ-EO-1: intent {intent_id} is signed already; its bytes are re-sent, never signed again"
            )));
        }
        let finalized = match self.chain.finalized().await {
            Ok(f) => f,
            Err(e) => return Ok(Submitted::NotSigned(e.to_string())),
        };
        if u64_of(valid_until) <= finalized.number {
            return Ok(Submitted::Refused(format!(
                "REQ-HN-3: prepared transaction {prepared_id}'s era ended at block {valid_until}; nothing is signed"
            )));
        }
        // Review M2: the whole signed payload, decoded, against the order, the lane and the chain.
        match self
            .check_payload(prepared_id, &order, &signing_payload, &finalized)
            .await?
        {
            PayloadCheck::Matches => {}
            PayloadCheck::Unavailable(e) => return Ok(Submitted::NotSigned(e)),
            PayloadCheck::Stale(why) => {
                return Ok(Submitted::Refused(format!(
                    "REQ-EO-4: prepared transaction {prepared_id} {why}; nothing is signed"
                )));
            }
            PayloadCheck::Mismatch(why) => {
                self.anomaly(intent_id, "payload", &why).await?;
                return Ok(Submitted::Refused(format!(
                    "REQ-EO-1: intent {intent_id}'s signed payload {why}; nothing is signed"
                )));
            }
        }

        // The signature, over the link: Client::sign verifies it over the recorded bytes and starts
        // the compromise path if it is not (AC-I1.2).
        let response = match bezent_signer::link::Client::connect(
            &self.signer.socket,
            &self.signer.token,
            self.signer.uid,
        )
        .await
        {
            Ok(mut client) => client.sign(self.pool, intent_id).await,
            Err(e) => Err(e),
        };
        let (scheme, public_key, signature) = match response {
            Ok(bezent_signer::link::Response::Signed {
                key,
                scheme,
                public_key,
                signature,
                ..
            }) => {
                let decode = |s: &str| s.strip_prefix("0x").and_then(|d| hex::decode(d).ok());
                if key != self.lane.key {
                    let why = format!(
                        "the signer signed with key {key}, not the lane's {}",
                        self.lane.key
                    );
                    self.signer_mismatch(intent_id, &why, &public_key, &signature)
                        .await?;
                    return Ok(Submitted::NotSigned(why));
                }
                (scheme, decode(&public_key), decode(&signature))
            }
            Ok(other) => return Ok(Submitted::NotSigned(format!("{other:?}"))),
            Err(e) => return Ok(Submitted::NotSigned(e.to_string())),
        };
        let (Some(public_key), Some(signature)) = (public_key, signature) else {
            self.signer_mismatch(intent_id, "the signer's answer does not decode", "", "")
                .await?;
            return Ok(Submitted::NotSigned(
                "the signer's answer does not decode".into(),
            ));
        };
        if Scheme::parse(&scheme) != Some(self.lane.scheme)
            || public_key != self.lane.public_key
            || signature.len() != self.lane.scheme.signature_len()
        {
            let why = "the signature is not the lane key's";
            self.signer_mismatch(
                intent_id,
                why,
                &format!("0x{}", hex::encode(&public_key)),
                &format!("0x{}", hex::encode(&signature)),
            )
            .await?;
            return Ok(Submitted::NotSigned(why.into()));
        }
        let Some(extrinsic) =
            extrinsic_from_payload(self.lane, &signature, &call, &signing_payload)
                .filter(|x| kreivo::decode_extrinsic(x).is_some_and(|d| d.call == call))
        else {
            return Ok(Submitted::Refused(
                "the prepared payload does not split into call, extra and implicit".into(),
            ));
        };
        let hash = blake2_256(&extrinsic);

        // REQ-EO-1, INV-9: the exact bytes are stored before they are submitted.
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO signed_transactions (intent_id, attempt, bytes, tx_hash) \
             VALUES ($1, 1, $2, $3) RETURNING id",
        )
        .bind(intent_id)
        .bind(&extrinsic)
        .bind(hash.as_slice())
        .fetch_one(self.pool)
        .await?;
        tracing::info!(network = self.network, intent = intent_id, signed = id, hash = %hex::encode(hash), "REQ-EO-1: signed bytes persisted; submitting");
        Ok(match self.chain.submit(&extrinsic).await {
            Ok(()) => Submitted::Submitted { id, hash },
            Err(e) => Submitted::Persisted {
                id,
                reason: e.to_string(),
            },
        })
    }

    /// Re-sends every signed transaction of this network whose era is live at the finalized head
    /// and whose outcome is not recorded: the same bytes, never a new signature (`REQ-EO-1`,
    /// `AC-D2.1`). Returns how many were sent.
    pub async fn resend(&self) -> Result<usize, sqlx::Error> {
        let Ok(finalized) = self.chain.finalized().await else {
            return Ok(0);
        };
        let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(
            "SELECT st.id, st.bytes FROM signed_transactions st \
             JOIN intents si ON si.id = st.intent_id \
             JOIN prepared_transactions p ON p.id::text = si.payload ->> 'prepared' \
             WHERE si.network = $1 AND p.valid_until_block > $2 \
               AND NOT EXISTS (SELECT 1 FROM non_execution_proofs n WHERE n.signed_transaction_id = st.id) \
               AND NOT EXISTS (SELECT 1 FROM observations ob WHERE ob.network = $1 \
                 AND ob.position_kind = 'transaction' AND ob.detail ->> 'signed_transaction' = st.id::text) \
             ORDER BY p.nonce",
        )
        .bind(self.network)
        .bind(i64_of(finalized.number)?)
        .fetch_all(self.pool)
        .await?;
        let mut sent = 0;
        for (id, bytes) in rows {
            match self.chain.submit(&bytes).await {
                Ok(()) => sent += 1,
                Err(e) => {
                    tracing::warn!(network = self.network, signed = id, error = %e, "a re-send was not accepted")
                }
            }
        }
        Ok(sent)
    }

    /// Review M2 (`REQ-EO-1`, `REQ-EO-4`, `REQ-HN-3`; PLAN `DEC-3`, `DEC-5`): decodes the whole
    /// signing payload of prepared transaction `prepared_id` and compares every part with the
    /// order, the lane and the chain: the call is the order's message to the contract, with value
    /// zero and its dry run's limits; the era is mortal, `MAX_ERA` blocks, born at the block of its
    /// dry run, a block of the finalized chain at most `MAX_ERA` blocks back; the nonce is the
    /// row's and the lane's next; no tip, no `PassAuthenticate`, no fee asset; a runtime the
    /// endpoint has a format for, the finalized head's; this network's genesis. Last, the payload
    /// rebuilt from those is the same bytes.
    async fn check_payload(
        &self,
        prepared_id: i64,
        order: &Order,
        payload: &[u8],
        finalized: &BlockRef,
    ) -> Result<PayloadCheck, sqlx::Error> {
        type Row = (i64, i64, i64, Vec<u8>, i64, Vec<u8>, String);
        let (nonce, from, until, checkpoint, dry_block, dry_hash, dry_run): Row = sqlx::query_as(
            "SELECT nonce, valid_from_block, valid_until_block, checkpoint_hash, \
               dry_run_block_number, dry_run_block_hash, dry_run::text \
             FROM prepared_transactions WHERE id = $1",
        )
        .bind(prepared_id)
        .fetch_one(self.pool)
        .await?;
        let mismatch = |why: &str| Ok(PayloadCheck::Mismatch(why.to_owned()));
        let Some(d) = kreivo::decode_signing_payload(payload) else {
            return mismatch(
                "does not decode whole (a contract call, no PassAuthenticate, a mortal era, no fee asset, nothing left over)",
            );
        };
        let (nonce, from, until) = (u64_of(nonce), u64_of(from), u64_of(until));
        let era = Mortality::born_at(from);
        if era.birth != from || d.era != era.bytes() || until != era.last_valid() {
            return mismatch(
                "carries an era that is not MAX_ERA blocks born at its prepared block",
            );
        }
        let Ok(birth_hash) = <[u8; 32]>::try_from(checkpoint.as_slice()) else {
            return mismatch("names no 32-byte birth block");
        };
        if d.birth_hash != birth_hash || dry_hash != checkpoint || u64_of(dry_block) != from {
            return mismatch("is not born at the block its dry run was made at");
        }
        if u64::from(d.nonce) != nonce {
            return mismatch("carries another nonce than its prepared transaction's");
        }
        if d.tip != 0 {
            return mismatch("carries a tip");
        }
        if d.genesis != self.genesis {
            return mismatch("is for another network's genesis");
        }
        if !SPEC_VERSIONS.contains(&d.spec_version)
            || kreivo::known_transaction_version(d.spec_version) != Some(d.transaction_version)
        {
            return mismatch("is for a runtime the endpoint has no format for");
        }
        if d.contract != self.contract
            || d.value != 0
            || Message::decode(&d.data).as_ref() != Some(&order.message)
        {
            return mismatch("does not call what its order says");
        }
        let Some(limits) = dry_run_limits(&dry_run) else {
            return mismatch("has no recorded dry run to bound its limits");
        };
        if d.limits.storage_deposit_limit.is_none() || d.limits != limits {
            return mismatch("carries other limits than its dry run's");
        }
        let expected = kreivo::signing_payload(
            &kreivo::contracts_call(&self.contract, &limits, &order.message.data()),
            &TxParams {
                genesis: self.genesis,
                spec_version: d.spec_version,
                transaction_version: d.transaction_version,
                era,
                birth_hash,
                nonce: d.nonce,
                tip: 0,
            },
        );
        if expected != payload {
            return mismatch("is not the payload its order, lane and dry run give");
        }

        // The chain: born at a block of the finalized chain, under the runtime it still runs.
        let blocks = match ancestry(self.chain, finalized, from, MAX_ERA).await {
            Ok(Some(b)) if !b.is_empty() => b,
            Ok(_) => return mismatch("is not born at a recent finalized block"),
            Err(e) => return Ok(PayloadCheck::Unavailable(e.to_string())),
        };
        if blocks[0].hash != birth_hash {
            return mismatch("is born at a block that is not on the finalized chain");
        }
        let version = match self.chain.runtime_version(finalized).await {
            Ok(v) => v,
            Err(e) => return Ok(PayloadCheck::Unavailable(e.to_string())),
        };
        if version.spec_version != d.spec_version
            || version.transaction_version != d.transaction_version
        {
            return Ok(PayloadCheck::Stale(
                "was built for another runtime than the finalized head's".into(),
            ));
        }
        // The lane's next nonce: not consumed, every nonce below it held by a live prepared
        // transaction of the lane, and no other live signed transaction of the lane at it.
        let chain_nonce = match self
            .chain
            .account_nonce(finalized, &self.lane.account)
            .await
        {
            Ok(n) => n,
            Err(e) => return Ok(PayloadCheck::Unavailable(e.to_string())),
        };
        if nonce < chain_nonce {
            return Ok(PayloadCheck::Stale(format!(
                "takes lane nonce {nonce}, consumed already (the lane is at {chain_nonce})"
            )));
        }
        let held: i64 = sqlx::query_scalar(
            "SELECT count(DISTINCT nonce) FROM prepared_transactions \
             WHERE network = $1 AND lane = $2 AND dry_run_passed AND valid_until_block > $3 \
               AND nonce >= $4 AND nonce < $5",
        )
        .bind(self.network)
        .bind(self.lane.account.as_slice())
        .bind(i64_of(finalized.number)?)
        .bind(i64_of(chain_nonce)?)
        .bind(i64_of(nonce)?)
        .fetch_one(self.pool)
        .await?;
        if u64_of(held) != nonce - chain_nonce {
            return Ok(PayloadCheck::Stale(format!(
                "takes lane nonce {nonce}, past the lane's next (the lane is at {chain_nonce})"
            )));
        }
        let twin: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS (SELECT 1 FROM prepared_transactions p \
               WHERE p.network = $1 AND p.lane = $2 AND p.nonce = $3 AND p.id <> $4 \
                 AND p.valid_until_block > $5 AND {SIGNED})"
        ))
        .bind(self.network)
        .bind(self.lane.account.as_slice())
        .bind(i64_of(nonce)?)
        .bind(prepared_id)
        .bind(i64_of(finalized.number)?)
        .fetch_one(self.pool)
        .await?;
        if twin {
            return mismatch("takes a nonce another live signed transaction of the lane holds");
        }
        Ok(PayloadCheck::Matches)
    }

    /// Records, once, that what signing intent `intent_id` names does not answer its order
    /// (review S3: never only a log line).
    async fn anomaly(&self, intent_id: i64, what: &str, why: &str) -> Result<(), sqlx::Error> {
        signal::anomaly_once(
            self.pool,
            &format!("{}:intent:{intent_id}:{what}", self.network),
            json!({"network": self.network, "intent": intent_id, "reason": why,
                   "spec": "REQ-EO-1, INV-9"}),
        )
        .await
    }

    /// The signer answered for the lane with something that is not the lane key's signature:
    /// the signer is not signing what it must (`AC-I1.2`), so the compromise path starts, once.
    async fn signer_mismatch(
        &self,
        intent_id: i64,
        why: &str,
        public_key: &str,
        signature: &str,
    ) -> Result<(), sqlx::Error> {
        signal::raise_once(
            self.pool,
            &format!("{}:intent:{intent_id}:signer", self.network),
            json!({"network": self.network, "source": "signer-link", "reason": why,
                   "requested_intent": intent_id, "key": self.lane.key,
                   "public_key": public_key, "signature": signature}),
        )
        .await
    }

    /// Review S4 (`AC-I1.2`): the lane's nonce on chain is past every nonce bezent signed, so the
    /// delivery key was used while the lane was idle. Raised once per nonce seen.
    async fn check_idle_use(&self, chain_nonce: u64) -> Result<(), sqlx::Error> {
        let signed: Option<i64> = sqlx::query_scalar(&format!(
            "SELECT max(p.nonce) FROM prepared_transactions p \
             WHERE p.network = $1 AND p.lane = $2 AND {SIGNED}"
        ))
        .bind(self.network)
        .bind(self.lane.account.as_slice())
        .fetch_one(self.pool)
        .await?;
        let Some(highest) = signed.map(u64_of) else {
            return Ok(());
        };
        if chain_nonce <= highest + 1 {
            return Ok(());
        }
        signal::raise_once(
            self.pool,
            &format!(
                "{}:lane:{}:nonce:{chain_nonce}",
                self.network,
                hex::encode(self.lane.account)
            ),
            json!({"network": self.network, "source": "lane",
                   "reason": "the lane's nonce on chain is past every nonce bezent signed",
                   "public_key": format!("0x{}", hex::encode(&self.lane.public_key)),
                   "highest_signed_nonce": highest, "chain_nonce": chain_nonce}),
        )
        .await
    }
}

/// What checking a signing payload found.
enum PayloadCheck {
    /// It is exactly what its order, the lane and the chain give.
    Matches,
    /// It no longer fits the chain (the runtime or the lane moved): not signed, and not an anomaly.
    Stale(String),
    /// It does not answer its order or its row: not signed, and recorded as an anomaly.
    Mismatch(String),
    /// The chain did not answer: try again.
    Unavailable(String),
}

/// The limits a recorded dry run gives (`DryRun::limits` with [`LIMIT_MARGIN`]).
fn dry_run_limits(dry_run: &str) -> Option<kreivo::CallLimits> {
    let v: Value = serde_json::from_str(dry_run).ok()?;
    let gas = v.get("gas_required")?;
    let d = kreivo::DryRun {
        gas_required: kreivo::Weight {
            ref_time: gas.get("ref_time")?.as_u64()?,
            proof_size: gas.get("proof_size")?.as_u64()?,
        },
        storage_deposit: v.get("storage_deposit")?.as_str()?.parse().ok()?,
        outcome: kreivo::DryRunOutcome::Passed,
    };
    Some(d.limits(LIMIT_MARGIN))
}

/// The extrinsic for `signature` over a prepared payload: `payload = call ‖ extra ‖ implicit`,
/// where `implicit` is the last 72 bytes (spec and transaction versions, genesis, birth hash).
pub fn extrinsic_from_payload(
    lane: &Lane,
    signature: &[u8],
    call: &[u8],
    payload: &[u8],
) -> Option<Vec<u8>> {
    const IMPLICIT: usize = 4 + 4 + 32 + 32;
    let extra = payload.get(call.len()..payload.len().checked_sub(IMPLICIT)?)?;
    if !payload.starts_with(call) {
        return None;
    }
    let mut body = vec![kreivo::SIGNED_V4, kreivo::ADDRESS_ID];
    body.extend_from_slice(&lane.account);
    body.push(lane.scheme.variant());
    body.extend_from_slice(signature);
    body.extend_from_slice(extra);
    body.extend_from_slice(call);
    let mut out = parity_scale_codec::Encode::encode(&parity_scale_codec::Compact(
        u32::try_from(body.len()).ok()?,
    ));
    out.extend_from_slice(&body);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req_eo_1_the_extrinsic_from_a_prepared_payload_is_the_one_built_directly() {
        let lane = Lane {
            key: "k".into(),
            scheme: Scheme::Sr25519,
            public_key: vec![3; 32],
            account: [3; 32],
        };
        let p = TxParams {
            genesis: [1; 32],
            spec_version: 134,
            transaction_version: 13,
            era: Mortality::born_at(500),
            birth_hash: [2; 32],
            nonce: 300,
            tip: 0,
        };
        let call = kreivo::contracts_call(
            &[5; 32],
            &kreivo::CallLimits {
                gas_limit: kreivo::Weight::default(),
                storage_deposit_limit: None,
            },
            &kreivo::refund_mint_data(&[1; 32], &[2; 32]),
        );
        let payload = kreivo::signing_payload(&call, &p);
        assert_eq!(
            extrinsic_from_payload(&lane, &[9; 64], &call, &payload),
            Some(kreivo::extrinsic(
                &[3; 32],
                Scheme::Sr25519,
                &[9; 64],
                &p,
                &call
            ))
        );
        assert_eq!(extrinsic_from_payload(&lane, &[9; 64], &call, &call), None);
    }

    #[test]
    fn req_eo_4_the_lane_is_declared() {
        let lane = Lane {
            key: "kreivo-mint".into(),
            scheme: Scheme::Sr25519,
            public_key: vec![3; 32],
            account: [3; 32],
        };
        let d = lane.declaration("kreivo");
        assert!(d.contains("REQ-EO-4") && d.contains("kreivo-mint") && d.contains("order is kept"));
    }

    #[test]
    fn inv_11_an_order_amount_is_an_exact_decimal_string() {
        let order = |amount: Value| {
            Order::parse(
                1,
                "mint",
                Some(&[1; 32]),
                &json!({"order": {"asset": "Here(101)", "to": format!("0x{}", hex::encode([2; 32]))}, "amount": amount}),
            )
        };
        assert_eq!(order(json!("1000000")).unwrap().amount, 1_000_000);
        for bad in [json!(1), json!("1.5"), json!("-1"), json!("")] {
            assert!(order(bad).is_err());
        }
        assert!(Order::parse(1, "mint", None, &json!({})).is_err());
        assert!(
            Order::parse(
                1,
                "destroy",
                Some(&[1; 32]),
                &json!({"order": {}, "amount": "1"})
            )
            .is_err()
        );
    }
}
