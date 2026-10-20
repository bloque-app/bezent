// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The state machine (`F-003`, T-003-02 to T-003-07).
//!
//! [`Core::cycle`] reads durable state and nothing else (`REQ-AR-1`, `REQ-ST-7`), and moves each
//! open transfer along the §5.4 table by appending transitions the store checks (`REQ-TR-6`).
//! Every decision is a function of what is recorded: a transfer is locked, read, decided and moved
//! in one transaction, and a step that already happened is not chosen again, because the state it
//! starts from has changed or the row it would write exists. So a restart, or a second cycle,
//! reprocesses nothing twice (T-003-02).
//!
//! # Deliveries, in two steps (PLAN `DEC-3`; PR #31, F-007 NOTES decision 1)
//!
//! 1. The core records an **order**: an intent with the purpose (`mint`, `refund-mint`,
//!    `payout`), the network, the on-chain reference and its scope, the attempt, and a payload
//!    `{"order": {…}, "amount": "<minor units>"}` with no `message`, so no signer signs it.
//! 2. The endpoint **prepares** it: builds the transaction, dry-runs it at a final block and records
//!    the exact bytes in `prepared_transactions`.
//! 3. For a prepared transaction whose dry run passed, the core records the **signing intent**:
//!    `{"message", "amount", "prepared"}`, the exact bytes to sign (`INV-9`).
//! 4. The endpoint asks the signer for the signature and persists the signed bytes; the core
//!    **verifies** every signature recorded for its signing intents against the intent's bytes
//!    (`AC-I1.2`): one that does not verify raises `key-compromise` and the pause request, once,
//!    and the transfer does not move. A verified signature with its signed bytes recorded is
//!    `Delivering` (§5.4: "a destination transaction is signed and recorded").
//!
//! A dry run that fails holds the condition its refusal names (`CND-DestinationRefused`,
//! `CND-MintWindow`, `CND-FeeShort`, `CND-PayoutWindow`), and a new order is recorded after
//! [`Settings::retry_preparation_after`]. A payout the vault's outflow allowance cannot take this
//! window holds `CND-PayoutWindow` (amendment 0012): never review, never a refund; each new order
//! is prepared again, and the first whose dry run passes resumes it, once the allowance refills.
//! The hold is named only from the structured refusal the EVM endpoint records when both providers
//! returned the same Roles `ConditionViolation` (status 17, the token's delivery allowance key),
//! never from the reason's text (PR #52 review S-2). Taking and leaving the hold rest on the
//! destination network's latest final head: the time held does not count toward the attempt's
//! deadline, whose clock restarts when the hold is left (review M-1), and a hold longer than
//! [`Settings::payout_window_alert_after`] pages once, then daily (review S-3).
//!
//! # Forced non-execution, with review (`REQ-RV-1`, `REQ-NX-4`, `REQ-RV-2`; issue #41)
//!
//! `REQ-RV-1` and `REQ-NX-4` apply together, not in sequence (orchestrator's reading,
//! 2026-10-15). When an attempt's destination leg is unknown at its deadline, the transfer goes to
//! `Needs-Review` at that moment, with its owner and service level (`AC-B1.4`). On an origin
//! network, where a transaction never expires, the core at the same time orders a **cancel** of
//! each of the attempt's signed transactions that has no proof, in the same two steps: an order of
//! purpose `cancel`, `{"order": {"cancel": <signed transaction>}, "amount": "0"}`, with no
//! reference and no transfer (it moves nothing and is no attempt of the transfer's); the EVM
//! endpoint prepares and dry-runs it (a transfer of nothing from the key to itself at the payout's
//! nonce); the core records the signing intent over those bytes and verifies the signature. The
//! core drives the cancel while the transfer is in review. The endpoint's proof that the payout's
//! nonce was consumed by another transaction at a final block is **evidence for the review**
//! (`REQ-RV-2`): the core attaches it to the review by itself (`review-evidence`), and the review
//! ends as any other, by a resolution naming it; a new attempt or the refund mint moves value and
//! waits for its second approver (`REQ-GV-5`, `INV-8`, `REQ-EO-6`). A cancel that stays unknown
//! leaves the review open, with the ordered cancel on record and no evidence attached; nothing is
//! refunded (`REQ-RF-1`). On the issuing network a transaction expires with its era: no cancel is
//! ordered, and the expiry at finality (`REQ-NX-1`) is attached as evidence in the same way.
//!
//! # Invariants checked here (`INV-6`, `INV-7`, `INV-16`)
//!
//! No order is recorded unless the transfer reached `Source-Final` with its milestone *and* the
//! source observation is final as observed, at a recorded final point. A store that let a transfer
//! begin at `Source-Final` without a final observation (the store admits that insert, `REQ-IN-3`)
//! still never gets a mint or a payout from the core.

use bezent_core::{Condition, State, review_exit};
use bezent_signer::compromise::{self, Answer, Returned};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};

use crate::cutoff;
use crate::refs;
use crate::store::{self, Move, Observation, Point, ProofPoint, Transfer};

/// How the core behaves, beyond what the store declares.
#[derive(Clone, Debug)]
pub struct Settings {
    /// After a failed dry run, how long before a new order is recorded for the same attempt.
    pub retry_preparation_after: Duration,
    /// The deadline of a transfer whose route declares none (a refund): from the start of the leg.
    pub default_deadline: Duration,
    /// How long a transfer may hold `CND-PayoutWindow`, on the destination network's clock, before
    /// a human is paged (once, then daily). Two daily allowance periods by default (F-008 NOTES:
    /// the delivery allowances refill per day): a payout above the allowance's maximum, or a
    /// refill set to nothing, would otherwise wait unseen (PR #52 review S-3).
    pub payout_window_alert_after: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            retry_preparation_after: Duration::seconds(60),
            default_deadline: Duration::hours(24),
            payout_window_alert_after: Duration::days(2),
        }
    }
}

/// What one cycle did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Withdrawals made from redemptions.
    pub withdrawals: usize,
    /// Transfers that moved or changed condition.
    pub moved: usize,
    /// Intents recorded (orders and signing intents).
    pub intents: usize,
    /// Transfers whose step failed; each is retried next cycle.
    pub failed: usize,
}

/// The core.
#[derive(Clone, Debug)]
pub struct Core {
    pool: PgPool,
    settings: Settings,
}

/// What a step did to a transfer.
#[derive(Default)]
struct Did {
    moved: bool,
    intents: usize,
}

impl Did {
    fn moved() -> Self {
        Self {
            moved: true,
            intents: 0,
        }
    }
    fn intent() -> Self {
        Self {
            moved: false,
            intents: 1,
        }
    }
}

/// A transfer's destination leg, as observed (review M1).
enum Leg {
    /// None observed.
    None,
    /// Exactly one.
    One(Box<Observation>),
    /// More than one carries the reference: unknown, never a pick.
    Ambiguous,
}

/// What a destination leg moved that its order did not (`REQ-TR-3`, `REQ-TR-5`).
struct Mismatch {
    /// The amount ordered, in the destination network's minor units.
    expected: Option<u128>,
    /// The recipient ordered.
    recipient: Option<String>,
    /// Whether the leg executed, and moved exactly the amount ordered.
    amount_ok: bool,
}

/// The purpose of a transfer's delivery, and the event its destination leg is observed as.
fn purpose(t: &Transfer) -> (&'static str, &'static str) {
    match (t.kind.as_str(), t.refund_of.is_some()) {
        ("deposit", _) => ("mint", "Minted"),
        ("refund", true) => ("refund-mint", "RefundMinted"),
        // A withdrawal's payout, and the refund of an unassociated arrival, are payouts on an
        // origin network (F-009).
        _ => ("payout", "Payout"),
    }
}

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn pow10(exp: i16) -> Option<u128> {
    10u128.checked_pow(u32::try_from(exp).ok()?)
}

/// The conditions the gates before a delivery hold (`CND-Paused`, `CND-CutOff`,
/// `CND-ReserveShort`): they clear by themselves when the gate opens.
fn gate_condition(c: Condition) -> bool {
    matches!(
        c,
        Condition::Paused | Condition::CutOff | Condition::ReserveShort
    )
}

/// The Roles modifier's `Status.AllowanceExceeded` (Zodiac Roles v2): the status of its
/// `ConditionViolation(uint8 status, bytes32 info)` when a call is over an allowance, whose key is
/// `info`.
const ALLOWANCE_EXCEEDED: u64 = 17;

/// The delivery allowance key of origin token `token` (an EVM address), as F-008's `VaultRoles`
/// declares it: `keccak256("bezent.delivery.allowance" ‖ token)`. `None` when `token` is not an
/// address. A delivery's only allowance is its token's (`VaultRoles.transferConditions`).
fn delivery_allowance_key(token: &str) -> Option<[u8; 32]> {
    use sha3::{Digest, Keccak256};
    let address = hex::decode(token.strip_prefix("0x")?).ok()?;
    if address.len() != 20 {
        return None;
    }
    let mut h = Keccak256::new();
    h.update(b"bezent.delivery.allowance");
    h.update(&address);
    Some(h.finalize().into())
}

/// Whether a failed dry run is the vault's refusal of a payout over its outflow allowance
/// (amendment 0012): the structured `violation` the EVM endpoint records only when both providers
/// returned the same `ConditionViolation` revert data (PLAN `DEC-12`), with status 17 and, as its
/// key, `allowance`, the delivery allowance key of the payout's token. Nothing is read from the
/// reason's text, which any provider or reverting contract can word (PR #52 review S-2).
fn allowance_spent(dry_run: &Value, allowance: Option<&[u8; 32]>) -> bool {
    let Some(v) = dry_run.get("violation").filter(|v| v.is_object()) else {
        return false;
    };
    let key = v
        .get("key")
        .and_then(Value::as_str)
        .and_then(|k| k.strip_prefix("0x"))
        .and_then(|k| hex::decode(k).ok());
    v.get("status").and_then(Value::as_u64) == Some(ALLOWANCE_EXCEEDED)
        && allowance.is_some_and(|a| key.as_deref() == Some(a.as_slice()))
}

/// What a failed dry run holds the transfer in, if anything resumes it by itself. `allowance` is
/// the delivery allowance key of a payout's token ([`allowance_spent`]).
fn dry_run_condition(dry_run: &Value, allowance: Option<&[u8; 32]>) -> Option<Condition> {
    if allowance_spent(dry_run, allowance) {
        // The reserve's outflow allowance for the window is spent: wait for it to refill
        // (amendment 0012; REQ-RN-4, REQ-KY-3).
        return Some(Condition::PayoutWindow);
    }
    let reason = dry_run.get("reason").and_then(Value::as_str).unwrap_or("");
    if dry_run.get("violation").is_some_and(Value::is_object)
        || reason.contains("ConditionViolation(")
    {
        // Another Roles refusal, or one the providers did not return alike: no condition of any
        // kind, and never the fee rule below (the key's hex may spell "fee"; S-2).
        None
    } else if reason.contains("AboveWindow") || reason.contains("AboveCeiling") {
        Some(Condition::MintWindow)
    } else if reason.starts_with("Kreivo") {
        // Kreivo's assets API refused the mint: the destination cannot receive the asset
        // (REQ-IN-8, REQ-IN-9). bezent never checks the destination itself.
        Some(Condition::DestinationRefused)
    } else if reason.to_ascii_lowercase().contains("fee")
        || reason.contains("Payment")
        || reason.contains("InsufficientBalance")
    {
        Some(Condition::FeeShort)
    } else {
        None
    }
}

impl Core {
    /// The core over `pool`, which connects as the core's login (a member of `bezent_core`
    /// only, #46).
    pub fn new(pool: PgPool, settings: Settings) -> Self {
        Self { pool, settings }
    }

    /// The store pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// One cycle at wall-clock time `now`: withdrawals from new redemptions, then a step for every
    /// open transfer, then the per-(asset, network) conditions raised and cleared.
    pub async fn cycle(&self, now: DateTime<Utc>) -> sqlx::Result<Report> {
        // F-004: every final movement is in the book before anything reads it (REQ-BK-7). An
        // observation the book cannot post is quarantined and paged there; nothing it does stops
        // the cycle (INV-1, review M1).
        if let Err(e) = crate::reserve::book::post_pending(&self.pool).await {
            tracing::warn!(error = %e, "REQ-BK-7: pending movements not posted; the cycle goes on");
        }
        let mut report = Report {
            withdrawals: self.withdrawals_from_redemptions().await?,
            ..Report::default()
        };
        let open: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM transfers WHERE NOT is_terminal_state(state) ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await?;
        for id in open {
            match self.step(id, now).await {
                Ok(did) => {
                    report.moved += usize::from(did.moved);
                    report.intents += did.intents;
                }
                Err(e) => {
                    report.failed += 1;
                    tracing::warn!(transfer = id, error = %e, "step failed; retried next cycle");
                }
            }
        }
        self.clear_reserve_conditions().await?;
        Ok(report)
    }

    // -----------------------------------------------------------------------------------------
    // Withdrawals: a redemption is the request (REQ-RQ-4, REQ-OT-1, REQ-OT-2).

    /// Makes a withdrawal of every executed redemption on the issuing network that has none, in
    /// `Received` and accepted at once (§5.4: within route rules, the source leg not yet final as
    /// far as the transfer knows; the next step finds it final).
    async fn withdrawals_from_redemptions(&self) -> sqlx::Result<usize> {
        let found: Vec<Observation> = sqlx::query_as(&format!(
            "SELECT {} FROM observations o \
             LEFT JOIN observation_outcomes x ON x.observation_id = o.id \
             JOIN networks n ON n.network = o.network AND n.role = 'issuing' \
             WHERE o.executed AND o.detail ->> 'event' = 'Redeemed' \
               AND o.on_chain_reference IS NOT NULL AND o.sender IS NOT NULL \
               AND NOT EXISTS (SELECT 1 FROM transfers t WHERE t.kind = 'withdrawal' \
                 AND t.source_network = o.network AND t.source_block_number = o.block_number \
                 AND t.source_block_hash = o.block_hash \
                 AND t.source_position_kind = o.position_kind \
                 AND t.source_position_index = o.position_index) \
             ORDER BY o.id",
            store::OBSERVATION
        ))
        .fetch_all(&self.pool)
        .await?;
        let mut made = 0;
        for o in found {
            let mut tx = self.pool.begin().await?;
            if self.withdrawal_from(&mut tx, &o).await? {
                made += 1;
            }
            tx.commit().await?;
        }
        Ok(made)
    }

    async fn withdrawal_from(
        &self,
        conn: &mut PgConnection,
        o: &Observation,
    ) -> sqlx::Result<bool> {
        let (Some(issuing_asset), Some(amount), Some(redeemer), Some(reference)) = (
            o.token.as_deref(),
            o.amount(),
            o.sender.as_deref(),
            o.on_chain_reference.as_deref(),
        ) else {
            return Ok(false);
        };
        // The backend the redemption names, as the endpoint records it (F-007): {"kind", "chain_id"
        // or "id"}, mapped to an origin network by declaration.
        let backend = &o.detail["backend"];
        let backend = match (backend["kind"].as_str(), backend["chain_id"].as_u64()) {
            (Some("evm"), Some(id)) => format!("evm:{id}"),
            (Some("attested"), _) => {
                // REQ-RQ-9: an attested backend's redemption carries only a commitment, never an
                // address; its payout is the requester's, reported against the relayed
                // instructions, not an EVM payout. A commitment never becomes a destination
                // (review S2), so the core makes no payout from it.
                tracing::warn!(
                    observation = o.id,
                    "a redemption to an attested backend; no EVM payout is made from a commitment"
                );
                return Ok(false);
            }
            _ => return Ok(false),
        };
        // A public backend's payout instructions are inline: the beneficiary's address (REQ-RQ-9,
        // REQ-OT-4).
        let Some(destination) = o.detail["payout"]
            .get("address")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            tracing::warn!(
                observation = o.id,
                "a redemption to a public backend with no payout address; no withdrawal"
            );
            return Ok(false);
        };
        let row: Option<(String, String, i16, i16)> = sqlx::query_as(
            "SELECT a.asset, n.network, a.precision, t.precision FROM assets a \
             JOIN networks n ON n.backend = $2 \
             JOIN admitted_tokens t ON t.asset = a.asset AND t.network = n.network \
             WHERE a.issuing_asset = $1",
        )
        .bind(issuing_asset)
        .bind(&backend)
        .fetch_optional(&mut *conn)
        .await?;
        let Some((asset, network, asset_precision, token_precision)) = row else {
            tracing::warn!(
                observation = o.id,
                "a redemption of an undeclared asset or backend; no withdrawal"
            );
            return Ok(false);
        };
        // The transfer's amount is in the asset's book unit, the origin token's minor unit
        // (F-001 NOTES, S6): the redeemed amount times the declared ratio (REQ-AS-2, REQ-AS-4).
        let Some(book) =
            pow10(token_precision - asset_precision).and_then(|r| amount.checked_mul(r))
        else {
            return Ok(false);
        };
        let c = o.coordinate();
        // The bridge reference is the redemption's reference, (redeemer, reference); the payout
        // writes it on the origin network, so its leg is this withdrawal's alone (REQ-OT-2,
        // INV-2, PLAN DEC-10; review M1).
        let bridge_reference = refs::withdrawal(redeemer, reference);
        let id: Option<i64> = sqlx::query_scalar(
            "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                 destination_network, destination, source_network, source_block_number, \
                 source_block_hash, source_position_kind, source_position_index, \
                 on_chain_reference, redeemer, destination_reference, state) \
             VALUES ($1, $2, 'withdrawal', $3, $4::numeric, $5, $6, $7, $8, $9, $10::position_kind, \
                 $11, $12, $13, $14, 'Received') \
             ON CONFLICT (bridge_reference) DO NOTHING RETURNING id",
        )
        .bind(&bridge_reference)
        .bind(format!("redemption:{redeemer}:{}", hex0x(reference)))
        .bind(&asset)
        .bind(book.to_string())
        .bind(&network)
        .bind(&destination)
        .bind(&c.network)
        .bind(c.block_number)
        .bind(&c.block_hash)
        .bind(&c.position_kind)
        .bind(c.position_index)
        .bind(reference)
        .bind(redeemer)
        .bind(refs::payout_reference(&bridge_reference).to_vec())
        .fetch_optional(&mut *conn)
        .await?;
        let Some(id) = id else {
            return Ok(false);
        };
        store::append(
            conn,
            id,
            &Move {
                from: State::Received,
                to: State::Accepted,
                condition: None,
                point: None,
                attempt: None,
                manual_action: None,
            },
        )
        .await?;
        Ok(true)
    }

    // -----------------------------------------------------------------------------------------
    // One transfer.

    async fn step(&self, id: i64, now: DateTime<Utc>) -> sqlx::Result<Did> {
        let mut tx = self.pool.begin().await?;
        let Some(t) = store::lock_transfer(&mut tx, id).await? else {
            return Ok(Did::default());
        };
        // Issue #24: an endpoint's delivery anomaly for this transfer is surfaced before anything
        // else is decided about it.
        let surfaced = self.surface_anomalies(&mut tx, &t).await?;
        let did = match surfaced {
            Some(did) => did,
            None => match t.state() {
                State::Received => self.received(&mut tx, &t).await?,
                State::Accepted => self.accepted(&mut tx, &t).await?,
                State::SourceFinal => self.source_final(&mut tx, &t, now).await?,
                State::Delivering => self.delivering(&mut tx, &t, now).await?,
                State::Delivered => self.delivered(&mut tx, &t, now).await?,
                State::Refunding => self.refunding(&mut tx, &t).await?,
                State::NeedsReview => self.review(&mut tx, &t, now).await?,
                _ => Did::default(),
            },
        };
        if t.condition() == Some(Condition::PayoutWindow) {
            self.payout_window_alert(&mut tx, &t).await?;
        }
        tx.commit().await?;
        Ok(did)
    }

    /// Surfaces the delivery anomalies the endpoints recorded for `t` (issue #24; F-007 review;
    /// `INV-8`, `REQ-NX-3`, `REQ-RV-1`). An endpoint records a `delivery-anomaly` when what it
    /// reads contradicts itself: bytes that do not answer their rows, a nonce consumed by another
    /// payload for the same reference, a reference the contract says was used though the chain
    /// shows the transaction not executed. It proves nothing then, and the transfer's leg is
    /// unknown; so a transfer being delivered goes to `Needs-Review` at once, with its owner and
    /// service level, instead of waiting for its deadline, and the anomaly is attached to the
    /// review as an event. A transfer already in review gets the event only. Nothing is refunded or
    /// retried because of it (`REQ-RF-1`), and a review ends only by evidence (`REQ-RV-2`).
    ///
    /// An anomaly is the transfer's only if it is of the transfer's destination network and names
    /// one of the transfer's intents, signed transactions or its leg's on-chain reference: an
    /// endpoint cannot send another network's transfer to review. Each is surfaced once (the
    /// event names it), so a resolved review is not reopened by the anomaly that caused it.
    /// `Some` when the transfer moved.
    async fn surface_anomalies(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
    ) -> sqlx::Result<Option<Did>> {
        let from = t.state();
        if !matches!(
            from,
            State::SourceFinal | State::Delivering | State::Delivered | State::NeedsReview
        ) {
            return Ok(None);
        }
        let leg = Self::leg_reference(t).map(hex0x);
        // A number in a payload is cast only when it is a whole one that fits.
        let id = |key: &str| {
            format!(
                "CASE WHEN (e.payload ->> '{key}') ~ '^[0-9]{{1,15}}$' \
                      THEN (e.payload ->> '{key}')::bigint END"
            )
        };
        let sql = format!(
            "SELECT e.seq, e.payload ->> 'reason' FROM event_log e \
             WHERE e.kind = 'delivery-anomaly' \
               AND e.payload ->> 'network' = $2 \
               AND ( \
                 EXISTS (SELECT 1 FROM signed_transactions st \
                         JOIN intents i ON i.id = st.intent_id \
                         WHERE i.transfer_id = $1 AND i.network = $2 \
                           AND st.id IN ({signed_transaction}, {signed})) \
                 OR EXISTS (SELECT 1 FROM intents i \
                            WHERE i.transfer_id = $1 AND i.network = $2 AND i.id = {intent}) \
                 OR (lower(e.payload ->> 'on_chain_reference') = $3 \
                     OR lower(e.payload ->> 'reference') = $3)) \
               AND NOT EXISTS (SELECT 1 FROM event_log s \
                               WHERE s.kind = 'delivery-anomaly-surfaced' AND s.transfer_id = $1 \
                                 AND s.payload ->> 'anomaly' = e.seq::text) \
             ORDER BY e.seq",
            signed_transaction = id("signed_transaction"),
            signed = id("signed"),
            intent = id("intent"),
        );
        let anomalies: Vec<(i64, Option<String>)> = sqlx::query_as(&sql)
            .bind(t.id)
            .bind(&t.destination_network)
            .bind(leg)
            .fetch_all(&mut *conn)
            .await?;
        if anomalies.is_empty() {
            return Ok(None);
        }
        for (seq, reason) in &anomalies {
            store::event(
                conn,
                "delivery-anomaly-surfaced",
                Some(t.id),
                &json!({"spec": "INV-8, REQ-NX-3, REQ-RV-1", "anomaly": seq.to_string(),
                        "bridge_reference": t.bridge_reference, "state": from.as_str(),
                        "reason": reason}),
            )
            .await?;
            tracing::error!(transfer = %t.bridge_reference, anomaly = seq, ?reason, "INV-8: an endpoint recorded a delivery anomaly for this transfer");
        }
        if from == State::NeedsReview {
            return Ok(None);
        }
        store::append(
            conn,
            t.id,
            &Move {
                from,
                to: State::NeedsReview,
                condition: None,
                point: None,
                attempt: None,
                manual_action: None,
            },
        )
        .await?;
        Ok(Some(Did::moved()))
    }

    /// `Received`: a deposit is the ingress's; a refund of a withdrawal is accepted at once; the
    /// refund of an unassociated arrival only with the operator's recorded, approved release
    /// (`REQ-IN-6`, `REQ-RQ-6`; the store refuses it otherwise).
    async fn received(&self, conn: &mut PgConnection, t: &Transfer) -> sqlx::Result<Did> {
        if t.kind != "refund" {
            return Ok(Did::default());
        }
        let manual = if let Some(original) = t.refund_of {
            // Review M1b of PR #56: a refund of a withdrawal is accepted only once the withdrawal
            // it replaces is `Refunding`, the edge that proved every attempt not executed at
            // finality (INV-8, REQ-RF-1); the store lets only the core create it (0047).
            let refunding: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM transfers WHERE id = $1 AND kind = 'withdrawal' \
                   AND state = 'Refunding')",
            )
            .bind(original)
            .fetch_one(&mut *conn)
            .await?;
            if !refunding {
                tracing::error!(transfer = %t.bridge_reference, original, "INV-8: a refund whose withdrawal is not Refunding; not accepted");
                return Ok(Did::default());
            }
            None
        } else {
            let release: Option<i64> = sqlx::query_scalar(
                "SELECT a.id FROM manual_actions a JOIN manual_approvals p ON p.action_id = a.id \
                 WHERE a.transfer_id = $1 AND a.kind = 'release-unassociated' \
                   AND NOT EXISTS (SELECT 1 FROM transitions x WHERE x.manual_action_id = a.id) \
                 ORDER BY a.id LIMIT 1",
            )
            .bind(t.id)
            .fetch_optional(&mut *conn)
            .await?;
            match release {
                Some(a) => Some(a),
                None => return Ok(Did::default()),
            }
        };
        store::append(
            conn,
            t.id,
            &Move {
                from: State::Received,
                to: State::Accepted,
                condition: None,
                point: None,
                attempt: None,
                manual_action: manual,
            },
        )
        .await?;
        Ok(Did::moved())
    }

    /// The deadline of a leg that started at `start`: the route's completion budget (`NFR-2`), or
    /// the default for a transfer with no route. The spec names "the route's deadline" without a
    /// declaration of its own (F-003 NOTES).
    async fn deadline(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        start: DateTime<Utc>,
    ) -> sqlx::Result<DateTime<Utc>> {
        let budget: Option<i64> = match t.route() {
            Some((asset, network, direction)) => {
                sqlx::query_scalar(
                    "SELECT completion_budget_seconds FROM routes \
                 WHERE asset = $1 AND network = $2 AND direction = $3",
                )
                .bind(asset)
                .bind(network)
                .bind(direction)
                .fetch_optional(&mut *conn)
                .await?
            }
            None => None,
        };
        Ok(start + budget.map_or(self.settings.default_deadline, Duration::seconds))
    }

    /// The latest final head on `network`, if it is past `deadline` (`REQ-OB-5`: decided at a final
    /// block). A move on a deadline rests on that head and records it (`REQ-TR-2`, review S7).
    async fn past(
        &self,
        conn: &mut PgConnection,
        network: &str,
        deadline: DateTime<Utc>,
    ) -> sqlx::Result<Option<Point>> {
        Ok(store::latest_head(conn, network)
            .await?
            .filter(|h| h.block_time > deadline))
    }

    /// Sends `t` from `from` to review on the deadline that `head` is past, resting on `head`
    /// (`REQ-RV-1`, `REQ-TR-2`).
    async fn deadline_review(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        from: State,
        condition: Option<Condition>,
        head: &Point,
    ) -> sqlx::Result<Did> {
        store::append(
            conn,
            t.id,
            &Move {
                from,
                to: State::NeedsReview,
                condition,
                point: Some(head),
                attempt: None,
                manual_action: None,
            },
        )
        .await?;
        Ok(Did::moved())
    }

    /// `Accepted`: the source leg final, withdrawn, or
    /// unknown past its deadline (§5.4, `INV-16`).
    async fn accepted(&self, conn: &mut PgConnection, t: &Transfer) -> sqlx::Result<Did> {
        let from = State::Accepted;
        // No request kind of V0 makes an Accepted transfer without a source binding (deposits
        // bind an observed arrival, withdrawals are made from redemptions), so Expired is never
        // chosen here (F-003 NOTES).
        let Some(source) = t.source() else {
            return Ok(Did::default());
        };
        let Some(o) = store::observation_at(conn, &source).await? else {
            return Ok(Did::default());
        };
        // INV-1, INV-2, REQ-IN-3 (review MF1): an operator top-up is a surplus contribution, never
        // a deposit's source. The store refuses the binding and Source-Final both; a transfer
        // bound before that rule existed stays where it is, and the log says why.
        if t.kind == "deposit" {
            let funding: bool = sqlx::query_scalar("SELECT funding_arrival($1, $2, $3, $4)")
                .bind(t.asset.as_deref())
                .bind(&o.network)
                .bind(o.sender.as_deref())
                .bind(o.block_number)
                .fetch_one(&mut *conn)
                .await?;
            if funding {
                tracing::error!(
                    transfer = t.id,
                    "INV-1, REQ-IN-3: the deposit's source is an operator funding arrival; never minted"
                );
                return Ok(Did::default());
            }
        }
        match (
            o.outcome.as_deref(),
            o.final_block_number,
            o.final_block_hash.as_deref(),
        ) {
            (Some(outcome), Some(n), Some(h)) => {
                let Some(point) = store::final_point(conn, &o.network, n, h).await? else {
                    return Ok(Did::default());
                };
                let to = if outcome == "final" && o.executed {
                    State::SourceFinal
                } else if outcome == "withdrawn" {
                    // AC-A2.3, REQ-IN-7: anything waiting on it stops; nothing moved.
                    State::Withdrawn
                } else {
                    return Ok(Did::default());
                };
                store::append(
                    conn,
                    t.id,
                    &Move {
                        from,
                        to,
                        condition: None,
                        point: Some(&point),
                        attempt: None,
                        manual_action: None,
                    },
                )
                .await?;
                Ok(Did::moved())
            }
            _ => {
                // Observed, neither final nor withdrawn: review past its deadline (REQ-RV-1).
                let deadline = self.deadline(conn, t, o.block_time).await?;
                if let Some(head) = self.past(conn, &o.network, deadline).await? {
                    return self
                        .deadline_review(conn, t, from, t.condition(), &head)
                        .await;
                }
                Ok(Did::default())
            }
        }
    }

    /// Takes or clears a condition in place (§5.4: any non-terminal state, itself). Taking or
    /// leaving `CND-PayoutWindow` rests on the destination network's latest final head, the clock
    /// the hold is measured on: its alert ([`Core::payout_window_alert`]) and the attempt's
    /// deadline, which restarts when the hold is left ([`Core::delivering_deadline`]; review M-1).
    async fn hold(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        c: Option<Condition>,
    ) -> sqlx::Result<Did> {
        if t.condition() == c {
            return Ok(Did::default());
        }
        let state = t.state();
        let window = [t.condition(), c].contains(&Some(Condition::PayoutWindow));
        let head = if window {
            store::latest_head(conn, &t.destination_network).await?
        } else {
            None
        };
        store::append(
            conn,
            t.id,
            &Move {
                from: state,
                to: state,
                condition: c,
                point: head.as_ref(),
                attempt: None,
                manual_action: None,
            },
        )
        .await?;
        Ok(Did::moved())
    }

    /// The delivery allowance key of the token a payout of `t` moves: the one token admitted for
    /// its asset on its destination network (`REQ-AS-2`), an EVM address there. `None` for any
    /// other delivery, or a token that is not one address (review S-2).
    async fn delivery_allowance(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
    ) -> sqlx::Result<Option<[u8; 32]>> {
        if purpose(t).0 != "payout" {
            return Ok(None);
        }
        let tokens: Vec<String> = sqlx::query_scalar(
            "SELECT token FROM admitted_tokens WHERE network = $1 AND asset = $2 ORDER BY token",
        )
        .bind(&t.destination_network)
        .bind(&t.asset)
        .fetch_all(conn)
        .await?;
        Ok(match tokens.as_slice() {
            [token] => delivery_allowance_key(token),
            _ => None,
        })
    }

    /// `CND-PayoutWindow` held longer than [`Settings::payout_window_alert_after`] pages a human
    /// (`payout-window-alert`, in the event log too), once, then once a day while it lasts (review
    /// S-3). The hold is measured on the destination network's clock, from the final head its
    /// taking rested on to the latest; it is no review and no refund (amendment 0012).
    async fn payout_window_alert(&self, conn: &mut PgConnection, t: &Transfer) -> sqlx::Result<()> {
        let held: Option<(i64, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT x.id, x.block_time FROM ( \
                 SELECT id, block_time, condition, \
                        lag(condition) OVER (ORDER BY id) AS before \
                 FROM transitions WHERE transfer_id = $1) x \
             WHERE x.condition = 'CND-PayoutWindow' \
               AND x.before IS DISTINCT FROM 'CND-PayoutWindow' \
               AND EXISTS (SELECT 1 FROM transfers WHERE id = $1 \
                           AND condition = 'CND-PayoutWindow') \
             ORDER BY x.id DESC LIMIT 1",
        )
        .bind(t.id)
        .fetch_optional(&mut *conn)
        .await?;
        let Some((hold, since)) = held else {
            return Ok(());
        };
        let Some(head) = store::latest_head(conn, &t.destination_network).await? else {
            return Ok(());
        };
        let since = since
            .or(t.source_final_block_time)
            .unwrap_or(head.block_time);
        let bound = self.settings.payout_window_alert_after;
        if head.block_time <= since + bound {
            return Ok(());
        }
        let hold = hold.to_string();
        let paged_today: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM event_log WHERE kind = 'payout-window-alert' \
               AND transfer_id = $1 AND payload ->> 'hold' = $2 \
               AND (payload ->> 'head_time')::timestamptz > $3 - interval '1 day')",
        )
        .bind(t.id)
        .bind(&hold)
        .bind(head.block_time)
        .fetch_one(&mut *conn)
        .await?;
        if paged_today {
            return Ok(());
        }
        let detail = json!({"spec": "CND-PayoutWindow", "bridge_reference": t.bridge_reference,
                            "hold": hold, "held_since": since.to_rfc3339(),
                            "network": head.network, "head_block": head.block_number,
                            "head_time": head.block_time.to_rfc3339(),
                            "bound_seconds": bound.num_seconds()});
        store::event(conn, "payout-window-alert", Some(t.id), &detail).await?;
        tracing::error!(kind = "payout-window-alert", transfer = %t.bridge_reference, %detail,
            "page: CND-PayoutWindow held past its bound; is the payout above the allowance's maximum, or the refill off?");
        Ok(())
    }

    /// `Source-Final`: the gates, then the first delivery attempt; or review when a condition that
    /// blocks delivery is past its deadline (§5.4, `REQ-IN-9`, `REQ-CV-6`).
    async fn source_final(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        now: DateTime<Utc>,
    ) -> sqlx::Result<Did> {
        if let Some(c) = t.condition().filter(|c| c.blocks_delivery()) {
            let start = t.source_final_block_time.unwrap_or(now);
            let deadline = self.deadline(conn, t, start).await?;
            if let Some(head) = self.past(conn, &t.destination_network, deadline).await? {
                return self
                    .deadline_review(conn, t, State::SourceFinal, Some(c), &head)
                    .await;
            }
            // A conversion is the requester's (REQ-CV-2, F-010): nothing is minted meanwhile.
            if c == Condition::AwaitingConversion {
                return Ok(Did::default());
            }
        }
        // REQ-IN-5, DEC-15 (defence in depth; the ingress refuses it first): a destination that is a
        // declared ledger contract account credits no one, and one that cannot be told is no
        // destination. Nothing is ordered to it, and the transfer is not left in Source-Final: an
        // unbindable destination is an operator matter, so it goes to review with the reason.
        if let Some(why) = self.destination_unbindable(conn, t).await? {
            return self.destination_review(conn, t, why).await;
        }
        if let Some(c) = self.gate(conn, t, now).await? {
            return self.hold(conn, t, Some(c)).await;
        }
        // The gate opened: its condition clears by itself (§10), then the delivery starts.
        if t.condition().is_some_and(gate_condition) {
            return self.hold(conn, t, None).await;
        }
        self.deliver(conn, t, State::SourceFinal, 1, None, None)
            .await
    }

    /// Why `t`'s destination cannot be delivered to (`REQ-IN-5`): it is on the declared list of
    /// ledger contract accounts, or it, or the list, does not parse for its network. `None` when
    /// it is clear.
    async fn destination_unbindable(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
    ) -> sqlx::Result<Option<&'static str>> {
        let format: Option<String> =
            sqlx::query_scalar("SELECT address_format FROM networks WHERE network = $1")
                .bind(&t.destination_network)
                .fetch_optional(&mut *conn)
                .await?;
        let declared: Vec<(String, Vec<u8>)> = sqlx::query_as(
            "SELECT address, account FROM ledger_accounts_listed WHERE network = $1",
        )
        .bind(&t.destination_network)
        .fetch_all(&mut *conn)
        .await?;
        Ok(match format {
            Some(f) => match bezent_core::listing(&f, &t.destination, &declared) {
                bezent_core::Listing::Clear => None,
                bezent_core::Listing::Listed => Some("destination-is-a-ledger-account"),
                bezent_core::Listing::Unknown => Some("destination-or-list-unreadable"),
            },
            None => Some("destination-or-list-unreadable"),
        })
    }

    /// Sends `t` from `Source-Final` to review for an unbindable destination, with the reason in
    /// the log (`REQ-IN-5`). No mint is ordered and none is refunded: the operator decides.
    async fn destination_review(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        why: &str,
    ) -> sqlx::Result<Did> {
        tracing::error!(transfer = %t.bridge_reference, why, "REQ-IN-5: the destination cannot be delivered to; the transfer goes to review");
        store::event(
            conn,
            "destination-unbindable",
            Some(t.id),
            &json!({"spec": "REQ-IN-5", "bridge_reference": t.bridge_reference,
                    "network": t.destination_network, "reason": why}),
        )
        .await?;
        store::append(
            conn,
            t.id,
            &Move {
                from: State::SourceFinal,
                to: State::NeedsReview,
                condition: Some(Condition::DestinationRefused),
                point: None,
                attempt: None,
                manual_action: None,
            },
        )
        .await?;
        Ok(Did::moved())
    }

    /// The conditions that hold new work before a delivery (`REQ-GV-1`, `REQ-LM-1`, `REQ-OT-5`).
    /// A refund completes work in flight: no pause or cut-off holds it (F-006 NOTES).
    async fn gate(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        now: DateTime<Utc>,
    ) -> sqlx::Result<Option<Condition>> {
        let Some((asset, network, direction)) = t.route() else {
            return Ok(None);
        };
        let paused: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pauses WHERE (asset IS NULL OR asset = $1) \
               AND (network IS NULL OR network = $2) AND (direction IS NULL OR direction = $3))",
        )
        .bind(&asset)
        .bind(&network)
        .bind(direction)
        .fetch_one(&mut *conn)
        .await?;
        // REQ-BE-6: a reversal that took the surplus below its floor pauses new mints of the
        // asset until the attesting party restores the reserve (F-004).
        if paused
            || (t.kind == "deposit" && crate::reserve::book::mints_paused(conn, &asset).await?)
        {
            return Ok(Some(Condition::Paused));
        }
        let cutoffs: Option<Value> = sqlx::query_scalar(
            "SELECT cutoffs FROM routes WHERE asset = $1 AND network = $2 AND direction = $3",
        )
        .bind(&asset)
        .bind(&network)
        .bind(direction)
        .fetch_optional(&mut *conn)
        .await?;
        if cutoffs.is_some_and(|c| cutoff::closed(&c, now)) {
            return Ok(Some(Condition::CutOff));
        }
        if t.kind == "withdrawal" {
            // One reserve check at a time per (asset, network), held until this step commits with
            // the order it lets through, so the next check counts it (review S1).
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
                .bind(format!("bezent/reserve-check/{asset}/{network}"))
                .execute(&mut *conn)
                .await?;
            if !self.reserve_covers(conn, t, &asset, &network).await? {
                self.raise_reserve_short(conn, &asset, &network).await?;
                return Ok(Some(Condition::ReserveShort));
            }
        }
        Ok(None)
    }

    /// Whether the asset's reserve on `network` covers the withdrawal: the book's reserve less what
    /// every other open withdrawal may take (`REQ-OT-5`): one signed, or one with a payout ordered
    /// or signing intent recorded and not yet signed (review S1). A withdrawal that is refunding
    /// takes nothing: each of its attempts is proven not executed (INV-8). The book is F-004's.
    async fn reserve_covers(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        asset: &str,
        network: &str,
    ) -> sqlx::Result<bool> {
        sqlx::query_scalar(
            "SELECT COALESCE((SELECT balance FROM book_balances \
                              WHERE asset = $1 AND account = 'reserve' AND network = $2), 0) \
                  - COALESCE((SELECT sum(w.amount) FROM transfers w WHERE w.kind = 'withdrawal' \
                              AND w.asset = $1 AND w.destination_network = $2 AND w.id <> $3 \
                              AND NOT is_terminal_state(w.state) AND w.state <> 'Refunding' \
                              AND (w.state IN ('Delivering', 'Delivered') \
                                   OR EXISTS (SELECT 1 FROM intents i WHERE i.transfer_id = w.id \
                                              AND i.purpose = 'payout'))), 0) \
                  >= $4::numeric",
        )
        .bind(asset)
        .bind(network)
        .bind(t.id)
        .bind(&t.amount)
        .fetch_one(conn)
        .await
    }

    /// Whether `CND-ReserveShort` is raised for (asset, network): the last raise or clear in the
    /// log.
    async fn reserve_short_raised(
        conn: &mut PgConnection,
        asset: &str,
        network: &str,
    ) -> sqlx::Result<bool> {
        let last: Option<String> = sqlx::query_scalar(
            "SELECT kind FROM event_log WHERE kind IN ('condition-raised', 'condition-cleared') \
               AND payload ->> 'condition' = 'CND-ReserveShort' \
               AND payload ->> 'asset' = $1 AND payload ->> 'network' = $2 \
             ORDER BY seq DESC LIMIT 1",
        )
        .bind(asset)
        .bind(network)
        .fetch_optional(conn)
        .await?;
        Ok(last.as_deref() == Some("condition-raised"))
    }

    /// Raises `CND-ReserveShort` once per (asset, network) to the requesters (`REQ-OT-5`,
    /// `AC-B3.1`), in the sequenced log.
    async fn raise_reserve_short(
        &self,
        conn: &mut PgConnection,
        asset: &str,
        network: &str,
    ) -> sqlx::Result<()> {
        // One raise at a time per position, across concurrent steps.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!(
                "bezent/condition/CND-ReserveShort/{asset}/{network}"
            ))
            .execute(&mut *conn)
            .await?;
        if Self::reserve_short_raised(conn, asset, network).await? {
            return Ok(());
        }
        store::event(
            conn,
            "condition-raised",
            None,
            &json!({"spec": "REQ-OT-5, AC-B3.1", "condition": "CND-ReserveShort",
                    "asset": asset, "network": network}),
        )
        .await
    }

    /// Clears every raised `CND-ReserveShort` no withdrawal holds any more (`AC-B3.2`).
    async fn clear_reserve_conditions(&self) -> sqlx::Result<()> {
        let mut tx = self.pool.begin().await?;
        let raised: Vec<(String, String)> = sqlx::query_as(
            "SELECT DISTINCT payload ->> 'asset', payload ->> 'network' FROM event_log \
             WHERE kind = 'condition-raised' AND payload ->> 'condition' = 'CND-ReserveShort'",
        )
        .fetch_all(&mut *tx)
        .await?;
        for (asset, network) in raised {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
                .bind(format!(
                    "bezent/condition/CND-ReserveShort/{asset}/{network}"
                ))
                .execute(&mut *tx)
                .await?;
            if !Self::reserve_short_raised(&mut tx, &asset, &network).await? {
                continue;
            }
            let held: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM transfers WHERE kind = 'withdrawal' AND asset = $1 \
                   AND destination_network = $2 AND condition = 'CND-ReserveShort')",
            )
            .bind(&asset)
            .bind(&network)
            .fetch_one(&mut *tx)
            .await?;
            if !held {
                store::event(
                    &mut tx,
                    "condition-cleared",
                    None,
                    &json!({"spec": "REQ-OT-5, AC-B3.2", "condition": "CND-ReserveShort",
                            "asset": asset, "network": network}),
                )
                .await?;
            }
        }
        tx.commit().await
    }

    /// `INV-6`, `INV-7`, `INV-16`, checked in the service: the transfer reached `Source-Final`
    /// with its milestone, and its source observation is final as observed, executed, at a
    /// recorded final point.
    async fn source_is_final(&self, conn: &mut PgConnection, t: &Transfer) -> sqlx::Result<bool> {
        if t.source_final_block_time.is_none() {
            return Ok(false);
        }
        let Some(source) = t.source() else {
            return Ok(false);
        };
        let Some(o) = store::observation_at(conn, &source).await? else {
            return Ok(false);
        };
        Ok(
            match (
                o.outcome.as_deref(),
                o.final_block_number,
                o.final_block_hash.as_deref(),
            ) {
                (Some("final"), Some(n), Some(h)) if o.executed => {
                    store::final_point(conn, &o.network, n, h).await?.is_some()
                }
                _ => false,
            },
        )
    }

    /// The order payload and the amount the destination leg must move, in the destination
    /// network's minor units (`REQ-TR-5`).
    ///
    /// A deposit's mint and the payout of an unassociated arrival's refund move what the bound
    /// observation carries, re-read here, never the amount a request wrote: the store refuses a
    /// transfer whose amount or asset differs from its observation's (0048), and the order is
    /// refused here too if they ever do (`REQ-IN-4`, `REQ-CV-2`, `REQ-RQ-7`; review M1c).
    async fn order(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
    ) -> sqlx::Result<Option<(Value, u128)>> {
        let Some(amount) = t.amount() else {
            return Ok(None);
        };
        let (purpose, _) = purpose(t);
        Ok(match purpose {
            "mint" => {
                // The book unit is the origin token's; the mint is in the asset's precision on the
                // issuing network. A remainder (REQ-AS-9) is surplus, the book's (F-004).
                let Some(source) = t.source() else {
                    return Ok(None);
                };
                let row: Option<(Option<String>, i16, i16, Option<String>)> = sqlx::query_as(
                    "SELECT a.issuing_asset, a.precision, k.precision, o.amount::text \
                     FROM assets a \
                     JOIN observations o ON o.network = $2 AND o.block_number = $3 \
                       AND o.block_hash = $4 AND o.position_kind = $5::position_kind \
                       AND o.position_index = $6 \
                     JOIN admitted_tokens k ON k.network = o.network AND k.token = o.token \
                       AND k.asset = a.asset \
                     WHERE a.asset = $1",
                )
                .bind(&t.asset)
                .bind(&source.network)
                .bind(source.block_number)
                .bind(&source.block_hash)
                .bind(&source.position_kind)
                .bind(source.position_index)
                .fetch_optional(&mut *conn)
                .await?;
                let Some((Some(issuing_asset), asset_precision, token_precision, observed)) = row
                else {
                    return Ok(None);
                };
                // The arrival (or the conversion's output) as observed, in the origin token's
                // minor units: what the deposit is minted against (REQ-IN-4, REQ-CV-2).
                let Some(observed) = observed.and_then(|a| a.parse::<u128>().ok()) else {
                    return Ok(None);
                };
                if observed != amount {
                    tracing::error!(transfer = %t.bridge_reference, observed, amount, "REQ-IN-4: the deposit's amount is not its arrival's; nothing is minted");
                    return Ok(None);
                }
                let (Some(ratio), Some(to)) = (
                    pow10(token_precision - asset_precision),
                    refs::account32(&t.destination),
                ) else {
                    return Ok(None);
                };
                // REQ-AS-9: rounded down; the remainder is the book's surplus.
                let minted = observed / ratio;
                Some((
                    json!({"order": {"asset": issuing_asset, "to": hex0x(&to)},
                           "amount": minted.to_string()}),
                    minted,
                ))
            }
            "refund-mint" => {
                // One refund mint per redemption, of what it destroyed (REQ-RF-4, INV-8).
                let Some(source) = t.source() else {
                    return Ok(None);
                };
                let Some(o) = store::observation_at(conn, &source).await? else {
                    return Ok(None);
                };
                let (Some(redeemed), Some(redeemer)) = (o.amount(), t.redeemer.as_deref()) else {
                    return Ok(None);
                };
                // The asset it re-mints, by its issuing identity: the redemption's (the signer
                // keeps its window cap per asset, issue #23; the store checks it, 0049).
                let Some(asset) = o.token.as_deref() else {
                    return Ok(None);
                };
                Some((
                    json!({"order": {"redeemer": redeemer, "asset": asset},
                           "amount": redeemed.to_string()}),
                    redeemed,
                ))
            }
            _ => {
                let amount = if t.kind == "refund" && t.refund_of.is_none() {
                    match self.unassociated_amount(conn, t).await? {
                        Some(observed) => observed,
                        None => return Ok(None),
                    }
                } else {
                    amount
                };
                Some((
                    json!({"order": {"to": t.destination, "asset": t.asset, "token": t.token},
                           "amount": amount.to_string()}),
                    amount,
                ))
            }
        })
    }

    /// The amount the refund of an unassociated arrival pays out: the arrival's, as observed,
    /// when the refund names its token or the token's asset, and the arrival's amount
    /// (`REQ-IN-6`, `REQ-RQ-7`; review M1c). `None` otherwise: nothing is ordered.
    async fn unassociated_amount(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
    ) -> sqlx::Result<Option<u128>> {
        let Some(source) = t.source() else {
            return Ok(None);
        };
        let Some(o) = store::observation_at(conn, &source).await? else {
            return Ok(None);
        };
        let (Some(observed), Some(token)) = (o.amount(), o.token.as_deref()) else {
            return Ok(None);
        };
        let family: Option<String> = sqlx::query_scalar(
            "SELECT asset FROM admitted_tokens WHERE network = $1 AND token = $2",
        )
        .bind(&o.network)
        .bind(token)
        .fetch_optional(&mut *conn)
        .await?;
        let same_family = match (t.asset.as_deref(), t.token.as_deref()) {
            (Some(asset), None) => family.as_deref() == Some(asset),
            (None, Some(named)) => named == token,
            _ => false,
        };
        if !same_family || t.amount() != Some(observed) {
            tracing::error!(transfer = %t.bridge_reference, observed, "REQ-IN-4, REQ-RQ-7: the refund's amount or token is not its arrival's; nothing is paid out");
            return Ok(None);
        }
        Ok(Some(observed))
    }

    /// The deposit's mint reference, or the payout reference of the refund of an unassociated
    /// arrival, derived from the source coordinate and written once (`REQ-IN-4`, `INV-2`). A
    /// withdrawal and its refund carry the redemption's from their creation.
    async fn reference(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
    ) -> sqlx::Result<Option<Vec<u8>>> {
        if let Some(r) = &t.on_chain_reference {
            return Ok(Some(r.clone()));
        }
        let Some(source) = t.source() else {
            return Ok(None);
        };
        let reference = match (t.kind.as_str(), t.refund_of) {
            ("deposit", _) => refs::mint_reference(&source).to_vec(),
            ("refund", None) => refs::unassociated_refund_reference(&source).to_vec(),
            _ => return Ok(None),
        };
        sqlx::query("UPDATE transfers SET on_chain_reference = $2 WHERE id = $1")
            .bind(t.id)
            .bind(&reference)
            .execute(conn)
            .await?;
        Ok(Some(reference))
    }

    /// The reference the delivery of `t` writes on its destination network, with its scope: a
    /// withdrawal's payout writes its own destination reference, unique to it and unscoped
    /// (review M1, PLAN DEC-10); every other delivery writes the transfer's on-chain reference, in
    /// its redeemer's scope when it has one (amendment 0008).
    async fn delivery_reference(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
    ) -> sqlx::Result<Option<(Vec<u8>, Option<String>)>> {
        if t.kind == "withdrawal" {
            return Ok(t.destination_reference.clone().map(|r| (r, None)));
        }
        Ok(self
            .reference(conn, t)
            .await?
            .map(|r| (r, t.redeemer.clone())))
    }

    /// The reference the destination leg of `t` is observed with: the one its delivery writes.
    fn leg_reference(t: &Transfer) -> Option<&[u8]> {
        if t.kind == "withdrawal" {
            t.destination_reference.as_deref()
        } else {
            t.on_chain_reference.as_deref()
        }
    }

    /// Delivery attempt `attempt` of `t`, out of `from`, in the two steps of the module
    /// documentation. `point` is the edge's coordinate (a new attempt rests on the previous one's
    /// proof; a review resolution on its evidence), `manual` the review's recorded action.
    #[allow(clippy::too_many_arguments)]
    async fn deliver(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        from: State,
        attempt: i32,
        point: Option<&Point>,
        manual: Option<i64>,
    ) -> sqlx::Result<Did> {
        if !self.source_is_final(conn, t).await? {
            tracing::error!(transfer = %t.bridge_reference, "INV-6, INV-7, INV-16: the source leg is not final as observed; nothing is delivered");
            return Ok(Did::default());
        }
        let Some((reference, scope)) = self.delivery_reference(conn, t).await? else {
            tracing::error!(transfer = %t.bridge_reference, "no destination reference; nothing is delivered (REQ-OT-2)");
            return Ok(Did::default());
        };
        let scope = scope.as_deref();
        let (purpose, _) = purpose(t);
        let Some((order, amount)) = self.order(conn, t).await? else {
            tracing::warn!(transfer = %t.bridge_reference, "no order can be made: a declaration is missing");
            return Ok(Did::default());
        };

        // 1. The order.
        let latest_order: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM intents WHERE transfer_id = $1 AND attempt = $2 AND purpose = $3 \
               AND payload ? 'order' ORDER BY id DESC LIMIT 1",
        )
        .bind(t.id)
        .bind(attempt)
        .bind(purpose)
        .fetch_optional(&mut *conn)
        .await?;
        let Some(order_id) = latest_order else {
            self.record_intent(conn, t, purpose, &reference, scope, attempt, &order)
                .await?;
            return Ok(Did::intent());
        };

        // 2. The endpoint's preparation (F-007's `prepared_transactions`).
        // Whether the retry delay has passed is read on the store's clock, the one that stamped
        // the preparation.
        // PR #39 second review MF1 (INV-9): only a preparation on the order's own network, the
        // transfer's destination, is read; the store refuses any other (0036), and the networks
        // are compared again below before a signing intent is recorded.
        type Prepared = (i64, i64, Vec<u8>, bool, Value, bool, String, String);
        let prepared: Option<Prepared> = sqlx::query_as(
            "SELECT p.id, p.order_intent_id, p.message, p.dry_run_passed, p.dry_run, \
                    p.recorded_at <= now() - $4::bigint * interval '1 second', p.network, o.network \
             FROM prepared_transactions p JOIN intents o ON o.id = p.order_intent_id \
             WHERE o.transfer_id = $1 AND o.attempt = $2 AND o.purpose = $3 \
               AND p.network = o.network AND o.network = $5 \
             ORDER BY p.id DESC LIMIT 1",
        )
        .bind(t.id)
        .bind(attempt)
        .bind(purpose)
        .bind(self.settings.retry_preparation_after.num_seconds())
        .bind(&t.destination_network)
        .fetch_optional(&mut *conn)
        .await?;
        let Some((
            prepared_id,
            prepared_order,
            message,
            passed,
            dry_run,
            retry_due,
            prepared_network,
            order_network,
        )) = prepared
        else {
            return Ok(Did::default());
        };
        if prepared_network != order_network || order_network != t.destination_network {
            tracing::error!(transfer = %t.bridge_reference, prepared = prepared_id, %prepared_network, %order_network,
                "INV-9: a preparation on another network than its order's; no signing intent is recorded");
            return Ok(Did::default());
        }
        if !passed {
            if prepared_order != order_id {
                // A new order waits for its preparation.
                return Ok(Did::default());
            }
            // A refusal that is not the allowance's does not keep a payout window hold (S-2).
            let allowance = self.delivery_allowance(conn, t).await?;
            let held = dry_run_condition(&dry_run, allowance.as_ref())
                .or(t.condition().filter(|c| *c != Condition::PayoutWindow));
            let mut did = self.hold(conn, t, held).await?;
            // A failed dry run is not prepared again: a new order, after a while (F-007 NOTES 5).
            if retry_due {
                self.record_intent(conn, t, purpose, &reference, scope, attempt, &order)
                    .await?;
                did.intents += 1;
            }
            return Ok(did);
        }

        // The dry run passed: the allowance refilled, and a payout window hold is left here,
        // resting on the destination's final head; the attempt's clock restarts there (review M-1).
        let mut did = if t.condition() == Some(Condition::PayoutWindow) {
            tracing::info!(transfer = %t.bridge_reference, "CND-PayoutWindow: the allowance refilled; the payout resumes");
            self.hold(conn, t, None).await?
        } else {
            Did::default()
        };

        // 3. The signing intent over the exact bytes the endpoint prepared.
        let signing: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM intents WHERE transfer_id = $1 AND attempt = $2 \
               AND payload ->> 'prepared' = $3::text ORDER BY id LIMIT 1",
        )
        .bind(t.id)
        .bind(attempt)
        .bind(prepared_id)
        .fetch_optional(&mut *conn)
        .await?;
        let signed_any: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM intents i JOIN signatures s ON s.intent_id = i.id \
               WHERE i.transfer_id = $1 AND i.attempt = $2)",
        )
        .bind(t.id)
        .bind(attempt)
        .fetch_one(&mut *conn)
        .await?;
        if signing.is_none() && !signed_any {
            let payload = json!({
                "message": hex0x(&message),
                "amount": amount.to_string(),
                "prepared": prepared_id,
            });
            self.record_intent(conn, t, purpose, &reference, scope, attempt, &payload)
                .await?;
            did.intents += 1;
            return Ok(did);
        }

        // 4. Verify every signature recorded for this attempt (AC-I1.2, INV-9).
        type Sig = (i64, String, String, Vec<u8>, Vec<u8>, bool);
        let signatures: Vec<Sig> = sqlx::query_as(
            "SELECT s.intent_id, s.key_id, s.scheme, s.public_key, s.signature, \
                    EXISTS (SELECT 1 FROM signed_transactions st WHERE st.intent_id = s.intent_id) \
             FROM signatures s JOIN intents i ON i.id = s.intent_id \
             WHERE i.transfer_id = $1 AND i.attempt = $2 ORDER BY s.id",
        )
        .bind(t.id)
        .bind(attempt)
        .fetch_all(&mut *conn)
        .await?;
        let mut ready = false;
        for (intent, key, scheme, public_key, signature, recorded) in signatures {
            if !self
                .verified(conn, intent, &key, &scheme, &public_key, &signature)
                .await?
            {
                return Ok(did);
            }
            ready |= recorded;
        }
        if !ready {
            return Ok(did);
        }
        store::append(
            conn,
            t.id,
            &Move {
                from,
                to: State::Delivering,
                condition: None,
                point,
                attempt: Some(attempt),
                manual_action: manual,
            },
        )
        .await?;
        Ok(Did::moved())
    }

    /// Whether a recorded signature is exactly the named key's over its intent's recorded bytes.
    /// One that is not raises `key-compromise` and the pause request once (`AC-I1.2`,
    /// `REQ-IC-4`), through the compromise path of F-005, and is never used.
    async fn verified(
        &self,
        conn: &mut PgConnection,
        intent: i64,
        key: &str,
        scheme: &str,
        public_key: &[u8],
        signature: &[u8],
    ) -> sqlx::Result<bool> {
        // The core's own alert for this intent, by its detection key (0049): a row another role
        // wrote never stands in for it.
        if compromise::raised(
            &mut *conn,
            compromise::DETECTOR_CORE,
            &format!("intent:{intent}"),
        )
        .await?
        {
            return Ok(false);
        }
        let answer = Answer {
            intent,
            key,
            scheme,
            public_key: &hex0x(public_key),
            signature: &hex0x(signature),
        };
        Ok(
            match compromise::returned(&self.pool, intent, answer).await? {
                Returned::Verified => true,
                Returned::Compromise { alert_seq, .. } => {
                    tracing::error!(
                        intent,
                        alert_seq,
                        "AC-I1.2: a signature that is not over the intent's bytes; key compromise raised"
                    );
                    false
                }
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn record_intent(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        purpose: &str,
        reference: &[u8],
        scope: Option<&str>,
        attempt: i32,
        payload: &Value,
    ) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO intents (transfer_id, network, purpose, on_chain_reference, \
                 reference_scope, attempt, payload) \
             VALUES ($1, $2, $3, $4, $5, $6, $7::text::jsonb)",
        )
        .bind(t.id)
        .bind(&t.destination_network)
        .bind(purpose)
        .bind(reference)
        .bind(scope)
        .bind(attempt)
        .bind(payload.to_string())
        .execute(conn)
        .await
        .map(|_| ())
    }

    /// Whether observation `o` is the destination leg of `t`: on its destination network, with the
    /// reference its delivery writes and the event its purpose is observed as; a refund mint's, to
    /// its redeemer, whose scope the redemption's reference is in (amendment 0008). Never by amount
    /// or time (`REQ-OT-2`).
    fn is_leg_of(t: &Transfer, o: &Observation) -> bool {
        let (_, event) = purpose(t);
        let reference = Self::leg_reference(t);
        o.network == t.destination_network
            && reference.is_some()
            && o.on_chain_reference.as_deref() == reference
            && o.detail.get("event").and_then(Value::as_str) == Some(event)
            && (event != "RefundMinted"
                || matches!((o.recipient.as_deref(), t.redeemer.as_deref()),
                            (Some(r), Some(w)) if refs::same_account(r, w)))
    }

    /// The executed destination leg of `t`, matched by the reference its delivery writes
    /// (`REQ-ID-2`, `REQ-OT-2`). Exactly one leg, or none: two observations of one reference are
    /// ambiguous, and an ambiguous leg is unknown, never a pick (review M1).
    async fn destination_leg(&self, conn: &mut PgConnection, t: &Transfer) -> sqlx::Result<Leg> {
        let Some(reference) = Self::leg_reference(t) else {
            return Ok(Leg::None);
        };
        let (_, event) = purpose(t);
        let found: Vec<Observation> = sqlx::query_as(&format!(
            "SELECT {} FROM observations o \
             LEFT JOIN observation_outcomes x ON x.observation_id = o.id \
             WHERE o.network = $1 AND o.on_chain_reference = $2 AND o.executed \
               AND o.detail ->> 'event' = $3 \
               AND (x.outcome IS DISTINCT FROM 'withdrawn') \
             ORDER BY o.id",
            store::OBSERVATION
        ))
        .bind(&t.destination_network)
        .bind(reference)
        .bind(event)
        .fetch_all(conn)
        .await?;
        let mut legs: Vec<Observation> = found
            .into_iter()
            .filter(|o| Self::is_leg_of(t, o))
            .collect();
        Ok(match legs.len() {
            0 => Leg::None,
            1 => Leg::One(Box::new(legs.remove(0))),
            n => {
                tracing::error!(
                    transfer = %t.bridge_reference,
                    legs = n,
                    "REQ-OT-2: more than one leg carries this transfer's reference; it is unknown"
                );
                Leg::Ambiguous
            }
        })
    }

    /// Whether the leg `o` moved what `t` orders, to where (`REQ-TR-3`, `REQ-TR-5`): executed, the
    /// full amount, to the destination (review S3). `None` when it did.
    async fn leg_mismatch(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        o: &Observation,
    ) -> sqlx::Result<Option<Mismatch>> {
        let expected = self.order(conn, t).await?.map(|(_, a)| a);
        let recipient = match purpose(t).0 {
            "refund-mint" => t.redeemer.clone(),
            _ => Some(t.destination.clone()),
        };
        let amount_ok = o.executed && expected.is_some() && o.amount() == expected;
        let recipient_ok = matches!((o.recipient.as_deref(), recipient.as_deref()),
                                    (Some(r), Some(w)) if refs::same_account(r, w));
        Ok((!amount_ok || !recipient_ok).then_some(Mismatch {
            expected,
            recipient,
            amount_ok,
        }))
    }

    /// `Delivering`: included, proven not executed, or unknown past its deadline.
    async fn delivering(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        now: DateTime<Utc>,
    ) -> sqlx::Result<Did> {
        let attempt = store::current_attempt(conn, t.id).await?;
        match self.destination_leg(conn, t).await? {
            Leg::One(o) => {
                if let Some(m) = self.leg_mismatch(conn, t, &o).await? {
                    return self.mismatch(conn, t, State::Delivering, &o, &m).await;
                }
                let point = o.point();
                store::append(
                    conn,
                    t.id,
                    &Move {
                        from: State::Delivering,
                        to: State::Delivered,
                        condition: None,
                        point: Some(&point),
                        attempt: None,
                        manual_action: None,
                    },
                )
                .await?;
                return Ok(Did::moved());
            }
            // An ambiguous leg is unknown: no proof is read against it (REQ-NX-3). Something
            // carrying the reference executed, so nothing is forced out (REQ-NX-4: "not
            // included"): review at the deadline.
            Leg::Ambiguous => {
                return self.delivering_deadline(conn, t, attempt, false, now).await;
            }
            Leg::None => {}
        }
        match store::proof_of(conn, t.id, attempt).await? {
            Some(ProofPoint::Final(proof)) => {
                return self
                    .after_proof(conn, t, State::Delivering, attempt, &proof, None, now)
                    .await;
            }
            Some(ProofPoint::Untimed) => return Ok(Did::default()),
            None => {}
        }
        self.delivering_deadline(conn, t, attempt, true, now).await
    }

    /// `Delivering`, unknown: review past its deadline, never a refund or a retry (`REQ-RV-1`,
    /// `AC-B1.4`). The attempt's clock is the chain's: it starts at the final point it rests on
    /// (the previous attempt's proof), or at the source leg's finality for the first; and it
    /// restarts at the final head where the transfer last left `CND-PayoutWindow`, so a payout
    /// released after its original deadline is neither reviewed nor cancelled for the time it
    /// waited (amendment 0012; review M-1). On a network
    /// where transactions never expire, the cancel of an attempt of which nothing was observed
    /// (`forcible`) is ordered at the same moment ([`Core::order_cancels`], `REQ-NX-4`).
    async fn delivering_deadline(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        attempt: i32,
        forcible: bool,
        now: DateTime<Utc>,
    ) -> sqlx::Result<Did> {
        // The later of the attempt's own point and the last time the transfer left
        // CND-PayoutWindow: time held waiting for the allowance never counts (review M-1). Every
        // such point is on the destination network, whose clock only moves forward.
        let started: Option<DateTime<Utc>> = sqlx::query_scalar(
            "SELECT max(x.block_time) FROM ( \
                 SELECT block_time, attempt, condition, \
                        lag(condition) OVER (ORDER BY id) AS before \
                 FROM transitions WHERE transfer_id = $1) x \
             WHERE x.attempt = $2 \
                OR (x.before = 'CND-PayoutWindow' \
                    AND x.condition IS DISTINCT FROM 'CND-PayoutWindow')",
        )
        .bind(t.id)
        .bind(attempt)
        .fetch_one(&mut *conn)
        .await?;
        let started = started.or(t.source_final_block_time).unwrap_or(now);
        let deadline = self.deadline(conn, t, started).await?;
        let Some(head) = self.past(conn, &t.destination_network, deadline).await? else {
            return Ok(Did::default());
        };
        let mut did = self
            .deadline_review(conn, t, State::Delivering, t.condition(), &head)
            .await?;
        if forcible && self.never_expires(conn, &t.destination_network).await? {
            did.intents += self.order_cancels(conn, t, attempt).await?;
        }
        Ok(did)
    }

    /// Whether a transaction on `network` never expires, so that its non-execution is proven only
    /// by consuming its nonce with another (`REQ-NX-1`, `REQ-NX-4`): an origin network. The
    /// issuing network's transactions expire with their era, and that expiry is the proof.
    async fn never_expires(&self, conn: &mut PgConnection, network: &str) -> sqlx::Result<bool> {
        Ok(
            sqlx::query_scalar::<_, String>("SELECT role FROM networks WHERE network = $1")
                .bind(network)
                .fetch_optional(conn)
                .await?
                .is_some_and(|role| role == "origin"),
        )
    }

    /// `REQ-NX-4`, at the deadline that just sent `t` to review: the order of a cancel of each of
    /// attempt `attempt`'s signed transactions with no proof, on a network where they never
    /// expire; returns how many it recorded. The review records which transactions are being
    /// forced out (`review-cancel-ordered`); [`Core::review`] drives each cancel's later steps. A
    /// signature whose bytes no endpoint recorded has nothing to force out: the review holds alone.
    async fn order_cancels(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        attempt: i32,
    ) -> sqlx::Result<usize> {
        let targets: Vec<i64> = sqlx::query_scalar(
            "SELECT st.id FROM signed_transactions st JOIN intents i ON i.id = st.intent_id \
             WHERE i.transfer_id = $1 AND i.attempt = $2 AND i.network = $3 \
               AND NOT EXISTS (SELECT 1 FROM non_execution_proofs n \
                               WHERE n.signed_transaction_id = st.id) \
             ORDER BY st.id",
        )
        .bind(t.id)
        .bind(attempt)
        .bind(&t.destination_network)
        .fetch_all(&mut *conn)
        .await?;
        if targets.is_empty() {
            return Ok(0);
        }
        let mut recorded = 0;
        for signed in &targets {
            recorded += self.cancel(conn, t, *signed, true).await?;
        }
        store::event(
            conn,
            "review-cancel-ordered",
            Some(t.id),
            &json!({"spec": "REQ-NX-4", "bridge_reference": t.bridge_reference,
                    "attempt": attempt.to_string(), "signed_transactions": targets}),
        )
        .await?;
        tracing::info!(transfer = %t.bridge_reference, attempt, "REQ-RV-1, REQ-NX-4: unknown at its deadline; review, and a cancel is ordered");
        Ok(recorded)
    }

    /// `Needs-Review` from `Delivering` or `Delivered` (`REQ-RV-2`): the later steps of every
    /// cancel ordered at the deadline whose payout is still unproven, and the attempt's proof of
    /// non-execution, once final, attached to review `review` as its evidence (`review-evidence`,
    /// once per review): the cancel's (`nonce-consumed-by-other`) or, on the issuing network, the
    /// era's expiry. Nothing here moves the transfer; the review ends by its resolution.
    async fn review_evidence(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        review: i64,
    ) -> sqlx::Result<Did> {
        let attempt = store::current_attempt(conn, t.id).await?;
        let mut did = Did::default();
        let cancelling: Vec<i64> = sqlx::query_scalar(
            "SELECT st.id FROM signed_transactions st JOIN intents i ON i.id = st.intent_id \
             WHERE i.transfer_id = $1 AND i.attempt = $2 AND i.network = $3 \
               AND NOT EXISTS (SELECT 1 FROM non_execution_proofs n \
                               WHERE n.signed_transaction_id = st.id) \
               AND EXISTS (SELECT 1 FROM intents c \
                           WHERE c.network = $3 AND c.purpose = 'cancel' \
                             AND c.transfer_id IS NULL AND c.payload ? 'order' \
                             AND c.payload -> 'order' ->> 'cancel' = st.id::text) \
             ORDER BY st.id",
        )
        .bind(t.id)
        .bind(attempt)
        .bind(&t.destination_network)
        .fetch_all(&mut *conn)
        .await?;
        // Review S-4: once the payout itself is observed, the race with its cancel is decided (one
        // of the two consumes the nonce; the endpoint records the other as not executed). Its
        // cancel is no longer driven: no new order and no signing, only the signatures already
        // returned are verified (AC-I1.2).
        let leg = self.destination_leg(conn, t).await?;
        let drive = matches!(leg, Leg::None);
        for signed in &cancelling {
            did.intents += self.cancel(conn, t, *signed, drive).await?;
        }
        let review = review.to_string();
        match leg {
            Leg::None => {}
            // Something carrying the reference executed: unknown, no evidence (REQ-NX-3).
            Leg::Ambiguous => return Ok(did),
            Leg::One(o) => {
                // The payout was included: the evidence for the review is its inclusion, at the
                // leg's own coordinate, and the review resolves as Delivered (REQ-RV-2), never as
                // a refund. Attached once, on a review that ordered a cancel, for a leg that moved
                // what was ordered (a mismatch is no delivery).
                if !cancelling.is_empty()
                    && self.leg_mismatch(conn, t, &o).await?.is_none()
                    && !store::has_event(conn, "review-evidence", Some(t.id), "review", &review)
                        .await?
                {
                    let p = o.point();
                    store::event(
                        conn,
                        "review-evidence",
                        Some(t.id),
                        &json!({"spec": "REQ-RV-2", "bridge_reference": t.bridge_reference,
                                "review": review, "attempt": attempt.to_string(),
                                "kind": "included", "resolves_to": State::Delivered.as_str(),
                                "network": p.network, "block_number": p.block_number,
                                "block_hash": hex0x(&p.block_hash),
                                "block_time": p.block_time.to_rfc3339()}),
                    )
                    .await?;
                    tracing::info!(transfer = %t.bridge_reference, attempt, "REQ-RV-2: the payout was included; its cancel is not driven, and the inclusion is attached to the review");
                }
                return Ok(did);
            }
        }
        let Some(ProofPoint::Final(proof)) = store::proof_of(conn, t.id, attempt).await? else {
            return Ok(did);
        };
        if store::has_event(conn, "review-evidence", Some(t.id), "review", &review).await? {
            return Ok(did);
        }
        let kinds: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT n.kind::text FROM non_execution_proofs n \
             JOIN signed_transactions st ON st.id = n.signed_transaction_id \
             JOIN intents i ON i.id = st.intent_id \
             WHERE i.transfer_id = $1 AND i.attempt = $2 ORDER BY 1",
        )
        .bind(t.id)
        .bind(attempt)
        .fetch_all(&mut *conn)
        .await?;
        store::event(
            conn,
            "review-evidence",
            Some(t.id),
            &json!({"spec": "REQ-RV-2", "bridge_reference": t.bridge_reference,
                    "review": review, "attempt": attempt.to_string(), "kind": "not-executed",
                    "proof": kinds,
                    "network": proof.network, "block_number": proof.block_number,
                    "block_hash": hex0x(&proof.block_hash),
                    "block_time": proof.block_time.to_rfc3339()}),
        )
        .await?;
        tracing::info!(transfer = %t.bridge_reference, attempt, "REQ-RV-2: the proof of non-execution is attached to the review");
        Ok(did)
    }

    /// The cancel of signed transaction `signed`, a delivery of `t`, in the two steps of PLAN
    /// `DEC-3`; returns how many intents it recorded. One signing intent per signed transaction:
    /// a cancel that is signed and still unknown is not replaced, and the review stays open. A
    /// failed dry run gets a new order after [`Settings::retry_preparation_after`]; the transfer
    /// takes no condition from it. A preparation the endpoint refuses records nothing, and the
    /// endpoint tries the same order again. When `drive` is false (the payout itself was observed,
    /// review S-4) nothing new is recorded: no order and no signing intent; the signatures already
    /// returned are still verified.
    async fn cancel(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        signed: i64,
        drive: bool,
    ) -> sqlx::Result<usize> {
        let network = t.destination_network.as_str();
        let order = json!({"order": {"cancel": signed}, "amount": "0"});

        // 1. The order.
        let latest_order: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM intents WHERE network = $1 AND purpose = 'cancel' \
               AND transfer_id IS NULL AND payload ? 'order' \
               AND payload -> 'order' ->> 'cancel' = $2::text \
             ORDER BY id DESC LIMIT 1",
        )
        .bind(network)
        .bind(signed)
        .fetch_optional(&mut *conn)
        .await?;
        let Some(order_id) = latest_order else {
            if !drive {
                return Ok(0);
            }
            self.record_cancel(conn, network, &order).await?;
            tracing::info!(transfer = %t.bridge_reference, signed, "REQ-NX-4: not included past its deadline; a cancel is ordered");
            return Ok(1);
        };

        // 2. The endpoint's preparation, on the order's own network (INV-9). Whether the retry
        // delay has passed is read on the store's clock, the one that stamped the preparation.
        type Prepared = (i64, i64, Vec<u8>, bool, bool);
        let prepared: Option<Prepared> = sqlx::query_as(
            "SELECT p.id, p.order_intent_id, p.message, p.dry_run_passed, \
                    p.recorded_at <= now() - $3::bigint * interval '1 second' \
             FROM prepared_transactions p JOIN intents o ON o.id = p.order_intent_id \
             WHERE o.network = $1 AND p.network = o.network AND o.purpose = 'cancel' \
               AND o.transfer_id IS NULL AND o.payload -> 'order' ->> 'cancel' = $2::text \
             ORDER BY p.id DESC LIMIT 1",
        )
        .bind(network)
        .bind(signed)
        .bind(self.settings.retry_preparation_after.num_seconds())
        .fetch_optional(&mut *conn)
        .await?;
        let Some((prepared_id, prepared_order, message, passed, retry_due)) = prepared else {
            return Ok(0);
        };
        if !passed {
            // A new order waits for its preparation; a failed one is ordered again, after a while.
            if prepared_order != order_id || !retry_due || !drive {
                return Ok(0);
            }
            self.record_cancel(conn, network, &order).await?;
            return Ok(1);
        }

        // 3. The signing intent over the exact bytes prepared, once per signed transaction.
        let signing: Vec<i64> = sqlx::query_scalar(
            "SELECT si.id FROM intents si \
             JOIN prepared_transactions p ON p.id::text = si.payload ->> 'prepared' \
             JOIN intents o ON o.id = p.order_intent_id \
             WHERE si.network = $1 AND si.purpose = 'cancel' AND si.transfer_id IS NULL \
               AND o.payload -> 'order' ->> 'cancel' = $2::text \
             ORDER BY si.id",
        )
        .bind(network)
        .bind(signed)
        .fetch_all(&mut *conn)
        .await?;
        if signing.is_empty() {
            if !drive {
                return Ok(0);
            }
            let payload = json!({
                "message": hex0x(&message),
                "amount": "0",
                "prepared": prepared_id,
            });
            self.record_cancel(conn, network, &payload).await?;
            return Ok(1);
        }

        // 4. Verify every signature returned for it (AC-I1.2, INV-9): one that does not verify
        // raises the compromise path and the pause request, once.
        type Sig = (i64, String, String, Vec<u8>, Vec<u8>);
        let signatures: Vec<Sig> = sqlx::query_as(
            "SELECT intent_id, key_id, scheme, public_key, signature FROM signatures \
             WHERE intent_id = ANY($1) ORDER BY id",
        )
        .bind(&signing)
        .fetch_all(&mut *conn)
        .await?;
        for (intent, key, scheme, public_key, signature) in signatures {
            if !self
                .verified(conn, intent, &key, &scheme, &public_key, &signature)
                .await?
            {
                tracing::error!(transfer = %t.bridge_reference, intent, "AC-I1.2: a cancel's signature does not verify");
            }
        }
        Ok(0)
    }

    /// Records a cancel's order or signing intent (`REQ-NX-4`): purpose `cancel` on `network`,
    /// with no transfer, no reference and attempt 1. It moves nothing, so it is no attempt of the
    /// transfer's and no proof is read for it; its order names the signed transaction it forces
    /// out, and the EVM endpoint and the signer bound the rest (F-009, F-005).
    async fn record_cancel(
        &self,
        conn: &mut PgConnection,
        network: &str,
        payload: &Value,
    ) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO intents (network, purpose, payload) \
             VALUES ($1, 'cancel', $2::text::jsonb)",
        )
        .bind(network)
        .bind(payload.to_string())
        .execute(conn)
        .await
        .map(|_| ())
    }

    /// A delivery of another amount, or to another recipient: review, reporting both
    /// (`REQ-TR-5`, `REQ-TR-3`). A wrong amount holds `CND-AmountMismatch`.
    async fn mismatch(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        from: State,
        o: &Observation,
        m: &Mismatch,
    ) -> sqlx::Result<Did> {
        let (kind, spec, condition) = if m.amount_ok {
            ("recipient-mismatch", "REQ-TR-3", None)
        } else {
            (
                "amount-mismatch",
                "REQ-TR-5, CND-AmountMismatch",
                Some(Condition::AmountMismatch),
            )
        };
        store::event(
            conn,
            kind,
            Some(t.id),
            &json!({"spec": spec, "bridge_reference": t.bridge_reference,
                    "expected": m.expected.map(|a| a.to_string()), "delivered": o.amount,
                    "expected_recipient": m.recipient, "delivered_recipient": o.recipient,
                    "observation_id": o.id}),
        )
        .await?;
        let point = o.point();
        store::append(
            conn,
            t.id,
            &Move {
                from,
                to: State::NeedsReview,
                condition,
                point: Some(&point),
                attempt: None,
                manual_action: None,
            },
        )
        .await?;
        Ok(Did::moved())
    }

    /// After attempt `attempt` is proven not executed at finality (`REQ-RF-1`): a new attempt, or,
    /// for a withdrawal past its deadline, the refund mint (`REQ-RF-5`, `REQ-OT-6`, `AC-B1.3`).
    ///
    /// The choice is made once (review M2): the order of attempt n+1 records it. Once any intent
    /// of attempt n+1 exists, it is a new attempt whatever the route declares by then; the refund
    /// is chosen only while no later attempt was ever ordered, and the store refuses it otherwise.
    #[allow(clippy::too_many_arguments)]
    async fn after_proof(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        from: State,
        attempt: i32,
        proof: &Point,
        manual: Option<i64>,
        now: DateTime<Utc>,
    ) -> sqlx::Result<Did> {
        let retrying = store::attempt_has_intents(conn, t.id, attempt + 1).await?;
        if t.kind == "withdrawal" && !retrying {
            let start = t.source_final_block_time.unwrap_or(proof.block_time);
            let deadline = self.deadline(conn, t, start).await?;
            if proof.block_time > deadline {
                return self.refund(conn, t, from, proof, manual).await;
            }
        }
        // A new attempt is new work for its route: the gates hold it as they hold the first.
        if let Some(c) = self.gate(conn, t, now).await? {
            return self.hold(conn, t, Some(c)).await;
        }
        if t.condition().is_some_and(gate_condition) {
            return self.hold(conn, t, None).await;
        }
        self.deliver(conn, t, from, attempt + 1, Some(proof), manual)
            .await
    }

    /// Makes the refund mint of a withdrawal and starts it (`REQ-RF-3`, `REQ-RF-4`, `INV-8`: one
    /// per redemption; `refund_of` is unique in the store and the contract refunds a redemption
    /// once). Only when every attempt ordered, prepared or signed is proven not executed (review
    /// M2); the store checks it again, and closes the attempt set with the refund.
    async fn refund(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        from: State,
        proof: &Point,
        manual: Option<i64>,
    ) -> sqlx::Result<Did> {
        let unproven = store::attempts_unproven(conn, t.id).await?;
        if !unproven.is_empty() {
            tracing::error!(transfer = %t.bridge_reference, attempts = ?unproven, "INV-8: an attempt is not proven not executed; no refund");
            return Ok(Did::default());
        }
        let issuing: Option<String> =
            sqlx::query_scalar("SELECT network FROM networks WHERE role = 'issuing'")
                .fetch_optional(&mut *conn)
                .await?;
        let (Some(issuing), Some(redeemer), Some(source)) =
            (issuing, t.redeemer.as_deref(), t.source())
        else {
            return Ok(Did::default());
        };
        sqlx::query(
            "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                 destination_network, destination, refund_of, source_network, source_block_number, \
                 source_block_hash, source_position_kind, source_position_index, \
                 on_chain_reference, redeemer, state) \
             VALUES ($1, $2, 'refund', $3, $4::numeric, $5, $6, $7, $8, $9, $10, $11::position_kind, \
                 $12, $13, $14, 'Received') \
             ON CONFLICT (bridge_reference) DO NOTHING",
        )
        .bind(refs::refund(&t.bridge_reference))
        .bind(format!("{}:refund", t.request_reference))
        .bind(&t.asset)
        .bind(&t.amount)
        .bind(&issuing)
        .bind(redeemer)
        .bind(t.id)
        .bind(&source.network)
        .bind(source.block_number)
        .bind(&source.block_hash)
        .bind(&source.position_kind)
        .bind(source.position_index)
        .bind(&t.on_chain_reference)
        .bind(redeemer)
        .execute(&mut *conn)
        .await?;
        store::append(
            conn,
            t.id,
            &Move {
                from,
                to: State::Refunding,
                condition: None,
                point: Some(proof),
                attempt: None,
                manual_action: manual,
            },
        )
        .await?;
        Ok(Did::moved())
    }

    /// `Delivered`: final and executed in full, withdrawn, proven not executed, or unknown past
    /// its deadline.
    async fn delivered(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        now: DateTime<Utc>,
    ) -> sqlx::Result<Did> {
        let attempt = store::current_attempt(conn, t.id).await?;
        // The leg the inclusion rested on, read back by its own coordinate from the transition
        // that recorded it (review S4), and still this transfer's leg.
        type Included = (
            DateTime<Utc>,
            String,
            i64,
            Vec<u8>,
            Option<String>,
            Option<i64>,
        );
        let included: Option<Included> = sqlx::query_as(
            "SELECT m.block_time, x.network, x.block_number, x.block_hash, \
                    x.position_kind::text, x.position_index \
             FROM attempt_milestones m JOIN transitions x ON x.id = m.transition_id \
             WHERE m.transfer_id = $1 AND m.attempt = $2 AND m.milestone = 'included'",
        )
        .bind(t.id)
        .bind(attempt)
        .fetch_optional(&mut *conn)
        .await?;
        let Some((included_at, network, block_number, block_hash, kind, index)) = included else {
            return Ok(Did::default());
        };
        let leg = match (kind, index) {
            (Some(position_kind), Some(position_index)) => {
                store::observation_at(
                    conn,
                    &refs::Coordinate {
                        network,
                        block_number,
                        block_hash,
                        position_kind,
                        position_index,
                    },
                )
                .await?
            }
            _ => None,
        }
        .filter(|o| Self::is_leg_of(t, o));
        if let Some(o) = &leg
            && let (Some(outcome), Some(n), Some(h)) = (
                o.outcome.as_deref(),
                o.final_block_number,
                o.final_block_hash.as_deref(),
            )
        {
            let Some(point) = store::final_point(conn, &o.network, n, h).await? else {
                return Ok(Did::default());
            };
            if outcome == "withdrawn" {
                // The inclusion stopped matching the chain: unknown until evidence (F-001 NOTES
                // S5; REQ-IN-7).
                store::append(
                    conn,
                    t.id,
                    &Move {
                        from: State::Delivered,
                        to: State::NeedsReview,
                        condition: Some(Condition::Withdrawn),
                        point: Some(&point),
                        attempt: None,
                        manual_action: None,
                    },
                )
                .await?;
                return Ok(Did::moved());
            }
            if let Some(m) = self.leg_mismatch(conn, t, o).await? {
                return self.mismatch(conn, t, State::Delivered, o, &m).await;
            }
            // REQ-TR-3: final and executed in full, to the destination.
            store::append(
                conn,
                t.id,
                &Move {
                    from: State::Delivered,
                    to: State::Completed,
                    condition: None,
                    point: Some(&point),
                    attempt: None,
                    manual_action: None,
                },
            )
            .await?;
            return Ok(Did::moved());
        }
        if let Some(ProofPoint::Final(proof)) = store::proof_of(conn, t.id, attempt).await? {
            return self
                .after_proof(conn, t, State::Delivered, attempt, &proof, None, now)
                .await;
        }
        let deadline = self.deadline(conn, t, included_at).await?;
        if let Some(head) = self.past(conn, &t.destination_network, deadline).await? {
            return self
                .deadline_review(conn, t, State::Delivered, t.condition(), &head)
                .await;
        }
        Ok(Did::default())
    }

    /// `Refunding`: refunded once its refund transfer is `Completed`, resting on the refund's
    /// final coordinate.
    async fn refunding(&self, conn: &mut PgConnection, t: &Transfer) -> sqlx::Result<Did> {
        let refund: Option<i64> =
            sqlx::query_scalar("SELECT id FROM transfers WHERE refund_of = $1")
                .bind(t.id)
                .fetch_optional(&mut *conn)
                .await?;
        let Some(refund) = refund else {
            return Ok(Did::default());
        };
        let Some(r) = store::transfer(conn, refund).await? else {
            return Ok(Did::default());
        };
        let (State::Completed, Some(network), Some(number), Some(hash), Some(time)) = (
            r.state(),
            r.destination_final_network.clone(),
            r.destination_final_block_number,
            r.destination_final_block_hash.clone(),
            r.destination_final_block_time,
        ) else {
            return Ok(Did::default());
        };
        let point = Point {
            network,
            block_number: number,
            block_hash: hash,
            block_time: time,
            position: None,
        };
        store::append(
            conn,
            t.id,
            &Move {
                from: State::Refunding,
                to: State::Refunded,
                condition: None,
                point: Some(&point),
                attempt: None,
                manual_action: None,
            },
        )
        .await?;
        Ok(Did::moved())
    }

    // -----------------------------------------------------------------------------------------
    // Review (REQ-RV-1, REQ-RV-2, REQ-GV-5).

    /// `Needs-Review`: ends only by a recorded resolution whose evidence the core verifies itself,
    /// and then runs the ordinary step that edge is (`REQ-RV-2`). A resolution that moves value
    /// waits for its second approver (`REQ-GV-5`). Nothing else moves a transfer out of review: no
    /// timer, no retry, no refund (`REQ-RV-1`). A review opened on a delivery first gathers its
    /// evidence ([`Core::review_evidence`]): the cancel ordered at the deadline is driven, and a
    /// proof of non-execution is attached.
    async fn review(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        now: DateTime<Utc>,
    ) -> sqlx::Result<Did> {
        let entered: Option<(String, i64, DateTime<Utc>)> = sqlx::query_as(
            "SELECT state_before_review::text, entered_transition_id, entered_at \
             FROM reviews WHERE transfer_id = $1",
        )
        .bind(t.id)
        .fetch_optional(&mut *conn)
        .await?;
        let Some((before, review, entered_at)) = entered else {
            return Ok(Did::default());
        };
        let Some(before) = State::parse(&before) else {
            return Ok(Did::default());
        };
        let mut did = if matches!(before, State::Delivering | State::Delivered) {
            self.review_evidence(conn, t, review).await?
        } else {
            Did::default()
        };
        let resolved = self.resolve(conn, t, before, entered_at, now).await?;
        did.moved |= resolved.moved;
        did.intents += resolved.intents;
        Ok(did)
    }

    /// The recorded resolution of the review `t` entered at `entered_at` from `before`, if one
    /// stands: verified, approved where it moves value, and run as its ordinary step.
    async fn resolve(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        before: State,
        entered_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> sqlx::Result<Did> {
        type Action = (i64, String, String, i64, Vec<u8>, DateTime<Utc>, bool);
        let action: Option<Action> = sqlx::query_as(
            "SELECT a.id, a.to_state::text, a.network, a.block_number, a.block_hash, a.block_time, \
                    EXISTS (SELECT 1 FROM manual_approvals p WHERE p.action_id = a.id) \
             FROM manual_actions a \
             WHERE a.transfer_id = $1 AND a.kind = 'review-resolution' AND a.recorded_at >= $2 \
               AND NOT EXISTS (SELECT 1 FROM transitions x WHERE x.manual_action_id = a.id) \
               AND NOT EXISTS (SELECT 1 FROM event_log e WHERE e.kind = 'review-evidence-refused' \
                               AND e.payload ->> 'manual_action_id' = a.id::text) \
             ORDER BY a.id LIMIT 1",
        )
        .bind(t.id)
        .bind(entered_at)
        .fetch_optional(&mut *conn)
        .await?;
        let Some((action, to, network, number, hash, time, approved)) = action else {
            return Ok(Did::default());
        };
        let Some(to) = State::parse(&to) else {
            return Ok(Did::default());
        };
        let evidence = Point {
            network,
            block_number: number,
            block_hash: hash,
            block_time: time,
            position: None,
        };
        let attempt = store::current_attempt(conn, t.id).await?;
        let new_attempt = to == State::Delivering && before != State::SourceFinal;
        if !review_exit(before, to, new_attempt) {
            return self
                .refuse_evidence(
                    conn,
                    t,
                    action,
                    "not one ordinary edge from the state held before review",
                )
                .await;
        }
        if bezent_core::resolution_moves_value(to) && !approved {
            // Waits for the second approver (REQ-GV-5).
            return Ok(Did::default());
        }
        let verified = self
            .evidence_holds(conn, t, before, to, attempt, &evidence)
            .await?;
        if !verified {
            return self
                .refuse_evidence(
                    conn,
                    t,
                    action,
                    "the evidence does not show that edge on chain",
                )
                .await;
        }
        // The transition rests on the evidence itself (the store checks they are the same).
        let point = evidence;
        match to {
            State::Delivering => {
                // A delivery a resolution runs is new work like any other: the pause, cut-off and
                // reserve gates hold it, in review, until they open (INV-15, REQ-GV-1, REQ-OT-5;
                // review S5).
                if let Some(c) = self.gate(conn, t, now).await? {
                    return self.hold(conn, t, Some(c)).await;
                }
                if t.condition().is_some_and(gate_condition) {
                    return self.hold(conn, t, None).await;
                }
                let next = if before == State::SourceFinal {
                    1
                } else {
                    attempt + 1
                };
                self.deliver(
                    conn,
                    t,
                    State::NeedsReview,
                    next,
                    Some(&point),
                    Some(action),
                )
                .await
            }
            State::Refunding => {
                self.refund(conn, t, State::NeedsReview, &point, Some(action))
                    .await
            }
            _ => {
                let at = if to == State::Delivered {
                    // The inclusion rests on the leg's own coordinate, which the evidence is.
                    match self.destination_leg(conn, t).await? {
                        Leg::One(o) => o.point(),
                        _ => point,
                    }
                } else {
                    point
                };
                store::append(
                    conn,
                    t.id,
                    &Move {
                        from: State::NeedsReview,
                        to,
                        condition: None,
                        point: Some(&at),
                        attempt: None,
                        manual_action: Some(action),
                    },
                )
                .await?;
                Ok(Did::moved())
            }
        }
    }

    /// Whether `evidence` is the coordinate of the edge `before -> to`, from what the endpoints
    /// recorded at final blocks (`REQ-RV-2`: verified by bezent, never taken from the caller).
    async fn evidence_holds(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        before: State,
        to: State,
        attempt: i32,
        evidence: &Point,
    ) -> sqlx::Result<bool> {
        let at = |o: &Observation| {
            o.final_block_number == Some(evidence.block_number)
                && o.final_block_hash.as_deref() == Some(evidence.block_hash.as_slice())
                && o.network == evidence.network
        };
        // Every edge but an inclusion rests on a final point the endpoints recorded; an inclusion
        // rests on the leg's own coordinate, observed at a final block.
        if to != State::Delivered {
            let is_final_head = store::final_point(
                conn,
                &evidence.network,
                evidence.block_number,
                &evidence.block_hash,
            )
            .await?
            .is_some_and(|p| p.block_time == evidence.block_time);
            if !is_final_head {
                return Ok(false);
            }
        }
        Ok(match (before, to) {
            (State::Accepted, State::SourceFinal) | (State::Accepted, State::Withdrawn) => {
                let want = if to == State::SourceFinal {
                    "final"
                } else {
                    "withdrawn"
                };
                match t.source() {
                    Some(c) => store::observation_at(conn, &c).await?.is_some_and(|o| {
                        at(&o)
                            && o.outcome.as_deref() == Some(want)
                            && (want == "withdrawn" || o.executed)
                    }),
                    None => false,
                }
            }
            (State::Delivering, State::Delivered) => match self.destination_leg(conn, t).await? {
                Leg::One(o) => {
                    let p = o.point();
                    (
                        p.network.as_str(),
                        p.block_number,
                        p.block_hash.as_slice(),
                        p.block_time,
                    ) == (
                        evidence.network.as_str(),
                        evidence.block_number,
                        evidence.block_hash.as_slice(),
                        evidence.block_time,
                    ) && self.leg_mismatch(conn, t, &o).await?.is_none()
                }
                _ => false,
            },
            (State::Delivered, State::Completed) => match self.destination_leg(conn, t).await? {
                Leg::One(o) => {
                    at(&o)
                        && o.outcome.as_deref() == Some("final")
                        && self.leg_mismatch(conn, t, &o).await?.is_none()
                }
                _ => false,
            },
            (State::SourceFinal, State::Delivering) => evidence.network == t.destination_network,
            (State::Delivering | State::Delivered, State::Delivering | State::Refunding) => {
                let proven = matches!(store::proof_of(conn, t.id, attempt).await?,
                         Some(ProofPoint::Final(p)) if p.network == evidence.network
                             && p.block_number == evidence.block_number
                             && p.block_hash == evidence.block_hash);
                // A refund needs every attempt ordered, prepared or signed proven not executed,
                // not only the one the evidence names (INV-8; review M2); only a withdrawal has
                // one (REQ-RF-4).
                proven
                    && (to != State::Refunding
                        || (t.kind == "withdrawal"
                            && store::attempts_unproven(conn, t.id).await?.is_empty()))
            }
            _ => false,
        })
    }

    /// Records, once, that a resolution's evidence was refused; the transfer stays in review.
    async fn refuse_evidence(
        &self,
        conn: &mut PgConnection,
        t: &Transfer,
        action: i64,
        why: &str,
    ) -> sqlx::Result<Did> {
        store::event(
            conn,
            "review-evidence-refused",
            Some(t.id),
            &json!({"spec": "REQ-RV-2", "manual_action_id": action.to_string(),
                    "bridge_reference": t.bridge_reference, "reason": why}),
        )
        .await?;
        Ok(Did::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req_in_9_a_mint_the_issuing_network_refuses_holds_cnd_destination_refused() {
        let reason = |r: &str| dry_run_condition(&json!({"outcome": "refused", "reason": r}), None);
        assert_eq!(
            reason("Kreivo(Unknown)"),
            Some(Condition::DestinationRefused)
        );
        assert_eq!(reason("AboveWindow"), Some(Condition::MintWindow));
        // A pause is bezent's own declaration (the gate); a contract that refuses as paused when
        // bezent declares no pause holds nothing and is retried.
        assert_eq!(reason("Paused"), None);
        assert_eq!(reason("ReferenceUsed"), None);
    }

    /// F-008's `deliveryAllowanceKey`: `keccak256(abi.encodePacked("bezent.delivery.allowance",
    /// token))`, checked against `cast keccak` of the same bytes.
    #[test]
    fn cnd_payout_window_s2_the_delivery_allowance_key_is_the_vaults() {
        let key = delivery_allowance_key("0x41e94eb019c0762f9bfcf9fb1e58725bfb0e7582").unwrap();
        assert_eq!(
            hex0x(&key),
            "0x589f5a06170aa64c7c46d132050a52a2451178f059520e3f59a3644692ec8bd0"
        );
        assert_eq!(delivery_allowance_key("usdc"), None);
        assert_eq!(delivery_allowance_key("0x41e94eb0"), None);
    }

    /// Amendment 0012, PR #52 review S-2: only the structured refusal both providers returned
    /// alike, status 17 over the payout token's own delivery allowance key, holds
    /// `CND-PayoutWindow`. Free text never does, whatever it says, and neither does a refusal the
    /// providers disagree on (no `violation`), another status, or another key; none of those is
    /// read as a fee shortfall because the key's hex spells "fee".
    #[test]
    fn cnd_payout_window_s2_only_the_structured_allowance_refusal_holds_the_window() {
        let token = "0x41e94eb019c0762f9bfcf9fb1e58725bfb0e7582";
        let ours = delivery_allowance_key(token).unwrap();
        let theirs = format!("0x{}fee{}", "ab".repeat(14), "cd".repeat(16));
        let text = format!(
            "Roles: ConditionViolation(status 17, {}) (execution reverted)",
            hex0x(&ours)
        );
        let run = |violation: Value, reason: &str| {
            dry_run_condition(
                &json!({"passed": false, "reason": reason, "violation": violation}),
                Some(&ours),
            )
        };
        assert_eq!(
            run(json!({"status": 17, "key": hex0x(&ours)}), &text),
            Some(Condition::PayoutWindow)
        );
        // The same words with no structured refusal: one provider's text, or a disagreement.
        assert_eq!(run(Value::Null, &text), None);
        assert_eq!(
            run(
                Value::Null,
                &format!("Roles: ConditionViolation(status 17, {theirs})")
            ),
            None
        );
        // An Error(string) that imitates it.
        assert_eq!(
            run(Value::Null, &format!("{text} (execution reverted)")),
            None
        );
        // Another key, another status, or a key for no payout.
        assert_eq!(run(json!({"status": 17, "key": theirs}), &text), None);
        assert_eq!(run(json!({"status": 7, "key": hex0x(&ours)}), &text), None);
        assert_eq!(
            dry_run_condition(
                &json!({"reason": text, "violation": {"status": 17, "key": hex0x(&ours)}}),
                None
            ),
            None
        );
    }
}
