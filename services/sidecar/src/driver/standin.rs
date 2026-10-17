// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The endpoints' own half of a delivery, which runs by itself (PLAN `DEC-3`): for every order the
//! core recorded on a network, a prepared transaction whose dry run passes (or fails, once, when
//! the harness asked for it: [`super::Driver::fail_next_dry_run`]); for every signing intent the
//! core recorded over a prepared transaction, a signature from the sidecar's test signer, over the
//! link, verified against the intent's bytes (`AC-I1.2`), and the signed bytes persisted
//! (`REQ-EO-1`). Nothing is submitted: the harness says when the transaction is included
//! ([`super::Driver::include`]) or proven not executed ([`super::Driver::not_executed`]).
//!
//! What a real endpoint builds is what the stand-in builds (the signer decodes it and compares it
//! with the core's order, the audit of PR #56 must-fix 2): an EVM payout is the vault's delivery
//! call on the origin network's chain id, through the contracts the signer's fixture declares; a
//! Kreivo mint or refund mint is a call of the on-chain component, on the fixture's genesis. The
//! keys are the published test keys of the signer's fixture, bound to (network, purpose) as its
//! configuration binds them.

use alloy::primitives::Address;
use bezent_evm_endpoint::evm::{self, Gas};
use bezent_signer::delivery;
use bezent_signer::link::{Client, LinkError, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use super::chain::{self, DRIVER};
use super::{Driver, Error};
use crate::setup::{ISSUING, ORIGIN, ORIGIN_CHAIN_ID, TOKEN};

/// How many blocks a prepared transaction is valid for, from the final head it is born at.
pub const VALIDITY: i64 = 16;

/// The gas of a payout the stand-in builds.
const GAS: Gas = Gas {
    limit: 200_000,
    max_fee: 100_000_000_000,
    priority_fee: 1_000_000_000,
};

/// What one round did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tended {
    /// Orders prepared.
    pub prepared: usize,
    /// Signing intents signed, their signed bytes recorded.
    pub signed: usize,
}

/// A signing key's lane: its key id and the sender its nonces count on.
#[derive(Clone, Debug)]
pub struct Lane {
    /// The key id.
    pub key: String,
    /// The sender: an EVM address, or a Kreivo account.
    pub sender: Vec<u8>,
}

impl Driver {
    /// One round of the endpoints' own work on both networks. Serialised with the driver's
    /// calls, so a call never sees half a preparation.
    pub async fn tend(&self) -> Result<Tended, Error> {
        let _one = self.lock.lock().await;
        self.guard().await?;
        let mut done = Tended::default();
        for network in [ISSUING, ORIGIN] {
            done.prepared += self.prepare_orders(network).await?;
        }
        for network in [ISSUING, ORIGIN] {
            done.signed += self.sign_intents(network).await?;
        }
        Ok(done)
    }

    async fn prepare_orders(&self, network: &str) -> Result<usize, Error> {
        let pool = self.pool(network)?;
        type Order = (i64, String, Option<Vec<u8>>, Value);
        let orders: Vec<Order> = sqlx::query_as(
            "SELECT o.id, o.purpose, o.on_chain_reference, o.payload FROM intents o \
             WHERE o.network = $1 AND o.payload ? 'order' AND NOT o.payload ? 'prepared' \
               AND o.purpose IN ('mint', 'refund-mint', 'payout') \
               AND NOT EXISTS (SELECT 1 FROM prepared_transactions p WHERE p.order_intent_id = o.id) \
             ORDER BY o.id",
        )
        .bind(network)
        .fetch_all(pool)
        .await?;
        let mut done = 0;
        for (id, purpose, reference, payload) in orders {
            let Some(lane) = self.lanes.get(&(network.to_owned(), purpose.clone())) else {
                tracing::warn!(
                    order = id,
                    network,
                    purpose,
                    "no test key is bound to this network and purpose; the order stays unprepared"
                );
                continue;
            };
            self.prepare(pool, network, id, reference.as_deref(), &payload, lane)
                .await?;
            done += 1;
        }
        Ok(done)
    }

    async fn prepare(
        &self,
        pool: &PgPool,
        network: &str,
        order: i64,
        reference: Option<&[u8]>,
        payload: &Value,
        lane: &Lane,
    ) -> Result<(), Error> {
        let mut tx = pool.begin().await?;
        let head = chain::final_head(&mut tx, network).await?;
        let nonce: i64 = sqlx::query_scalar(
            "SELECT COALESCE(max(nonce) + 1, 0) FROM prepared_transactions \
             WHERE network = $1 AND lane = $2",
        )
        .bind(network)
        .bind(&lane.sender)
        .fetch_one(&mut *tx)
        .await?;
        let amount = payload["amount"]
            .as_str()
            .and_then(|a| a.parse::<u128>().ok())
            .unwrap_or(0);
        let mut failure = self.next_dry_run_failure(network);
        let reference: [u8; 32] = match reference.and_then(|r| r.try_into().ok()) {
            Some(r) => r,
            None => {
                failure.get_or_insert_with(|| "the order's reference is not 32 bytes".to_owned());
                [0; 32]
            }
        };
        let fixture = fixture_network(network);
        // The exact bytes the network's endpoint builds (the signer decodes them and compares them
        // with the core's order: audit must-fix 2).
        let (call, signing_payload, message) = if network == ORIGIN {
            // The vault's delivery call: a transfer of the origin token to the beneficiary and
            // its reference, through the delivery role, on the origin network's chain id.
            let to = payload["order"]["to"]
                .as_str()
                .and_then(|a| a.parse::<Address>().ok());
            if to.is_none() && failure.is_none() {
                failure = Some("the payout's destination is not an EVM address".to_owned());
            }
            let contracts = fixture.delivery.unwrap_or(delivery::EvmContracts {
                roles: [0; 20],
                multisend: [0; 20],
                emitter: [0; 20],
                role_key: [0; 32],
            });
            let token = TOKEN.parse::<Address>().unwrap_or(Address::ZERO);
            let input = delivery::evm_payout_call(
                &contracts,
                &delivery::EvmPayout {
                    token: token.into_array(),
                    to: to.unwrap_or(Address::ZERO).into_array(),
                    amount,
                    reference,
                },
            );
            let roles = Address::from(contracts.roles);
            let t = evm::transaction(
                ORIGIN_CHAIN_ID,
                u64::try_from(nonce).unwrap_or_default(),
                roles,
                input.clone(),
                GAS,
            );
            let mut call = roles.to_vec();
            call.extend_from_slice(&input);
            let message = evm::signing_payload(&t);
            (call, message.clone(), message)
        } else {
            // A call of the on-chain component: the mint or refund mint the order names, at the
            // lane's nonce, born at the final head.
            let order = &payload["order"];
            let account = |field: &str| -> Option<[u8; 32]> {
                order[field]
                    .as_str()
                    .and_then(|t| t.strip_prefix("0x"))
                    .and_then(|t| hex::decode(t).ok())
                    .and_then(|t| t.try_into().ok())
            };
            let asset = order["asset"]
                .as_str()
                .and_then(|a| a.strip_prefix("Here("))
                .and_then(|a| a.strip_suffix(')'))
                .and_then(|a| a.parse::<u32>().ok());
            let message = match (order.get("redeemer").is_some(), asset, account("to")) {
                (false, Some(asset), Some(to)) => Some(delivery::KreivoMessage::Mint {
                    asset,
                    to,
                    amount,
                    reference,
                }),
                (true, _, _) => {
                    account("redeemer").map(|redeemer| delivery::KreivoMessage::RefundMint {
                        redeemer,
                        reference,
                    })
                }
                _ => None,
            };
            match message {
                None => {
                    failure.get_or_insert_with(|| {
                        "the order names no mint the contract takes".to_owned()
                    });
                    let named = format!("bezent-sidecar-driver:{}:{order}", payload["order"]);
                    let named = named.into_bytes();
                    (named.clone(), named.clone(), named)
                }
                Some(message) => {
                    let call = message.data();
                    let payload = delivery::kreivo_signing_payload(&delivery::KreivoTx {
                        contract: fixture.contract,
                        value: 0,
                        ref_time: 1_000_000_000,
                        proof_size: 100_000,
                        storage_deposit_limit: None,
                        data: call.clone(),
                        era: [0x05, 0x00],
                        nonce: u32::try_from(nonce).unwrap_or(u32::MAX),
                        tip: 0,
                        spec_version: 134,
                        transaction_version: 13,
                        genesis: fixture.genesis,
                        birth_hash: head.hash.clone().try_into().unwrap_or([0; 32]),
                    });
                    let message = delivery::kreivo_message_to_sign(&payload);
                    (call, payload, message)
                }
            }
        };
        let passed = failure.is_none();
        let dry_run = json!({
            "passed": passed,
            "reason": failure,
            "driver": DRIVER,
            "block": head.number,
        });
        sqlx::query(
            "INSERT INTO prepared_transactions (order_intent_id, attempt, network, lane, key_id, \
               nonce, valid_from_block, valid_until_block, checkpoint_hash, call, signing_payload, \
               message, dry_run_block_number, dry_run_block_hash, dry_run_passed, dry_run) \
             VALUES ($1, 1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $13, $6, $8, $11, $12::text::jsonb)",
        )
        .bind(order)
        .bind(network)
        .bind(&lane.sender)
        .bind(&lane.key)
        .bind(nonce)
        .bind(head.number)
        .bind(head.number + VALIDITY)
        .bind(&head.hash)
        .bind(&call)
        .bind(&signing_payload)
        .bind(passed)
        .bind(dry_run.to_string())
        .bind(&message)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn sign_intents(&self, network: &str) -> Result<usize, Error> {
        let pool = self.pool(network)?;
        let intents: Vec<(i64, Vec<u8>)> = sqlx::query_as(
            "SELECT i.id, p.message FROM intents i \
             JOIN prepared_transactions p ON p.id::text = i.payload ->> 'prepared' \
             WHERE i.network = $1 AND p.network = $1 AND p.dry_run_passed \
               AND NOT EXISTS (SELECT 1 FROM signed_transactions st WHERE st.intent_id = i.id) \
             ORDER BY i.id",
        )
        .bind(network)
        .fetch_all(pool)
        .await?;
        if intents.is_empty() {
            return Ok(0);
        }
        let mut client =
            match Client::connect(&self.link.socket, &self.link.token, self.link.uid).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = ?e, "the signer link is not up; signing waits");
                    return Ok(0);
                }
            };
        let mut done = 0;
        for (intent, message) in intents {
            let signature = match client.sign(pool, intent).await {
                Ok(Response::Signed { signature, .. }) => signature,
                Ok(Response::Refused { spec, reason, .. }) => {
                    tracing::warn!(intent, spec, reason, "the test signer refused the intent");
                    continue;
                }
                Ok(other) => {
                    tracing::warn!(intent, answer = ?other, "the test signer did not sign");
                    continue;
                }
                Err(LinkError::Forged { .. }) => {
                    tracing::error!(
                        intent,
                        "AC-I1.2: the signer returned a signature over other bytes"
                    );
                    continue;
                }
                Err(e) => {
                    tracing::warn!(intent, error = ?e, "the signer link failed; signing waits");
                    return Ok(done);
                }
            };
            let Some(signature) = signature
                .strip_prefix("0x")
                .and_then(|h| hex::decode(h).ok())
            else {
                continue;
            };
            let (bytes, hash) = if network == ORIGIN {
                let Some(signed) =
                    evm::decode_signing_payload(&message).and_then(|t| evm::signed(&t, &signature))
                else {
                    tracing::error!(intent, "the signature does not make a signed transaction");
                    continue;
                };
                (signed.bytes, signed.hash.to_vec())
            } else {
                let mut bytes = message;
                bytes.extend_from_slice(&signature);
                let hash = Sha256::digest(&bytes).to_vec();
                (bytes, hash)
            };
            sqlx::query(
                "INSERT INTO signed_transactions (intent_id, attempt, bytes, tx_hash) \
                 VALUES ($1, 1, $2, $3)",
            )
            .bind(intent)
            .bind(&bytes)
            .bind(&hash)
            .execute(pool)
            .await?;
            done += 1;
        }
        Ok(done)
    }
}

/// What the signer's published fixture declares for a network: what a delivery's bytes name there
/// (audit must-fix 2: the signer compares them with the core's order).
struct FixtureNetwork {
    /// The vault's contracts, on the origin network.
    delivery: Option<delivery::EvmContracts>,
    /// The on-chain component's account, on the issuing network.
    contract: [u8; 32],
    /// The issuing network's genesis.
    genesis: [u8; 32],
}

fn fixture_network(id: &str) -> FixtureNetwork {
    let hex_n = |text: Option<&str>| -> Vec<u8> {
        text.map(|t| t.strip_prefix("0x").unwrap_or(t))
            .and_then(|t| hex::decode(t).ok())
            .unwrap_or_default()
    };
    let config = bezent_signer::config::Config::parse(crate::setup::SIGNER_CONFIG).ok();
    let network = config
        .as_ref()
        .and_then(|c| c.networks.iter().find(|n| n.id == id));
    let delivery = network.and_then(|n| n.delivery.as_ref()).and_then(|d| {
        Some(delivery::EvmContracts {
            roles: hex_n(Some(&d.roles)).try_into().ok()?,
            multisend: hex_n(Some(&d.multisend)).try_into().ok()?,
            emitter: hex_n(Some(&d.emitter)).try_into().ok()?,
            role_key: delivery::role_key(&d.role)?,
        })
    });
    FixtureNetwork {
        delivery,
        contract: hex_n(network.and_then(|n| n.contract_account.as_deref()))
            .try_into()
            .unwrap_or([0; 32]),
        genesis: hex_n(network.and_then(|n| n.genesis.as_deref()))
            .try_into()
            .unwrap_or([0; 32]),
    }
}
