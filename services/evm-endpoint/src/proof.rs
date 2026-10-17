// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The outcome of every signed transaction, from chain data only (`T-009-06`; `REQ-NX-1` to
//! `REQ-NX-3`, `REQ-OB-6`, `AC-C1.1`, `AC-C1.2`, `AC-C2.1`, `INV-8`; PLAN `DEC-8`, `DEC-12`).
//!
//! On an origin network a transaction does not expire, so it is **not executed** only when its
//! signer's nonce is consumed at a final block by a different transaction, or when it was included
//! and refused, final (`REQ-NX-1`). Every read is made through both providers, at blocks both give
//! with the same hash, below the final point both stand on (`DEC-12`, `REQ-OB-5`). For each signed
//! transaction of the lane with no recorded outcome:
//!
//! 1. The lane's nonce at the final point. At or below `N` (the transaction's nonce): not consumed,
//!    nothing is known yet; the transaction is re-sent by the submitter.
//! 2. Above `N`: the consuming block `X` is found by bisecting the nonce between the dry-run block
//!    (at which the nonce was not consumed) and the final point, and the lane's transaction at
//!    nonce `N` is read from `X` and identified (PR #31's lesson: no proof without the consumer
//!    identified):
//!    * **the same signed transaction** (same hash): its receipt says executed or refused. Executed
//!      is recorded as a `transaction` observation; refused, as one with `executed = false` and a
//!      `refused` proof.
//!    * **a re-signed copy** (the same signing payload, another signature): the signature is matched
//!      against the recorded intents (a copy bezent did not sign starts the compromise path). It
//!      executed: that is this payout executing, recorded. It was refused: unknown, never proof.
//!    * **a wrapped copy** (another payload that carries this reference anywhere in its calldata):
//!      unknown. One bezent did not sign starts the compromise path (`AC-I1.2`: alert and pause,
//!      PR #39 review S2); one it did sign is a `delivery-anomaly`. Wrappers are not read into.
//!    * **a recognised bezent transaction**: its signature answers a recorded intent whose message is
//!      exactly its signing payload, that intent's reference is not this one, and the call is a
//!      delivery of another reference or a cancel of this lane. Then this transaction is **not
//!      executed**: a `nonce-consumed-by-other` proof.
//!    * anything else (a transaction bezent did not sign, one that is not EIP-1559, none at `N` in
//!      `X`): unknown, and the compromise path.
//! 3. **The chain agrees** before any proof: the vault is read up to the final point and no
//!    payout of this reference is observed (at another nonce, or by another sender); otherwise
//!    unknown and a `delivery-anomaly`. Only the vault's own movement counts (PR #39 review S1,
//!    `INV-8`, `INV-14`): a `Payout`, or a reference in a transaction where the vault's own
//!    transfer moved value. A bare emit (anyone can call the emitter) never blocks a proof.
//!
//! A proof names network, signer, nonce, the transaction's validity (from its dry-run block, no
//! end), the final block that proves it and the consuming block and position: checkable by anyone
//! from public chain data (`REQ-NX-2`, `AC-C2.1`). An observation and its proof are one store
//! transaction. Silence, an error, a disagreement or an undecodable transaction is unknown, never
//! proof (`REQ-NX-3`).

use alloy::consensus::{SignableTransaction, TxEip1559};
use alloy::primitives::{Address, B256, Signature, TxKind};
use bezent_signer::compromise::ObservedSignature;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::evm::{self, Contracts};
use crate::pair::Pair;
use crate::rpc::{Header, Rpc, Tx};
use crate::signal::{self, Seen};
use crate::store::{self, Head, hex0x, i64_of};
use crate::submit::Lane;

/// What the prover found for one signed transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Included and executed, final: recorded as a `transaction` observation.
    Executed {
        /// The block.
        block: u64,
    },
    /// Not executed, proven: the proof's kind (`refused` or `nonce-consumed-by-other`).
    NotExecuted {
        /// The kind.
        kind: &'static str,
        /// The consuming block.
        block: u64,
    },
    /// Nothing is known yet (not consumed at the final point, or not read that far).
    Pending(String),
    /// Unknown, never proof (`REQ-NX-3`): why.
    Unknown(String),
}

/// A live signed transaction: id, bytes, hash, intent, purpose, reference, nonce, validity, lane.
type Live = (
    i64,
    Vec<u8>,
    Option<Vec<u8>>,
    i64,
    String,
    Option<Vec<u8>>,
    i64,
    i64,
    i64,
    Vec<u8>,
);

/// Proves the outcome of the lane's signed transactions. See the module documentation.
pub struct Prover<'a, R> {
    /// The two providers.
    pub pair: &'a Pair<R>,
    /// The store, as the EVM endpoint's role.
    pub pool: &'a PgPool,
    /// The network id.
    pub network: &'a str,
    /// The lane.
    pub lane: &'a Lane,
    /// The vault's contracts (to recognise a delivery).
    pub contracts: Contracts,
}

/// The signing payload of an EIP-1559 transaction as a block lists it, if it is one with no access
/// list (bezent builds no other).
pub fn payload_of(t: &Tx) -> Option<TxEip1559> {
    (t.tx_type == evm::EIP1559 && t.access_list_empty).then_some(())?;
    Some(TxEip1559 {
        chain_id: t.chain_id?,
        nonce: t.nonce,
        gas_limit: t.gas,
        max_fee_per_gas: t.max_fee_per_gas?,
        max_priority_fee_per_gas: t.max_priority_fee_per_gas?,
        to: t.to.map_or(TxKind::Create, TxKind::Call),
        value: t.value,
        access_list: Default::default(),
        input: t.input.clone(),
    })
}

/// The address `t`'s signature recovers to over `tx`'s signing hash.
fn signer_of(t: &Tx, tx: &TxEip1559) -> Option<Address> {
    let (&v, rs) = t.signature.split_last()?;
    if rs.len() != 64 || v > 1 {
        return None;
    }
    Signature::from_bytes_and_parity(rs, v == 1)
        .recover_address_from_prehash(&tx.signature_hash())
        .ok()
}

impl<R: Rpc> Prover<'_, R> {
    /// Looks at every live signed transaction of the lane once.
    pub async fn step(&self) -> Result<Vec<(i64, Outcome)>, sqlx::Error> {
        let live: Vec<Live> = sqlx::query_as(
            "SELECT st.id, st.bytes, st.tx_hash, si.id, si.purpose, si.on_chain_reference, \
               p.nonce, p.valid_from_block, p.valid_until_block, p.signing_payload \
             FROM signed_transactions st \
             JOIN intents si ON si.id = st.intent_id \
             JOIN prepared_transactions p ON p.id::text = si.payload ->> 'prepared' \
             WHERE si.network = $1 AND p.network = $1 AND p.lane = $2 \
               AND NOT EXISTS (SELECT 1 FROM non_execution_proofs n WHERE n.signed_transaction_id = st.id) \
               AND NOT EXISTS (SELECT 1 FROM observations ob WHERE ob.network = $1 \
                 AND ob.position_kind = 'transaction' AND ob.detail ->> 'signed_transaction' = st.id::text) \
             ORDER BY p.nonce, st.id",
        )
        .bind(self.network)
        .bind(self.lane.address.to_vec())
        .fetch_all(self.pool)
        .await?;
        if live.is_empty() {
            return Ok(Vec::new());
        }
        let head = match self.pair.final_point().await {
            Ok(h) => h,
            Err(e) => {
                let why = e.to_string();
                return Ok(live
                    .iter()
                    .map(|l| (l.0, Outcome::Pending(why.clone())))
                    .collect());
            }
        };
        {
            let mut conn = self.pool.acquire().await?;
            if let Head::Conflict(_) = store::final_head(&mut conn, self.network, &head).await? {
                return Ok(live
                    .iter()
                    .map(|l| {
                        (
                            l.0,
                            Outcome::Unknown(
                                "REQ-OB-2: another hash is final at this height".into(),
                            ),
                        )
                    })
                    .collect());
            }
        }
        let mut out = Vec::new();
        for l in live {
            let id = l.0;
            let outcome = self.prove(&head, l).await?;
            match &outcome {
                Outcome::Pending(_) => {}
                o => tracing::info!(network = self.network, signed = id, outcome = ?o, "REQ-NX-1"),
            }
            out.push((id, outcome));
        }
        Ok(out)
    }

    async fn prove(&self, head: &Header, l: Live) -> Result<Outcome, sqlx::Error> {
        let (
            signed_id,
            _bytes,
            tx_hash,
            intent_id,
            purpose,
            reference,
            nonce,
            valid_from,
            valid_until,
            payload,
        ) = l;
        let n = u64::try_from(nonce).unwrap_or(u64::MAX);
        let lane = self.lane.address;
        let at_final = match self.pair.nonce_at(lane, head.number).await {
            Ok(v) => v,
            Err(e) => return Ok(Outcome::Pending(e.to_string())),
        };
        if at_final <= n {
            return Ok(Outcome::Pending(format!(
                "nonce {n} is not consumed at final block {}",
                head.number
            )));
        }
        // The consuming block: the first at which the nonce passed N, after the dry run.
        let mut lo = u64::try_from(valid_from).unwrap_or(0);
        match self.pair.nonce_at(lane, lo).await {
            Ok(v) if v <= n => {}
            Ok(v) => {
                return Ok(Outcome::Unknown(format!(
                    "the lane's nonce was {v} at the dry-run block {lo}, past {n}"
                )));
            }
            Err(e) => return Ok(Outcome::Pending(e.to_string())),
        }
        let mut hi = head.number;
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            match self.pair.nonce_at(lane, mid).await {
                Ok(v) if v > n => hi = mid,
                Ok(_) => lo = mid,
                Err(e) => return Ok(Outcome::Pending(e.to_string())),
            }
        }
        let x = hi;
        let (header, txs) = match self.pair.block_transactions(x).await {
            Ok(Some(b)) => b,
            Ok(None) => return Ok(Outcome::Pending(format!("block {x} is missing"))),
            Err(e) => return Ok(Outcome::Pending(e.to_string())),
        };
        let consumers: Vec<&Tx> = txs
            .iter()
            .filter(|t| t.from == lane && t.nonce == n)
            .collect();
        let dedupe = format!("{}:signed:{signed_id}", self.network);
        let [consumer] = consumers.as_slice() else {
            signal::raise_once(
                self.pool,
                &format!("{dedupe}:consumer"),
                json!({"network": self.network, "lane": lane, "nonce": n, "block": x,
                       "reason": format!("{} transactions of the lane at its nonce in the consuming block", consumers.len())}),
            )
            .await?;
            return Ok(Outcome::Unknown(format!(
                "{} transactions of the lane at nonce {n} in block {x}",
                consumers.len()
            )));
        };
        let consumer = *consumer;
        let observed = ObservedSignature {
            network: self.network.to_owned(),
            public_key: self.lane.public_key.clone(),
            signature: consumer.signature.clone(),
            block_number: i64_of(x)?,
            block_hash: header.hash.to_vec(),
            position_index: i64_of(consumer.index)?,
        };
        let Some(their) = payload_of(consumer).filter(|t| signer_of(consumer, t) == Some(lane))
        else {
            signal::raise_once(
                self.pool,
                &format!("{dedupe}:undecoded"),
                json!({"network": self.network, "lane": lane, "nonce": n, "block": x,
                       "tx_hash": consumer.hash,
                       "reason": "the lane's nonce was consumed by a transaction bezent does not build"}),
            )
            .await?;
            return Ok(Outcome::Unknown(format!(
                "nonce {n} was consumed in block {x} by {}, not an EIP-1559 transaction of the lane",
                consumer.hash
            )));
        };
        let reference = reference
            .as_deref()
            .and_then(|r| <[u8; 32]>::try_from(r).ok());

        if evm::signing_payload(&their) == payload {
            let ours = tx_hash.as_deref() == Some(consumer.hash.as_slice());
            if !ours && signal::check_signature(self.pool, &observed).await? == Seen::Compromise {
                tracing::error!(
                    network = self.network,
                    signed = signed_id,
                    "a re-signed copy bezent did not sign"
                );
            }
            let receipt = match self.pair.receipt(consumer.hash).await {
                Ok(Some(r)) if r.block_number == x && r.block_hash == header.hash => r,
                Ok(_) => {
                    return Ok(Outcome::Pending(format!(
                        "no receipt of {} in block {x}",
                        consumer.hash
                    )));
                }
                Err(e) => return Ok(Outcome::Pending(e.to_string())),
            };
            if receipt.status {
                self.record(head, &header, consumer, signed_id, true, !ours, None)
                    .await?;
                return Ok(Outcome::Executed { block: x });
            }
            if !ours {
                return Ok(Outcome::Unknown(format!(
                    "a re-signed copy {} was refused in block {x}; its refusal proves nothing",
                    consumer.hash
                )));
            }
            if let Some(why) = self.disagrees(head, reference.as_ref(), &dedupe).await? {
                return Ok(why);
            }
            let proof = Proof {
                signed_id,
                nonce,
                valid_from,
                valid_until,
                at_final,
                kind: "refused",
                detail: json!({"tx_hash": consumer.hash, "receipt_status": 0}),
            };
            self.record(
                head,
                &header,
                consumer,
                signed_id,
                false,
                false,
                Some(proof),
            )
            .await?;
            return Ok(Outcome::NotExecuted {
                kind: "refused",
                block: x,
            });
        }

        // Another payload. Its signature is matched first (PR #39 review S2): bezent never signs
        // a second payload for the same reference and attempt, so a lane transaction carrying
        // this reference that answers no recorded intent is the key used outside bezent, and
        // starts the compromise path (AC-I1.2: alert and pause), not only an anomaly.
        let seen = signal::check_signature(self.pool, &observed).await?;
        // A wrapper that carries this reference is never read into: unknown, whoever signed it.
        if let Some(r) = &reference
            && evm::carries(&their.input, &B256::from(*r))
        {
            if seen != Seen::Compromise {
                signal::anomaly_once(
                    self.pool,
                    &format!("{dedupe}:wrapped"),
                    json!({"spec": "INV-8, REQ-NX-3", "network": self.network, "signed": signed_id,
                           "consumer": consumer.hash, "block": x,
                           "reason": "the nonce was consumed by another transaction that carries this reference"}),
                )
                .await?;
            }
            return Ok(Outcome::Unknown(format!(
                "nonce {n} was consumed by {}, which carries this reference",
                consumer.hash
            )));
        }
        // A recognised bezent transaction: a recorded signature over exactly its payload.
        let Seen::Recorded {
            intent_id: theirs_intent,
        } = seen
        else {
            return Ok(Outcome::Unknown(format!(
                "nonce {n} was consumed by {}, which no recorded intent signed",
                consumer.hash
            )));
        };
        let row: Option<(Option<Vec<u8>>, Value, String)> = sqlx::query_as(
            "SELECT on_chain_reference, payload, network FROM intents WHERE id = $1",
        )
        .bind(theirs_intent)
        .fetch_optional(self.pool)
        .await?;
        let recognised = row.is_some_and(|(their_ref, p, network)| {
            let message = p
                .get("message")
                .and_then(Value::as_str)
                .and_then(|m| m.strip_prefix("0x"))
                .and_then(|m| hex::decode(m).ok());
            let shape = match their.to.to() {
                Some(&to) if to == self.contracts.roles => {
                    evm::decode_delivery_call(&self.contracts, &their.input)
                        .is_some_and(|d| Some(d.reference.0) != reference)
                }
                Some(&to) if to == lane => their.input.is_empty() && their.value.is_zero(),
                _ => false,
            };
            network == self.network
                && theirs_intent != intent_id
                && message.as_deref() == Some(evm::signing_payload(&their).as_slice())
                && their_ref.as_deref() != reference.as_ref().map(|r| r.as_slice())
                && shape
        });
        if !recognised {
            signal::anomaly_once(
                self.pool,
                &format!("{dedupe}:unrecognised"),
                json!({"spec": "INV-8, REQ-NX-3", "network": self.network, "signed": signed_id,
                       "consumer": consumer.hash, "intent": theirs_intent, "block": x,
                       "reason": "a recorded signature over a transaction that is not a delivery of another reference or a cancel"}),
            )
            .await?;
            return Ok(Outcome::Unknown(format!(
                "nonce {n} was consumed by {}, signed for intent {theirs_intent}, which is not a \
                 recognised delivery or cancel",
                consumer.hash
            )));
        }
        if let Some(why) = self.disagrees(head, reference.as_ref(), &dedupe).await? {
            return Ok(why);
        }
        let proof = Proof {
            signed_id,
            nonce,
            valid_from,
            valid_until,
            at_final,
            kind: "nonce-consumed-by-other",
            detail: json!({"consumer": consumer.hash, "consumer_intent": theirs_intent,
                           "purpose": purpose}),
        };
        let mut tx = self.pool.begin().await?;
        self.insert_proof(&mut tx, head, &header, consumer, &proof)
            .await?;
        tx.commit().await?;
        Ok(Outcome::NotExecuted {
            kind: "nonce-consumed-by-other",
            block: x,
        })
    }

    /// The chain agrees (step 3): the vault is read up to the final point, and no observation of
    /// this network shows the vault paying `reference`: a `Payout`, or a reference in a
    /// transaction where the vault's own transfer moved value (S1). Otherwise the outcome stays
    /// unknown. A reference anyone else emitted, with no movement of the vault beside it, is not
    /// a payout and never blocks a proof.
    async fn disagrees(
        &self,
        head: &Header,
        reference: Option<&[u8; 32]>,
        dedupe: &str,
    ) -> Result<Option<Outcome>, sqlx::Error> {
        let watermark = store::watermark(self.pool, self.network).await?;
        if watermark.is_none_or(|w| w < head.number) {
            return Ok(Some(Outcome::Pending(format!(
                "the vault is read up to {watermark:?}, below the final point {}",
                head.number
            ))));
        }
        let Some(r) = reference else {
            return Ok(None);
        };
        let used: Option<(i64, String)> = sqlx::query_as(
            "SELECT o.id, coalesce(o.detail ->> 'event', '') FROM observations o \
             LEFT JOIN observation_outcomes x ON x.observation_id = o.id \
             WHERE o.network = $1 AND o.on_chain_reference = $2 \
               AND x.outcome IS DISTINCT FROM 'withdrawn' \
               AND (o.detail ->> 'event' = 'Payout' \
                 OR EXISTS (SELECT 1 FROM observations v WHERE v.network = o.network \
                   AND v.block_hash = o.block_hash \
                   AND v.detail ->> 'tx_hash' = o.detail ->> 'tx_hash' \
                   AND v.detail ->> 'event' IN ('Payout', 'Outflow', 'ConversionInput'))) \
             LIMIT 1",
        )
        .bind(self.network)
        .bind(r.as_slice())
        .fetch_optional(self.pool)
        .await?;
        let Some((observation, event)) = used else {
            return Ok(None);
        };
        signal::anomaly_once(
            self.pool,
            &format!("{dedupe}:reference-used"),
            json!({"spec": "INV-8", "network": self.network, "observation": observation,
                   "event": event, "reference": hex0x(r),
                   "reason": "the reference is on chain, though this transaction did not execute it"}),
        )
        .await?;
        Ok(Some(Outcome::Unknown(format!(
            "the reference is on chain ({event}, observation {observation}); nothing is proven"
        ))))
    }

    /// Records the `transaction` observation of `t` (`REQ-OB-6`), with its outcome, and the proof
    /// if there is one, in one store transaction.
    #[allow(clippy::too_many_arguments)]
    async fn record(
        &self,
        head: &Header,
        block: &Header,
        t: &Tx,
        signed_id: i64,
        executed: bool,
        copy: bool,
        proof: Option<Proof>,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        // Kind `transaction` (migration 0034): bezent's own signed transaction, at its position. Its
        // detail names no `event`, which would have to agree with the kind.
        let detail = json!({
            "tx_hash": t.hash,
            "nonce": t.nonce,
            "signed_transaction": signed_id.to_string(),
            "copy": copy,
        });
        let id: Option<i64> = sqlx::query_scalar(
            "INSERT INTO observations (network, block_number, block_hash, block_time, \
               position_kind, position_index, sender, recipient, executed, detail, kind) \
             VALUES ($1, $2, $3, to_timestamp(0) + $4::bigint * interval '1 second', \
               'transaction', $5, $6, $7, $8, $9::text::jsonb, 'transaction') \
             ON CONFLICT ON CONSTRAINT observations_coordinate_once DO NOTHING RETURNING id",
        )
        .bind(self.network)
        .bind(i64_of(block.number)?)
        .bind(block.hash.as_slice())
        .bind(i64_of(block.timestamp)?)
        .bind(i64_of(t.index)?)
        .bind(t.from.to_checksum(None))
        .bind(t.to.map(|a| a.to_checksum(None)))
        .bind(executed)
        .bind(detail.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(id) = id {
            sqlx::query(
                "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
                   final_block_hash) VALUES ($1, 'final', $2, $3)",
            )
            .bind(id)
            .bind(i64_of(head.number)?)
            .bind(head.hash.as_slice())
            .execute(&mut *tx)
            .await?;
        }
        if let Some(p) = proof {
            self.insert_proof(&mut tx, head, block, t, &p).await?;
        }
        tx.commit().await
    }

    async fn insert_proof(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        head: &Header,
        block: &Header,
        consumer: &Tx,
        p: &Proof,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO non_execution_proofs (signed_transaction_id, network, signer, nonce, \
               valid_from_block, valid_until_block, final_block_number, final_block_hash, \
               nonce_at_final, kind, at_block_number, at_block_hash, at_position_index, detail) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14::text::jsonb) \
             ON CONFLICT (signed_transaction_id) DO NOTHING",
        )
        .bind(p.signed_id)
        .bind(self.network)
        .bind(self.lane.address.to_vec())
        .bind(p.nonce)
        .bind(p.valid_from)
        .bind(p.valid_until)
        .bind(i64_of(head.number)?)
        .bind(head.hash.as_slice())
        .bind(i64_of(p.at_final)?)
        .bind(p.kind)
        .bind(i64_of(block.number)?)
        .bind(block.hash.as_slice())
        .bind(i64_of(consumer.index)?)
        .bind(p.detail.to_string())
        .execute(&mut **tx)
        .await?;
        Ok(())
    }
}

/// A proof to write.
struct Proof {
    signed_id: i64,
    nonce: i64,
    valid_from: i64,
    valid_until: i64,
    at_final: u64,
    kind: &'static str,
    detail: Value,
}
