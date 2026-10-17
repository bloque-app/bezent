// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Payouts, in the two steps of PLAN `DEC-3` (`T-009-04`; `REQ-EO-1`, `REQ-EO-4`, `REQ-NX-4`,
//! `INV-9`, `INV-14`, `AC-D2.1`).
//!
//! 1. **The order** (the core's, `F-003`): an intent on this network of purpose `payout`, with the
//!    withdrawal's 32-byte payout reference and `{"order": {"to", "asset", "token"}, "amount"}`, or
//!    of purpose `cancel` with `{"order": {"cancel": <signed transaction id>}, "amount": "0"}` and
//!    no reference: the core's order to force the non-execution of a signed payout (`REQ-NX-4`).
//!    Neither carries a `message`, so no signer signs it.
//! 2. **Preparation** ([`Submitter::prepare`]): the endpoint builds the transaction (a payout is
//!    `Roles.execTransactionWithRole` of the delivery role, the 356-byte MultiSend of the
//!    origin-token transfer and `emitReference(ref)`; a cancel is a transfer of nothing from the
//!    delivery key to itself at the payout's nonce, with fees above the payout's), takes the lane's
//!    nonce, bounds gas and fees, checks the lane can pay them, dry-runs the call on both providers
//!    (`eth_call` at each one's latest block), and records it, append-only, in
//!    `prepared_transactions`, the dry run included. One preparation per order: a failed dry run is
//!    final for that order, and a retry is the core's (a new order).
//! 3. **The signing intent** (the core's): `{"message", "amount", "prepared"}` over the recorded
//!    bytes ([`signing_intent_payload`]).
//! 4. **Signing and submission** ([`Submitter::sign_and_submit`]): before asking for a signature,
//!    the endpoint decodes the whole signing payload and compares every field with the order and
//!    the lane ([`Submitter::check`]): chain id, target, value, access list, nonce, gas limit and
//!    fees (the recorded dry run's, within the declared bounds), and the call itself (the delivery
//!    role, MultiSendCallOnly, the order's token, beneficiary, amount and reference; or a cancel's
//!    empty transfer to the lane). A mismatch signs nothing and records a `delivery-anomaly`. The
//!    signature comes over the signer link, which checks it against the recorded bytes
//!    (`Client::sign`); the endpoint checks it is the lane key's and recovers the lane's address,
//!    persists the exact signed transaction in `signed_transactions` (`REQ-EO-1`, `INV-9`), and only
//!    then sends it to both providers.
//! 5. **After a restart** ([`Submitter::resend`]): every signed transaction whose outcome is not
//!    recorded is re-sent as the same bytes; nothing is signed twice for an intent (`AC-D2.1`).
//!
//! **The key used outside bezent** (`AC-I1.2`; PR #39 second review SF1): before every preparation
//! and every signature the endpoint reads the lane. A nonce on chain past every nonce bezent signed
//! for the lane plus one, or code at the lane (an EIP-7702 delegation), is the delivery key used
//! where bezent recorded nothing: the compromise path starts (an alert and a global pause request,
//! once per nonce or per lane) and nothing is prepared or signed.
//!
//! **Signing lane** (`REQ-EO-4`), declared by [`Lane::declaration`]: one lane per delivery key, the
//! key's nonce sequence; every payout of the network is in it, in nonce order. A prepared
//! transaction whose dry run passed holds its nonce until it is signed and proven; one the core
//! never signs holds the lane (an EVM transaction does not expire). Stated plainly: one unknown
//! payout holds every later one until it executes or a cancel consumes its nonce.

use std::path::PathBuf;

use alloy::consensus::TxEip1559;
use alloy::consensus::{Transaction as _, TxEnvelope};
use alloy::eips::eip2718::Decodable2718;
use alloy::primitives::{Address, B256, U256};
use bezent_signer::link::{Client, LinkError, Response};
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::evm::{self, Contracts, Delivery, Gas};
use crate::network::Declarations;
use crate::pair::{Pair, Read};
use crate::rpc::{At, Call, Rpc};
use crate::signal;
use crate::store::{hex0x, i64_of};

/// The purposes the endpoint prepares.
pub const PURPOSES: &[&str] = &["payout", "cancel"];

/// An EVM transaction's validity has no end (`REQ-NX-1`): `prepared_transactions.valid_until_block`.
pub const NO_EXPIRY: i64 = i64::MAX;

/// The gas of a plain transfer, a cancel's.
pub const CANCEL_GAS: u64 = 21_000;

/// A signing lane: one delivery key's address and its nonce sequence (`REQ-EO-4`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lane {
    /// The signer's id for the key.
    pub key: String,
    /// Its compressed secp256k1 public key.
    pub public_key: Vec<u8>,
    /// Its address.
    pub address: Address,
}

impl Lane {
    /// The lane's declaration (`REQ-EO-4`).
    pub fn declaration(&self, network: &str) -> String {
        format!(
            "REQ-EO-4: one signing lane on {network}: key {} ({}); every payout and cancel of the \
             network in it, in nonce order; one whose outcome is unknown holds the later ones until \
             it executes or a cancel consumes its nonce",
            self.key, self.address
        )
    }
}

/// The signer link's coordinates. The token is never printed.
#[derive(Clone)]
pub struct SignerLink {
    /// The socket.
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
            .finish_non_exhaustive()
    }
}

/// Bounds on what a transaction may cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GasBounds {
    /// The highest gas limit.
    pub max_gas: u64,
    /// The highest `maxFeePerGas`, in wei.
    pub max_fee: u128,
    /// The margin over the estimate, in percent (at least 100).
    pub margin_percent: u64,
}

/// What an order asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    /// A payout from the vault.
    Payout(Delivery),
    /// The non-execution of signed transaction `signed`, forced at its nonce.
    Cancel {
        /// The signed transaction.
        signed: i64,
    },
}

/// An order, as the core recorded it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Order {
    /// The intent.
    pub intent_id: i64,
    /// Its purpose.
    pub purpose: String,
    /// Its on-chain reference.
    pub reference: Option<Vec<u8>>,
    /// The amount, in minor units.
    pub amount: u128,
    /// What it asks for.
    pub ask: Ask,
}

/// The amount field of an intent's payload: a decimal string of minor units (`INV-11`).
pub fn amount_field(payload: &Value) -> Result<u128, String> {
    payload
        .get("amount")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| "the amount is not a decimal string of minor units (INV-11)".into())
}

impl Order {
    /// Parses an order intent against the network's declarations.
    pub fn parse(
        intent_id: i64,
        purpose: &str,
        reference: Option<Vec<u8>>,
        payload: &Value,
        declarations: &Declarations,
        vault: Address,
    ) -> Result<Self, String> {
        let order = payload.get("order").ok_or("the intent is not an order")?;
        let amount = amount_field(payload)?;
        let ask = match purpose {
            "payout" => {
                let r: [u8; 32] = reference
                    .as_deref()
                    .and_then(|r| r.try_into().ok())
                    .ok_or("REQ-ID-2: a payout carries a 32-byte reference")?;
                let to: Address = order
                    .get("to")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse().ok())
                    .ok_or("the beneficiary is not an EVM address (REQ-OT-4)")?;
                if to == vault || to == Address::ZERO {
                    return Err(format!(
                        "INV-14: a transfer to {to} is not a payout (F-008 NOTES)"
                    ));
                }
                let asset = order
                    .get("asset")
                    .and_then(Value::as_str)
                    .ok_or("the order names no asset")?;
                let (token, declared) = declarations
                    .token_of(asset)
                    .ok_or_else(|| format!("REQ-AS-2: no one token is admitted for {asset}"))?;
                if let Some(named) = order.get("token").and_then(Value::as_str)
                    && named != declared.text
                    && named.parse::<Address>().ok() != Some(token)
                {
                    return Err(format!(
                        "REQ-AS-3: the order names token {named}, the asset's is {}",
                        declared.text
                    ));
                }
                Ask::Payout(Delivery {
                    token,
                    to,
                    amount: U256::from(amount),
                    reference: B256::from(r),
                })
            }
            "cancel" => {
                if reference.is_some() || amount != 0 {
                    return Err("a cancel carries no reference and moves nothing".into());
                }
                Ask::Cancel {
                    signed: order
                        .get("cancel")
                        .and_then(Value::as_i64)
                        .ok_or("a cancel names its signed transaction")?,
                }
            }
            other => return Err(format!("purpose {other} is not the EVM endpoint's")),
        };
        Ok(Self {
            intent_id,
            purpose: purpose.to_owned(),
            reference,
            amount,
            ask,
        })
    }
}

/// The payload of the signing intent the core records for prepared transaction `prepared`
/// (`INV-9`: only the core records it; the signer signs `message` as it is).
pub fn signing_intent_payload(prepared: i64, message: &[u8], amount: u128) -> Value {
    json!({
        "message": hex0x(message),
        "amount": amount.to_string(),
        "prepared": prepared,
    })
}

/// What preparing an order did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prepared {
    /// Recorded, its dry run passed: ready for a signing intent.
    Ready {
        /// The prepared transaction.
        id: i64,
        /// What the key signs the Keccak-256 of.
        message: Vec<u8>,
        /// The nonce.
        nonce: u64,
    },
    /// Recorded, its dry run failed: the reason, the network's own words.
    DryRunFailed {
        /// The prepared transaction.
        id: i64,
        /// Why.
        reason: String,
    },
    /// Not recorded: the order cannot be prepared as it is.
    Refused(String),
    /// Not recorded: a provider failed or the providers disagree; tried again next cycle.
    Unavailable(String),
}

/// What signing and submitting a signing intent did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Submitted {
    /// Persisted and accepted by a provider.
    Submitted {
        /// The signed transaction.
        id: i64,
        /// Its hash.
        hash: B256,
    },
    /// Persisted, not accepted by either provider; re-sent later.
    Persisted {
        /// The signed transaction.
        id: i64,
        /// The providers' words.
        reason: String,
    },
    /// Not signed: the intent does not answer its order (a `delivery-anomaly` is recorded), or
    /// the lane's state forbids it now.
    Refused(String),
    /// The signer did not sign, or answered with something that is not the lane key's.
    NotSigned(String),
    /// A provider failed; tried again next cycle.
    Unavailable(String),
}

/// Builds, signs and submits the network's payouts. See the module documentation.
pub struct Submitter<'a, R> {
    /// The two providers.
    pub pair: &'a Pair<R>,
    /// The store, as the EVM endpoint's role.
    pub pool: &'a PgPool,
    /// The network id.
    pub network: &'a str,
    /// Its chain id.
    pub chain_id: u64,
    /// The vault's contracts.
    pub contracts: Contracts,
    /// The vault.
    pub vault: Address,
    /// The store's declarations.
    pub declarations: &'a Declarations,
    /// The lane.
    pub lane: &'a Lane,
    /// The signer link.
    pub signer: &'a SignerLink,
    /// The gas bounds.
    pub gas: GasBounds,
}

/// A prepared transaction's row, as far as signing reads it.
type PreparedRow = (
    i64,
    Vec<u8>,
    String,
    i64,
    Vec<u8>,
    Vec<u8>,
    Vec<u8>,
    bool,
    Value,
);

/// What reading the lane before a preparation or a signature found (SF1, S5).
enum LaneUse {
    /// Nothing bezent did not do.
    Clean,
    /// The key was used outside bezent: the compromise path is in the event log.
    Outside(String),
    /// The chain did not answer: try again.
    Unreadable(String),
}

/// An intent's row: id, network, purpose, reference, payload, transfer, attempt.
type IntentRow = (
    i64,
    String,
    String,
    Option<Vec<u8>>,
    Value,
    Option<i64>,
    i32,
);

const INTENT: &str =
    "SELECT id, network, purpose, on_chain_reference, payload, transfer_id, attempt FROM intents";

fn read_err(e: Read) -> String {
    e.to_string()
}

impl<R: Rpc> Submitter<'_, R> {
    fn lane_bytes(&self) -> Vec<u8> {
        self.lane.address.to_vec()
    }

    /// The orders on this network with no prepared transaction.
    pub async fn orders_to_prepare(&self) -> Result<Vec<i64>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT o.id FROM intents o \
             WHERE o.network = $1 AND o.purpose = ANY($2) AND o.payload ? 'order' \
               AND NOT EXISTS (SELECT 1 FROM prepared_transactions p WHERE p.order_intent_id = o.id \
                 AND p.network = o.network) \
             ORDER BY o.id",
        )
        .bind(self.network)
        .bind(PURPOSES)
        .fetch_all(self.pool)
        .await
    }

    /// The lane's next nonce: the lowest at or above the chain's (the higher of the two
    /// providers' latest) that no live prepared transaction of the lane holds.
    async fn next_nonce(&self) -> Result<Result<u64, String>, sqlx::Error> {
        let chain = match self.pair.latest_nonce(self.lane.address).await {
            Ok(n) => n,
            Err(e) => return Ok(Err(read_err(e))),
        };
        let held: Vec<i64> = sqlx::query_scalar(
            "SELECT DISTINCT p.nonce FROM prepared_transactions p \
             WHERE p.network = $1 AND p.lane = $2 AND p.dry_run_passed AND p.nonce >= $3 \
             ORDER BY p.nonce",
        )
        .bind(self.network)
        .bind(self.lane_bytes())
        .bind(i64_of(chain)?)
        .fetch_all(self.pool)
        .await?;
        let mut n = chain;
        for h in held {
            if u64::try_from(h).ok() == Some(n) {
                n += 1;
            }
        }
        Ok(Ok(n))
    }

    /// Prepares order `order_intent_id`. See the module documentation.
    pub async fn prepare(&self, order_intent_id: i64) -> Result<Prepared, sqlx::Error> {
        let row: Option<IntentRow> = sqlx::query_as(&format!("{INTENT} WHERE id = $1"))
            .bind(order_intent_id)
            .fetch_optional(self.pool)
            .await?;
        let Some((_, network, purpose, reference, payload, _, _)) = row else {
            return Ok(Prepared::Refused(format!(
                "no intent {order_intent_id} (INV-9)"
            )));
        };
        if network != self.network || !PURPOSES.contains(&purpose.as_str()) {
            return Ok(Prepared::Refused(format!(
                "intent {order_intent_id} is a {purpose} on {network}"
            )));
        }
        let prepared: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM prepared_transactions WHERE order_intent_id = $1)",
        )
        .bind(order_intent_id)
        .fetch_one(self.pool)
        .await?;
        if prepared {
            return Ok(Prepared::Refused(format!(
                "order {order_intent_id} is prepared already; a retry is a new order"
            )));
        }
        let order = match Order::parse(
            order_intent_id,
            &purpose,
            reference,
            &payload,
            self.declarations,
            self.vault,
        ) {
            Ok(o) => o,
            Err(e) => {
                tracing::error!(order = order_intent_id, reason = %e, "an order that cannot be prepared");
                return Ok(Prepared::Refused(e));
            }
        };

        // SF1, S5: nothing is prepared from a lane the key was used on outside bezent.
        match self.lane_use(order_intent_id).await? {
            LaneUse::Clean => {}
            LaneUse::Outside(why) => return Ok(Prepared::Refused(why)),
            LaneUse::Unreadable(e) => return Ok(Prepared::Unavailable(e)),
        }
        // What the transaction is, and at which nonce.
        let (to, input, nonce, floor) = match &order.ask {
            Ask::Payout(d) => {
                let nonce = match self.next_nonce().await? {
                    Ok(n) => n,
                    Err(e) => return Ok(Prepared::Unavailable(e)),
                };
                (
                    self.contracts.roles,
                    evm::delivery_call(&self.contracts, d),
                    nonce,
                    None,
                )
            }
            Ask::Cancel { signed } => match self.cancel_target(*signed).await? {
                Ok(target) => (
                    self.lane.address,
                    Vec::new(),
                    target.nonce,
                    Some((target.max_fee_per_gas, target.max_priority_fee_per_gas)),
                ),
                Err(e) => return Ok(Prepared::Refused(e)),
            },
        };
        let call = Call {
            from: self.lane.address,
            to,
            data: input.clone().into(),
        };

        // Gas, fees and the lane's balance; a failure here is a failed dry run, recorded.
        let mut failure: Option<String> = None;
        let limit = if order.ask_is_cancel() {
            CANCEL_GAS
        } else {
            match self.pair.estimate_gas(&call).await {
                Ok(Ok(g)) => g.saturating_mul(self.gas.margin_percent) / 100,
                Ok(Err(reason)) => {
                    failure = Some(reason);
                    0
                }
                Err(e) => return Ok(Prepared::Unavailable(read_err(e))),
            }
        };
        if failure.is_none() && limit > self.gas.max_gas {
            failure = Some(format!(
                "the gas limit {limit} is above the declared max_gas {}",
                self.gas.max_gas
            ));
        }
        let fees = match self.fees(floor).await {
            Ok(f) => f,
            Err(e) => return Ok(Prepared::Unavailable(e)),
        };
        let (max_fee, priority) = match fees {
            Ok(f) => f,
            Err(reason) => {
                failure.get_or_insert(reason);
                (self.gas.max_fee, 0)
            }
        };
        if failure.is_none() {
            let balance = match self.pair.latest_balance(self.lane.address).await {
                Ok(b) => b,
                Err(e) => return Ok(Prepared::Unavailable(read_err(e))),
            };
            let needed = U256::from(limit) * U256::from(max_fee);
            if balance < needed {
                failure = Some(format!(
                    "InsufficientBalance: the lane's {balance} wei does not pay the fee of {needed} wei"
                ));
            }
        }
        let tx = evm::transaction(
            self.chain_id,
            nonce,
            to,
            input.clone(),
            Gas {
                limit,
                max_fee,
                priority_fee: priority,
            },
        );
        let message = evm::signing_payload(&tx);

        // The dry run, on both providers.
        let run = match self.pair.dry_run(&call).await {
            Ok(r) => r,
            Err(e) => return Ok(Prepared::Unavailable(read_err(e))),
        };
        let reason = failure.or_else(|| run.reason().map(str::to_owned));
        let passed = reason.is_none() && run.passed();
        let (_, at, _) = &run.runs[0];
        // The Roles refusal both providers returned alike, decoded (PR #52 review S-2): the one
        // signal the core names CND-PayoutWindow from. Null otherwise.
        let violation = run
            .violation()
            .filter(|_| !passed)
            .map(|(status, key)| json!({"status": status, "key": hex0x(key.as_slice())}));
        let dry_run = json!({
            "passed": passed,
            "reason": reason,
            "violation": violation,
            "gas_limit": limit,
            "max_fee_per_gas": max_fee.to_string(),
            "max_priority_fee_per_gas": priority.to_string(),
            "runs": run.runs.iter().map(|(name, h, r)| json!({
                "provider": name,
                "block": h.number,
                "hash": h.hash,
                "result": r.as_ref().map(|b| hex0x(b)).ok(),
                "revert": r.as_ref().err(),
            })).collect::<Vec<_>>(),
        });
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO prepared_transactions (order_intent_id, attempt, network, lane, key_id, \
               nonce, valid_from_block, valid_until_block, checkpoint_hash, call, signing_payload, \
               message, dry_run_block_number, dry_run_block_hash, dry_run_passed, dry_run) \
             VALUES ($1, 1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10, $6, $8, $11, $12::text::jsonb) \
             RETURNING id",
        )
        .bind(order_intent_id)
        .bind(self.network)
        .bind(self.lane_bytes())
        .bind(&self.lane.key)
        .bind(i64_of(nonce)?)
        .bind(i64_of(at.number)?)
        .bind(NO_EXPIRY)
        .bind(at.hash.as_slice())
        .bind(call_bytes(to, &input))
        .bind(&message)
        .bind(passed)
        .bind(dry_run.to_string())
        .fetch_one(self.pool)
        .await?;
        Ok(match reason {
            None if passed => Prepared::Ready { id, message, nonce },
            other => Prepared::DryRunFailed {
                id,
                reason: other.unwrap_or_else(|| "the dry run failed".into()),
            },
        })
    }

    /// `maxFeePerGas` and `maxPriorityFeePerGas`: twice the higher base fee plus the higher
    /// priority fee, capped by the declared bound; above a cancel's `floor` by an eighth (a
    /// replacement must pay more). `Ok(Err(..))` when the bound cannot pay the network's fee.
    async fn fees(
        &self,
        floor: Option<(u128, u128)>,
    ) -> Result<Result<(u128, u128), String>, String> {
        let mut base = 0u128;
        for p in [&self.pair.a, &self.pair.b] {
            let h = p
                .header(At::Latest)
                .await
                .map_err(|e| e.to_string())?
                .ok_or("no latest block")?;
            base = base.max(h.base_fee.unwrap_or(0));
        }
        let mut priority = self.pair.priority_fee().await.map_err(read_err)?;
        let mut max_fee = base.saturating_mul(2).saturating_add(priority);
        if let Some((f, p)) = floor {
            priority = priority.max(p.saturating_add(p / 8).saturating_add(1));
            max_fee = max_fee
                .max(f.saturating_add(f / 8).saturating_add(1))
                .max(base.saturating_add(priority));
        }
        if base.saturating_add(priority) > self.gas.max_fee {
            return Ok(Err(format!(
                "fee: the network asks {} wei per gas, above the declared max_fee_per_gas {}",
                base.saturating_add(priority),
                self.gas.max_fee
            )));
        }
        Ok(Ok((max_fee.min(self.gas.max_fee), priority)))
    }

    /// Why a cancel must not be prepared or signed now, if the lane holds code (PR #39 review S5):
    /// a transfer of nothing to itself would run that code (an EIP-7702 delegation) as a member
    /// of the delivery role. bezent's delivery key has none, so code is the key used outside
    /// bezent: the compromise path (`AC-I1.2`). A read that fails refuses too, raising nothing.
    async fn lane_runs_code(&self, intent: i64) -> Result<Option<String>, sqlx::Error> {
        match self.pair.has_code(self.lane.address).await {
            Ok(false) => Ok(None),
            Err(e) => Ok(Some(format!("S5: the lane's code cannot be read: {e}"))),
            Ok(true) => {
                signal::raise_once(
                    self.pool,
                    &format!("{}:lane-code:{}", self.network, self.lane.address),
                    json!({"network": self.network, "lane": self.lane.address, "intent": intent,
                           "reason": "the delivery key holds code (an EIP-7702 delegation); a cancel would run it"}),
                )
                .await?;
                Ok(Some(format!(
                    "AC-I1.2, S5: the lane {} holds code; no cancel is prepared or signed",
                    self.lane.address
                )))
            }
        }
    }

    /// PR #39 second review SF1 (`AC-I1.2`, as the Kreivo endpoint's review S4): whether the
    /// delivery key was used outside bezent. The lane holds code (S5), or its nonce on chain (the
    /// higher of the two providers') is past every nonce bezent signed for it plus one: a nonce
    /// moved while bezent had nothing in flight. Either raises the compromise path, once per lane
    /// or per nonce seen. A lane bezent never signed for raises nothing on its nonce.
    async fn lane_use(&self, intent: i64) -> Result<LaneUse, sqlx::Error> {
        if let Some(why) = self.lane_runs_code(intent).await? {
            return Ok(LaneUse::Outside(why));
        }
        let chain = match self.pair.latest_nonce(self.lane.address).await {
            Ok(n) => n,
            Err(e) => return Ok(LaneUse::Unreadable(read_err(e))),
        };
        let signed: Option<i64> = sqlx::query_scalar(
            "SELECT max(p.nonce) FROM prepared_transactions p \
             WHERE p.network = $1 AND p.lane = $2 \
               AND EXISTS (SELECT 1 FROM intents si JOIN signed_transactions st ON st.intent_id = si.id \
                 WHERE si.network = p.network AND si.payload ->> 'prepared' = p.id::text)",
        )
        .bind(self.network)
        .bind(self.lane_bytes())
        .fetch_one(self.pool)
        .await?;
        let Some(highest) = signed.and_then(|n| u64::try_from(n).ok()) else {
            return Ok(LaneUse::Clean);
        };
        if chain <= highest.saturating_add(1) {
            return Ok(LaneUse::Clean);
        }
        signal::raise_once(
            self.pool,
            &format!("{}:lane:{}:nonce:{chain}", self.network, self.lane.address),
            json!({"network": self.network, "source": "lane", "lane": self.lane.address,
                   "intent": intent, "highest_signed_nonce": highest, "chain_nonce": chain,
                   "reason": "the lane's nonce on chain is past every nonce bezent signed"}),
        )
        .await?;
        Ok(LaneUse::Outside(format!(
            "AC-I1.2, SF1: the lane {}'s nonce {chain} is past every nonce bezent signed ({highest}); \
             key compromise raised, nothing is prepared or signed",
            self.lane.address
        )))
    }

    /// The payout a cancel forces out: a signed transaction of this lane, with no proof and no
    /// recorded outcome, decoded from its persisted bytes.
    async fn cancel_target(&self, signed: i64) -> Result<Result<TxEip1559, String>, sqlx::Error> {
        let row: Option<(Vec<u8>, Vec<u8>, String, bool)> = sqlx::query_as(
            "SELECT st.bytes, p.lane, si.network, \
               EXISTS (SELECT 1 FROM non_execution_proofs n WHERE n.signed_transaction_id = st.id) \
               OR EXISTS (SELECT 1 FROM observations ob WHERE ob.position_kind = 'transaction' \
                 AND ob.detail ->> 'signed_transaction' = st.id::text) \
             FROM signed_transactions st JOIN intents si ON si.id = st.intent_id \
             JOIN prepared_transactions p ON p.id::text = si.payload ->> 'prepared' \
             WHERE st.id = $1",
        )
        .bind(signed)
        .fetch_optional(self.pool)
        .await?;
        let Some((bytes, lane, network, settled)) = row else {
            return Ok(Err(format!("REQ-NX-4: no signed transaction {signed}")));
        };
        if network != self.network || lane != self.lane_bytes() {
            return Ok(Err(format!(
                "REQ-NX-4: signed transaction {signed} is not of this lane"
            )));
        }
        if settled {
            return Ok(Err(format!(
                "REQ-NX-4: signed transaction {signed} has its outcome already"
            )));
        }
        Ok(decode_signed(&bytes)
            .map(|(tx, _)| tx)
            .ok_or_else(|| format!("signed transaction {signed} does not decode")))
    }

    /// The signing intents on this network with no signed transaction yet.
    pub async fn intents_to_sign(&self) -> Result<Vec<i64>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT si.id FROM intents si \
             JOIN prepared_transactions p ON p.id::text = si.payload ->> 'prepared' \
             WHERE si.network = $1 AND si.purpose = ANY($2) AND p.network = si.network AND p.lane = $3 \
               AND NOT EXISTS (SELECT 1 FROM signed_transactions st WHERE st.intent_id = si.id) \
             ORDER BY p.nonce, si.id",
        )
        .bind(self.network)
        .bind(PURPOSES)
        .bind(self.lane_bytes())
        .fetch_all(self.pool)
        .await
    }

    /// `T-009-04`: decodes the whole signing payload of signing intent `intent_id` and compares
    /// every field with its prepared transaction, its order and the lane. Returns the transaction
    /// to sign, or why it must not be signed.
    pub async fn check(&self, intent_id: i64) -> Result<Result<TxEip1559, String>, sqlx::Error> {
        let si: Option<IntentRow> = sqlx::query_as(&format!("{INTENT} WHERE id = $1"))
            .bind(intent_id)
            .fetch_optional(self.pool)
            .await?;
        let Some((_, network, purpose, reference, payload, transfer, attempt)) = si else {
            return Ok(Err(format!("no intent {intent_id}")));
        };
        let prepared_id = payload
            .get("prepared")
            .and_then(Value::as_i64)
            .ok_or("the intent names no prepared transaction");
        let prepared_id = match prepared_id {
            Ok(p) => p,
            Err(e) => return Ok(Err(e.into())),
        };
        let p: Option<PreparedRow> = sqlx::query_as(
            "SELECT order_intent_id, lane, key_id, nonce, call, signing_payload, message, \
               dry_run_passed, dry_run FROM prepared_transactions WHERE id = $1 AND network = $2",
        )
        .bind(prepared_id)
        .bind(self.network)
        .fetch_optional(self.pool)
        .await?;
        let Some((order_id, lane, key_id, nonce, call, signing_payload, message, passed, dry_run)) =
            p
        else {
            return Ok(Err(format!(
                "INV-9: no prepared transaction {prepared_id} on {}",
                self.network
            )));
        };
        let o: Option<IntentRow> = sqlx::query_as(&format!("{INTENT} WHERE id = $1"))
            .bind(order_id)
            .fetch_optional(self.pool)
            .await?;
        let Some((_, o_network, o_purpose, o_reference, o_payload, o_transfer, o_attempt)) = o
        else {
            return Ok(Err(format!("no order {order_id}")));
        };
        let order = match Order::parse(
            order_id,
            &o_purpose,
            o_reference.clone(),
            &o_payload,
            self.declarations,
            self.vault,
        ) {
            Ok(o) => o,
            Err(e) => return Ok(Err(e)),
        };
        // The intent answers its order (INV-9).
        let amount = match amount_field(&payload) {
            Ok(a) => a,
            Err(e) => return Ok(Err(e)),
        };
        if network != self.network
            || o_network != self.network
            || purpose != o_purpose
            || reference != o_reference
            || transfer != o_transfer
            || attempt != o_attempt
            || amount != order.amount
        {
            return Ok(Err(format!(
                "INV-9: intent {intent_id} does not answer order {order_id} (network, purpose, \
                 reference, transfer, attempt or amount)"
            )));
        }
        if !passed {
            return Ok(Err(format!(
                "DEC-3: prepared transaction {prepared_id} did not pass its dry run"
            )));
        }
        if lane != self.lane_bytes() || key_id != self.lane.key {
            return Ok(Err(format!(
                "REQ-EO-4: prepared transaction {prepared_id} is not of this lane"
            )));
        }
        let named = payload
            .get("message")
            .and_then(Value::as_str)
            .and_then(|m| m.strip_prefix("0x"))
            .and_then(|m| hex::decode(m).ok());
        if named.as_deref() != Some(message.as_slice()) || message != signing_payload {
            return Ok(Err(format!(
                "INV-9: intent {intent_id}'s message is not prepared transaction {prepared_id}'s payload"
            )));
        }
        let Some(tx) = evm::decode_signing_payload(&message) else {
            return Ok(Err(
                "REQ-EO-1: the payload is not one EIP-1559 transaction".into()
            ));
        };
        let recorded = |field: &str| dry_run.get(field).cloned().unwrap_or(Value::Null);
        // SF-B (third review): the order is on this endpoint's network (checked above), so the
        // chain id the payload carries must be that network's, the one both providers serve.
        let mismatch = if tx.chain_id != self.chain_id {
            Some(format!("chain id {}", tx.chain_id))
        } else if i64::try_from(tx.nonce).ok() != Some(nonce) {
            Some(format!("nonce {}", tx.nonce))
        } else if !tx.value.is_zero() {
            Some(format!("value {}", tx.value))
        } else if !tx.access_list.is_empty() {
            Some("an access list".into())
        } else if tx.to().map(|to| call_bytes(to, &tx.input)) != Some(call.clone()) {
            Some("calldata other than the prepared call".into())
        } else if tx.gas_limit > self.gas.max_gas || json!(tx.gas_limit) != recorded("gas_limit") {
            Some(format!("gas limit {}", tx.gas_limit))
        } else if tx.max_fee_per_gas > self.gas.max_fee
            || json!(tx.max_fee_per_gas.to_string()) != recorded("max_fee_per_gas")
        {
            Some(format!("max fee {}", tx.max_fee_per_gas))
        } else if tx.max_priority_fee_per_gas > tx.max_fee_per_gas
            || json!(tx.max_priority_fee_per_gas.to_string())
                != recorded("max_priority_fee_per_gas")
        {
            Some(format!("priority fee {}", tx.max_priority_fee_per_gas))
        } else {
            match (&order.ask, tx.to()) {
                (Ask::Payout(d), Some(to)) if to == self.contracts.roles => {
                    match evm::decode_delivery_call(&self.contracts, &tx.input) {
                        Some(decoded) if decoded == *d => None,
                        Some(decoded) => Some(format!("a delivery of {decoded:?}")),
                        None => Some("a call that is not the delivery role's".into()),
                    }
                }
                (Ask::Cancel { signed }, Some(to)) if to == self.lane.address => {
                    match self.cancel_target(*signed).await? {
                        Ok(target) if target.nonce == tx.nonce && tx.input.is_empty() => None,
                        Ok(_) => Some("a cancel at another nonce, or with calldata".into()),
                        Err(e) => Some(e),
                    }
                }
                (_, to) => Some(format!("target {to:?}")),
            }
        };
        if let Some(m) = mismatch {
            return Ok(Err(format!(
                "T-009-04: the payload of prepared transaction {prepared_id} does not match its \
                 order {order_id}: {m}"
            )));
        }
        Ok(Ok(tx))
    }

    /// Signs signing intent `intent_id`, persists the signed bytes, then submits them. See the
    /// module documentation.
    pub async fn sign_and_submit(&self, intent_id: i64) -> Result<Submitted, sqlx::Error> {
        let signed: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM signed_transactions WHERE intent_id = $1)",
        )
        .bind(intent_id)
        .fetch_one(self.pool)
        .await?;
        if signed {
            return Ok(Submitted::Refused(format!(
                "REQ-EO-1: intent {intent_id} is signed already; its bytes are re-sent, never re-signed"
            )));
        }
        let tx = match self.check(intent_id).await? {
            Ok(tx) => tx,
            Err(why) => {
                signal::anomaly_once(
                    self.pool,
                    &format!("{}:intent:{intent_id}:check", self.network),
                    json!({"spec": "T-009-04, INV-9", "network": self.network,
                           "intent": intent_id, "reason": why}),
                )
                .await?;
                return Ok(Submitted::Refused(why));
            }
        };
        // SF1, S5: the key may have been used outside bezent since the order was prepared; that
        // is asked first, so a nonce moved by someone else is a compromise, not just a stale nonce.
        match self.lane_use(intent_id).await? {
            LaneUse::Clean => {}
            LaneUse::Outside(why) => return Ok(Submitted::Refused(why)),
            LaneUse::Unreadable(e) => return Ok(Submitted::Unavailable(e)),
        }
        // The lane: the nonce is not consumed, and no other live signed transaction holds it.
        let chain = match self.pair.latest_nonce(self.lane.address).await {
            Ok(n) => n,
            Err(e) => return Ok(Submitted::Unavailable(read_err(e))),
        };
        if chain > tx.nonce {
            return Ok(Submitted::Refused(format!(
                "REQ-EO-4: nonce {} of the lane is consumed already (the chain is at {chain})",
                tx.nonce
            )));
        }
        let rivals: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM signed_transactions st \
             JOIN intents si ON si.id = st.intent_id \
             JOIN prepared_transactions p ON p.id::text = si.payload ->> 'prepared' \
             WHERE p.network = $1 AND p.lane = $2 AND p.nonce = $3 AND si.purpose <> 'cancel' \
               AND NOT EXISTS (SELECT 1 FROM non_execution_proofs n WHERE n.signed_transaction_id = st.id)",
        )
        .bind(self.network)
        .bind(self.lane_bytes())
        .bind(i64_of(tx.nonce)?)
        .fetch_one(self.pool)
        .await?;
        let purpose: String = sqlx::query_scalar("SELECT purpose FROM intents WHERE id = $1")
            .bind(intent_id)
            .fetch_one(self.pool)
            .await?;
        // A cancel replaces exactly one signed payout; a payout replaces nothing.
        if (purpose == "cancel" && rivals != 1) || (purpose != "cancel" && rivals != 0) {
            return Ok(Submitted::Refused(format!(
                "REQ-EO-1: nonce {} holds {rivals} signed payout(s) with no proof",
                tx.nonce
            )));
        }

        let mut client =
            match Client::connect(&self.signer.socket, &self.signer.token, self.signer.uid).await {
                Ok(c) => c,
                Err(e) => return Ok(Submitted::NotSigned(format!("the signer link: {e:?}"))),
            };
        let (key, scheme, public_key, signature) = match client.sign(self.pool, intent_id).await {
            Ok(Response::Signed {
                key,
                scheme,
                public_key,
                signature,
                ..
            }) => (key, scheme, public_key, signature),
            Ok(Response::Refused { spec, reason, .. }) => {
                return Ok(Submitted::NotSigned(format!(
                    "the signer refused ({spec}): {reason}"
                )));
            }
            Ok(other) => return Ok(Submitted::NotSigned(format!("the signer: {other:?}"))),
            Err(LinkError::Forged { .. }) => {
                return Ok(Submitted::NotSigned(
                    "AC-I1.2: the signer returned a signature over other bytes; key compromise raised"
                        .into(),
                ));
            }
            Err(e) => return Ok(Submitted::NotSigned(format!("the signer link: {e:?}"))),
        };
        let decode = |s: &str| s.strip_prefix("0x").and_then(|h| hex::decode(h).ok());
        let signed = (key == self.lane.key
            && scheme == "secp256k1-evm"
            && decode(&public_key).as_deref() == Some(self.lane.public_key.as_slice()))
        .then(|| decode(&signature))
        .flatten()
        .and_then(|sig| evm::signed(&tx, &sig))
        .filter(|s| s.from == self.lane.address);
        let Some(signed) = signed else {
            signal::raise_once(
                self.pool,
                &format!("{}:intent:{intent_id}:signer-key", self.network),
                json!({"network": self.network, "intent": intent_id, "key": key,
                       "public_key": public_key, "signature": signature,
                       "reason": "the signer answered with a signature that is not the lane key's"}),
            )
            .await?;
            return Ok(Submitted::NotSigned(
                "AC-I1.2: the signature is not the lane key's".into(),
            ));
        };

        // REQ-EO-1, INV-9: the exact bytes are stored before they are submitted.
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO signed_transactions (intent_id, attempt, bytes, tx_hash) \
             VALUES ($1, 1, $2, $3) RETURNING id",
        )
        .bind(intent_id)
        .bind(&signed.bytes)
        .bind(signed.hash.as_slice())
        .fetch_one(self.pool)
        .await?;
        tracing::info!(network = self.network, intent = intent_id, signed = id, hash = %signed.hash, "REQ-EO-1: signed bytes persisted; submitting");
        Ok(match self.pair.send_raw(&signed.bytes).await {
            Ok(_) => Submitted::Submitted {
                id,
                hash: signed.hash,
            },
            Err(reason) => Submitted::Persisted { id, reason },
        })
    }

    /// Re-sends every signed transaction of this lane whose outcome is not recorded: the same
    /// bytes, never a new signature (`REQ-EO-1`, `AC-D2.1`). Returns how many a provider took.
    pub async fn resend(&self) -> Result<usize, sqlx::Error> {
        let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(
            "SELECT st.id, st.bytes FROM signed_transactions st \
             JOIN intents si ON si.id = st.intent_id \
             JOIN prepared_transactions p ON p.id::text = si.payload ->> 'prepared' \
             WHERE si.network = $1 AND p.lane = $2 \
               AND NOT EXISTS (SELECT 1 FROM non_execution_proofs n WHERE n.signed_transaction_id = st.id) \
               AND NOT EXISTS (SELECT 1 FROM observations ob WHERE ob.network = $1 \
                 AND ob.position_kind = 'transaction' AND ob.detail ->> 'signed_transaction' = st.id::text) \
             ORDER BY p.nonce, st.id",
        )
        .bind(self.network)
        .bind(self.lane_bytes())
        .fetch_all(self.pool)
        .await?;
        let mut sent = 0;
        for (id, bytes) in rows {
            match self.pair.send_raw(&bytes).await {
                Ok(_) => sent += 1,
                Err(e) => {
                    tracing::debug!(network = self.network, signed = id, error = %e, "a re-send was not taken")
                }
            }
        }
        Ok(sent)
    }
}

impl Order {
    fn ask_is_cancel(&self) -> bool {
        matches!(self.ask, Ask::Cancel { .. })
    }
}

/// What `prepared_transactions.call` holds for an EVM transaction: its target, then its
/// calldata (a cancel has no calldata).
pub fn call_bytes(to: Address, input: &[u8]) -> Vec<u8> {
    [to.as_slice(), input].concat()
}

/// A persisted signed transaction: its EIP-1559 fields and its signature (`r ‖ s ‖ parity`).
pub fn decode_signed(bytes: &[u8]) -> Option<(TxEip1559, Vec<u8>)> {
    let mut buf = bytes;
    let TxEnvelope::Eip1559(signed) = TxEnvelope::decode_2718(&mut buf).ok()? else {
        return None;
    };
    if !buf.is_empty() {
        return None;
    }
    let sig = signed.signature();
    let mut out = Vec::with_capacity(65);
    out.extend_from_slice(&sig.r().to_be_bytes::<32>());
    out.extend_from_slice(&sig.s().to_be_bytes::<32>());
    out.push(u8::from(sig.v()));
    Some((signed.tx().clone(), out))
}
