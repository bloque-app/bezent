// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #26 (second review of PR #72): the core's late-mint check against the real endpoint on the
//! in-memory chain (`AC-I1.2`, `REQ-IC-4`, `REQ-RF-5`, `INV-8`). The observer (`Follower`), the
//! prover (`Prover`), the submitter and the signer are the endpoint's own; the core is
//! `Core::cycle` on its own login. Only the declarations and the arrival are the fixture's.
//!
//! The race the check had to survive: the retry of a reference is signed, included in the last
//! block of its era and minted there; the observer records the mint as final under a head past the
//! era; the prover, a separate worker, has not stepped. The core must wait for the prover's record,
//! not read the head as a decision.

mod common;

use bezent_core_service::late_mint::HELD_PAGE_AFTER_CYCLES;
use bezent_core_service::{Core, Settings};
use bezent_kreivo_endpoint::kreivo::{AssetId, ContractEvent, MAX_ERA};
use bezent_kreivo_endpoint::observe::Step;
use bezent_kreivo_endpoint::proof::{Disposition, Prover};
use bezent_kreivo_endpoint::submit::{Prepared, Submitted};
use bezent_kreivo_endpoint::supply::Supplies;
use chrono::{DateTime, Duration, Utc};
use common::*;
use serde_json::Value;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

const ORIGIN: &str = "polygon-test";
const ASSET: &str = "bUSDC";
const TOKEN_ID: &str = "usdc";
const RESERVE: &str = "0x00000000000000000000000000000000000000aa";
/// Alice, prefix 42, and her account.
const DESTINATION: &str = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";
const DESTINATION_ACCOUNT: [u8; 32] = [
    0xd4, 0x35, 0x93, 0xc7, 0x15, 0xfd, 0xd3, 0x1c, 0x61, 0x14, 0x1a, 0xbd, 0x04, 0xa9, 0x9f, 0xd6,
    0x82, 0x2c, 0x85, 0x58, 0x85, 0x4c, 0xcd, 0xe3, 0x9a, 0x56, 0x84, 0xe7, 0xa5, 0x6d, 0xa2, 0x7d,
];
const AMOUNT: u128 = 5_000;

async fn exec(pool: &PgPool, sql: &str) {
    sqlx::raw_sql(sql)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// The core, on a pool of its own logged in as the core's login (a member of `bezent_core` only):
/// one pool, one login (#27, #46).
async fn core(f: &Fixture, _connect: &PgConnectOptions) -> Core {
    let pool = role_pool(&f.admin, bezent_store::CORE_ROLE).await;
    Core::new(
        pool,
        Settings {
            retry_preparation_after: Duration::zero(),
            default_deadline: Duration::hours(1),
            ..Settings::default()
        },
    )
}

/// The declarations (the operator's), a final arrival on the origin network (its endpoint's) and
/// the deposit request binding it, accepted (the ingress's). Returns the transfer.
async fn deposit(f: &Fixture) -> i64 {
    exec(
        &f.admin,
        &format!(
            "INSERT INTO networks (network, role, address_format, backend) VALUES \
               ('{ORIGIN}', 'origin', 'evm', 'evm:80002'); \
             INSERT INTO assets (asset, precision, assurance_class, issuing_asset) VALUES \
               ('{ASSET}', 6, 'public', 'Here(101)'); \
             INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
               ('{ORIGIN}', '{TOKEN_ID}', '{ASSET}', 6); \
             INSERT INTO reserves (asset, network, address) VALUES ('{ASSET}', '{ORIGIN}', '{RESERVE}'); \
             INSERT INTO routes (asset, network, direction, minimum, maximum, window_limit, \
                 window_seconds, capacity, cutoffs, completion_budget_seconds) VALUES \
               ('{ASSET}', '{ORIGIN}', 'deposit', 1, 1000000000, 10000000000, 86400, 100, '[]', 3600); \
             INSERT INTO observations (network, block_number, block_hash, block_time, \
                 position_kind, position_index, token, amount, sender, recipient, executed, \
                 kind) \
               VALUES ('{ORIGIN}', 10, '\\x0a', '2026-10-13T00:00:10Z', 'log', 0, '{TOKEN_ID}', \
                 {AMOUNT}, '0xfunder', '{RESERVE}', TRUE, 'arrival'); \
             INSERT INTO final_heads (network, block_number, block_hash, block_time) VALUES \
               ('{ORIGIN}', 12, '\\x0c', '2026-10-13T00:00:12Z'); \
             INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
                 final_block_hash) SELECT id, 'final', 12, '\\x0c' FROM observations;"
        ),
    )
    .await;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, refund_destination_network, refund_destination, \
             source_network, source_block_number, source_block_hash, source_position_kind, \
             source_position_index, state) \
         VALUES ('bz-d-1', 'req-1', 'deposit', $1, $2::numeric, $3, $4, $5, $6, $5, 10, '\\x0a', \
             'log', 0, 'Received') RETURNING id",
    )
    .bind(ASSET)
    .bind(AMOUNT.to_string())
    .bind(NETWORK)
    .bind(DESTINATION)
    .bind(ORIGIN)
    .bind("0x52908400098527886e0f7030069857d2e4169ee7")
    .fetch_one(&f.admin)
    .await
    .unwrap();
    exec(
        &f.admin,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state) \
             VALUES ({id}, 'Received', 'Accepted')"
        ),
    )
    .await;
    id
}

/// One core cycle at `now`, which must not fail a step.
async fn cycle(core: &Core, now: DateTime<Utc>) {
    let report = core.cycle(now).await.expect("cycle");
    assert_eq!(report.failed, 0, "a step failed: {report:?}");
}

/// The latest intent of transfer `id` whose payload has `key`.
async fn intent_with(f: &Fixture, id: i64, key: &str) -> Option<(i64, Value)> {
    sqlx::query_as(
        "SELECT id, payload FROM intents WHERE transfer_id = $1 AND payload ? $2 \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(id)
    .bind(key)
    .fetch_optional(&f.admin)
    .await
    .unwrap()
}

fn now() -> DateTime<Utc> {
    "2026-10-13T01:00:00Z".parse().unwrap()
}

/// The supply reader records the finalized head, as the endpoint's loop does each pass (it is the
/// reader, not the prover, that writes `final_heads`).
async fn record_head(f: &Fixture) {
    let supplies = Supplies {
        chain: &f.chain,
        pool: &f.endpoint,
        network: NETWORK,
        contract: CONTRACT,
    };
    supplies.record().await.unwrap();
}

/// The deposit's retry, signed and not yet sent: attempt 1 was signed and never sent, its era
/// ended, the prover proved it not executed, the core ordered attempt 2 with the same reference
/// (`REQ-RF-5`), and the endpoint prepared and signed it. The node does not accept submissions,
/// so attempt 2 waits in the endpoint's store for the test to send it at a block of its choosing.
struct Retry {
    f: Fixture,
    core: Core,
    reference: [u8; 32],
    /// Attempt 2's signed transaction and the last block of its era.
    signed: i64,
    until: u64,
}

async fn retry(pool: PgPoolOptions, connect: PgConnectOptions) -> Retry {
    let f = Fixture::new(pool, connect.clone()).await;
    let core = core(&f, &connect).await;
    let id = deposit(&f).await;
    let s = f.submitter();
    cycle(&core, now()).await;
    cycle(&core, now()).await;
    let (order, _) = intent_with(&f, id, "order").await.expect("the order");
    let reference: Vec<u8> =
        sqlx::query_scalar("SELECT on_chain_reference FROM intents WHERE id = $1")
            .bind(order)
            .fetch_one(&f.admin)
            .await
            .unwrap();
    let reference: [u8; 32] = reference.try_into().unwrap();

    // Attempt 1: signed, never sent.
    f.chain.refuse_submissions(true);
    let prepared = s.prepare(order).await.unwrap();
    assert!(
        matches!(prepared, Prepared::Ready { attempt: 1, .. }),
        "{prepared:?}"
    );
    cycle(&core, now()).await;
    let (signing, _) = intent_with(&f, id, "prepared")
        .await
        .expect("signing intent");
    let out = s.sign_and_submit(signing).await.unwrap();
    assert!(matches!(out, Submitted::Persisted { .. }), "{out:?}");
    cycle(&core, now()).await;
    // Its era ends; the prover proves it not executed at a final block below any later mint.
    f.chain.advance(MAX_ERA + 1);
    let found = f.prover().step().await.unwrap();
    assert!(
        matches!(found.as_slice(), [(_, Disposition::NotExecuted(_))]),
        "{found:?}"
    );
    // The observer reads the chain up to the final head the proof names (the core reads a proof
    // against the heads the store holds).
    let follower = f.follower();
    while let Step::Read { .. } = follower.step().await.unwrap() {}
    record_head(&f).await;
    for _ in 0..3 {
        cycle(&core, now()).await;
    }

    // Attempt 2: the same reference, prepared and signed, held back.
    let (order2, _) = intent_with(&f, id, "order")
        .await
        .expect("the retry's order");
    assert_ne!(order2, order, "the core ordered attempt 2");
    let finalized = f.chain.finalized_ref();
    assert!(
        s.orders_to_prepare(&finalized)
            .await
            .unwrap()
            .contains(&order2)
    );
    let prepared = s.prepare(order2).await.unwrap();
    assert!(
        matches!(prepared, Prepared::Ready { .. }),
        "retry not prepared"
    );
    cycle(&core, now()).await;
    let (signing2, _) = intent_with(&f, id, "prepared")
        .await
        .expect("signing intent 2");
    let Submitted::Persisted { id: signed, .. } = s.sign_and_submit(signing2).await.unwrap() else {
        panic!("attempt 2 was sent");
    };
    let until: i64 = f
        .count(&format!(
            "SELECT p.valid_until_block FROM signed_transactions st \
             JOIN intents i ON i.id = st.intent_id \
             JOIN prepared_transactions p ON p.id::text = i.payload ->> 'prepared' \
             WHERE st.id = {signed}"
        ))
        .await;
    Retry {
        f,
        core,
        reference,
        signed,
        until: until as u64,
    }
}

impl Retry {
    /// The node accepts attempt 2 when the chain is at the block before the last of its era, and
    /// includes it in that last block, where it mints. The chain then finalizes past the era.
    async fn included_in_the_last_block_of_its_era(&self) {
        let f = &self.f;
        while f.chain.best() < self.until - 1 {
            f.chain.produce();
        }
        f.chain.refuse_submissions(false);
        assert_eq!(f.submitter().resend().await.unwrap(), 1);
        f.chain.produce();
        assert_eq!(f.chain.best(), self.until);
        f.chain.produce();
        f.chain.produce();
        f.chain.finalize(f.chain.best());
    }

    /// The observer reads every final block, as it does when it is caught up.
    async fn observe(&self) {
        let follower = self.f.follower();
        let mut steps = 0;
        while let Step::Read { .. } = follower.step().await.unwrap() {
            steps += 1;
            assert!(steps < 1_000);
        }
        record_head(&self.f).await;
    }

    async fn alerts(&self) -> i64 {
        self.f
            .count(
                "SELECT count(*) FROM event_log WHERE kind = 'key-compromise' \
                   AND payload ->> 'source' = 'late-mint'",
            )
            .await
    }

    async fn pauses(&self) -> i64 {
        self.f
            .count("SELECT count(*) FROM event_log WHERE kind = 'pause-requested'")
            .await
    }

    async fn pages(&self) -> i64 {
        self.f
            .count("SELECT count(*) FROM event_log WHERE kind = 'late-mint-prover-silent'")
            .await
    }

    /// The verdict on the mint of the reference, if it is queued and settled.
    async fn verdict(&self) -> Option<String> {
        sqlx::query_scalar("SELECT verdict FROM late_mint_checks")
            .fetch_optional(&self.f.admin)
            .await
            .unwrap()
            .flatten()
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_5_a_retry_minted_in_its_eras_last_block_waits_for_the_prover_and_raises_nothing(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let r = retry(pool, connect).await;
    r.included_in_the_last_block_of_its_era().await;
    r.observe().await;
    let head: i64 = r.f.count("SELECT max(block_number) FROM final_heads").await;
    assert!(head > r.until as i64, "the observer's head is past the era");
    // The prover has not stepped: it has recorded nothing for attempt 2.
    for _ in 0..3 {
        cycle(&r.core, now()).await;
    }
    assert_eq!(
        (r.alerts().await, r.pauses().await, r.pages().await),
        (0, 0, 0)
    );
    assert_eq!(r.verdict().await, None, "the row stays open");
    // The prover steps and records the inclusion; the row settles, with no alarm.
    let found = r.f.prover().step().await.unwrap();
    assert!(
        matches!(found.as_slice(), [(i, Disposition::Executed { .. })] if *i == r.signed),
        "{found:?}"
    );
    cycle(&r.core, now()).await;
    assert_eq!(r.verdict().await.as_deref(), Some("explained"));
    assert_eq!(
        (r.alerts().await, r.pauses().await, r.pages().await),
        (0, 0, 0)
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_nx_3_an_inclusion_older_than_max_walk_is_never_recorded_and_pages_instead_of_raising(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let r = retry(pool, connect).await;
    r.included_in_the_last_block_of_its_era().await;
    r.observe().await;
    // A prover whose `max_walk` is shorter than the walk from the finalized head back to the
    // era's birth: it can never identify the consumer of the nonce, and records nothing.
    let short = Prover {
        max_walk: 5,
        ..r.f.prover()
    };
    for _ in 0..HELD_PAGE_AFTER_CYCLES - 1 {
        let found = short.step().await.unwrap();
        assert!(
            matches!(found.as_slice(), [(_, Disposition::Unknown(why))] if why.contains("max_walk")),
            "{found:?}"
        );
        cycle(&r.core, now()).await;
    }
    assert_eq!(r.pages().await, 0);
    short.step().await.unwrap();
    cycle(&r.core, now()).await;
    assert_eq!(r.pages().await, 1, "the page, at the bound");
    for _ in 0..3 {
        short.step().await.unwrap();
        cycle(&r.core, now()).await;
    }
    // Once; no alert, no pause; the row is open.
    assert_eq!(
        (r.pages().await, r.alerts().await, r.pauses().await),
        (1, 0, 0)
    );
    assert_eq!(r.verdict().await, None);
    assert_eq!(
        r.f.count("SELECT count(*) FROM observations WHERE position_kind = 'transaction'")
            .await,
        0,
        "nothing was recorded"
    );
    let page: Value =
        sqlx::query_scalar("SELECT payload FROM event_log WHERE kind = 'late-mint-prover-silent'")
            .fetch_one(&r.f.admin)
            .await
            .unwrap();
    assert_eq!(
        page["attempts"][0]["signed_transaction"],
        serde_json::json!(r.signed)
    );
    assert_eq!(page["attempts"][0]["attempt"], serde_json::json!(2));
    assert_eq!(
        page["on_chain_reference"],
        serde_json::json!(format!("0x{}", hex::encode(r.reference)))
    );
    // An operator widens the walk (the endpoint's setting, not changed here); the prover records
    // the inclusion and the row settles. The page stands as the record of the wait.
    r.f.prover().step().await.unwrap();
    cycle(&r.core, now()).await;
    assert_eq!(r.verdict().await.as_deref(), Some("explained"));
    assert_eq!(
        (r.alerts().await, r.pauses().await, r.pages().await),
        (0, 0, 1)
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_key_that_mints_the_reference_is_paged_while_the_prover_is_silent_and_raised_when_it_decides(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let r = retry(pool, connect).await;
    let f = &r.f;
    // The attempt stays out of the chain; the delivery key's thief mints the reference under
    // another nonce, in a call the lane's nonce does not see: the contract holds the reference
    // used, and `Minted` is emitted for it.
    f.chain.use_reference_outside(r.reference);
    f.chain.emit(ContractEvent::Minted {
        seq: 0,
        reference: r.reference,
        asset: AssetId::Here(101),
        to: DESTINATION_ACCOUNT,
        amount: AMOUNT,
    });
    f.chain.produce();
    f.chain.advance(MAX_ERA + 2);
    r.observe().await;
    // The prover is silent: the core waits, pages once at the bound, and raises nothing.
    for _ in 0..HELD_PAGE_AFTER_CYCLES + 3 {
        cycle(&r.core, now()).await;
    }
    assert_eq!(
        (r.pages().await, r.alerts().await, r.pauses().await),
        (1, 0, 0)
    );
    assert_eq!(r.verdict().await, None);
    // The prover decides against the retry: the era is over, its nonce was never consumed, but the
    // contract says the reference is used, so it is an anomaly and no proof (`INV-8`). The alarm
    // raises, once.
    let found = f.prover().step().await.unwrap();
    assert!(
        matches!(found.as_slice(), [(i, Disposition::Unknown(_))] if *i == r.signed),
        "{found:?}"
    );
    cycle(&r.core, now()).await;
    cycle(&r.core, now()).await;
    assert_eq!((r.alerts().await, r.pauses().await), (1, 1));
    assert_eq!(r.verdict().await.as_deref(), Some("alerted"));
    assert_eq!(r.pages().await, 1);
}

/// A signed transaction of the lane in a format `decode_extrinsic` rejects (a trailing byte after
/// the extrinsic), still read as the lane's by its sender: what an earlier build wrote.
fn in_a_format_this_build_rejects(bytes: &[u8]) -> Vec<u8> {
    use bezent_kreivo_endpoint::kreivo;
    let mut old = bytes.to_vec();
    old.push(0);
    assert!(kreivo::decode_extrinsic(&old).is_none());
    assert!(kreivo::signed_sender(&old).is_some());
    old
}

impl Retry {
    /// What the core would do about a transfer whose delivery nobody can read: the transfer's
    /// state, how many refunds exist for it, and the non-execution proofs of attempt 2.
    async fn transfer(&self) -> (String, i64, i64) {
        let state: String =
            sqlx::query_scalar("SELECT state FROM transfers WHERE kind = 'deposit'")
                .fetch_one(&self.f.admin)
                .await
                .unwrap();
        let refunds = self
            .f
            .count("SELECT count(*) FROM transfers WHERE kind = 'refund' OR refund_of IS NOT NULL")
            .await;
        let proofs = self
            .f
            .count(&format!(
                "SELECT count(*) FROM non_execution_proofs WHERE signed_transaction_id = {}",
                self.signed
            ))
            .await;
        (state, refunds, proofs)
    }
}

/// Review of PR #80: a recorded extrinsic this build cannot decode consumes attempt 2's nonce and
/// mints the reference, in a block of attempt 2's era. The prover records an anomaly (nothing is
/// proven, `REQ-NX-3`); that anomaly says the prover cannot read the attempt, never that the
/// attempt did not mint, so the core must not decide the mint on it: it stays held and pages, and
/// raises nothing. The transfer goes to review at once, with no refund (`REQ-RF-1`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_mint_by_a_recorded_attempt_this_build_cannot_read_is_held_and_paged_never_raised(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let r = retry(pool, connect).await;
    let f = &r.f;
    let bytes: Vec<u8> = sqlx::query_scalar("SELECT bytes FROM signed_transactions WHERE id = $1")
        .bind(r.signed)
        .fetch_one(&f.admin)
        .await
        .unwrap();
    // Another recorded signed transaction of the lane, written in the older format; it is the one
    // that lands, takes attempt 2's nonce, and mints.
    let old = in_a_format_this_build_rejects(&bytes);
    sqlx::query(
        "INSERT INTO signed_transactions (intent_id, attempt, bytes, tx_hash) \
         SELECT intent_id, 3, $1, $2 FROM signed_transactions WHERE id = $3",
    )
    .bind(&old)
    .bind(bezent_kreivo_endpoint::kreivo::blake2_256(&old).as_slice())
    .bind(r.signed)
    .execute(&f.admin)
    .await
    .unwrap();
    while f.chain.best() < r.until - 2 {
        f.chain.produce();
    }
    f.chain.consume_nonce(f.lane.account);
    f.chain.include_raw(old);
    f.chain.use_reference_outside(r.reference);
    f.chain.emit(ContractEvent::Minted {
        seq: 0,
        reference: r.reference,
        asset: AssetId::Here(101),
        to: DESTINATION_ACCOUNT,
        amount: AMOUNT,
    });
    f.chain.produce();
    f.chain.advance(MAX_ERA + 2);
    r.observe().await;
    let found = f.prover().step().await.unwrap();
    assert!(
        found
            .iter()
            .any(|(i, d)| *i == r.signed && matches!(d, Disposition::Unknown(_))),
        "{found:?}"
    );
    let marked = f
        .count(
            "SELECT count(*) FROM event_log WHERE kind = 'delivery-anomaly' \
               AND payload ->> 'dedupe' LIKE '%:recorded-undecodable:%' \
               AND payload @> '{\"recorded_undecodable\": true}'",
        )
        .await;
    assert_eq!(marked, 1, "the anomaly carries its marker");
    // The old-format attempt itself has an anomaly of its own, before the chain is read; marked too.
    let bytes_anomalies = |marked: bool| {
        let sql = format!(
            "SELECT count(*) FROM event_log WHERE kind = 'delivery-anomaly' \
               AND payload ->> 'dedupe' LIKE '%:bytes' \
               AND {}(payload @> '{{\"bytes_unreadable\": true}}')",
            if marked { "" } else { "NOT " }
        );
        async move { f.count(&sql).await }
    };
    assert_eq!(
        (bytes_anomalies(true).await, bytes_anomalies(false).await),
        (1, 0)
    );
    // Held, paged at the bound, and nothing raised; however many cycles pass.
    for _ in 0..HELD_PAGE_AFTER_CYCLES + 3 {
        cycle(&r.core, now()).await;
    }
    assert_eq!(
        (r.alerts().await, r.pauses().await, r.pages().await),
        (0, 0, 1)
    );
    assert_eq!(r.verdict().await, None, "the row stays open");
    // The transfer is in review at once; nothing was proven and nothing was refunded.
    assert_eq!(
        r.transfer().await,
        ("Needs-Review".to_owned(), 0, 0),
        "state, refunds, proofs of attempt 2"
    );
    assert!(
        f.count("SELECT count(*) FROM event_log WHERE kind = 'delivery-anomaly-surfaced'")
            .await
            >= 1
    );
}

/// The other side: a mint outside bezent, beside an attempt of bezent's that the chain refused,
/// is still the alarm. The anomaly marker changes nothing for what the prover can decide.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_rogue_mint_beside_an_attempt_refused_on_chain_still_raises(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let r = retry(pool, connect).await;
    let f = &r.f;
    // The reference is minted outside bezent, then attempt 2 is sent in its era and refused.
    f.chain.use_reference_outside(r.reference);
    f.chain.emit(ContractEvent::Minted {
        seq: 0,
        reference: r.reference,
        asset: AssetId::Here(101),
        to: DESTINATION_ACCOUNT,
        amount: AMOUNT,
    });
    f.chain.produce();
    f.chain.refuse_on_chain(true);
    f.chain.refuse_submissions(false);
    assert_eq!(f.submitter().resend().await.unwrap(), 1);
    f.chain.produce();
    f.chain.advance(MAX_ERA + 2);
    r.observe().await;
    let found = f.prover().step().await.unwrap();
    assert!(
        matches!(found.as_slice(), [(i, Disposition::Unknown(_))] if *i == r.signed),
        "{found:?}"
    );
    // The anomaly is the one that says the chain shows the attempt not executed: it decides.
    assert_eq!(
        f.count(
            "SELECT count(*) FROM event_log WHERE kind = 'delivery-anomaly' \
               AND payload ->> 'dedupe' LIKE '%:reference-used' \
               AND NOT (payload ? 'recorded_undecodable')"
        )
        .await,
        1
    );
    cycle(&r.core, now()).await;
    cycle(&r.core, now()).await;
    assert_eq!((r.alerts().await, r.pauses().await), (1, 1));
    assert_eq!(r.verdict().await.as_deref(), Some("alerted"));
}
