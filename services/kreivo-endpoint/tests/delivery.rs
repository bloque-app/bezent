// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-003 with F-007: the two-step delivery of PLAN `DEC-3`, end to end, with every step taken by
//! the process that owns it, each on its own role (`INV-9`, `AC-I1.2`, `REQ-AR-3`).
//!
//! The core (`bezent_core_service`, on a login holding `bezent_core` only) records the order;
//! the endpoint (on a login holding `bezent_kreivo_endpoint` only) prepares it against the in-memory chain, dry-runs
//! it and records the exact bytes; the core records the signing intent over them; the endpoint
//! asks the signer (in process, over its real link) for the signature, checks it, persists the
//! signed bytes and submits them; the core verifies the signature against the intent's bytes and
//! moves the transfer to `Delivering`. Only declarations, the origin network's arrival and the
//! ingress's intake are written as the owner, as the operator and the other services would.

mod common;

use bezent_core_service::{Core, Settings};
use bezent_kreivo_endpoint::chain::fake::DryRunMode;
use bezent_kreivo_endpoint::kreivo;
use bezent_kreivo_endpoint::signal::{self, Seen};
use bezent_kreivo_endpoint::submit::{Prepared, Submitted, signing_intent_payload};
use bezent_signer::compromise::ObservedSignature;
use chrono::{DateTime, Duration, Utc};
use common::*;
use serde_json::{Value, json};
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

async fn state(f: &Fixture, id: i64) -> (String, Option<String>) {
    sqlx::query_as("SELECT state::text, condition::text FROM transfers WHERE id = $1")
        .bind(id)
        .fetch_one(&f.admin)
        .await
        .unwrap()
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

/// DEC-3, INV-9, AC-I1.2: order -> prepare (dry run on the in-memory chain, exact bytes) ->
/// signing intent -> signer -> verified signature -> submission record, each by its own process
/// and role, and the transfer `Delivering` on that record.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_3_order_to_submission_end_to_end(pool: PgPoolOptions, connect: PgConnectOptions) {
    let f = Fixture::new(pool, connect.clone()).await;
    let core = core(&f, &connect).await;
    let now: DateTime<Utc> = "2026-10-13T01:00:00Z".parse().unwrap();
    let id = deposit(&f).await;
    let s = f.submitter();

    // 1. The core: Source-Final at the arrival's final point, then the order. No message, so no
    //    signer would sign it.
    cycle(&core, now).await;
    assert_eq!(state(&f, id).await.0, "Source-Final");
    cycle(&core, now).await;
    let (order, payload) = intent_with(&f, id, "order").await.expect("the order");
    assert_eq!(
        payload,
        json!({"order": {"asset": "Here(101)",
                         "to": format!("0x{}", hex::encode(DESTINATION_ACCOUNT))},
               "amount": AMOUNT.to_string()})
    );
    assert!(intent_with(&f, id, "message").await.is_none());
    let reference: Vec<u8> =
        sqlx::query_scalar("SELECT on_chain_reference FROM intents WHERE id = $1")
            .bind(order)
            .fetch_one(&f.admin)
            .await
            .unwrap();

    // 2. The endpoint finds the core's order and prepares it: an era born at the finalized
    //    head, a dry run that passed, the exact bytes recorded.
    let finalized = f.chain.finalized_ref();
    assert_eq!(s.orders_to_prepare(&finalized).await.unwrap(), vec![order]);
    let prepared = s.prepare(order).await.unwrap();
    let Prepared::Ready {
        id: prepared_id,
        attempt: 1,
        nonce: 0,
        message,
    } = &prepared
    else {
        panic!("not prepared: {prepared:?}");
    };
    // The core has nothing to sign yet; it waits for the preparation, not for a timer.
    assert_eq!(state(&f, id).await.0, "Source-Final");

    // 3. The core records the signing intent over exactly those bytes.
    cycle(&core, now).await;
    let (signing, payload) = intent_with(&f, id, "prepared")
        .await
        .expect("the signing intent");
    assert_eq!(
        payload,
        signing_intent_payload(*prepared_id, message, AMOUNT)
    );
    let recorded: Vec<u8> =
        sqlx::query_scalar("SELECT message FROM prepared_transactions WHERE id = $1")
            .bind(*prepared_id)
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert_eq!(&recorded, message);
    assert_eq!(state(&f, id).await.0, "Source-Final");

    // 4. The endpoint asks the signer, checks the signature, persists the signed bytes and
    //    submits them.
    assert_eq!(s.intents_to_sign(&finalized).await.unwrap(), vec![signing]);
    let submitted = s.sign_and_submit(signing).await.unwrap();
    let Submitted::Submitted {
        id: signed_id,
        hash,
    } = submitted
    else {
        panic!("not submitted: {submitted:?}");
    };
    assert_eq!(f.signatures().await, 1);
    let (intent_of, bytes, tx_hash): (i64, Vec<u8>, Vec<u8>) =
        sqlx::query_as("SELECT intent_id, bytes, tx_hash FROM signed_transactions WHERE id = $1")
            .bind(signed_id)
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert_eq!(intent_of, signing);
    assert_eq!(tx_hash, hash.to_vec());
    assert_eq!(tx_hash, kreivo::blake2_256(&bytes).to_vec());
    assert_eq!(f.chain.submitted(), vec![bytes]);

    // 5. The core verifies the signature against the intent's bytes and moves on the submission
    //    record: Delivering, attempt 1, no compromise raised.
    cycle(&core, now).await;
    assert_eq!(state(&f, id).await, ("Delivering".into(), None));
    let attempt: Option<i32> = sqlx::query_scalar(
        "SELECT attempt FROM transitions WHERE transfer_id = $1 AND to_state = 'Delivering'",
    )
    .bind(id)
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(attempt, Some(1));
    assert_eq!(
        f.count(
            "SELECT count(*) FROM event_log \
             WHERE kind IN ('key-compromise', 'pause-requested', 'delivery-anomaly')"
        )
        .await,
        0
    );
    // One order, one signing intent, one signature, one signed transaction, all for the
    // transfer's own reference.
    assert_eq!(
        f.count(&format!(
            "SELECT count(*) FROM intents WHERE transfer_id = {id} \
             AND on_chain_reference = '\\x{}'",
            hex::encode(&reference)
        ))
        .await,
        2
    );
    assert_eq!(f.count("SELECT count(*) FROM signed_transactions").await, 1);

    // The chain includes it at the lane's nonce.
    f.chain.advance(1);
    assert_eq!(f.chain.nonce(&f.lane.account), 1);
}

/// The bridge's compromise pauses: global, each resting on its alert (migration 0095).
const COMPROMISE_PAUSES: &str = "SELECT count(*) FROM pauses WHERE cause = 'REQ-IC-4' \
     AND asset IS NULL AND network IS NULL AND direction IS NULL AND alert_seq IS NOT NULL";

/// What the endpoint's prover does on a signature that answers no recorded intent, on its own
/// login: nothing but this and the store is running.
async fn detect(f: &Fixture, position_index: i64) -> Seen {
    signal::check_signature(
        &f.endpoint,
        &ObservedSignature {
            network: NETWORK.into(),
            public_key: f.lane.public_key.clone(),
            signature: vec![0x5a; 64],
            block_number: 500,
            block_hash: vec![0xcc; 32],
            position_index,
        },
    )
    .await
    .unwrap()
}

/// `AC-I1.2`, `REQ-IC-4`, `REQ-IC-2`, issue #77: end to end from a detection to signing stopped,
/// each step by the process that owns it, each on its own role. The prover sees a signature bezent
/// did not record; the pause is in force when that write commits, with no person involved; the
/// core records no signing intent for the order it already holds, so the endpoint has nothing to
/// ask the signer for and the signer signs nothing, on any later cycle. The pause is lifted (by
/// the owner here: the two-login path is `CTR-OPS`), and the same order runs to its submission.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_detection_stops_signing_end_to_end(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect.clone()).await;
    let core = core(&f, &connect).await;
    let now: DateTime<Utc> = "2026-10-13T01:00:00Z".parse().unwrap();
    let id = deposit(&f).await;
    let s = f.submitter();
    cycle(&core, now).await;
    cycle(&core, now).await;
    let (order, _) = intent_with(&f, id, "order").await.expect("the order");
    assert!(matches!(
        s.prepare(order).await.unwrap(),
        Prepared::Ready { .. }
    ));

    assert_eq!(detect(&f, 3).await, Seen::Compromise);
    assert_eq!(f.count(COMPROMISE_PAUSES).await, 1, "no pause applied");

    for _ in 0..3 {
        cycle(&core, now).await;
    }
    assert_eq!(
        state(&f, id).await,
        ("Source-Final".into(), Some("CND-Paused".into()))
    );
    assert!(
        intent_with(&f, id, "prepared").await.is_none(),
        "a signing intent was recorded under the pause"
    );
    let finalized = f.chain.finalized_ref();
    assert_eq!(
        s.intents_to_sign(&finalized).await.unwrap(),
        Vec::<i64>::new()
    );
    assert_eq!(f.signatures().await, 0);
    assert!(f.chain.submitted().is_empty());

    // A service cannot lift it, the one that raised it included.
    let lifted = sqlx::raw_sql("DELETE FROM pauses")
        .execute(&f.endpoint)
        .await;
    assert!(lifted.is_err(), "the endpoint lifted a pause");
    assert_eq!(f.count(COMPROMISE_PAUSES).await, 1);

    // Seeing the same signature again changes nothing while paused, and nothing once lifted.
    assert_eq!(detect(&f, 3).await, Seen::Compromise);
    assert_eq!(f.count(COMPROMISE_PAUSES).await, 1);
    exec(&f.admin, "DELETE FROM pauses").await;
    assert_eq!(detect(&f, 3).await, Seen::Compromise);
    assert_eq!(f.count(COMPROMISE_PAUSES).await, 0, "a repeat paused again");

    // The held order resumes by itself.
    cycle(&core, now).await;
    assert_eq!(state(&f, id).await, ("Source-Final".into(), None));
    cycle(&core, now).await;
    let (signing, _) = intent_with(&f, id, "prepared")
        .await
        .expect("the signing intent");
    assert_eq!(s.intents_to_sign(&finalized).await.unwrap(), vec![signing]);
    assert!(matches!(
        s.sign_and_submit(signing).await.unwrap(),
        Submitted::Submitted { .. }
    ));
    cycle(&core, now).await;
    assert_eq!(state(&f, id).await, ("Delivering".into(), None));

    // A new detection after the lift pauses again.
    assert_eq!(detect(&f, 4).await, Seen::Compromise);
    assert_eq!(f.count(COMPROMISE_PAUSES).await, 1);
}

/// `AC-I1.2`, issue #77: the core is down when the endpoint detects. The pause does not wait for
/// it: it is in force on the endpoint's commit, and the core's first cycle back holds new work and
/// records nothing to sign.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_the_core_down_at_detection_the_pause_holds_when_it_returns(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect.clone()).await;
    let id = deposit(&f).await;
    // No core exists yet in this test: only the endpoint and the store are running.
    assert_eq!(detect(&f, 3).await, Seen::Compromise);
    assert_eq!(f.count(COMPROMISE_PAUSES).await, 1, "no pause applied");

    let now: DateTime<Utc> = "2026-10-13T01:00:00Z".parse().unwrap();
    let core = core(&f, &connect).await;
    for _ in 0..3 {
        cycle(&core, now).await;
    }
    assert_eq!(
        state(&f, id).await,
        ("Source-Final".into(), Some("CND-Paused".into()))
    );
    assert_eq!(
        f.count("SELECT count(*) FROM intents").await,
        0,
        "the core ordered new work under the pause"
    );
}

/// `AC-I2.1`, `INV-15`, issue #77: an attempt signed before a detection is in flight, not new work.
/// The pause reaches it between the signature and the core's next step: the core still moves it to
/// `Delivering`, where before it held it in `Source-Final` as `CND-Paused` while the transaction was
/// on its way, so the transfer was reported as waiting for a delivery already made. A compromised
/// key's transaction bezent already signed is not unsigned by a pause; only the on-chain pause
/// refuses it.
///
/// A signing intent recorded before the pause and not yet signed is new work, held by the pause
/// (amendment 0022, A1b): the next test.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_an_attempt_signed_before_the_pause_is_in_flight_and_moves_on(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect.clone()).await;
    let core = core(&f, &connect).await;
    let now: DateTime<Utc> = "2026-10-13T01:00:00Z".parse().unwrap();
    let id = deposit(&f).await;
    let s = f.submitter();
    cycle(&core, now).await;
    cycle(&core, now).await;
    let (order, _) = intent_with(&f, id, "order").await.expect("the order");
    assert!(matches!(
        s.prepare(order).await.unwrap(),
        Prepared::Ready { .. }
    ));
    cycle(&core, now).await;
    let (signing, _) = intent_with(&f, id, "prepared")
        .await
        .expect("the signing intent");
    assert!(matches!(
        s.sign_and_submit(signing).await.unwrap(),
        Submitted::Submitted { .. }
    ));
    assert_eq!(f.signatures().await, 1);

    assert_eq!(detect(&f, 3).await, Seen::Compromise);
    assert_eq!(f.count(COMPROMISE_PAUSES).await, 1, "no pause applied");
    cycle(&core, now).await;
    assert_eq!(state(&f, id).await, ("Delivering".into(), None));
    assert_eq!(f.count(COMPROMISE_PAUSES).await, 1, "the pause stands");
}

/// `AC-I2.1`, `INV-15`, `REQ-GV-1`, `NFR-8` (amendment 0022, A1b, ruled `d-pause-0022`, 2026-11-06),
/// end to end on each process's own role: the core records a signing intent, and a moment later a
/// detection pauses the bridge before the endpoint asks for the signature. The intent is new work:
/// the signer refuses it, the endpoint keeps it and signs nothing, the core holds the transfer in
/// `Source-Final` as `CND-Paused`, on every later cycle. Once the pause is lifted (by the owner
/// here: the two-login path is `CTR-OPS`), the same intent is signed and submitted, within its era.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_a1b_an_intent_recorded_a_moment_before_the_pause_is_not_signed_until_it_is_lifted(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect.clone()).await;
    let core = core(&f, &connect).await;
    let now: DateTime<Utc> = "2026-10-13T01:00:00Z".parse().unwrap();
    let id = deposit(&f).await;
    let s = f.submitter();
    cycle(&core, now).await;
    cycle(&core, now).await;
    let (order, _) = intent_with(&f, id, "order").await.expect("the order");
    assert!(matches!(
        s.prepare(order).await.unwrap(),
        Prepared::Ready { .. }
    ));
    cycle(&core, now).await;
    let (signing, _) = intent_with(&f, id, "prepared")
        .await
        .expect("the signing intent, recorded before the pause");

    assert_eq!(detect(&f, 3).await, Seen::Compromise);
    assert_eq!(f.count(COMPROMISE_PAUSES).await, 1, "no pause applied");
    for _ in 0..3 {
        let Submitted::NotSigned(why) = s.sign_and_submit(signing).await.unwrap() else {
            panic!("an intent recorded before the pause was signed under it");
        };
        assert!(why.contains("REQ-GV-1"), "{why}");
        cycle(&core, now).await;
    }
    assert_eq!(f.signatures().await, 0);
    assert_eq!(f.count("SELECT count(*) FROM signed_transactions").await, 0);
    assert_eq!(
        state(&f, id).await,
        ("Source-Final".into(), Some("CND-Paused".into()))
    );

    exec(&f.admin, "DELETE FROM pauses").await;
    assert!(matches!(
        s.sign_and_submit(signing).await.unwrap(),
        Submitted::Submitted { .. }
    ));
    assert_eq!(f.signatures().await, 1);
    for _ in 0..2 {
        cycle(&core, now).await;
    }
    assert_eq!(state(&f, id).await, ("Delivering".into(), None));
}

/// `INV-15`, `REQ-GV-1`, issue #77 review: an attempt whose signature is recorded passes the pause
/// as in flight, but a preparation made after it that fails its dry run must not order a new
/// transaction under the pause. The signer recorded its signature and the endpoint went down before
/// persisting the signed transaction; the era passed; the endpoint prepared the order again and
/// the dry run was refused. Before the fix the core, with the gate open for the signed attempt,
/// recorded a new order for it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_15_a_signed_attempt_whose_new_preparation_fails_orders_nothing_under_a_pause(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect.clone()).await;
    let core = core(&f, &connect).await;
    let now: DateTime<Utc> = "2026-10-13T01:00:00Z".parse().unwrap();
    let id = deposit(&f).await;
    let s = f.submitter();
    cycle(&core, now).await;
    cycle(&core, now).await;
    let (order, _) = intent_with(&f, id, "order").await.expect("the order");
    assert!(matches!(
        s.prepare(order).await.unwrap(),
        Prepared::Ready { .. }
    ));
    cycle(&core, now).await;
    let (signing, _) = intent_with(&f, id, "prepared")
        .await
        .expect("the signing intent");
    // The signer's signature is recorded; the endpoint never persisted the signed transaction.
    exec(
        &f.admin,
        &format!(
            "INSERT INTO signatures (intent_id, key_id, scheme, public_key, signature, amount) \
             VALUES ({signing}, 'lane', 'sr25519', '\\x01', '\\x02', 0)"
        ),
    )
    .await;
    // The era passes; the order is prepared again and its dry run is refused.
    f.chain.advance(kreivo::MAX_ERA + 1);
    f.chain.set_dry_run(DryRunMode::Refuse(6));
    let finalized = f.chain.finalized_ref();
    assert_eq!(s.orders_to_prepare(&finalized).await.unwrap(), vec![order]);
    assert!(matches!(
        s.prepare(order).await.unwrap(),
        Prepared::DryRunFailed { .. }
    ));

    let orders = "SELECT count(*) FROM intents WHERE payload ? 'order'";
    let before = f.count(orders).await;
    assert_eq!(detect(&f, 3).await, Seen::Compromise);
    assert_eq!(f.count(COMPROMISE_PAUSES).await, 1, "no pause applied");
    for _ in 0..3 {
        cycle(&core, now).await;
    }
    assert_eq!(
        f.count(orders).await,
        before,
        "the core ordered new work under the pause"
    );
}
