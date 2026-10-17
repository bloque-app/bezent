// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! What became of each signed transaction, from chain data (`T-007-04`; `REQ-NX-1`, `REQ-NX-2`,
//! `REQ-NX-3`, `REQ-OB-6`, `AC-C2.1`, `AC-I1.2`, `INV-8`).
//!
//! **What the transaction is** comes from its signed bytes, never from the store's rows alone
//! (review M2): the persisted extrinsic gives the sender, the nonce `N`, the era, the tip and the
//! call; the prepared signing payload, whose signature by the lane key the extrinsic carries, gives
//! the hash of the era's birth block; the chain gives that block's number. So the transaction is
//! valid in blocks `valid_from` (the birth) to `valid_until` (`valid_from + period - 1`). Rows that
//! disagree with the bytes are an anomaly, recorded, and nothing is proven.
//!
//! **Its nonce** is consumed once, by whichever of the sender's transactions is included with it;
//! inclusion consumes it whether or not the dispatch succeeds. At the finalized head `F`:
//!
//! - **nonce at F ≤ N, and F ≥ valid_until**: the nonce was never consumed and the era has ended at
//!   a final block, so no copy of the transaction was included and none ever can be: **not
//!   executed** (`nonce-unconsumed`). Anyone reads `System::Account(sender).nonce` at `F`.
//! - **nonce at F > N**: it was consumed at the block `X` whose nonce first exceeds `N` (found by
//!   walking back from `F` by parent hash, never by number). The consumer is the extrinsic of `X`
//!   from the sender with nonce `N`, decoded. It is **this transaction** if it is the same signed
//!   payload (sender, era, nonce, tip and call), whatever its signature: a re-signed copy is the
//!   same mint (review M1). Its result is then `X`'s `ExtrinsicSuccess` or `ExtrinsicFailed` for
//!   it, recorded as an observation at its coordinate (`REQ-OB-6`); a failure is an on-chain
//!   refusal, which counts as not executed once final (`refused`, `REQ-NX-1`), unless the copy's
//!   signature is not one bezent recorded. It is **not this transaction** only if it is a
//!   recognised bezent transaction: its signature answers an intent bezent recorded, its call is a
//!   direct `Contracts::call` of a delivery message for that intent's reference, and that
//!   reference is not this one: then this one can never execute (`nonce-consumed-by-other`, with
//!   the consumer's coordinate). A consumer bezent did not sign is never one (review M1-bis): a
//!   wrapper such as `Utility::batch_all` can carry this reference to the contract with the lane
//!   as origin.
//! - **the contract agrees**: before any proof, the contract's own single-use state is read at
//!   `F` (`mint_reference_used`, or the redemption's `refunded` flag for a refund mint). A used
//!   reference is never proven not executed (`INV-8`): a `delivery-anomaly` is recorded instead.
//!   It catches a mint made under another nonce, or by a call that does not consume the lane's
//!   nonce at all (a proxy, a multisig, a `dispatch_as`).
//! - **anything else is unknown** (`REQ-NX-3`), and nothing is proven: the era is still open; the
//!   chain did not answer; a block or a nonce could not be read; the walk would go further back
//!   than `max_walk`; the consumer cannot be identified (no extrinsic of the sender in `X`
//!   decodes); or it carries this transaction's reference in another payload.
//!
//! **Compromise and anomaly signals** (`AC-I1.2`; [`crate::signal`]) are written to the store
//! before anything is recorded, and a proof is written only after them, once and complete: a
//! signature of the delivery key on chain that bezent did not record (a consumer, or a re-signed
//! copy), an extrinsic of the delivery key that does not decode, a nonce consumed with no extrinsic
//! of the key in its block, and bytes that do not answer their rows.
//!
//! The endpoint records proofs and inclusions; what they mean for a transfer is the core's.

use bezent_signer::compromise::ObservedSignature;
use serde_json::json;
use sqlx::PgPool;

use crate::chain::{BlockRef, Chain, ancestry};
use crate::kreivo::{self, AccountId32, Decoded, Message, Mortality, blake2_256};
use crate::signal::{self, Seen};
use crate::submit::Lane;

/// A signed transaction whose outcome is not recorded yet, as the store holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Watched {
    /// The `signed_transactions` row.
    pub id: i64,
    /// Its exact bytes.
    pub bytes: Vec<u8>,
    /// The signing payload of its prepared transaction.
    pub signing_payload: Vec<u8>,
    /// Its intent's on-chain reference.
    pub reference: Option<Vec<u8>>,
    /// The prepared transaction's nonce and era, as its row says: checked against the bytes.
    pub row: (u64, u64, u64),
}

/// A signed transaction as its bytes say (review M2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signed {
    /// The extrinsic, decoded.
    pub decoded: Decoded,
    /// Its hash.
    pub hash: [u8; 32],
    /// Its nonce.
    pub nonce: u64,
    /// The hash of the era's birth block.
    pub birth_hash: [u8; 32],
    /// The first block it is valid in: the era's birth.
    pub valid_from: u64,
    /// The last block it is valid in.
    pub valid_until: u64,
}

/// A proof of non-execution (`AC-C2.1`): every coordinate is public chain data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proof {
    /// The network.
    pub network: String,
    /// The sender.
    pub signer: [u8; 32],
    /// The nonce.
    pub nonce: u64,
    /// The era: first valid block.
    pub valid_from: u64,
    /// The era: last valid block.
    pub valid_until: u64,
    /// The final block that proves it.
    pub final_block: BlockRef,
    /// The sender's nonce at that block.
    pub nonce_at_final: u64,
    /// How.
    pub kind: ProofKind,
}

/// How a transaction is proven not executed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProofKind {
    /// The era ended at a final block with the nonce unconsumed.
    NonceUnconsumed,
    /// Another transaction, identified and not this one, consumed the nonce at `at`, `index`.
    NonceConsumedByOther {
        /// The block that consumed it.
        at: BlockRef,
        /// The consumer's index in it.
        index: u32,
        /// The consumer's hash.
        consumer: [u8; 32],
    },
    /// The transaction (these bytes, or a re-signed copy, `hash`) was included at `at`, position
    /// `index`, and refused by the network.
    Refused {
        /// The block.
        at: BlockRef,
        /// The extrinsic's index.
        index: u32,
        /// The included extrinsic's hash.
        hash: [u8; 32],
        /// The network's error.
        error: Option<String>,
    },
}

impl ProofKind {
    fn name(&self) -> &'static str {
        match self {
            ProofKind::NonceUnconsumed => "nonce-unconsumed",
            ProofKind::NonceConsumedByOther { .. } => "nonce-consumed-by-other",
            ProofKind::Refused { .. } => "refused",
        }
    }
}

/// What became of a signed transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Disposition {
    /// Included at `at`, position `index` (these bytes, or a re-signed copy, `hash`), and its
    /// dispatch succeeded.
    Executed {
        /// The block.
        at: BlockRef,
        /// The extrinsic's index.
        index: u32,
        /// The included extrinsic's hash.
        hash: [u8; 32],
    },
    /// Proven not executed.
    NotExecuted(Proof),
    /// Not known yet (`REQ-NX-3`).
    Unknown(String),
}

/// Finds what became of each signed transaction of a network. See the module documentation.
pub struct Prover<'a, C> {
    /// The chain.
    pub chain: &'a C,
    /// The store, as the endpoint's role.
    pub pool: &'a PgPool,
    /// The network id.
    pub network: &'a str,
    /// The contract: a consumer that calls it with this transaction's reference is never "other".
    pub contract: AccountId32,
    /// The lane.
    pub lane: &'a Lane,
    /// The most blocks walked back from the finalized head.
    pub max_walk: u64,
}

fn i64_of(n: u64) -> Result<i64, sqlx::Error> {
    i64::try_from(n).map_err(|e| sqlx::Error::Encode(Box::new(e)))
}

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn unknown(why: impl Into<String>) -> Disposition {
    Disposition::Unknown(why.into())
}

/// The contract message a call makes, if it is a direct `Contracts::call` of one of `contract`'s
/// delivery messages. Nothing else is read into: a wrapper (`Utility::batch`, `batch_all`,
/// `force_batch`, `Proxy::proxy`, a multisig, `dispatch_as`) is `None`.
fn message_to(contract: &AccountId32, call: &[u8]) -> Option<Message> {
    let (dest, _, _, data) = kreivo::decode_contracts_call(call)?;
    if dest != *contract {
        return None;
    }
    Message::decode(&data)
}

/// The on-chain reference a call carries to `contract`, if it is one of its messages.
fn reference_to(contract: &AccountId32, call: &[u8]) -> Option<[u8; 32]> {
    match message_to(contract, call)? {
        Message::Mint { reference, .. } | Message::RefundMint { reference, .. } => Some(reference),
    }
}

impl<C: Chain> Prover<'_, C> {
    /// The network's signed transactions with no recorded outcome.
    pub async fn watched(&self) -> Result<Vec<Watched>, sqlx::Error> {
        type Row = (i64, Vec<u8>, Vec<u8>, Option<Vec<u8>>, i64, i64, i64);
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT st.id, st.bytes, p.signing_payload, si.on_chain_reference, \
               p.nonce, p.valid_from_block, p.valid_until_block \
             FROM signed_transactions st \
             JOIN intents si ON si.id = st.intent_id \
             JOIN prepared_transactions p ON p.id::text = si.payload ->> 'prepared' \
             WHERE si.network = $1 AND p.lane = $2 \
               AND NOT EXISTS (SELECT 1 FROM non_execution_proofs n WHERE n.signed_transaction_id = st.id) \
               AND NOT EXISTS (SELECT 1 FROM observations ob WHERE ob.network = $1 \
                 AND ob.position_kind = 'transaction' AND ob.detail ->> 'signed_transaction' = st.id::text) \
             ORDER BY p.nonce, st.id",
        )
        .bind(self.network)
        .bind(self.lane.account.as_slice())
        .fetch_all(self.pool)
        .await?;
        let u = |n: i64| u64::try_from(n).unwrap_or(u64::MAX);
        Ok(rows
            .into_iter()
            .map(
                |(id, bytes, signing_payload, reference, nonce, from, until)| Watched {
                    id,
                    bytes,
                    signing_payload,
                    reference,
                    row: (u(nonce), u(from), u(until)),
                },
            )
            .collect())
    }

    /// What `w` is, from its bytes and the chain (review M2). `Err` is why it cannot be told;
    /// bytes that do not answer their rows are recorded as an anomaly.
    async fn signed(&self, w: &Watched) -> Result<Result<Signed, String>, sqlx::Error> {
        let Some(decoded) = kreivo::decode_extrinsic(&w.bytes) else {
            return self
                .anomaly(w, "the signed bytes do not decode as a delivery")
                .await;
        };
        let Some(payload) = kreivo::decode_signing_payload(&w.signing_payload) else {
            return self
                .anomaly(w, "the prepared signing payload does not decode")
                .await;
        };
        let message = kreivo::message_to_sign(&w.signing_payload);
        if decoded.account != self.lane.account
            || decoded.scheme != self.lane.scheme
            || decoded.call != payload.call
            || decoded.era != payload.era
            || decoded.nonce != payload.nonce
            || decoded.tip != payload.tip
            || !bezent_signer::verify::verifies(
                self.lane.scheme.name(),
                &self.lane.public_key,
                &message,
                &decoded.signature,
            )
        {
            return self
                .anomaly(
                    w,
                    "the signed bytes are not the lane key's signature over the prepared payload",
                )
                .await;
        }
        // "Our reference" is the one the signed call carries, never only the intent's row
        // (review note on M1); the two must agree.
        match reference_to(&self.contract, &decoded.call) {
            Some(r) if w.reference.as_deref() == Some(r.as_slice()) => {}
            _ => {
                return self
                    .anomaly(
                        w,
                        "the signed call is not a contract message for its intent's on-chain reference",
                    )
                    .await;
            }
        }
        let birth = match self.chain.header(&payload.birth_hash).await {
            Ok(h) => h.block.number,
            Err(e) => return Ok(Err(format!("the era's birth block: {e}"))),
        };
        let (period, _) = Mortality::period_phase(decoded.era);
        if Mortality::birth_at(decoded.era, birth) != birth {
            return self
                .anomaly(w, "the era's phase does not match its birth block")
                .await;
        }
        let signed = Signed {
            hash: blake2_256(&w.bytes),
            nonce: u64::from(decoded.nonce),
            birth_hash: payload.birth_hash,
            valid_from: birth,
            valid_until: birth + period - 1,
            decoded,
        };
        if w.row != (signed.nonce, signed.valid_from, signed.valid_until) {
            return self
                .anomaly(
                    w,
                    "the prepared transaction's nonce or era is not its signed bytes'",
                )
                .await;
        }
        Ok(Ok(signed))
    }

    async fn anomaly<T>(&self, w: &Watched, why: &str) -> Result<Result<T, String>, sqlx::Error> {
        signal::anomaly_once(
            self.pool,
            &format!("{}:signed:{}:bytes", self.network, w.id),
            json!({"network": self.network, "signed_transaction": w.id, "reason": why}),
        )
        .await?;
        Ok(Err(why.to_owned()))
    }

    /// What became of `w`, at the finalized head `finalized`. Records no outcome; writes the
    /// signals it finds first (see the module documentation). No proof is returned unless the
    /// contract itself, read at `finalized`, says the reference was not used (review M1-bis).
    pub async fn disposition(
        &self,
        w: &Watched,
        finalized: &BlockRef,
    ) -> Result<Disposition, sqlx::Error> {
        let s = match self.signed(w).await? {
            Ok(s) => s,
            Err(why) => return Ok(unknown(why)),
        };
        let d = self.chain_disposition(w, &s, finalized).await?;
        if let Disposition::NotExecuted(p) = &d
            && let Err(why) = self.reference_unused(w, &s, &p.final_block).await?
        {
            return Ok(unknown(why));
        }
        Ok(d)
    }

    /// The contract's own single-use state for `s`'s message at `at` (review M1-bis, `INV-8`):
    /// `mint_reference_used(reference)` for a mint, the redemption's `refunded` flag for a refund
    /// mint (a redemption the contract does not hold was never refunded: the contract never
    /// removes one). `Ok(())` only when the contract answers that it was not used; a used one is
    /// an anomaly, and an answer that cannot be read is unknown.
    async fn reference_unused(
        &self,
        w: &Watched,
        s: &Signed,
        at: &BlockRef,
    ) -> Result<Result<(), String>, sqlx::Error> {
        let Some(message) = message_to(&self.contract, &s.decoded.call) else {
            return self
                .anomaly(w, "the signed call is not a contract delivery message")
                .await;
        };
        let (input, reference) = match &message {
            Message::Mint { reference, .. } => {
                (kreivo::mint_reference_used_data(reference), *reference)
            }
            Message::RefundMint {
                redeemer,
                reference,
            } => (kreivo::redemption_data(redeemer, reference), *reference),
        };
        let args = kreivo::contracts_api_call_args(&self.lane.account, &self.contract, &input);
        let answer = match self.chain.contracts_api_call(at, &args).await {
            Ok(b) => b,
            Err(e) => return Ok(Err(format!("the contract's single-use state: {e}"))),
        };
        let value = match kreivo::decode_getter(&answer) {
            Ok(v) => v,
            Err(e) => return Ok(Err(e)),
        };
        let used = match &message {
            Message::Mint { .. } => kreivo::decode_exact::<bool>(&value),
            Message::RefundMint { .. } => {
                kreivo::decode_exact::<Option<kreivo::Redemption>>(&value)
                    .map(|r| r.is_some_and(|r| r.refunded))
            }
        };
        match used {
            Some(false) => Ok(Ok(())),
            None => Ok(Err("the contract's single-use state does not decode".into())),
            Some(true) => {
                signal::anomaly_once(
                    self.pool,
                    &format!("{}:signed:{}:reference-used", self.network, w.id),
                    json!({
                        "network": self.network,
                        "signed_transaction": w.id,
                        "reason": "the chain shows this transaction not executed, but the contract says its reference was used; nothing is proven (INV-8)",
                        "on_chain_reference": hex0x(&reference),
                        "block_number": at.number,
                        "block_hash": hex0x(&at.hash),
                    }),
                )
                .await?;
                Ok(Err(format!(
                    "the contract says reference {} was used at block {}",
                    hex0x(&reference),
                    at.number
                )))
            }
        }
    }

    /// What the chain's nonces and blocks say became of `s`.
    async fn chain_disposition(
        &self,
        w: &Watched,
        s: &Signed,
        finalized: &BlockRef,
    ) -> Result<Disposition, sqlx::Error> {
        let account = self.lane.account;
        let nonce_at_final = match self.chain.account_nonce(finalized, &account).await {
            Ok(n) => n,
            Err(e) => return Ok(unknown(e.to_string())),
        };
        let proof = |final_block: BlockRef, kind| Proof {
            network: self.network.to_owned(),
            signer: account,
            nonce: s.nonce,
            valid_from: s.valid_from,
            valid_until: s.valid_until,
            final_block,
            nonce_at_final,
            kind,
        };
        if nonce_at_final <= s.nonce {
            return Ok(if finalized.number >= s.valid_until {
                Disposition::NotExecuted(proof(*finalized, ProofKind::NonceUnconsumed))
            } else {
                unknown(format!(
                    "the era is open until block {} (finalized: {})",
                    s.valid_until, finalized.number
                ))
            });
        }
        // Consumed: find the block that consumed it, from the era's birth to the head.
        let blocks = match ancestry(self.chain, finalized, s.valid_from, self.max_walk).await {
            Ok(Some(b)) if !b.is_empty() => b,
            Ok(_) => {
                return Ok(unknown(format!(
                    "the era's birth {} is more than max_walk ({}) behind the finalized head",
                    s.valid_from, self.max_walk
                )));
            }
            Err(e) => return Ok(unknown(e.to_string())),
        };
        if blocks[0].hash != s.birth_hash {
            return Ok(unknown(format!(
                "the era's birth block {} is not on the finalized chain",
                s.valid_from
            )));
        }
        // The sender's nonce never decreases: the first block whose nonce exceeds N consumed it.
        let (mut lo, mut hi) = (0usize, blocks.len() - 1);
        match self.chain.account_nonce(&blocks[0], &account).await {
            Ok(n) if n > s.nonce => {
                return Ok(unknown(format!(
                    "the nonce {} was consumed at or before the era's birth {}",
                    s.nonce, s.valid_from
                )));
            }
            Ok(_) => {}
            Err(e) => return Ok(unknown(e.to_string())),
        }
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            match self.chain.account_nonce(&blocks[mid], &account).await {
                Ok(n) if n > s.nonce => hi = mid,
                Ok(_) => lo = mid,
                Err(e) => return Ok(unknown(e.to_string())),
            }
        }
        let at = blocks[hi];
        self.consumer(w, s, &at, finalized, nonce_at_final).await
    }

    /// Identifies the extrinsic of block `at` that consumed `s`'s nonce, and what that means.
    async fn consumer(
        &self,
        w: &Watched,
        s: &Signed,
        at: &BlockRef,
        finalized: &BlockRef,
        nonce_at_final: u64,
    ) -> Result<Disposition, sqlx::Error> {
        let account = self.lane.account;
        let extrinsics = match self.chain.extrinsics(at).await {
            Ok(x) => x,
            Err(e) => return Ok(unknown(e.to_string())),
        };
        let mut found = None;
        let mut undecodable = Vec::new();
        for (index, x) in extrinsics.iter().enumerate() {
            match kreivo::decode_extrinsic(x) {
                Some(d) if d.account == account && u64::from(d.nonce) == s.nonce => {
                    found = Some((index, d, x));
                    break;
                }
                Some(_) => {}
                None if kreivo::signed_sender(x) == Some(account) => undecodable.push(index),
                None => {}
            }
        }
        let coordinate = |index: Option<usize>| {
            json!({
                "network": self.network,
                "block_number": at.number,
                "block_hash": hex0x(&at.hash),
                "position_kind": "transaction",
                "position_index": index,
            })
        };
        let Some((index, d, x)) = found else {
            // REQ-NX-3: the consumer is not identified, so nothing is proven; AC-I1.2: the key
            // consumed a nonce in a way bezent cannot match to anything it recorded.
            let (dedupe, reason) = match undecodable.first() {
                Some(i) => (
                    format!("{}:{}:{i}", self.network, hex0x(&at.hash)),
                    "an extrinsic of the delivery key that the endpoint cannot decode consumed a lane nonce",
                ),
                None => (
                    format!("{}:{}:nonce:{}", self.network, hex0x(&at.hash), s.nonce),
                    "a lane nonce was consumed with no extrinsic of the delivery key in its block",
                ),
            };
            signal::raise_once(
                self.pool,
                &dedupe,
                json!({
                    "network": self.network,
                    "reason": reason,
                    "public_key": hex0x(&self.lane.public_key),
                    "nonce": s.nonce,
                    "coordinate": coordinate(undecodable.first().copied()),
                    "signed_transaction": w.id,
                }),
            )
            .await?;
            return Ok(unknown(format!(
                "the nonce {} was consumed at block {} by an extrinsic that is not identified",
                s.nonce, at.number
            )));
        };
        let index32 = u32::try_from(index).unwrap_or(u32::MAX);
        let hash = blake2_256(x);
        let observed = ObservedSignature {
            network: self.network.to_owned(),
            public_key: self.lane.public_key.clone(),
            signature: d.signature.clone(),
            block_number: i64_of(at.number)?,
            block_hash: at.hash.to_vec(),
            position_index: i64::try_from(index).unwrap_or(i64::MAX),
        };

        if d.same_payload(&s.decoded) {
            // This transaction, or a re-signed copy of it (REQ-NX-1, INV-8). A copy whose
            // signature bezent did not record is a compromise (AC-I1.2); its result is ours.
            let copy_unrecorded = *x != w.bytes
                && signal::check_signature(self.pool, &observed).await? == Seen::Compromise;
            let events = match self.chain.events(at, &[0; 32]).await {
                Ok(e) => e,
                Err(e) => return Ok(unknown(e.to_string())),
            };
            return Ok(match events.results.iter().find(|r| r.index == index32) {
                Some(r) if r.success => Disposition::Executed {
                    at: *at,
                    index: index32,
                    hash,
                },
                // M1-bis: a copy bezent did not sign proves nothing, not even a refusal.
                Some(_) if copy_unrecorded => unknown(format!(
                    "extrinsic {index} of block {} is a copy of this transaction bezent did not sign",
                    at.number
                )),
                Some(r) => Disposition::NotExecuted(Proof {
                    network: self.network.to_owned(),
                    signer: account,
                    nonce: s.nonce,
                    valid_from: s.valid_from,
                    valid_until: s.valid_until,
                    final_block: *finalized,
                    nonce_at_final,
                    kind: ProofKind::Refused {
                        at: *at,
                        index: index32,
                        hash,
                        error: r.error.clone(),
                    },
                }),
                None => unknown(format!(
                    "no execution result for extrinsic {index} of block {}",
                    at.number
                )),
            });
        }

        // Another payload. Its signature is matched first (AC-I1.2), then it must be another
        // order's: the same reference to the contract, in its call or in the intent its signature
        // answers, is never "other" (INV-8).
        let seen = signal::check_signature(self.pool, &observed).await?;
        // `signed` checked that our call carries our reference.
        let ours = reference_to(&self.contract, &s.decoded.call);
        let theirs = reference_to(&self.contract, &d.call);
        let recorded = match seen {
            Seen::Recorded { intent_id } => {
                let reference: Option<Option<Vec<u8>>> =
                    sqlx::query_scalar("SELECT on_chain_reference FROM intents WHERE id = $1")
                        .bind(intent_id)
                        .fetch_optional(self.pool)
                        .await?;
                Some(reference.flatten())
            }
            Seen::Compromise => None,
        };
        let ours_bytes = ours.as_ref().map(|o| o.as_slice());
        let twin = ours.is_some()
            && (theirs == ours
                || recorded
                    .as_ref()
                    .is_some_and(|r| r.as_deref() == ours_bytes));
        if twin {
            signal::anomaly_once(
                self.pool,
                &format!("{}:signed:{}:twin", self.network, w.id),
                json!({
                    "network": self.network,
                    "signed_transaction": w.id,
                    "reason": "the lane nonce was consumed by another payload for the same on-chain reference; nothing is proven (INV-8)",
                    "coordinate": coordinate(Some(index)),
                    "consumer": hex0x(&hash),
                }),
            )
            .await?;
            return Ok(unknown(format!(
                "the nonce {} was consumed at block {} by another payload for this reference",
                s.nonce, at.number
            )));
        }
        // M1-bis (INV-8): a consumer bezent did not sign is never a proof. Whatever its call
        // (a wrapper such as `Utility::batch_all` can carry this reference to the contract with
        // the lane as origin), nothing it did can be reasoned about; the compromise path holds
        // the lane.
        let Some(recorded) = recorded else {
            return Ok(unknown(format!(
                "the nonce {} was consumed at block {} by a transaction bezent did not sign",
                s.nonce, at.number
            )));
        };
        // A recognised bezent transaction: a direct call of a contract delivery message, for the
        // reference of the intent its signature answers. Anything else bezent signed is an
        // anomaly, and proves nothing.
        if theirs.is_none() || recorded.as_deref() != theirs.as_ref().map(|t| t.as_slice()) {
            signal::anomaly_once(
                self.pool,
                &format!("{}:signed:{}:unrecognised-consumer", self.network, w.id),
                json!({
                    "network": self.network,
                    "signed_transaction": w.id,
                    "reason": "the lane nonce was consumed by a transaction bezent signed that is not a direct contract message for its intent's reference; nothing is proven (INV-8)",
                    "coordinate": coordinate(Some(index)),
                    "consumer": hex0x(&hash),
                }),
            )
            .await?;
            return Ok(unknown(format!(
                "the nonce {} was consumed at block {} by a transaction that is not recognised",
                s.nonce, at.number
            )));
        }
        Ok(Disposition::NotExecuted(Proof {
            network: self.network.to_owned(),
            signer: account,
            nonce: s.nonce,
            valid_from: s.valid_from,
            valid_until: s.valid_until,
            final_block: *finalized,
            nonce_at_final,
            kind: ProofKind::NonceConsumedByOther {
                at: *at,
                index: index32,
                consumer: hash,
            },
        }))
    }

    /// Looks at every watched transaction once, and records what is proven. Returns the
    /// dispositions found, unknowns included. A store error stops the step: nothing half-done
    /// is left, and the next step looks again.
    pub async fn step(&self) -> Result<Vec<(i64, Disposition)>, sqlx::Error> {
        let Ok(finalized) = self.chain.finalized().await else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for w in self.watched().await? {
            let d = self.disposition(&w, &finalized).await?;
            self.record(&w, &d, &finalized).await?;
            out.push((w.id, d));
        }
        Ok(out)
    }

    /// Records a disposition, in one transaction: an inclusion as an observation at its coordinate
    /// (`REQ-OB-6`), a proof in `non_execution_proofs`, both for a refusal. An unknown records
    /// nothing.
    pub async fn record(
        &self,
        w: &Watched,
        d: &Disposition,
        finalized: &BlockRef,
    ) -> Result<(), sqlx::Error> {
        let included = match d {
            Disposition::Executed { at, index, hash } => Some((*at, *index, *hash, true, None)),
            Disposition::NotExecuted(Proof {
                kind:
                    ProofKind::Refused {
                        at,
                        index,
                        hash,
                        error,
                    },
                ..
            }) => Some((*at, *index, *hash, false, error.clone())),
            _ => None,
        };
        let proof = match d {
            Disposition::NotExecuted(p) => Some(p),
            _ => None,
        };
        if included.is_none() && proof.is_none() {
            return Ok(());
        }
        let signed_hash = blake2_256(&w.bytes);
        let mut tx = self.pool.begin().await?;
        if let Some((at, index, hash, executed, error)) = included {
            let time_ms = match self.chain.events(&at, &[0; 32]).await {
                Ok(e) => e.time_ms,
                Err(_) => 0,
            };
            let id: Option<i64> = sqlx::query_scalar(
                "INSERT INTO observations (network, block_number, block_hash, block_time, \
                   position_kind, position_index, sender, on_chain_reference, executed, detail, \
                   kind) \
                 VALUES ($1, $2, $3, to_timestamp(0) + $4 * interval '1 millisecond', \
                   'transaction', $5, $6, $7, $8, $9::text::jsonb, 'transaction') \
                 ON CONFLICT ON CONSTRAINT observations_coordinate_once DO NOTHING RETURNING id",
            )
            .bind(self.network)
            .bind(i64_of(at.number)?)
            .bind(at.hash.as_slice())
            .bind(i64_of(time_ms)?)
            .bind(i64::from(index))
            .bind(hex0x(&self.lane.account))
            .bind(w.reference.as_deref())
            .bind(executed)
            .bind(
                json!({
                    "signed_transaction": w.id.to_string(),
                    "tx_hash": hex0x(&hash),
                    "signed_tx_hash": hex0x(&signed_hash),
                    "resigned": hash != signed_hash,
                    "nonce": w.row.0,
                    "error": error,
                })
                .to_string(),
            )
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(id) = id {
                // Found by walking back from the finalized head: final as observed (REQ-OB-2).
                sqlx::query(
                    "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
                       final_block_hash) VALUES ($1, 'final', $2, $3)",
                )
                .bind(id)
                .bind(i64_of(finalized.number)?)
                .bind(finalized.hash.as_slice())
                .execute(&mut *tx)
                .await?;
            }
        }
        if let Some(p) = proof {
            let (at, index, detail) = match &p.kind {
                ProofKind::NonceUnconsumed => (None, None, json!({"tx_hash": hex0x(&signed_hash)})),
                ProofKind::NonceConsumedByOther {
                    at,
                    index,
                    consumer,
                } => (
                    Some(*at),
                    Some(i64::from(*index)),
                    json!({"tx_hash": hex0x(&signed_hash), "consumer_tx_hash": hex0x(consumer)}),
                ),
                ProofKind::Refused {
                    at, index, hash, ..
                } => (
                    Some(*at),
                    Some(i64::from(*index)),
                    json!({"tx_hash": hex0x(&signed_hash), "included_tx_hash": hex0x(hash)}),
                ),
            };
            sqlx::query(
                "INSERT INTO non_execution_proofs (signed_transaction_id, network, signer, nonce, \
                   valid_from_block, valid_until_block, final_block_number, final_block_hash, \
                   nonce_at_final, kind, at_block_number, at_block_hash, at_position_index, detail) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14::text::jsonb) \
                 ON CONFLICT (signed_transaction_id) DO NOTHING",
            )
            .bind(w.id)
            .bind(&p.network)
            .bind(p.signer.as_slice())
            .bind(i64_of(p.nonce)?)
            .bind(i64_of(p.valid_from)?)
            .bind(i64_of(p.valid_until)?)
            .bind(i64_of(p.final_block.number)?)
            .bind(p.final_block.hash.as_slice())
            .bind(i64_of(p.nonce_at_final)?)
            .bind(p.kind.name())
            .bind(at.map(|b| b.number).map(i64_of).transpose()?)
            .bind(at.map(|b| b.hash.to_vec()))
            .bind(index)
            .bind(detail.to_string())
            .execute(&mut *tx)
            .await?;
            tracing::info!(
                network = self.network,
                signed = w.id,
                kind = p.kind.name(),
                "REQ-NX-1: proven not executed"
            );
        }
        tx.commit().await?;
        Ok(())
    }
}
