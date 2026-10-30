// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Signing a recorded intent, behind the guards (`T-005-04`, `T-005-05`; `INV-9`, `REQ-AR-3`,
//! `REQ-EO-1`, `AC-D2.1`, `AC-I4.2`; PLAN `DEC-3`).
//!
//! The caller names an intent id and nothing else. The signer reads the intent through the
//! `bezent_signer` role, and:
//!
//! 1. refuses an intent the core did not record (`INV-9`), and one whose bytes and amount are not
//!    exactly those of the prepared transaction it names, read here, or whose preparation does
//!    not answer the intent's own order on its network (PR #39 third review MF-A, issue #38);
//! 2. returns the recorded signature if the intent was already signed, and signs nothing new: a
//!    restart, or a repeat, never makes a second signature for one intent (`AC-D2.1`, `REQ-EO-1`);
//! 3. refuses an intent for a network it does not know, or of another environment than its keys
//!    (`REQ-KY-6`, `AC-I4.2`), or with no key bound to its network and purpose (`REQ-KY-4`);
//! 4. refuses an intent whose network is not of the kind of the key bound to it (an EVM key for an
//!    EVM network, a Kreivo key otherwise: PR #39 second review MF1, `INV-9`), an EVM intent whose
//!    bytes are not an EIP-1559 transaction on its network's chain id (third review SF-B), and a
//!    destroy on the issuing network from any account but the on-chain component's (`DEC-3`,
//!    `INV-4`);
//! 5. refuses a one-time reference another intent's signature already used for the same attempt of
//!    the same movement (network, purpose, redeemer scope), and an attempt n+1 before attempt n is
//!    proven not executed (`DEC-3`, `INV-2`, `REQ-EO-6`; amendments 0008 and 0009);
//! 6. decodes the exact bytes itself ([`crate::delivery`]: an EVM payout through the vault's
//!    delivery role, an EVM cancel, a Kreivo mint or refund mint of the on-chain component) and
//!    compares what they do with the core's order, read through its own grant, never with the
//!    endpoint's record: the recipient, the asset or token, the amount, the reference, the network
//!    (chain id, or genesis and contract, and an era no longer than the endpoint's) and a cancel's
//!    nonce. Any mismatch refuses and raises the key-compromise alert and a global pause request
//!    (the audit of PR #56, must-fix 2; `REQ-AR-3`, `INV-9`, `AC-I1.2`, issue #23). A purpose it
//!    decodes no transaction of is refused; on an EVM network a nonce the key signed at already is
//!    signed again only once that transaction is proven not executed (`REQ-EO-4`);
//! 7. refuses an amount that takes the key past its cap for the window: what the decoded bytes
//!    move, summed per key, network and asset (`DEC-3`, `REQ-KY-3`, issue #23);
//! 8. signs the intent's exact bytes and records the signature, with the nonce and asset it
//!    decoded, append-only, before answering.
//!
//! An intent of purpose `statement` takes its own path (PLAN `DEC-16`, 2026-10-14; issue #42): it
//! names no transfer, reference or preparation; its bytes must be a bezent statement of the asset
//! and sequence the intent names, hashing to the SHA-256 it names and naming the statement key; it
//! is on a declared issuing network of the keys' environment; and the statement key signs it, no
//! other key. The key never signs a second, different statement for the same asset and sequence
//! (`REQ-PR-2`); a rotated statement key may sign the one place its predecessor signed and the
//! store never took (PR #50 review M3).
//!
//! These guards are a second line: the on-chain component and the vault stay the layer that
//! enforces single use and the caps (`INV-2`, `INV-3`, `REQ-AR-5`). Signing is serialised, in the
//! process and across processes (an advisory lock), so two requests cannot both pass a cap. The
//! signer keeps nothing in memory between requests: its only state is the store's.

use std::fmt;

use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgConnection, PgPool};

use crate::cancel;
use crate::config::{CANCEL, DESTROY, Policy, STATEMENT, parse_minor_units};
use crate::intent::Intent;
use crate::keys::{Environment, KeyError, SigningEngine};
use crate::log::Log;

/// The advisory lock every signing transaction takes (`hashtext('bezent-signer')` is not stable
/// across server versions; a constant is).
const SIGNING_LOCK: i64 = 0x6265_7a65_6e74_5347; // "bezentSG"

/// A signature the signer made or recorded earlier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    /// The intent signed.
    pub intent_id: i64,
    /// The key that signed it.
    pub key_id: String,
    /// The scheme (`signatures.scheme`).
    pub scheme: String,
    /// The key's public key.
    pub public_key: Vec<u8>,
    /// The signature.
    pub signature: Vec<u8>,
    /// Whether it was recorded before this request (`AC-D2.1`).
    pub replayed: bool,
}

/// Why the signer refused an intent. Each names the spec id that refuses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No intent with this id was recorded (`INV-9`).
    Unrecorded,
    /// The recorded intent lacks a field the signer needs, or has it in the wrong form.
    Malformed(&'static str),
    /// The intent's network is not declared to the signer.
    UnknownNetwork(String),
    /// The intent's network is of another environment than the signer's keys (`REQ-KY-6`).
    Environment {
        /// The network.
        network: String,
        /// Its environment.
        network_environment: Environment,
        /// The keys' environment.
        key_environment: Environment,
    },
    /// No key is bound to the intent's network and purpose (`REQ-KY-4`).
    NoKey {
        /// The network.
        network: String,
        /// The purpose.
        purpose: String,
    },
    /// A destroy on the issuing network from an account other than the on-chain component's.
    DestroyNotOwn,
    /// Another intent with this one-time reference was already signed, for the same attempt.
    ReferenceReused,
    /// A new attempt whose previous attempt is not proven not executed (`REQ-EO-6`).
    AttemptNotProven {
        /// The attempt.
        attempt: i32,
    },
    /// The amount takes the key past its cap for the window.
    WindowCap {
        /// The cap.
        cap: u128,
        /// What the key signed in the window so far.
        signed: u128,
        /// This intent's amount.
        amount: u128,
    },
    /// An intent labelled `cancel` whose bytes are not a transfer of nothing from the key to
    /// itself, on the network's chain, within the gas and fee caps (PR #39 review S6, SF2).
    NotACancel,
    /// The key bound to the intent's network and purpose is not of that network's kind: a Kreivo
    /// key for an EVM network's intent, or the other way round (PR #39 second review MF1).
    KeyNetwork {
        /// The network.
        network: String,
        /// The key.
        key: String,
    },
    /// The intent's bytes, amount, order or network are not those of the prepared transaction it
    /// names, or it names none (PR #39 third review MF-A, issue #38).
    NotPrepared {
        /// The prepared transaction named, if any.
        prepared: Option<i64>,
    },
    /// An EVM intent whose bytes are not an EIP-1559 transaction on the chain id the policy
    /// declares for its network (PR #39 third review SF-B).
    ChainId {
        /// The network.
        network: String,
        /// The chain id the bytes carry, if they carry one.
        carried: Option<u64>,
    },
    /// A statement intent that is not exactly a statement for the statement key (issue #42).
    NotAStatement(&'static str),
    /// The statement key already signed another statement for this asset and sequence
    /// (`REQ-PR-2`: one statement per place in the chain).
    StatementEquivocation {
        /// The asset.
        asset: String,
        /// The sequence.
        sequence: i64,
    },
    /// A statement at a place the chain does not allow: published or abandoned, or one that
    /// skips a place that is not abandoned, or does not name exactly the abandoned places it
    /// skips (`REQ-PR-2`; PR #50 re-check S-1, S-3).
    StatementPlace {
        /// The asset.
        asset: String,
        /// The sequence.
        sequence: i64,
        /// Why, as the store's rule says.
        why: String,
    },
    /// The bytes, decoded by the signer, do not answer the core's order: another recipient,
    /// asset or token, amount, reference, network or nonce, or no transaction of the order's kind
    /// at all (the audit of PR #56, must-fix 2). The key-compromise alert is raised.
    NotTheOrder {
        /// What does not answer.
        why: String,
        /// The event log position of the alert (0 when the store already held it).
        alert_seq: i64,
    },
    /// An intent of a purpose whose bytes the signer does not decode: it signs payouts, cancels,
    /// mints and refund mints, and statements, only.
    NotADelivery {
        /// The purpose.
        purpose: String,
    },
    /// The key already signed at this nonce, and that transaction is not proven not executed
    /// (`REQ-EO-4`): the lane waits.
    NonceInUse {
        /// The nonce.
        nonce: u64,
    },
}

impl Refusal {
    /// The spec id that refuses.
    pub fn spec(&self) -> &'static str {
        match self {
            Self::Unrecorded => "INV-9",
            Self::Malformed(_) => "INV-9",
            Self::UnknownNetwork(_) => "REQ-KY-4",
            Self::Environment { .. } => "REQ-KY-6",
            Self::NoKey { .. } => "REQ-KY-4",
            Self::DestroyNotOwn => "INV-4",
            Self::ReferenceReused => "INV-2",
            Self::AttemptNotProven { .. } => "REQ-EO-6",
            Self::WindowCap { .. } => "REQ-KY-3",
            Self::NotACancel => "REQ-NX-4",
            Self::KeyNetwork { .. } => "INV-9",
            Self::NotPrepared { .. } => "INV-9",
            Self::ChainId { .. } => "REQ-KY-4",
            Self::NotAStatement(_) => "REQ-PR-1",
            Self::StatementEquivocation { .. } | Self::StatementPlace { .. } => "REQ-PR-2",
            Self::NotTheOrder { .. } => "REQ-AR-3",
            Self::NotADelivery { .. } => "INV-9",
            Self::NonceInUse { .. } => "REQ-EO-4",
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let spec = self.spec();
        match self {
            Self::Unrecorded => write!(f, "{spec}: no recorded intent"),
            Self::Malformed(field) => {
                write!(f, "{spec}: the recorded intent's {field} is malformed")
            }
            Self::UnknownNetwork(n) => write!(f, "{spec}: network {n} is not declared"),
            Self::Environment {
                network,
                network_environment,
                key_environment,
            } => write!(
                f,
                "{spec} (AC-I4.2): network {network} is {network_environment}; the keys are {key_environment}"
            ),
            Self::NoKey { network, purpose } => {
                write!(f, "{spec}: no key signs {purpose} on {network}")
            }
            Self::DestroyNotOwn => write!(
                f,
                "{spec} (DEC-3): a destroy from an account other than the on-chain component's"
            ),
            Self::ReferenceReused => write!(
                f,
                "{spec} (DEC-3): the one-time reference was already signed for, for this attempt"
            ),
            Self::AttemptNotProven { attempt } => write!(
                f,
                "{spec}: attempt {attempt} is signed only once attempt {} is proven not executed",
                attempt - 1
            ),
            Self::WindowCap {
                cap,
                signed,
                amount,
            } => write!(
                f,
                "{spec} (DEC-3): {amount} after {signed} signed in the window exceeds the cap {cap}"
            ),
            Self::NotACancel => write!(
                f,
                "{spec} (INV-9): a cancel must be an EIP-1559 transfer of nothing from the key to itself, \
                 on the network's chain, with gas 21000 and fees within the network's cap"
            ),
            Self::KeyNetwork { network, key } => write!(
                f,
                "{spec} (MF1): key {key} is not of network {network}'s kind; it signs nothing for it"
            ),
            Self::NotPrepared { prepared: None } => write!(
                f,
                "{spec} (MF-A, issue #38): the intent names no prepared transaction"
            ),
            Self::NotPrepared {
                prepared: Some(prepared),
            } => write!(
                f,
                "{spec} (MF-A, issue #38): the intent's bytes, amount, order or network are not \
                 those of prepared transaction {prepared}"
            ),
            Self::ChainId { network, carried } => write!(
                f,
                "{spec} (SF-B): the bytes carry chain id {carried:?}, not network {network}'s"
            ),
            Self::NotAStatement(why) => write!(
                f,
                "{spec} (DEC-16): not a statement for the statement key: {why}"
            ),
            Self::StatementEquivocation { asset, sequence } => write!(
                f,
                "{spec}: the statement key already signed another statement {sequence} of {asset}"
            ),
            Self::StatementPlace {
                asset,
                sequence,
                why,
            } => write!(
                f,
                "{spec}: statement {sequence} of {asset} is not at a place the chain allows: {why}"
            ),
            Self::NotTheOrder { why, alert_seq } => write!(
                f,
                "{spec} (INV-9, AC-I1.2): the decoded bytes do not answer the core's order: {why}; \
                 key-compromise raised (event log {alert_seq})"
            ),
            Self::NotADelivery { purpose } => write!(
                f,
                "{spec}: the signer decodes no transaction of purpose {purpose}, so signs none"
            ),
            Self::NonceInUse { nonce } => write!(
                f,
                "{spec}: the key signed at nonce {nonce} already, and that transaction is not \
                 proven not executed"
            ),
        }
    }
}

/// What a request to sign came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Signed now, or earlier and returned.
    Signed(Signed),
    /// Refused by a guard.
    Refused(Refusal),
    /// The store or the key backend failed; nothing was signed and recorded. The caller retries.
    Unavailable(String),
}

/// What the decoded bytes of a delivery move, once they answer the core's order.
#[derive(Debug)]
struct Checked {
    /// What they move, in the destination's minor units: the window cap counts it.
    amount: u128,
    /// The asset the cap is kept for (issue #23: per key, network and asset).
    asset: Option<String>,
    /// The transaction's nonce in the key's lane.
    nonce: Option<u64>,
    /// For a cancel: the signing intent of the transaction it cancels.
    cancels: Option<i64>,
}

/// Whether the decoded bytes answer the core's order.
#[derive(Debug)]
enum Answer {
    /// They do.
    Answers(Checked),
    /// They do not: a compromise of whoever prepared them or recorded their intent.
    Mismatch(String),
    /// Refused for another reason, raising nothing.
    Hold(Refusal),
}

/// `0x` and 20 bytes of hex, any case.
fn address20(text: &str) -> Option<[u8; 20]> {
    let digits = text.strip_prefix("0x")?;
    hex::decode(digits).ok()?.try_into().ok()
}

/// `0x` and 32 bytes of hex, any case.
fn account32(text: &str) -> Option<[u8; 32]> {
    let digits = text.strip_prefix("0x")?;
    hex::decode(digits).ok()?.try_into().ok()
}

#[derive(Debug)]
enum Failure {
    Store(sqlx::Error),
    Key(KeyError),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(e) => write!(f, "store: {e}"),
            Self::Key(e) => write!(f, "{e}"),
        }
    }
}

impl From<sqlx::Error> for Failure {
    fn from(e: sqlx::Error) -> Self {
        Self::Store(e)
    }
}

impl From<KeyError> for Failure {
    fn from(e: KeyError) -> Self {
        Self::Key(e)
    }
}

/// Pool options for the signer's store connection: its own login, a member of `bezent_signer`
/// and of nothing else, acting as itself through the role's privileges. It never switches roles
/// (#27); a connection that comes back acting as another role is closed
/// ([`bezent_store::login::guard`]).
pub fn pool_options() -> PgPoolOptions {
    bezent_store::login::pool_options().max_connections(4)
}

/// Proves, before the signer serves, that `pool` logs in as a member of `bezent_signer` and of
/// nothing else, and can act as nothing else (`REQ-AR-3`; review S3; #46): the store's shared check
/// ([`bezent_store::login::check_login`]), which every service runs. What the role may do is its
/// rows of the grant matrix ([`bezent_store::grants::MATRIX`]): SELECT on intents and prepared
/// transactions, SELECT and INSERT on signatures, and the two functions that tell it whether an
/// attempt's gate is open and whether a statement's place is refused.
pub async fn check_login(pool: &PgPool) -> Result<(), String> {
    bezent_store::login::check_login(pool, bezent_store::SIGNER_ROLE).await
}

/// The signer.
#[derive(Debug)]
pub struct Signer<B> {
    pool: PgPool,
    policy: Policy,
    backend: B,
    log: Log,
    serial: tokio::sync::Mutex<()>,
}

impl<B: SigningEngine> Signer<B> {
    /// A signer over `pool` (connected with [`pool_options`]), deciding by `policy`, signing with
    /// `backend`, logging to `log`.
    pub fn new(pool: PgPool, policy: Policy, backend: B, log: Log) -> Self {
        Self {
            pool,
            policy,
            backend,
            log,
            serial: tokio::sync::Mutex::new(()),
        }
    }

    /// The signer's log.
    pub fn log(&self) -> &Log {
        &self.log
    }

    /// Signs recorded intent `intent_id`, or returns its recorded signature, or refuses it. Every
    /// outcome is logged with the intent id (`REQ-AR-3`).
    pub async fn sign(&self, intent_id: i64) -> Outcome {
        let _serial = self.serial.lock().await;
        match self.try_sign(intent_id).await {
            Ok(Ok(signed)) => {
                self.log.line(format_args!(
                    "{} intent={} key={} scheme={} public_key=0x{}",
                    if signed.replayed {
                        "replayed (AC-D2.1: the recorded signature; nothing new signed)"
                    } else {
                        "signed"
                    },
                    intent_id,
                    signed.key_id,
                    signed.scheme,
                    hex::encode(&signed.public_key)
                ));
                Outcome::Signed(signed)
            }
            Ok(Err(refusal)) => {
                self.log
                    .line(format_args!("refused intent={intent_id} {refusal}"));
                Outcome::Refused(refusal)
            }
            Err(failure) => {
                self.log
                    .line(format_args!("unavailable intent={intent_id}: {failure}"));
                Outcome::Unavailable(failure.to_string())
            }
        }
    }

    async fn try_sign(&self, intent_id: i64) -> Result<Result<Signed, Refusal>, Failure> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(SIGNING_LOCK)
            .execute(&mut *tx)
            .await?;

        type Row = (
            String,
            String,
            Option<Vec<u8>>,
            String,
            i32,
            Option<String>,
            Option<i64>,
        );
        let row: Option<Row> = sqlx::query_as(
            "SELECT network, purpose, on_chain_reference, payload::text, attempt, reference_scope, \
                    transfer_id \
             FROM intents WHERE id = $1",
        )
        .bind(intent_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((network, purpose, on_chain_reference, payload, attempt, scope, transfer)) = row
        else {
            return Ok(Err(Refusal::Unrecorded));
        };

        let recorded: Option<(String, String, Vec<u8>, Vec<u8>)> = sqlx::query_as(
            "SELECT key_id, scheme, public_key, signature FROM signatures WHERE intent_id = $1",
        )
        .bind(intent_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((key_id, scheme, public_key, signature)) = recorded {
            return Ok(Ok(Signed {
                intent_id,
                key_id,
                scheme,
                public_key,
                signature,
                replayed: true,
            }));
        }

        let Ok(payload) = serde_json::from_str::<serde_json::Value>(&payload) else {
            return Ok(Err(Refusal::Malformed("payload")));
        };
        let intent =
            match Intent::from_row(intent_id, network, purpose, on_chain_reference, &payload) {
                Ok(intent) => intent,
                Err(malformed) => return Ok(Err(Refusal::Malformed(malformed.0))),
            };

        // Issue #42 (DEC-16): a statement intent takes the statement key's path, and only it does.
        if intent.purpose == STATEMENT || payload.get("statement").is_some() {
            let outcome = self
                .statement(
                    &mut tx,
                    &intent,
                    &payload,
                    transfer,
                    attempt,
                    scope.as_deref(),
                )
                .await?;
            if outcome.is_ok() {
                tx.commit().await?;
            }
            return Ok(outcome);
        }

        // MF-A (third review; INV-9, issue #38): the signer signs only the exact bytes of the
        // prepared transaction the intent names, for its order's amount, and only when that
        // preparation answers this intent's own order (network, purpose, transfer, attempt,
        // reference and scope) on the intent's network and passed its dry run. It reads the
        // preparation itself, whatever the store's check did.
        let Some(prepared) = intent.prepared else {
            return Ok(Err(Refusal::NotPrepared { prepared: None }));
        };
        type Preparation = (
            Vec<u8>,
            String,
            bool,
            String,
            String,
            Option<i64>,
            i32,
            Option<Vec<u8>>,
            Option<String>,
            Option<String>,
            bool,
            Vec<u8>,
            String,
        );
        let preparation: Option<Preparation> = sqlx::query_as(
            "SELECT p.message, p.network, p.dry_run_passed, o.network, o.purpose, o.transfer_id, \
                    o.attempt, o.on_chain_reference, o.reference_scope, o.payload ->> 'amount', \
                    o.payload ? 'order' AND NOT o.payload ? 'prepared' \
                        AND NOT o.payload ? 'message', \
                    p.signing_payload, o.payload::text \
             FROM prepared_transactions p JOIN intents o ON o.id = p.order_intent_id \
             WHERE p.id = $1",
        )
        .bind(prepared)
        .fetch_optional(&mut *tx)
        .await?;
        // What the bytes must answer (audit must-fix 2): the core's order, read here through the
        // signer's own grant, and the full payload the endpoint says the message is of.
        let order_and_payload = preparation
            .as_ref()
            .map(|p| (p.11.clone(), p.12.clone(), p.9.clone()));
        let answers = preparation.is_some_and(
            |(
                message,
                p_network,
                passed,
                o_network,
                o_purpose,
                o_transfer,
                o_attempt,
                o_reference,
                o_scope,
                o_amount,
                is_order,
                _,
                _,
            )| {
                message == intent.message
                    && p_network == intent.network
                    && o_network == intent.network
                    && passed
                    && is_order
                    && o_purpose == intent.purpose
                    && o_transfer == transfer
                    && o_attempt == attempt
                    && o_reference == intent.on_chain_reference
                    && o_scope == scope
                    && o_amount.as_deref().and_then(parse_minor_units) == Some(intent.amount)
            },
        );
        if !answers {
            return Ok(Err(Refusal::NotPrepared {
                prepared: Some(prepared),
            }));
        }

        let Some(network) = self.policy.network(&intent.network) else {
            return Ok(Err(Refusal::UnknownNetwork(intent.network)));
        };
        if network.environment != self.backend.environment() {
            return Ok(Err(Refusal::Environment {
                network: intent.network,
                network_environment: network.environment,
                key_environment: self.backend.environment(),
            }));
        }
        let Some(binding) = self.policy.binding(&intent.network, &intent.purpose) else {
            return Ok(Err(Refusal::NoKey {
                network: intent.network,
                purpose: intent.purpose,
            }));
        };

        if network.issuing
            && intent.purpose == DESTROY
            && (intent.from.is_none() || intent.from != network.contract_account)
        {
            return Ok(Err(Refusal::DestroyNotOwn));
        }

        // One signature per (network, purpose, scope, reference, attempt), and attempt n+1 only
        // once attempt n is proven not executed (REQ-EO-6; the store enforces both as well).
        if let Some(reference) = &intent.on_chain_reference {
            let used: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM signatures WHERE network = $1 AND purpose = $2 \
                   AND reference_scope IS NOT DISTINCT FROM $3 AND on_chain_reference = $4 \
                   AND attempt = $5)",
            )
            .bind(&intent.network)
            .bind(&intent.purpose)
            .bind(&scope)
            .bind(reference)
            .bind(attempt)
            .fetch_one(&mut *tx)
            .await?;
            if used {
                return Ok(Err(Refusal::ReferenceReused));
            }
        }
        let open: bool = sqlx::query_scalar("SELECT attempt_gate_open($1, $2, $3, $4, $5)")
            .bind(&intent.network)
            .bind(&intent.purpose)
            .bind(&scope)
            .bind(&intent.on_chain_reference)
            .bind(attempt)
            .fetch_one(&mut *tx)
            .await?;
        if !open {
            return Ok(Err(Refusal::AttemptNotProven { attempt }));
        }

        // Audit must-fix 2 (REQ-AR-3, INV-9, issue #23): the signer decodes the exact bytes it is
        // asked to sign and compares what they do with the core's order, read here, not with the
        // endpoint's record of them. Bytes that do not answer the order are a compromise of the
        // process that prepared them or recorded their intent: refused, and the alert raised.
        let Some((signing_payload, order_text, order_amount)) = order_and_payload else {
            return Ok(Err(Refusal::NotPrepared {
                prepared: Some(prepared),
            }));
        };
        let checked = match self
            .answer_order(
                &mut tx,
                &intent,
                network,
                &binding.key,
                &signing_payload,
                &order_text,
                order_amount.as_deref(),
            )
            .await?
        {
            Answer::Answers(checked) => checked,
            Answer::Hold(refusal) => return Ok(Err(refusal)),
            Answer::Mismatch(why) => {
                tx.rollback().await?;
                let alert = serde_json::json!({
                    "spec": "AC-I1.2, REQ-AR-3, INV-9, REQ-IC-4",
                    "source": "signer-guard",
                    "reason": format!("the bytes asked to be signed do not answer the core's order: {why}"),
                    "requested_intent": intent_id,
                    "network": intent.network,
                    "purpose": intent.purpose,
                    "key": binding.key,
                    "actions": ["pause", "rotate", "report"],
                });
                let (alert_seq, _) = crate::compromise::raise(&self.pool, &alert).await?;
                return Ok(Err(Refusal::NotTheOrder { why, alert_seq }));
            }
        };

        // REQ-EO-4, on an EVM network, where a signed transaction never expires: a nonce the key
        // already signed at is signed again only once every transaction signed at it is proven
        // not executed; a cancel takes exactly the nonce of the transaction it cancels (checked
        // against the core's order above). Not a compromise: the lane waits. On the issuing
        // network every transaction is mortal, its era at most `MAX_ERA` blocks (checked above),
        // and the lane takes a nonce again once an era ends unsent (`REQ-NX-1`).
        if let (Some(nonce), Some(_)) = (checked.nonce, network.chain_id) {
            type AtNonce = (String, Option<String>, Option<Vec<u8>>, i32, i64);
            let at_nonce: Vec<AtNonce> = sqlx::query_as(
                "SELECT purpose, reference_scope, on_chain_reference, attempt, intent_id \
                 FROM signatures WHERE key_id = $1 AND network = $2 AND nonce = $3::numeric",
            )
            .bind(&binding.key)
            .bind(&intent.network)
            .bind(nonce.to_string())
            .fetch_all(&mut *tx)
            .await?;
            for (purpose, scope, reference, earlier, signed_intent) in at_nonce {
                if intent.purpose == CANCEL
                    && (purpose == CANCEL || Some(signed_intent) == checked.cancels)
                {
                    continue;
                }
                let proven: bool =
                    sqlx::query_scalar("SELECT attempt_gate_open($1, $2, $3, $4, $5)")
                        .bind(&intent.network)
                        .bind(&purpose)
                        .bind(&scope)
                        .bind(&reference)
                        .bind(earlier.saturating_add(1))
                        .fetch_one(&mut *tx)
                        .await?;
                if !proven {
                    return Ok(Err(Refusal::NonceInUse { nonce }));
                }
            }
        }

        // A reference counts once across its attempts (REQ-EO-6: each attempt replaces one proven
        // not executed): the latest signed attempt of every other movement, and this intent's own
        // amount for its movement. The cap is per key, network and asset (issue #23), and counts
        // what the bytes move, decoded, not what the intent declares.
        let signed_text: String = sqlx::query_scalar(
            "SELECT COALESCE(sum(amount), 0)::text FROM ( \
               SELECT DISTINCT ON (network, purpose, COALESCE(reference_scope, ''), on_chain_reference) \
                      amount \
               FROM signatures \
               WHERE key_id = $1 AND signed_at > now() - $2::bigint * interval '1 second' \
                 AND network = $3 AND asset IS NOT DISTINCT FROM $7 \
                 AND on_chain_reference IS NOT NULL \
                 AND NOT (network = $3 AND purpose = $4 \
                          AND reference_scope IS NOT DISTINCT FROM $5 \
                          AND on_chain_reference IS NOT DISTINCT FROM $6) \
               ORDER BY network, purpose, COALESCE(reference_scope, ''), on_chain_reference, \
                        attempt DESC \
             ) lanes",
        )
        .bind(&binding.key)
        .bind(i64::try_from(binding.window_seconds).unwrap_or(i64::MAX))
        .bind(&intent.network)
        .bind(&intent.purpose)
        .bind(&scope)
        .bind(&intent.on_chain_reference)
        .bind(&checked.asset)
        .fetch_one(&mut *tx)
        .await?;
        let unreferenced: String = sqlx::query_scalar(
            "SELECT COALESCE(sum(amount), 0)::text FROM signatures \
             WHERE key_id = $1 AND signed_at > now() - $2::bigint * interval '1 second' \
               AND network = $3 AND asset IS NOT DISTINCT FROM $4 \
               AND on_chain_reference IS NULL",
        )
        .bind(&binding.key)
        .bind(i64::try_from(binding.window_seconds).unwrap_or(i64::MAX))
        .bind(&intent.network)
        .bind(&checked.asset)
        .fetch_one(&mut *tx)
        .await?;
        let signed_text = match (signed_text.parse::<u128>(), unreferenced.parse::<u128>()) {
            (Ok(a), Ok(b)) => a
                .checked_add(b)
                .map_or_else(|| u128::MAX.to_string(), |t| t.to_string()),
            _ => u128::MAX.to_string(),
        };
        // A sum past u128 is past any cap.
        let signed = signed_text.parse::<u128>().unwrap_or(u128::MAX);
        if signed
            .checked_add(checked.amount)
            .is_none_or(|total| total > binding.window_cap)
        {
            return Ok(Err(Refusal::WindowCap {
                cap: binding.window_cap,
                signed,
                amount: checked.amount,
            }));
        }

        let scheme = self.backend.scheme(&binding.key)?;
        // MF1 (second review): the key about to sign is of the intent's network's kind. The
        // configuration is refused otherwise at start; this holds whatever the policy says.
        if !network.takes(scheme) {
            return Ok(Err(Refusal::KeyNetwork {
                network: intent.network,
                key: binding.key.clone(),
            }));
        }
        // SF-B (third review): an EVM intent's bytes, a payout's as well as a cancel's, are a
        // type-2 transaction on the chain id the policy declares for its network, so a key never
        // signs, for one network, a transaction another chain would take.
        if scheme == crate::keys::Scheme::Secp256k1Evm {
            let carried = cancel::chain_id(&intent.message);
            if carried.is_none() || carried != network.chain_id {
                return Ok(Err(Refusal::ChainId {
                    network: intent.network,
                    carried,
                }));
            }
        }
        let public_key = self.backend.public_key(&binding.key)?;
        // S6, SF2: a cancel moves nothing and carries no reference, so neither guard above holds
        // it; its bytes must be a transfer of nothing from the key to itself, on this network's
        // chain, with the gas of a bare transfer and fees within the network's cap.
        if intent.purpose == CANCEL {
            let shaped = match (network.chain_id, network.cancel_max_fee_per_gas) {
                (Some(chain_id), Some(max_fee)) => cancel::is_cancel(
                    &intent.message,
                    &public_key,
                    cancel::Bounds { chain_id, max_fee },
                ),
                _ => false,
            };
            if scheme.as_str() != "secp256k1-evm" || !shaped {
                return Ok(Err(Refusal::NotACancel));
            }
        }
        let signature = self.backend.sign(&binding.key, &intent.message).await?;

        sqlx::query(
            "INSERT INTO signatures (intent_id, key_id, scheme, public_key, signature, nonce, asset) \
             VALUES ($1, $2, $3, $4, $5, $6::numeric, $7)",
        )
        .bind(intent_id)
        .bind(&binding.key)
        .bind(scheme.as_str())
        .bind(&public_key)
        .bind(&signature)
        .bind(checked.nonce.map(|n| n.to_string()))
        .bind(&checked.asset)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(Ok(Signed {
            intent_id,
            key_id: binding.key.clone(),
            scheme: scheme.as_str().to_owned(),
            public_key,
            signature,
            replayed: false,
        }))
    }

    /// Decodes the exact bytes `intent` asks to be signed (and, on the issuing network, the
    /// payload they are of, or the hash of) and compares what they do with the core's order
    /// `order_text`, of amount `order_amount` (the audit of PR #56, must-fix 2; `REQ-AR-3`,
    /// `INV-9`, issue #23): the recipient, the asset or token, the amount, the reference, the
    /// network (its chain id or genesis, and the contract or vault it goes through) and, for a
    /// cancel, the nonce of the transaction the order names.
    #[allow(clippy::too_many_arguments)]
    async fn answer_order(
        &self,
        conn: &mut PgConnection,
        intent: &Intent,
        network: &crate::config::NetworkPolicy,
        key: &str,
        signing_payload: &[u8],
        order_text: &str,
        order_amount: Option<&str>,
    ) -> Result<Answer, Failure> {
        use crate::config::{MINT, PAYOUT, REFUND_MINT};
        use crate::delivery::{self, KreivoMessage};

        let mismatch = |why: &str| Ok(Answer::Mismatch(why.to_owned()));
        let Ok(order) = serde_json::from_str::<Value>(order_text) else {
            return mismatch("the order does not parse");
        };
        let o = &order["order"];
        let Some(ordered) = order_amount.and_then(parse_minor_units) else {
            return mismatch("the order names no amount");
        };
        let reference: Option<[u8; 32]> = intent
            .on_chain_reference
            .as_deref()
            .and_then(|r| r.try_into().ok());

        match intent.purpose.as_str() {
            PAYOUT | CANCEL if network.chain_id.is_some() => {
                if signing_payload != intent.message.as_slice() {
                    return mismatch("the message is not the prepared payload");
                }
                let Some(tx) = delivery::decode_evm(&intent.message) else {
                    return mismatch("the bytes are not one EIP-1559 transaction");
                };
                if Some(tx.chain_id) != network.chain_id {
                    return mismatch("the transaction is for another chain id");
                }
                if intent.purpose == CANCEL {
                    let Some(target) = o["cancel"].as_i64() else {
                        return mismatch("the cancel order names no signed transaction");
                    };
                    if ordered != 0 {
                        return mismatch("a cancel moves nothing");
                    }
                    let cancelled: Option<(i64, String, Option<String>, String)> = sqlx::query_as(
                        "SELECT i.id, i.network, i.payload ->> 'message', s.key_id \
                         FROM signed_transactions st JOIN intents i ON i.id = st.intent_id \
                         JOIN signatures s ON s.intent_id = i.id WHERE st.id = $1",
                    )
                    .bind(target)
                    .fetch_optional(&mut *conn)
                    .await?;
                    let Some((cancelled_intent, cancelled_network, message, cancelled_key)) =
                        cancelled
                    else {
                        return mismatch("the cancelled transaction was never signed here");
                    };
                    let cancelled_nonce = message
                        .as_deref()
                        .and_then(|m| m.strip_prefix("0x"))
                        .and_then(|m| hex::decode(m).ok())
                        .and_then(|m| delivery::decode_evm(&m))
                        .map(|t| t.nonce);
                    if cancelled_network != intent.network || cancelled_key != key {
                        return mismatch("the cancelled transaction is of another lane");
                    }
                    if cancelled_nonce != Some(tx.nonce) {
                        return mismatch(
                            "the cancel is at another nonce than the transaction it cancels",
                        );
                    }
                    return Ok(Answer::Answers(Checked {
                        amount: 0,
                        asset: None,
                        nonce: Some(tx.nonce),
                        cancels: Some(cancelled_intent),
                    }));
                }
                let Some(contracts) = network.delivery else {
                    return Ok(Answer::Hold(Refusal::NotADelivery {
                        purpose: intent.purpose.clone(),
                    }));
                };
                if tx.to != contracts.roles || tx.value != 0 {
                    return mismatch(
                        "the transaction is not a call of nothing to the vault's Roles modifier",
                    );
                }
                let Some(p) = delivery::decode_evm_payout(&contracts, &tx.data) else {
                    return mismatch("the call is not a payout through the vault's delivery role");
                };
                if o["to"].as_str().and_then(address20) != Some(p.to) {
                    return mismatch("the beneficiary is not the order's");
                }
                // The token: the one admitted for the order's asset on this network; or, for the
                // refund of an arrival of a token that is not admitted (REQ-IN-6), the token the
                // order names itself.
                let asset = match (o["asset"].as_str(), o["token"].as_str()) {
                    (Some(asset), named) => {
                        let tokens: Vec<String> = sqlx::query_scalar(
                            "SELECT token FROM admitted_tokens WHERE network = $1 AND asset = $2",
                        )
                        .bind(&intent.network)
                        .bind(asset)
                        .fetch_all(&mut *conn)
                        .await?;
                        let [token] = tokens.as_slice() else {
                            return mismatch(
                                "the order's asset has not exactly one admitted token here",
                            );
                        };
                        if address20(token) != Some(p.token) {
                            return mismatch("the token is not the order's asset's");
                        }
                        if let Some(named) = named
                            && named != token
                            && address20(named) != Some(p.token)
                        {
                            return mismatch("the token is not the one the order names");
                        }
                        asset.to_owned()
                    }
                    (None, Some(named)) => {
                        if address20(named) != Some(p.token) {
                            return mismatch("the token is not the one the order names");
                        }
                        named.to_ascii_lowercase()
                    }
                    (None, None) => return mismatch("the order names no asset nor token"),
                };
                if p.amount != ordered {
                    return mismatch("the amount is not the order's");
                }
                if Some(p.reference) != reference {
                    return mismatch("the reference is not the order's");
                }
                Ok(Answer::Answers(Checked {
                    amount: p.amount,
                    asset: Some(asset),
                    nonce: Some(tx.nonce),
                    cancels: None,
                }))
            }
            MINT | REFUND_MINT if network.issuing => {
                if delivery::kreivo_message_to_sign(signing_payload) != intent.message {
                    return mismatch("the message is not the prepared payload, nor its hash");
                }
                let Some(tx) = delivery::decode_kreivo(signing_payload) else {
                    return mismatch("the payload is not one call of the contract");
                };
                if Some(tx.genesis) != network.genesis {
                    return mismatch("the transaction is for another network's genesis");
                }
                if network.contract_account.as_deref() != Some(tx.contract.as_slice())
                    || tx.value != 0
                {
                    return mismatch("the call is not one of nothing to the on-chain component");
                }
                if tx.era_period() > delivery::MAX_ERA {
                    return mismatch("the era is longer than the endpoint's");
                }
                let Some(message) = KreivoMessage::decode(&tx.data) else {
                    return mismatch("the call is not the contract's mint or refund mint");
                };
                let checked = match (intent.purpose.as_str(), message) {
                    (
                        MINT,
                        KreivoMessage::Mint {
                            asset,
                            to,
                            amount,
                            reference: r,
                        },
                    ) => {
                        let named = format!("Here({asset})");
                        if o["asset"].as_str() != Some(named.as_str()) {
                            return mismatch("the asset is not the order's");
                        }
                        if o["to"].as_str().and_then(account32) != Some(to) {
                            return mismatch("the recipient is not the order's");
                        }
                        if amount != ordered {
                            return mismatch("the amount is not the order's");
                        }
                        if Some(r) != reference {
                            return mismatch("the reference is not the order's");
                        }
                        Checked {
                            amount,
                            asset: Some(named),
                            nonce: Some(u64::from(tx.nonce)),
                            cancels: None,
                        }
                    }
                    (
                        REFUND_MINT,
                        KreivoMessage::RefundMint {
                            redeemer,
                            reference: r,
                        },
                    ) => {
                        if o["redeemer"].as_str().and_then(account32) != Some(redeemer) {
                            return mismatch("the redeemer is not the order's");
                        }
                        if Some(r) != reference {
                            return mismatch("the reference is not the order's");
                        }
                        // The contract re-mints what the redemption destroyed: the bytes carry no
                        // amount, and the order's is the store's (0049: the redemption's).
                        Checked {
                            amount: ordered,
                            asset: o["asset"].as_str().map(str::to_owned),
                            nonce: Some(u64::from(tx.nonce)),
                            cancels: None,
                        }
                    }
                    _ => return mismatch("the call is another message than the order's"),
                };
                Ok(Answer::Answers(checked))
            }
            _ => Ok(Answer::Hold(Refusal::NotADelivery {
                purpose: intent.purpose.clone(),
            })),
        }
    }

    /// Signs statement intent `intent` with the statement key, behind its own guards (issue #42;
    /// `REQ-PR-1`, `REQ-PR-2`, `REQ-KY-6`, `INV-9`), recording the signature in `conn`.
    async fn statement(
        &self,
        conn: &mut PgConnection,
        intent: &Intent,
        payload: &Value,
        transfer: Option<i64>,
        attempt: i32,
        scope: Option<&str>,
    ) -> Result<Result<Signed, Refusal>, Failure> {
        if intent.purpose != STATEMENT
            || transfer.is_some()
            || intent.on_chain_reference.is_some()
            || scope.is_some()
            || attempt != 1
            || intent.prepared.is_some()
            || intent.amount != 0
            || intent.from.is_some()
        {
            return Ok(Err(Refusal::NotAStatement(
                "a statement intent carries a statement and nothing of a transfer",
            )));
        }
        let Some(network) = self.policy.network(&intent.network) else {
            return Ok(Err(Refusal::UnknownNetwork(intent.network.clone())));
        };
        if !network.issuing {
            return Ok(Err(Refusal::NotAStatement(
                "a statement intent is recorded on the issuing network",
            )));
        }
        if network.environment != self.backend.environment() {
            return Ok(Err(Refusal::Environment {
                network: intent.network.clone(),
                network_environment: network.environment,
                key_environment: self.backend.environment(),
            }));
        }
        let Some(key) = self.policy.statement_key() else {
            return Ok(Err(Refusal::NoKey {
                network: intent.network.clone(),
                purpose: intent.purpose.clone(),
            }));
        };
        let scheme = self.backend.scheme(key)?;
        if scheme != crate::keys::Scheme::Ed25519 {
            return Ok(Err(Refusal::NotAStatement(
                "the statement key is not ed25519",
            )));
        }
        let public_key = self.backend.public_key(key)?;

        let named = &payload["statement"];
        let (Some(asset), Some(sequence), Some(sha256)) = (
            named["asset"].as_str(),
            named["sequence"].as_i64(),
            named["sha256"].as_str(),
        ) else {
            return Ok(Err(Refusal::Malformed("statement")));
        };
        if format!("0x{}", hex::encode(Sha256::digest(&intent.message))) != sha256 {
            return Ok(Err(Refusal::NotAStatement(
                "the bytes do not hash to the SHA-256 the intent names",
            )));
        }
        let Ok(doc) = serde_json::from_slice::<Value>(&intent.message) else {
            return Ok(Err(Refusal::NotAStatement("the bytes are not a statement")));
        };
        if doc["statement"] != "bezent reserve statement"
            || doc["asset"] != asset
            || doc["sequence"] != sequence
            || doc["signing_key"]["public_key"] != format!("0x{}", hex::encode(&public_key))
        {
            return Ok(Err(Refusal::NotAStatement(
                "the bytes are not the statement of the asset and sequence the intent names, under this key",
            )));
        }
        // REQ-PR-2: one statement per place in the chain, ever, whatever the key (PR #50 re-check
        // S-1): a rotated key never signs a second statement for a place an earlier key signed,
        // published or not. A place whose signed statement could not be published is abandoned
        // by the core instead, and the chain goes on at the next place.
        let other: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM signatures g JOIN intents i ON i.id = g.intent_id \
               WHERE i.purpose = 'statement' AND i.payload -> 'statement' ->> 'asset' = $1 \
                 AND i.payload -> 'statement' ->> 'sequence' = $2 \
                 AND i.payload -> 'statement' ->> 'sha256' <> $3)",
        )
        .bind(asset)
        .bind(sequence.to_string())
        .bind(sha256)
        .fetch_one(&mut *conn)
        .await?;
        if other {
            return Ok(Err(Refusal::StatementEquivocation {
                asset: asset.to_owned(),
                sequence,
            }));
        }
        // REQ-PR-2 (PR #50 re-check S-1, S-3): and only at a place that skips nothing but
        // abandoned places, naming each of them exactly, after the published statement it names.
        // The store holds the rule; the signer reads it through its one function.
        let previous = match &doc["previous_sha256"] {
            Value::Null => None,
            Value::String(h) => match hex::decode(h.strip_prefix("0x").unwrap_or(h)) {
                Ok(h) => Some(h),
                Err(_) => return Ok(Err(Refusal::NotAStatement("previous_sha256 is not hex"))),
            },
            _ => return Ok(Err(Refusal::NotAStatement("previous_sha256 is not hex"))),
        };
        let place: Option<String> =
            sqlx::query_scalar("SELECT statement_place_refusal($1, $2, $3, $4::text::jsonb)")
                .bind(asset)
                .bind(sequence)
                .bind(previous)
                .bind(match doc.get("abandoned") {
                    None | Some(Value::Null) => "[]".to_owned(),
                    Some(named) => named.to_string(),
                })
                .fetch_one(&mut *conn)
                .await?;
        if let Some(why) = place {
            return Ok(Err(Refusal::StatementPlace {
                asset: asset.to_owned(),
                sequence,
                why,
            }));
        }
        let signature = self.backend.sign(key, &intent.message).await?;
        sqlx::query(
            "INSERT INTO signatures (intent_id, key_id, scheme, public_key, signature) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(intent.id)
        .bind(key)
        .bind(scheme.as_str())
        .bind(&public_key)
        .bind(&signature)
        .execute(&mut *conn)
        .await?;
        Ok(Ok(Signed {
            intent_id: intent.id,
            key_id: key.to_owned(),
            scheme: scheme.as_str().to_owned(),
            public_key,
            signature,
            replayed: false,
        }))
    }
}
