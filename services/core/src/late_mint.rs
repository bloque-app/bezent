// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! A mint of a reference bezent did not deliver (issue #26; `AC-I1.2`, `REQ-IC-4`, `INV-8`,
//! `INV-9`, `INV-16`).
//!
//! The Kreivo endpoint reads the contract's single-use state at the final block that proves a
//! delivery not executed, and records no proof if the reference was used by then (`INV-8`). That
//! read covers the chain only up to the proof's final block. A reference minted **after** it, by a
//! leaked delivery key, by governance, or by an attempt bezent did not record, is seen by nothing
//! else, and a refund may already be ordered. The observer already records every `Minted` and
//! `RefundMinted` of the contract with its reference, final, so the check is the core's, over what
//! is recorded; this module is the whole of it.
//!
//! # What raises the alert
//!
//! A final, executed `Minted` (or `RefundMinted`) observation on the issuing network, at block `b`,
//! for reference `R`, is a **late mint** when one of the two holds for the deliveries of `R`
//! (the intents of the same network, purpose and reference; a refund mint's also in its
//! redeemer's scope, amendment 0008, so a `RefundMinted` is matched only to the redemption of its
//! recipient):
//!
//! 1. **after a proof**: a signed transaction of `R` has a proof of non-execution whose final block
//!    is below `b`. At or above `b`, the endpoint's own read already covers the mint (it records an
//!    anomaly, not a proof), so nothing is matched;
//! 2. **nothing signed**: an order for `R` is recorded and no transaction of `R` was ever signed.
//!    The endpoint stores a transaction's exact bytes before it submits it (`INV-9`, `REQ-EO-1`),
//!    so a bezent mint always has its `signed_transactions` row first; a mint of `R` with none is
//!    a signature bezent did not record (`AC-I1.2`, `REQ-IC-4`), for a deposit's `Minted` as for a
//!    `RefundMinted`. A reference with **no order at all** is not matched: it is not one of
//!    bezent's, and `REQ-IC-2` makes the unexplained supply a divergence of the reserve equation
//!    (`REQ-BK-1`, `INV-4`), which the reserve check pauses for; backfilled history may hold such
//!    mints.
//!
//! and it is **not explained** by a later attempt of the same reference, which is legitimate
//! (`REQ-RF-5`: after a proof, a new attempt carries the same reference):
//!
//! - *explained*: a signed transaction of `R` was included, and executed, in the observation's own
//!   block (the endpoint records the inclusion as an observation, `REQ-OB-6`), and the block holds
//!   no more mint events of `R` than executed attempts. The contract mints a reference once
//!   (`INV-8`), so one executed attempt is one mint event; a second event in the block is not the
//!   attempt's. The observation does not record which extrinsic emitted an event (its position is
//!   the event's index in the block), so the two cannot be tied one to one; the count is what the
//!   record supports. Tying them needs the endpoint to record the event's extrinsic index (its
//!   phase, `ApplyExtrinsic(i)`) in the observation's `detail`, to compare with the inclusion's
//!   `position_index`;
//! - *undecided*: a signed transaction of `R` has no proof, no inclusion and no anomaly yet, and its
//!   era covers `b`. The endpoint's prover decides it when it next looks; the check waits, and
//!   decides when it has. **Only the prover's own record ends the wait**: a proof of non-execution
//!   (`non_execution_proofs`), an inclusion (an observation at its coordinate, executed or not,
//!   `REQ-OB-6`), or a `delivery-anomaly` that names the transaction. Those are the three things
//!   `proof::Prover::step` writes, and it writes nothing when it cannot decide (`unknown`).
//!   The network's final head is **not** the prover's progress: the observer and the supply reader
//!   write it, the prover is a separate worker, and a head past the era's end says only that the
//!   prover could have decided by now. Ending the wait on it raised a global pause on a legitimate
//!   retry whenever the prover was behind or down, included in the last blocks of its era, or the
//!   observer was catching up over old history (second review of PR #72). No other progress of the
//!   prover is on record, none is needed (see below), and none is invented.
//!
//! Otherwise the alert is raised through the compromise path of F-005: `key-compromise` and the
//! global `pause-requested`, once per observation (the store keeps one alert per detector and
//! detection key, migration 0049, so a restart or a replay of the observer raises nothing twice).
//!
//! # An attempt the prover does not decide
//!
//! The wait must not be silent, and must not be for ever without a human knowing. A held row stays
//! open and raises nothing; each cycle that finds an attempt undecided **with its era over at the
//! final head** (so the prover could have decided it) counts in `late_mint_checks.held_cycles`
//! (migration 0076). At [`HELD_PAGE_AFTER_CYCLES`] the core pages once, durably across restarts (an
//! event of its own kind, `late-mint-prover-silent`, deduplicated by the observation, as the
//! `late-mint-check-failed` page is), naming the reference, the attempt and the signed transaction:
//! the prover is not deciding and a mint waits on it. It is never a pause and never the alert: it
//! asks an operator to look, and the operator learns either that the prover is stuck or that a key
//! may be compromised. While the row is still held with the prover still silent, the page repeats
//! once a day ([`HELD_REPAGE_EVERY`]; review of the fix round of #77: an unbounded hold had no
//! further escalation). The last page is read from the event log, so a restart repeats nothing
//! early and forgets nothing; the repeat is a page too, never a pause and never the alert. The row stays open: if the prover later decides against the mint, the alert
//! still raises (once); if for the mint, the row settles.
//!
//! **`max_walk`.** When the nonce is consumed, the prover finds the consuming block by walking back
//! from the finalized head to the era's birth, and gives up (`unknown`, nothing recorded) when the
//! birth is more than `max_walk` behind; the gap only grows, so it gives up at every later pass.
//! An attempt that was included and whose inclusion is older than `max_walk` therefore never gets
//! a record, and without a bound the row would wait for ever. It is the page's case, not a raise:
//! the mint may be that very attempt's, and the core cannot tell. The page carries each attempt's
//! era and the final head, so the operator sees the gap, and resolves it by hand (the chain shows
//! who minted at the mint's block). `max_walk` itself is the endpoint's setting and is not changed.
//! A nonce that is not consumed does not walk, so it is not affected: it is proven once the era
//! ends. A transaction whose era is not on record (the signing path always records it: a signing
//! intent names its prepared transaction, migration 0037) is held and counts like any other, with an
//! error logged each cycle.
//!
//! # What is looked at, and how often
//!
//! Migration 0075 queues every final `Minted` and `RefundMinted` of an issuing network in
//! `late_mint_checks`, in the transaction that records its outcome, whatever its id or the time it
//! is finalised. A cycle reads the open rows only, a bounded batch, least recently looked at first,
//! and looks at each on its own: a failure of one is logged, counted, and pages a human when it
//! persists ([`PAGE_AFTER_FAILURES`]), and never stops the others. A cycle whose candidate query
//! itself fails (the store, the grant) is counted too, by [`Watcher`], and pages the same way
//! (`late-mint-watch-failed`): it used to be a warning only, and every mint would go unchecked
//! without anyone being told. A row is **settled** when its
//! verdict cannot change: alerted; explained by an attempt executed in its block; or not matched
//! with every signed transaction of the reference proven or decided (a proof is written once per
//! transaction, so none can arrive to match it later). A row that waits on an undecided attempt, or
//! on a transaction the endpoint has not yet decided, stays open (and pages after a bound, above).
//!
//! # Finality
//!
//! Only observations with a `final` outcome are queued (`INV-16`, `REQ-OB-5`): the alert is a
//! safety signal, not a disposition, but it rests on the same data and acts only on what is final;
//! the observer writes nothing else. A `withdrawn` or undecided observation raises nothing.
//!
//! # A mint with no reference of bezent's at all
//!
//! Not matched here, and not meant to be: see 2 above.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgConnection, PgPool};

use crate::refs;
use crate::store;
use bezent_signer::compromise;

/// How many consecutive failed looks at one mint page a human.
pub const PAGE_AFTER_FAILURES: i32 = 3;

/// How many cycles a mint may wait on an attempt the prover has not decided, with the attempt's era
/// over at the final head, before a human is paged: five minutes at the core's default cycle of 5
/// seconds. A page is not a pause. It can also mean the prover is only blocked behind the observer:
/// the endpoint's loop steps the prover once the observer is caught up, so during a long catch-up
/// over old history an undecided mint can page before the prover has had its turn. Otherwise the
/// prover is down or stuck, or it cannot decide (`max_walk`), or a key is compromised.
pub const HELD_PAGE_AFTER_CYCLES: i32 = 60;

/// How long after a `late-mint-prover-silent` page the next one is made, while the same mint is
/// still held with the prover still silent.
pub const HELD_REPAGE_EVERY: chrono::Duration = chrono::Duration::days(1);

/// How many open rows one cycle looks at.
const BATCH: i64 = 1_000;

/// A final mint observation to look at.
#[derive(Debug, sqlx::FromRow)]
struct Candidate {
    observation_id: i64,
    network: String,
    block_number: i64,
    block_hash: Vec<u8>,
    position_index: i64,
    recipient: Option<String>,
    reference: Vec<u8>,
    event: String,
    /// The final block its outcome was decided at: the head it was read under.
    final_block: i64,
}

/// One delivery (an intent) of the reference.
#[derive(Debug, sqlx::FromRow)]
struct Delivery {
    id: i64,
    scope: Option<String>,
}

/// One signed transaction of the reference, and what the store knows of it.
#[derive(Debug, sqlx::FromRow)]
struct Signed {
    id: i64,
    attempt: i32,
    /// The final block of its proof of non-execution, if it has one.
    proof: Option<i64>,
    /// Whether it was included, executed, in the candidate's block.
    executed_here: bool,
    /// Whether it was included at all (executed or refused), or an anomaly names it.
    decided: bool,
    /// Its era, if the prepared transaction is on record.
    valid_from: Option<i64>,
    valid_until: Option<i64>,
}

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

/// The dedupe key of the alert for the observation at (`network`, `block_hash`, `index`).
fn dedupe(c: &Candidate) -> String {
    format!(
        "late-mint:{}:{}:{}",
        c.network,
        hex0x(&c.block_hash),
        c.position_index
    )
}

/// Looks at the open final mint observations, and raises the key-compromise alert and the global
/// pause request for each that was a late mint. Returns how many alerts it raised. `pool` connects
/// as the core's login. Safe to repeat: an observation raises at most one. One mint that cannot be
/// looked at never stops the others.
pub async fn watch(pool: &PgPool, now: DateTime<Utc>) -> sqlx::Result<usize> {
    let mut conn = pool.acquire().await?;
    let candidates: Vec<Candidate> = sqlx::query_as(
        "SELECT o.id AS observation_id, o.network, o.block_number, o.block_hash, o.position_index, \
                o.recipient, o.on_chain_reference AS reference, o.detail ->> 'event' AS event, \
                x.final_block_number AS final_block \
         FROM late_mint_checks k \
         JOIN observations o ON o.id = k.observation_id \
         JOIN observation_outcomes x ON x.observation_id = o.id AND x.outcome = 'final' \
         WHERE k.settled_at IS NULL \
         ORDER BY k.checked_at NULLS FIRST, k.observation_id \
         LIMIT $1",
    )
    .bind(BATCH)
    .fetch_all(&mut *conn)
    .await?;
    let mut raised = 0;
    for c in candidates {
        match look(pool, &mut conn, &c, now).await {
            Ok(true) => raised += 1,
            Ok(false) => {}
            Err(e) => failed(&mut conn, &c, &e).await,
        }
    }
    Ok(raised)
}

/// The core's count of cycles in which [`watch`] failed outright (it could not list the open rows).
/// One per [`crate::Core`]; the page is once per streak, and a streak that outlives a restart pages
/// again after the same bound.
#[derive(Debug, Default)]
pub struct Watcher {
    failed: AtomicI32,
    paged: AtomicBool,
}

impl Watcher {
    /// [`watch`], and the count of its failures: a failure warns, and pages once when it has
    /// happened [`PAGE_AFTER_FAILURES`] cycles in a row. Never fails the cycle.
    pub async fn cycle(&self, pool: &PgPool, now: DateTime<Utc>) {
        let Err(e) = watch(pool, now).await else {
            self.failed.store(0, Ordering::Relaxed);
            self.paged.store(false, Ordering::Relaxed);
            return;
        };
        let failures = self.failed.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::warn!(error = %e, failures, "AC-I1.2: late mints not checked; the cycle goes on");
        if failures < PAGE_AFTER_FAILURES || self.paged.load(Ordering::Relaxed) {
            return;
        }
        let detail = json!({
            "spec": "AC-I1.2, REQ-IC-4",
            "failures": failures,
            "error": e.to_string(),
        });
        // Best effort: the store is probably what failed. The log line is the page either way, but
        // the streak counts as paged only when the event is in the log: a failed insert leaves it
        // to the next cycle, so the page is never a log line alone for good.
        let recorded = match pool.acquire().await {
            Ok(mut conn) => store::event(&mut conn, "late-mint-watch-failed", None, &detail)
                .await
                .is_ok(),
            Err(_) => false,
        };
        if recorded {
            self.paged.store(true, Ordering::Relaxed);
        }
        tracing::error!(kind = %"late-mint-watch-failed", %detail,
            "page: the core cannot list the mints to check against their proofs, cycle after cycle; no late mint would be seen");
    }
}

/// Looks at one candidate, and settles it or leaves it open. Returns whether it raised the alert.
async fn look(
    pool: &PgPool,
    conn: &mut PgConnection,
    c: &Candidate,
    now: DateTime<Utc>,
) -> sqlx::Result<bool> {
    if compromise::raised(
        &mut *conn,
        compromise::DETECTOR_CORE,
        &format!("dedupe:{}", dedupe(c)),
    )
    .await?
    {
        settle(conn, c, "alerted").await?;
        return Ok(false);
    }
    match late(conn, c).await? {
        Look::NotLate => settle(conn, c, "not-late").await?,
        Look::Explained => settle(conn, c, "explained").await?,
        Look::Held(wait) => hold(conn, c, &wait, now).await?,
        Look::Late(why) => {
            let alert = json!({
                "spec": "AC-I1.2, REQ-IC-4, INV-8",
                "source": "late-mint",
                "dedupe": dedupe(c),
                "network": c.network,
                "reason": why.text(),
                "event": c.event,
                "on_chain_reference": hex0x(&c.reference),
                "recipient": c.recipient,
                "coordinate": {
                    "network": c.network,
                    "block_number": c.block_number,
                    "block_hash": hex0x(&c.block_hash),
                    "position_kind": "log",
                    "position_index": c.position_index,
                },
                "proof": why.proof(),
                "actions": ["pause", "rotate", "report"],
            });
            let (alert_seq, pause_seq) = compromise::raise(pool, &alert).await?;
            tracing::error!(
                network = c.network,
                block = c.block_number,
                alert_seq,
                pause_seq,
                reason = why.text(),
                "AC-I1.2: a reference was minted that bezent did not deliver; key-compromise is in the event log and the bridge is paused"
            );
            settle(conn, c, "alerted").await?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// Settles the candidate: its verdict cannot change.
async fn settle(conn: &mut PgConnection, c: &Candidate, verdict: &str) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE late_mint_checks SET settled_at = now(), verdict = $2, checked_at = now(), \
             failures = 0 WHERE observation_id = $1 AND settled_at IS NULL",
    )
    .bind(c.observation_id)
    .bind(verdict)
    .execute(conn)
    .await
    .map(|_| ())
}

/// The candidate was looked at and stays open, waiting on the prover. A cycle that finds an
/// attempt with no record from the prover though its era is over counts, and the count reaching
/// [`HELD_PAGE_AFTER_CYCLES`] pages, and a cycle that still finds the prover silent pages again once a
/// day ([`HELD_REPAGE_EVERY`]). It never raises the alert and never pauses.
async fn hold(
    conn: &mut PgConnection,
    c: &Candidate,
    wait: &Wait,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    let cycles: i32 = sqlx::query_scalar(
        "UPDATE late_mint_checks SET checked_at = now(), failures = 0, \
             held_cycles = held_cycles + $2 WHERE observation_id = $1 RETURNING held_cycles",
    )
    .bind(c.observation_id)
    .bind(i32::from(wait.over))
    .fetch_one(&mut *conn)
    .await?;
    let id = c.observation_id.to_string();
    if cycles < HELD_PAGE_AFTER_CYCLES {
        return Ok(());
    }
    // The last page of this mint, from the log (a page before this change has no `at`: its
    // append time stands). Quiet within the day; a cycle that no longer finds the prover silent
    // (an era still open) has nothing to repeat.
    let last: Option<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT max(COALESCE((payload ->> 'at')::timestamptz, appended_at)) FROM event_log \
         WHERE kind = 'late-mint-prover-silent' AND transfer_id IS NULL AND payload ->> 'observation' = $1",
    )
    .bind(&id)
    .fetch_one(&mut *conn)
    .await?;
    if last.is_some_and(|l| !wait.over || now < l + HELD_REPAGE_EVERY) {
        return Ok(());
    }
    let detail = json!({
        "spec": "AC-I1.2, REQ-IC-4, REQ-RF-5",
        "observation": id,
        "network": c.network,
        "block_number": c.block_number,
        "on_chain_reference": hex0x(&c.reference),
        "event": c.event,
        "held_cycles": cycles,
        "repeat": last.is_some(),
        "at": now.to_rfc3339(),
        "final_head": wait.head,
        "attempts": wait.attempts,
        "meaning": "the prover has recorded nothing for these attempts of the reference (no proof, inclusion or anomaly) though their eras are over, and a mint of the reference waits on it: the prover is down or stuck, an inclusion older than its max_walk is never recorded, or a key is compromised; nothing is raised or paused",
    });
    store::event(conn, "late-mint-prover-silent", None, &detail).await?;
    tracing::error!(kind = %"late-mint-prover-silent", %detail,
        "page: a mint of a reference waits on an attempt the prover does not decide; it is the prover (stuck, down, max_walk) or a compromised key");
    Ok(())
}

/// The look at the candidate failed: it stays open and is tried again, and a failure that persists
/// pages a human, once (the same event log record and `page:` line as the core's other pages).
async fn failed(conn: &mut PgConnection, c: &Candidate, error: &sqlx::Error) {
    tracing::error!(
        observation = c.observation_id,
        network = c.network,
        block = c.block_number,
        error = %error,
        "AC-I1.2: a mint could not be checked for a late mint; it stays queued and the others go on"
    );
    let failures: sqlx::Result<i32> = sqlx::query_scalar(
        "UPDATE late_mint_checks SET failures = failures + 1, checked_at = now() \
         WHERE observation_id = $1 RETURNING failures",
    )
    .bind(c.observation_id)
    .fetch_one(&mut *conn)
    .await;
    let Ok(failures) = failures else { return };
    let id = c.observation_id.to_string();
    if failures < PAGE_AFTER_FAILURES
        || store::has_event(conn, "late-mint-check-failed", None, "observation", &id)
            .await
            .unwrap_or(true)
    {
        return;
    }
    let detail = json!({
        "spec": "AC-I1.2, REQ-IC-4",
        "observation": id,
        "network": c.network,
        "block_number": c.block_number,
        "on_chain_reference": hex0x(&c.reference),
        "failures": failures,
        "error": error.to_string(),
    });
    if store::event(conn, "late-mint-check-failed", None, &detail)
        .await
        .is_ok()
    {
        tracing::error!(kind = %"late-mint-check-failed", %detail,
            "page: a mint of the issuing network cannot be checked against its proofs, again and again; a late mint would go unseen");
    }
}

/// Why a mint is late.
enum Late {
    /// A transaction of the reference was proven not executed at final block `proof`, below the mint.
    AfterProof { proof: i64 },
    /// An order is recorded and nothing was ever signed for it.
    Unsigned,
}

impl Late {
    fn text(&self) -> &'static str {
        match self {
            Late::AfterProof { .. } => {
                "the reference was minted after its proof of non-execution, by no transaction bezent signed"
            }
            Late::Unsigned => {
                "the reference was minted though bezent signed nothing for it (a signature it did not record)"
            }
        }
    }

    fn proof(&self) -> serde_json::Value {
        match self {
            Late::AfterProof { proof } => json!({"final_block_number": proof}),
            Late::Unsigned => serde_json::Value::Null,
        }
    }
}

/// What a held candidate waits on.
struct Wait {
    /// The attempts of the reference with no record from the prover.
    attempts: Vec<serde_json::Value>,
    /// Whether one of them has its era over at the final head (or has none on record): the
    /// prover could have decided it by now, so the cycle counts toward the page.
    over: bool,
    /// The final head the core saw.
    head: i64,
}

/// What the look at a candidate found.
enum Look {
    /// Not a late mint, and nothing that is still to be recorded can make it one.
    NotLate,
    /// Explained by an attempt executed in its block.
    Explained,
    /// Not decided yet: an attempt is still to be decided by the prover.
    Held(Wait),
    /// A late mint that no attempt of bezent's explains.
    Late(Late),
}

/// Whether `c` is a late mint that no attempt of bezent's explains, and not one still undecided.
async fn late(conn: &mut PgConnection, c: &Candidate) -> sqlx::Result<Look> {
    let purpose = if c.event == "Minted" {
        "mint"
    } else {
        "refund-mint"
    };
    let deliveries: Vec<Delivery> = sqlx::query_as(
        "SELECT id, reference_scope AS scope FROM intents \
         WHERE network = $1 AND purpose = $2 AND on_chain_reference = $3",
    )
    .bind(&c.network)
    .bind(purpose)
    .bind(&c.reference)
    .fetch_all(&mut *conn)
    .await?;
    // A mint is of the reference's own scope: none for a mint, the redeemer's for a refund mint
    // (amendment 0008). A refund mint to another account is another redemption's.
    let ids: Vec<i64> = deliveries
        .iter()
        .filter(|d| match (&d.scope, &c.recipient, c.event.as_str()) {
            (None, _, "Minted") => true,
            (Some(scope), Some(to), "RefundMinted") => refs::same_account(scope, to),
            _ => false,
        })
        .map(|d| d.id)
        .collect();
    if ids.is_empty() {
        return Ok(Look::NotLate);
    }
    let signed: Vec<Signed> = sqlx::query_as(
        "SELECT st.id, i.attempt, \
                (SELECT min(p.final_block_number) FROM non_execution_proofs p \
                 WHERE p.signed_transaction_id = st.id) AS proof, \
                EXISTS (SELECT 1 FROM observations t \
                        WHERE t.detail ->> 'signed_transaction' = st.id::text \
                          AND t.position_kind = 'transaction' \
                          AND t.network = $2 AND t.block_hash = $3 AND t.executed) \
                  AS executed_here, \
                (EXISTS (SELECT 1 FROM observations t \
                         WHERE t.detail ->> 'signed_transaction' = st.id::text \
                           AND t.position_kind = 'transaction' AND t.network = $2) \
                 OR EXISTS (SELECT 1 FROM event_log e WHERE e.kind = 'delivery-anomaly' \
                            AND e.payload ->> 'signed_transaction' = st.id::text \
                            AND e.payload ->> 'network' = $2)) AS decided, \
                (SELECT p.valid_from_block FROM prepared_transactions p \
                 WHERE p.id::text = i.payload ->> 'prepared') AS valid_from, \
                (SELECT p.valid_until_block FROM prepared_transactions p \
                 WHERE p.id::text = i.payload ->> 'prepared') AS valid_until \
         FROM signed_transactions st JOIN intents i ON i.id = st.intent_id \
         WHERE st.intent_id = ANY($1) ORDER BY st.id",
    )
    .bind(&ids)
    .bind(&c.network)
    .bind(&c.block_hash)
    .fetch_all(&mut *conn)
    .await?;

    // The final head the observer has: it says the prover could have decided by now, never that it
    // has (see the module documentation).
    let head = store::latest_head(conn, &c.network)
        .await?
        .map_or(0, |p| p.block_number)
        .max(c.final_block);
    // The attempts the prover has not decided (no proof, no inclusion, no anomaly), and whether the
    // prover has had the time to: their eras are over at `head`. `covering` keeps those whose era
    // covers the mint's block, the only ones that can have minted it.
    let waiting = |covering: bool| -> Option<Wait> {
        let mut attempts = Vec::new();
        let mut over = false;
        for s in signed.iter().filter(|s| s.proof.is_none() && !s.decided) {
            match (s.valid_from, s.valid_until) {
                (Some(from), Some(until)) => {
                    if covering && !(from <= c.block_number && c.block_number <= until) {
                        continue;
                    }
                    over |= head > until;
                }
                _ => {
                    tracing::error!(
                        observation = c.observation_id,
                        network = c.network,
                        block = c.block_number,
                        signed = s.id,
                        "AC-I1.2: a signed transaction of a mint's reference has no era on record; it waits on the prover like any other, which should not occur"
                    );
                    over = true;
                }
            }
            attempts.push(json!({
                "signed_transaction": s.id,
                "attempt": s.attempt,
                "valid_from": s.valid_from,
                "valid_until": s.valid_until,
            }));
        }
        (!attempts.is_empty()).then_some(Wait {
            attempts,
            over,
            head,
        })
    };

    // The late-mint rule: after a proof, or with no signature recorded.
    let after_proof = signed
        .iter()
        .filter_map(|s| s.proof)
        .filter(|p| *p < c.block_number)
        .min();
    let matched = match after_proof {
        Some(proof) => Late::AfterProof { proof },
        None if signed.is_empty() => Late::Unsigned,
        None => {
            // Not matched. A transaction with no proof and no decision may still be proven not
            // executed, at a final block below this mint's (a prover that reads behind the
            // observer): the candidate stays open until each has one or the other.
            return Ok(waiting(false).map_or(Look::NotLate, Look::Held));
        }
    };
    // A transaction of the reference executed in this very block explains one mint event of it:
    // the contract mints a reference once, so a second event in the block is not that attempt's.
    let executed = signed.iter().filter(|s| s.executed_here).count() as i64;
    if executed > 0 {
        let events: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM observations o \
             JOIN observation_outcomes x ON x.observation_id = o.id AND x.outcome = 'final' \
             WHERE o.network = $1 AND o.block_hash = $2 AND o.position_kind = 'log' \
               AND o.on_chain_reference = $3 AND o.detail ->> 'event' = $4 AND o.executed \
               AND ($4 = 'Minted' OR o.recipient IS NOT DISTINCT FROM $5)",
        )
        .bind(&c.network)
        .bind(&c.block_hash)
        .bind(&c.reference)
        .bind(&c.event)
        .bind(&c.recipient)
        .fetch_one(&mut *conn)
        .await?;
        if events <= executed {
            return Ok(Look::Explained);
        }
    }
    // A transaction still to be decided by the prover, whose era covers the block, may yet be the
    // one: the mint waits on the prover's record, never on the head.
    Ok(waiting(true).map_or(Look::Late(matched), Look::Held))
}
