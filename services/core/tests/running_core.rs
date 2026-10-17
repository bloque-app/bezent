// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #42: the reserve check and the daily statement as the running core wires them. The check
//! reads what the endpoints recorded at final heads (`reserve_readings`), never a fake (`REQ-BK-8`,
//! `NFR-5`, `INV-1`, `REQ-IC-2`); the book and the statement are cut by commit order (PR #40 second
//! round, (1)); the statement is signed by the signer process through a recorded intent of purpose
//! `statement`, verified before anything is published (`REQ-PR-1`, `REQ-PR-2`, `INV-9`; PLAN
//! `DEC-16`, 2026-10-14).

mod common;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bezent_core_service::reserve::book::{self, Cost};
use bezent_core_service::reserve::check::{Outcome, ReserveCheck};
use bezent_core_service::reserve::readings::{Readings, Recorded};
use bezent_core_service::reserve::statement::{
    DirectoryStore, KeyBackendSigner, SignerProcess, StatementError, StatementSigner,
    StatementStore, Statements, StoreError, verify, verify_chain,
};
use bezent_signer::config::{Caller, Config as SignerConfig};
use bezent_signer::keys::{FileBackend, KeyBackend, KeyError};
use bezent_signer::link::{self, Callers};
use bezent_signer::log::Log;
use bezent_signer::signer::Signer;
use chrono::Duration;
use common::reserve::*;
use common::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// Records a reading at (`network`, `block`) as the network's endpoint would.
#[allow(clippy::too_many_arguments)]
async fn reading(
    pool: &PgPool,
    network: &str,
    block: i64,
    kind: &str,
    subject: &str,
    holder: &str,
    amount: Option<u128>,
    unknown: Option<&str>,
) {
    sqlx::query(
        "INSERT INTO reserve_readings (network, block_number, block_hash, kind, subject, holder, \
             amount, unknown_reason) VALUES ($1, $2, $3, $4, $5, $6, $7::numeric, $8)",
    )
    .bind(network)
    .bind(block)
    .bind(hash(network, block))
    .bind(kind)
    .bind(subject)
    .bind(holder)
    .bind(amount.map(|a| a.to_string()))
    .bind(unknown)
    .execute(pool)
    .await
    .unwrap();
}

/// Both networks read up to `block`, with supply and the reserve's balance recorded there.
async fn recorded_at(
    w: &World,
    block: i64,
    supply: u128,
    reserve: Option<u128>,
    why: Option<&str>,
) {
    read_all(&w.admin, block).await;
    reading(
        &w.admin,
        ISSUING,
        block,
        "supply",
        ISSUING_ASSET,
        "",
        Some(supply),
        None,
    )
    .await;
    reading(
        &w.admin, ORIGIN, block, "reserve", TOKEN, RESERVE, reserve, why,
    )
    .await;
}

fn running_check(w: &World) -> (ReserveCheck<Recorded, FakePager>, FakePager) {
    let pager = FakePager::default();
    (
        ReserveCheck::new(
            w.core.pool().clone(),
            Recorded::new(w.core.pool().clone()),
            pager.clone(),
        ),
        pager,
    )
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_8_nfr_5_the_running_check_reads_what_the_endpoints_recorded_at_its_final_points(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000).await;
    recorded_at(&w, 200, 0, Some(1_000), None).await;
    let (check, pager) = running_check(&w);

    let e = check.evaluate(ASSET, t0()).await.unwrap();
    assert_eq!(
        e.outcome,
        Outcome::Holds,
        "{:?} {:?}",
        e.reason,
        e.differences
    );
    // Every term at the one final point per network the readings rest on.
    assert_eq!(e.points[ISSUING].block_number, 200);
    assert_eq!(e.points[ORIGIN].block_number, 200);
    assert_eq!(e.chain.reserve[ORIGIN], 1_000);
    assert_eq!(e.chain, e.book);

    // A head with no readings recorded: incomplete, never taken as holding.
    read_all(&w.admin, 300).await;
    let e = check
        .evaluate(ASSET, t0() + Duration::seconds(30))
        .await
        .unwrap();
    assert_eq!(e.outcome, Outcome::Incomplete);
    assert!(
        e.reason
            .as_deref()
            .is_some_and(|r| r.contains("no supply reading")),
        "{:?}",
        e.reason
    );

    // A balance the endpoint read differently from the book diverges: paused and paged.
    recorded_at(&w, 400, 0, Some(999), None).await;
    let e = check
        .evaluate(ASSET, t0() + Duration::seconds(40))
        .await
        .unwrap();
    assert_eq!(e.outcome, Outcome::Diverges);
    assert!(pager.kinds().contains(&"reserve-divergence".to_owned()));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ic_2_dec_12_a_reading_the_providers_disagree_on_is_incomplete_and_pauses_past_the_bound(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000).await;
    recorded_at(
        &w,
        200,
        0,
        None,
        Some("DEC-12: the providers disagree on the balance: a answers 1000, b answers 1001"),
    )
    .await;
    let (check, pager) = running_check(&w);
    let e = check.evaluate(ASSET, t0()).await.unwrap();
    // Unknown: never the higher, never the lower.
    assert_eq!(e.outcome, Outcome::Incomplete);
    assert!(
        e.reason.as_deref().is_some_and(|r| r.contains("DEC-12")),
        "{:?}",
        e.reason
    );
    assert_eq!(
        w.count("SELECT count(*) FROM pauses WHERE asset = 'bUSDC'")
            .await,
        0
    );
    // Still unknown past the network's bound (a minute): the asset's routes pause (REQ-IC-2).
    let e = check
        .evaluate(ASSET, t0() + Duration::minutes(2))
        .await
        .unwrap();
    assert_eq!(e.outcome, Outcome::Incomplete);
    assert_eq!(
        w.count("SELECT count(*) FROM pauses WHERE asset = 'bUSDC' AND cause = 'REQ-IC-2'")
            .await,
        1
    );
    assert!(pager.kinds().contains(&"reserve-check-stalled".to_owned()));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_be_4_attested_reserve_reports_are_a_stub_no_attested_assets_configured(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let err = Recorded::new(w.core.pool().clone())
        .attested(ASSET, "swaps-test")
        .await
        .unwrap_err();
    assert!(err.0.contains("no attested assets configured"), "{err}");
}

/// A world the fake networks agree with, evaluated once (the statement states that evaluation).
async fn evaluated_world(w: &World) -> FakeReadings {
    fund(w, 5, 1_000_000).await;
    read_all(&w.admin, 200).await;
    let (check, readings, _) = checker(w);
    sync_chain(&w.admin, &readings).await;
    let e = check.evaluate(ASSET, t0()).await.unwrap();
    assert_eq!(e.outcome, Outcome::Holds, "{:?}", e.differences);
    readings
}

fn store_dir(label: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("bz42-{label}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn statement_key() -> FileBackend {
    let seed: [u8; 32] = Sha256::digest(b"bezent issue 42 statement key").into();
    FileBackend::parse(
        json!({"comment": "TEST KEY", "environment": "test",
               "keys": [{"id": "statement", "scheme": "ed25519",
                         "seed": format!("0x{}", hex::encode(seed))}]})
        .to_string()
        .as_bytes(),
    )
    .unwrap()
}

/// PR #40 second round, (1): an entry with a lower id whose transaction commits after the
/// evaluation read the book is in neither the evaluation's book nor its statement; the statement's
/// movements add up to the surplus the evaluation's book holds (`REQ-PR-1`, `REQ-BK-4`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pr_1_req_bk_4_the_book_and_the_statement_are_cut_by_commit_order_not_by_id(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000_000).await;
    read_all(&w.admin, 200).await;
    // A cost whose transaction is still open: the lower id.
    let mut slow = w.core.pool().begin().await.unwrap();
    let slow_id = book::cost(
        &mut slow,
        ASSET,
        Cost::Network,
        ORIGIN,
        25,
        "gas:slow",
        None,
    )
    .await
    .unwrap();
    // A cost recorded and committed after it: the higher id.
    let mut conn = w.core.pool().acquire().await.unwrap();
    let fast_id = book::cost(
        &mut conn,
        ASSET,
        Cost::Network,
        ORIGIN,
        10,
        "gas:fast",
        None,
    )
    .await
    .unwrap();
    drop(conn);
    assert!(slow_id < fast_id);
    // The networks show the committed cost only.
    let (check, readings, _) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    *readings
        .chain()
        .reserve
        .get_mut(&(TOKEN.to_owned(), RESERVE.to_owned()))
        .unwrap() -= 10;
    let e = check.evaluate(ASSET, t0()).await.unwrap();
    assert_eq!(e.outcome, Outcome::Holds, "{:?}", e.differences);
    assert_eq!(e.book.surplus, 1_000_000 - 10);
    // The slow one commits after the evaluation read the book.
    slow.commit().await.unwrap();

    let dir = store_dir("cut");
    let backend = statement_key();
    let public = backend.public_key("statement").unwrap();
    let statements = Statements::new(
        w.core.pool().clone(),
        KeyBackendSigner::new(backend, "statement").unwrap(),
        DirectoryStore::new(&dir),
    );
    let p = statements.publish(ASSET, t0()).await.unwrap();
    let sig = statements
        .store()
        .get(&p.signature_name)
        .await
        .unwrap()
        .unwrap();
    let doc = verify(&p.bytes, &sig, &public).unwrap();
    let surplus = &doc["terms"]["surplus"];
    let movements = surplus["movements"].as_array().unwrap();
    let references: Vec<&str> = movements
        .iter()
        .map(|m| m["reference"].as_str().unwrap())
        .collect();
    assert!(references.contains(&"gas:fast"), "{references:?}");
    assert!(!references.contains(&"gas:slow"), "{references:?}");
    let sum: i128 = movements
        .iter()
        .map(|m| m["amount"].as_str().unwrap().parse::<i128>().unwrap())
        .sum();
    assert_eq!(sum.to_string(), surplus["book_amount"].as_str().unwrap());
    std::fs::remove_dir_all(dir).ok();
}

/// The current user's id, as the kernel reports it to a peer.
fn my_uid() -> u32 {
    let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
    a.set_nonblocking(true).unwrap();
    tokio::net::UnixStream::from_std(a)
        .unwrap()
        .peer_cred()
        .unwrap()
        .uid()
}

const LINK_TOKEN: &str = "core-statement-token-for-tests";

/// Starts the signer process's link with the statement key, in process; returns its socket and the
/// statement key's public key.
async fn signer_with_statement_key(connect: &PgConnectOptions, dir: &Path) -> Vec<u8> {
    signer_with_statement_seed(connect, dir, b"bezent issue 42 statement key").await
}

/// As [`signer_with_statement_key`], with the statement key made from `label`: another label is
/// a rotated statement key.
async fn signer_with_statement_seed(
    connect: &PgConnectOptions,
    dir: &Path,
    label: &[u8],
) -> Vec<u8> {
    let seed: [u8; 32] = Sha256::digest(label).into();
    let mint: [u8; 32] = Sha256::digest(b"bezent issue 42 mint key").into();
    let keys = dir.join("keys.json");
    std::fs::write(
        &keys,
        serde_json::to_vec(&json!({"comment": "TEST KEYS", "environment": "test", "keys": [
            {"id": "statement", "scheme": "ed25519", "seed": format!("0x{}", hex::encode(seed))},
            {"id": "kreivo-mint", "scheme": "sr25519", "seed": format!("0x{}", hex::encode(mint))},
        ]}))
        .unwrap(),
    )
    .unwrap();
    let caller = Caller {
        name: "core".into(),
        uid: my_uid(),
        token_sha256: vec![hex::encode(Sha256::digest(LINK_TOKEN.as_bytes()))],
    };
    let config = SignerConfig::parse(
        &serde_json::to_vec(&json!({
            "socket": dir.join("s.sock"),
            "callers": [{"name": caller.name, "uid": caller.uid,
                         "token_sha256": caller.token_sha256}],
            "networks": [{"id": ISSUING, "environment": "test", "issuing": true,
                          "contract_account": "0xc0c0c0", "genesis": format!("0x{}", "6b".repeat(32))}],
            "keys": {"backend": "file", "path": keys},
            "bindings": [{"key": "kreivo-mint", "network": ISSUING, "purposes": ["mint"],
                          "window_cap": "1000", "window_seconds": 3600}],
            "statement_key": "statement",
        }))
        .unwrap(),
    )
    .unwrap();
    let backend = FileBackend::load(&keys).unwrap();
    let public = backend.public_key("statement").unwrap();
    let policy = config.policy(&backend).unwrap();
    let owner = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(connect.clone())
        .await
        .unwrap();
    let pool = login_pool(&owner, SIGNER_LOGIN, bezent_store::SIGNER_ROLE, 4).await;
    owner.close().await;
    let (log, _captured) = Log::capture();
    let signer = Arc::new(Signer::new(pool, policy, backend, log));
    let listener = link::bind(&dir.join("s.sock")).unwrap();
    tokio::spawn(link::serve(
        listener,
        Arc::new(Callers::sharing_the_signer_uid_in_tests(&[caller])),
        signer,
    ));
    public
}

fn signer_process(w: &World, dir: &Path, public: &[u8]) -> SignerProcess {
    SignerProcess::new(
        w.core.pool().clone(),
        dir.join("s.sock"),
        my_uid(),
        LINK_TOKEN.into(),
        public.to_vec(),
    )
    .unwrap()
}

/// PLAN DEC-16 (2026-10-14): the statement is signed by the signer process through a recorded
/// intent of purpose `statement`; the core verifies the signature before it publishes, and holds
/// only the key's public half (`REQ-PR-1`, `REQ-AR-3`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pr_1_dec_16_the_statement_is_signed_by_the_signer_through_a_recorded_intent(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect.clone()).await;
    evaluated_world(&w).await;
    let dir = store_dir("signer");
    let public = signer_with_statement_key(&connect, &dir).await;
    let statements = Statements::new(
        w.core.pool().clone(),
        signer_process(&w, &dir, &public),
        DirectoryStore::new(dir.join("statements")),
    );
    let p = statements.publish(ASSET, t0()).await.unwrap();

    // The intent the core recorded: the exact bytes, nothing of a transfer.
    let (intent, purpose, network, payload): (i64, String, String, Value) = sqlx::query_as(
        "SELECT id, purpose, network, payload FROM intents WHERE purpose = 'statement'",
    )
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!((purpose.as_str(), network.as_str()), ("statement", ISSUING));
    assert_eq!(
        payload["message"],
        format!("0x{}", hex::encode(&p.bytes)).as_str()
    );
    assert_eq!(payload["statement"]["asset"], ASSET);
    assert_eq!(payload["statement"]["sequence"], 1);
    // Signed by the statement key, recorded by the signer.
    let (key, scheme): (String, String) =
        sqlx::query_as("SELECT key_id, scheme FROM signatures WHERE intent_id = $1")
            .bind(intent)
            .fetch_one(&w.admin)
            .await
            .unwrap();
    assert_eq!((key.as_str(), scheme.as_str()), ("statement", "ed25519"));
    // And published: the document verifies with the statement key.
    let sig = statements
        .store()
        .get(&p.signature_name)
        .await
        .unwrap()
        .unwrap();
    let doc = verify(&p.bytes, &sig, &public).unwrap();
    assert_eq!(doc["sequence"], 1);
    std::fs::remove_dir_all(dir).ok();
}

/// The published fixtures agree: the core's configuration names, by its public half, the statement
/// key the signer's fixture holds and names apart (`REQ-PR-1`; issue #42).
#[test]
fn req_pr_1_the_fixture_core_names_the_fixture_signers_statement_key() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let core = bezent_core_service::Config::from_file(&here.join("fixtures/core.toml")).unwrap();
    let signer = SignerConfig::load(&here.join("../signer/fixtures/signer.json")).unwrap();
    let keys = FileBackend::load(&here.join("../signer/fixtures/test-keys.json")).unwrap();
    assert_eq!(
        signer.policy(&keys).unwrap().statement_key(),
        Some("statement")
    );
    assert_eq!(
        core.statement
            .unwrap()
            .configured()
            .unwrap()
            .public_key_bytes()
            .unwrap(),
        keys.public_key("statement").unwrap()
    );
}

/// PR #50 re-check M-1: the fixture's `environment = "test"` and its published test statement key
/// hold only while the store declares test networks only, read through the core's own login with
/// the rule the sidecar uses. One production network declared, and the same file is refused.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m_1_req_ky_6_a_test_configuration_is_refused_on_a_store_with_a_production_network(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = bezent_core_service::Config::from_file(&here.join("fixtures/core.toml")).unwrap();
    assert!(fixture.needs_test_store());
    let production = bezent_store::environment::production_networks(w.core.pool())
        .await
        .unwrap();
    assert!(production.is_empty(), "{production:?}");
    fixture.confirm_environment(&production).unwrap();

    exec(
        &w.admin,
        "INSERT INTO networks (network, role, address_format, backend) \
         VALUES ('base', 'origin', 'evm', 'evm:8453')",
    )
    .await;
    let production = bezent_store::environment::production_networks(w.core.pool())
        .await
        .unwrap();
    assert_eq!(production, vec!["base".to_owned()]);
    let err = fixture.confirm_environment(&production).unwrap_err();
    assert!(err.contains("environment = \"test\" refused"), "{err}");
}

/// A signer that answers with a signature that does not verify.
struct Forging(Vec<u8>);

impl StatementSigner for Forging {
    fn public_key(&self) -> Result<Vec<u8>, KeyError> {
        Ok(self.0.clone())
    }

    async fn sign(&self, _message: &[u8]) -> Result<Vec<u8>, KeyError> {
        Ok(vec![0x5a; 64])
    }
}

/// `INV-9`: a statement whose returned signature does not verify with the statement key is never
/// published nor recorded.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_9_a_statement_whose_signature_does_not_verify_is_never_published(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    evaluated_world(&w).await;
    let dir = store_dir("forged");
    let public = statement_key().public_key("statement").unwrap();
    let statements = Statements::new(
        w.core.pool().clone(),
        Forging(public),
        DirectoryStore::new(&dir),
    );
    let err = statements.publish(ASSET, t0()).await.unwrap_err();
    assert!(matches!(err, StatementError::Key(_)), "{err}");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    assert_eq!(w.count("SELECT count(*) FROM reserve_statements").await, 0);
    std::fs::remove_dir_all(dir).ok();
}

/// A store whose writes fail while `down` is set.
struct Flaky {
    inner: DirectoryStore,
    down: Arc<AtomicBool>,
}

impl StatementStore for Flaky {
    async fn put(&self, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
        if self.down.load(Ordering::SeqCst) {
            return Err(StoreError::Io("the bucket is unreachable".into()));
        }
        self.inner.put(name, bytes).await
    }

    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.get(name).await
    }
}

/// `REQ-PR-2`: a statement the key signed whose publish was cut short is that place's statement:
/// the next publish puts it out as signed, and the key never signs another for the same place.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pr_2_a_statement_signed_but_not_stored_is_published_as_signed(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect.clone()).await;
    let readings = evaluated_world(&w).await;
    let dir = store_dir("flaky");
    let public = signer_with_statement_key(&connect, &dir).await;
    let down = Arc::new(AtomicBool::new(true));
    let statements = Statements::new(
        w.core.pool().clone(),
        signer_process(&w, &dir, &public),
        Flaky {
            inner: DirectoryStore::new(dir.join("statements")),
            down: down.clone(),
        },
    );
    let err = statements.publish(ASSET, t0()).await.unwrap_err();
    assert!(matches!(err, StatementError::Store(_)), "{err}");
    let signed: String = scalar(
        &w.admin,
        "SELECT i.payload ->> 'message' FROM intents i JOIN signatures g ON g.intent_id = i.id \
         WHERE i.purpose = 'statement'",
    )
    .await;

    // Later, with a newer evaluation, the store is back.
    down.store(false, Ordering::SeqCst);
    read_all(&w.admin, 400).await;
    let (check, _, _) = checker(&w);
    {
        let mut c = check.readings().chain();
        let fresh = readings.chain();
        c.supply = fresh.supply.clone();
        c.reserve = fresh.reserve.clone();
    }
    let later = t0() + Duration::minutes(5);
    assert_eq!(
        check.evaluate(ASSET, later).await.unwrap().outcome,
        Outcome::Holds
    );
    let p = statements.publish(ASSET, later).await.unwrap();
    assert_eq!(format!("0x{}", hex::encode(&p.bytes)), signed);
    assert_eq!(p.sequence, 1);
    assert_eq!(
        w.count("SELECT count(*) FROM intents WHERE purpose = 'statement'")
            .await,
        1
    );
    let sig = statements
        .store()
        .get(&p.signature_name)
        .await
        .unwrap()
        .unwrap();
    verify(&p.bytes, &sig, &public).unwrap();
    std::fs::remove_dir_all(dir).ok();
}

/// A supply reading at (`ISSUING`, `block`) as the Kreivo endpoint records it: the issuance is the
/// amount, and a component count that differs flags it (`services/kreivo-endpoint/src/supply.rs`).
async fn supply_read(w: &World, block: i64, contract: u128, issuance: u128) {
    sqlx::query(
        "INSERT INTO reserve_readings (network, block_number, block_hash, kind, subject, holder, \
             amount, mismatch, detail) \
         VALUES ($1, $2, $3, 'supply', $4, '', $5::numeric, $6, $7::text::jsonb)",
    )
    .bind(ISSUING)
    .bind(block)
    .bind(hash(ISSUING, block))
    .bind(ISSUING_ASSET)
    .bind(issuance.to_string())
    .bind(contract != issuance)
    .bind(
        json!({"contract_supply": contract.to_string(), "issuance": issuance.to_string()})
            .to_string(),
    )
    .execute(&w.admin)
    .await
    .unwrap();
}

/// Both networks read up to `block`: supply as the endpoint records it, and the reserve.
async fn read_at(w: &World, block: i64, contract: u128, issuance: u128, reserve: u128) {
    read_all(&w.admin, block).await;
    supply_read(w, block, contract, issuance).await;
    reading(
        &w.admin,
        ORIGIN,
        block,
        "reserve",
        TOKEN,
        RESERVE,
        Some(reserve),
        None,
    )
    .await;
}

/// PR #50 review M1; PLAN DEC-16, 2026-10-15: a difference between the on-chain component's supply
/// count and the issuance is a breach of INV-4. The core reads the flag the endpoint recorded,
/// raises key-compromise once per mismatch, and pauses the asset's routes under INV-4; the pause
/// lasts until the operator lifts it, and while the mismatch stands the check never holds, not
/// even after a book correction that matches the issuance.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_4_a_supply_mismatch_raises_key_compromise_once_and_pauses_until_the_operator_lifts_it(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000).await;
    let (check, pager) = running_check(&w);
    let alerts = "SELECT count(*) FROM event_log WHERE kind = 'key-compromise' \
                  AND payload ->> 'spec' LIKE 'INV-4%' AND payload ->> 'asset' = 'bUSDC'";
    let paused = "SELECT count(*) FROM pauses WHERE asset = 'bUSDC' AND cause = 'INV-4'";

    // Scenario 2: the component counts 5 while the issuance equals the book (0), for example a
    // counter bug, or a destroy outside the component matched by a mint outside it. The equation
    // itself holds; the breach does not let the check say so.
    read_at(&w, 200, 5, 0, 1_000).await;
    let e = check.evaluate(ASSET, t0()).await.unwrap();
    assert_eq!(e.outcome, Outcome::Diverges, "{:?}", e.differences);
    assert_eq!(
        e.differences
            .iter()
            .map(|d| d.term.as_str())
            .collect::<Vec<_>>(),
        vec!["supply_mismatch"]
    );
    assert_eq!(w.count(alerts).await, 1);
    assert_eq!(w.count(paused).await, 1);
    let kinds = pager.kinds();
    assert!(kinds.contains(&"key-compromise".to_owned()), "{kinds:?}");
    assert!(
        kinds.contains(&"supply-mismatch-pause".to_owned()),
        "{kinds:?}"
    );
    let raised: Value = sqlx::query_scalar(
        "SELECT payload FROM event_log WHERE kind = 'key-compromise' ORDER BY seq LIMIT 1",
    )
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(raised["scope"], "asset");
    assert_eq!(raised["difference"], "-5");
    assert_eq!(raised["actions"], json!(["pause", "rotate", "report"]));

    // Once: a later evaluation of the same mismatch raises nothing new.
    let e = check
        .evaluate(ASSET, t0() + Duration::seconds(30))
        .await
        .unwrap();
    assert_eq!(e.outcome, Outcome::Diverges);
    assert_eq!(w.count(alerts).await, 1);
    assert_eq!(w.count(paused).await, 1);

    // Lifted by the operator while the mismatch stands: paused again at once.
    exec(&w.admin, "DELETE FROM pauses").await;
    check
        .evaluate(ASSET, t0() + Duration::seconds(60))
        .await
        .unwrap();
    assert_eq!(w.count(paused).await, 1);
    assert_eq!(w.count(alerts).await, 1);

    // Scenario 1: someone with asset-admin power mints 7 outside the component. The issuance is
    // the book's supply plus 7: a supply difference, and a new mismatch, raised on its own.
    read_at(&w, 300, 0, 7, 1_000).await;
    let e = check
        .evaluate(ASSET, t0() + Duration::seconds(90))
        .await
        .unwrap();
    assert_eq!(e.outcome, Outcome::Diverges);
    assert!(e.differences.iter().any(|d| d.term == "supply"));
    assert_eq!(w.count(alerts).await, 2);
    // The operator books a correction that matches the issuance (REQ-BK-7): the equation holds
    // again, but the mismatch stands, so the check still does not hold, and the pause stays.
    let original: i64 = scalar(&w.admin, "SELECT max(id) FROM book_entries").await;
    let mut conn = w.core.pool().acquire().await.unwrap();
    book::correction(
        &mut conn,
        original,
        ASSET,
        "the issuance outside the component, matched",
        vec![
            book::posting("surplus", None, book::Side::Debit, 7, None),
            book::posting("supply", None, book::Side::Credit, 7, None),
        ],
    )
    .await
    .unwrap();
    drop(conn);
    let e = check
        .evaluate(ASSET, t0() + Duration::seconds(120))
        .await
        .unwrap();
    assert_eq!(
        e.differences
            .iter()
            .map(|d| d.term.as_str())
            .collect::<Vec<_>>(),
        vec!["supply_mismatch"]
    );
    assert_eq!(e.outcome, Outcome::Diverges);
    assert_eq!(w.count(paused).await, 1);

    // The component's count agrees again: the check holds, and the INV-4 pause stays until the
    // operator lifts it; no evaluation lifts it.
    read_at(&w, 400, 7, 7, 1_000).await;
    let e = check
        .evaluate(ASSET, t0() + Duration::seconds(150))
        .await
        .unwrap();
    assert_eq!(e.outcome, Outcome::Holds, "{:?}", e.differences);
    assert_eq!(w.count(paused).await, 1);
    exec(&w.admin, "DELETE FROM pauses").await;
    check
        .evaluate(ASSET, t0() + Duration::seconds(180))
        .await
        .unwrap();
    assert_eq!(w.count(paused).await, 0);
    assert_eq!(w.count(alerts).await, 2);

    // A mismatch recorded at a head the check never reads at (gone by its point) is still raised
    // and still pauses: the flag is read from the store, not from the point alone.
    read_through(&w.admin, ISSUING, 450).await;
    supply_read(&w, 450, 9, 7).await;
    read_at(&w, 500, 7, 7, 1_000).await;
    let e = check
        .evaluate(ASSET, t0() + Duration::seconds(210))
        .await
        .unwrap();
    assert_eq!(e.points[ISSUING].block_number, 500);
    assert_eq!(e.outcome, Outcome::Holds, "{:?}", e.differences);
    assert_eq!(w.count(alerts).await, 3);
    assert_eq!(w.count(paused).await, 1);
}

/// PR #50 re-check S-2: the key-compromise alert is kept to once per mismatch event, not once per
/// size of difference. A mismatch that clears (the counts agree again) and later comes back with
/// the same difference is a new event, raised again; one that stands is raised once.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s_2_inv_4_a_mismatch_of_the_same_size_after_it_cleared_is_raised_again(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000).await;
    let (check, _pager) = running_check(&w);
    let alerts = "SELECT count(*) FROM event_log WHERE kind = 'key-compromise' \
                  AND payload ->> 'spec' LIKE 'INV-4%' AND payload ->> 'asset' = 'bUSDC'";

    // A mismatch of -5, raised once however often it is evaluated while it stands.
    read_at(&w, 200, 5, 0, 1_000).await;
    check.evaluate(ASSET, t0()).await.unwrap();
    read_at(&w, 250, 5, 0, 1_000).await;
    check
        .evaluate(ASSET, t0() + Duration::seconds(30))
        .await
        .unwrap();
    assert_eq!(w.count(alerts).await, 1);

    // The counts agree again: the event is over.
    read_at(&w, 300, 0, 0, 1_000).await;
    let e = check
        .evaluate(ASSET, t0() + Duration::seconds(60))
        .await
        .unwrap();
    assert_eq!(e.outcome, Outcome::Holds, "{:?}", e.differences);
    assert_eq!(w.count(alerts).await, 1);

    // The same difference again: a new event, raised again, naming its own first reading.
    read_at(&w, 400, 5, 0, 1_000).await;
    let e = check
        .evaluate(ASSET, t0() + Duration::seconds(90))
        .await
        .unwrap();
    assert_eq!(e.outcome, Outcome::Diverges);
    assert_eq!(w.count(alerts).await, 2);
    let blocks: Vec<i64> = sqlx::query_scalar(
        "SELECT (payload ->> 'block_number')::bigint FROM event_log \
         WHERE kind = 'key-compromise' ORDER BY seq",
    )
    .fetch_all(&w.admin)
    .await
    .unwrap();
    assert_eq!(blocks, vec![200, 400]);
    check
        .evaluate(ASSET, t0() + Duration::seconds(120))
        .await
        .unwrap();
    assert_eq!(w.count(alerts).await, 2);
}

/// The one statement intent signed so far: its bytes (hex) and their SHA-256 as it names them.
async fn signed_intent(w: &World) -> (String, String) {
    sqlx::query_as(
        "SELECT i.payload ->> 'message', i.payload -> 'statement' ->> 'sha256' FROM intents i \
         JOIN signatures g ON g.intent_id = i.id WHERE i.purpose = 'statement'",
    )
    .fetch_one(&w.admin)
    .await
    .unwrap()
}

/// PR #50 re-check S-1 (and review M3): the statement key is rotated while statement 1 is signed
/// under the old key and not stored yet. The signer never signs a second statement for place 1,
/// whatever the key; the old one is never written to the append-only store either. Place 1 is
/// abandoned, recorded with the hash and key of what was signed, and statement 2 is published
/// under the new key naming it, so the chain goes on and a verifier sees an explained gap.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s_1_req_pr_2_a_place_signed_under_a_rotated_key_is_abandoned_never_signed_again(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect.clone()).await;
    evaluated_world(&w).await;
    let old_dir = store_dir("rotated-old");
    let old_key = signer_with_statement_key(&connect, &old_dir).await;
    let dir = store_dir("rotated");
    let down = Arc::new(AtomicBool::new(true));
    let flaky = |down: &Arc<AtomicBool>| Flaky {
        inner: DirectoryStore::new(dir.join("statements")),
        down: down.clone(),
    };
    let err = Statements::new(
        w.core.pool().clone(),
        signer_process(&w, &old_dir, &old_key),
        flaky(&down),
    )
    .publish(ASSET, t0())
    .await
    .unwrap_err();
    assert!(matches!(err, StatementError::Store(_)), "{err}");
    let (old_bytes, old_sha) = signed_intent(&w).await;

    // The statement key is rotated; the store is back.
    let new_dir = store_dir("rotated-new");
    let new_key =
        signer_with_statement_seed(&connect, &new_dir, b"bezent rotated statement key").await;
    assert_ne!(old_key, new_key);
    down.store(false, Ordering::SeqCst);

    // The new key signs nothing for place 1: one statement per place, whatever the key.
    let place_1 = serde_json::to_vec_pretty(&json!({
        "statement": "bezent reserve statement",
        "asset": ASSET,
        "sequence": 1,
        "previous_sha256": null,
        "signing_key": {"scheme": "ed25519", "public_key": format!("0x{}", hex::encode(&new_key))},
    }))
    .unwrap();
    let refused = signer_process(&w, &new_dir, &new_key)
        .sign(&place_1)
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("REQ-PR-2"), "{refused}");

    let pager = FakePager::default();
    let statements = Statements::new(
        w.core.pool().clone(),
        signer_process(&w, &new_dir, &new_key),
        flaky(&down),
    )
    .with_pager(pager.clone());
    let p = statements
        .publish(ASSET, t0() + Duration::minutes(1))
        .await
        .unwrap();
    assert_eq!(p.sequence, 2);
    // Nothing was ever stored for place 1.
    assert!(
        statements
            .store()
            .get(&format!("{ASSET}/{:010}.json", 1))
            .await
            .unwrap()
            .is_none()
    );
    let sig = statements
        .store()
        .get(&p.signature_name)
        .await
        .unwrap()
        .unwrap();
    let doc = verify(&p.bytes, &sig, &new_key).unwrap();
    assert_eq!(doc["previous_sha256"], Value::Null);
    let abandoned = doc["abandoned"].as_array().unwrap();
    assert_eq!(abandoned.len(), 1);
    assert_eq!(abandoned[0]["sequence"], 1);
    assert_eq!(abandoned[0]["sha256"], old_sha.as_str());
    assert_eq!(
        abandoned[0]["signing_key"],
        format!("0x{}", hex::encode(&old_key)).as_str()
    );
    assert!(
        abandoned[0]["reason"]
            .as_str()
            .unwrap()
            .contains("not the statement key"),
        "{abandoned:?}"
    );
    verify_chain(std::slice::from_ref(&p.bytes)).unwrap();
    // The record: the hash and key of what was signed, why, and the login that abandoned it.
    let (sequence, sha, key, actor): (i64, Vec<u8>, Vec<u8>, String) = sqlx::query_as(
        "SELECT sequence, sha256, public_key, actor FROM reserve_statement_abandoned",
    )
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(sequence, 1);
    assert_eq!(format!("0x{}", hex::encode(sha)), old_sha);
    assert_eq!(key, old_key);
    assert!(!actor.is_empty());
    assert_eq!(
        Sha256::digest(hex::decode(&old_bytes[2..]).unwrap()).to_vec(),
        hex::decode(&old_sha[2..]).unwrap()
    );
    assert_eq!(pager.kinds(), ["statement-abandoned"]);
    // Signed once by each key, for two places.
    assert_eq!(
        w.count("SELECT count(*) FROM signatures WHERE scheme = 'ed25519'")
            .await,
        2
    );
    let recorded: (i64, Vec<u8>) =
        sqlx::query_as("SELECT sequence, public_key FROM reserve_statements")
            .fetch_one(&w.admin)
            .await
            .unwrap();
    assert_eq!(recorded, (2, new_key));
    for d in [old_dir, new_dir, dir] {
        std::fs::remove_dir_all(d).ok();
    }
}

/// PR #50 re-check S-3 (and review M3): a statement signed by the current key that never reached
/// the store is written only when it is exactly what bezent re-derives from the book at its
/// evaluation. One that is not (here the declarations it states changed since, as a deploy would
/// change them) is never written to the append-only store, and the chain is not left stuck until
/// a key rotation: the place is abandoned, a human is told once, and the next statement names it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s_3_req_pr_2_a_signed_statement_that_no_longer_rederives_is_abandoned_and_the_next_names_it(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect.clone()).await;
    evaluated_world(&w).await;
    let dir = store_dir("rederive");
    let public = signer_with_statement_key(&connect, &dir).await;
    let down = Arc::new(AtomicBool::new(true));
    let pager = FakePager::default();
    let statements = Statements::new(
        w.core.pool().clone(),
        signer_process(&w, &dir, &public),
        Flaky {
            inner: DirectoryStore::new(dir.join("statements")),
            down: down.clone(),
        },
    )
    .with_pager(pager.clone());
    assert!(matches!(
        statements.publish(ASSET, t0()).await.unwrap_err(),
        StatementError::Store(_)
    ));
    let (_, signed_sha) = signed_intent(&w).await;
    exec(
        &w.admin,
        &format!(
            "INSERT INTO surplus_funding_addresses (asset, network, address, reference) \
             VALUES ('{ASSET}', '{ORIGIN}', '0x00000000000000000000000000000000000000bb', 'late')"
        ),
    )
    .await;
    down.store(false, Ordering::SeqCst);
    let p = statements
        .publish(ASSET, t0() + Duration::minutes(1))
        .await
        .unwrap();
    assert_eq!(p.sequence, 2);
    let doc: Value = serde_json::from_slice(&p.bytes).unwrap();
    assert_eq!(doc["abandoned"][0]["sequence"], 1);
    assert_eq!(doc["abandoned"][0]["sha256"], signed_sha.as_str());
    assert!(
        doc["abandoned"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("re-derives"),
        "{doc}"
    );
    // Only statement 2 and its signature were written; nothing for place 1.
    let written = std::fs::read_dir(dir.join("statements").join(ASSET))
        .unwrap()
        .count();
    assert_eq!(written, 2);
    assert_eq!(
        w.count("SELECT count(*) FROM reserve_statements WHERE sequence = 2")
            .await,
        1
    );
    assert_eq!(w.count("SELECT count(*) FROM reserve_statements").await, 1);
    // Told once, never paged as stuck.
    assert_eq!(pager.kinds(), ["statement-abandoned"]);
    assert_eq!(
        w.count("SELECT count(*) FROM event_log WHERE kind = 'statement-abandoned'")
            .await,
        1
    );
    verify_chain(std::slice::from_ref(&p.bytes)).unwrap();
    std::fs::remove_dir_all(dir).ok();
}

/// PR #50 review M2: a statement that falls due and is not produced (here the store keeps
/// failing, as a store or signer that keeps failing would) pages `statement-overdue` once the
/// grace has passed, once per due time, and again when the next one falls due unpublished.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pr_1_a_statement_that_is_due_and_not_produced_pages(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    evaluated_world(&w).await;
    let dir = store_dir("overdue");
    let down = Arc::new(AtomicBool::new(true));
    let pager = FakePager::default();
    let statements = Statements::new(
        w.core.pool().clone(),
        KeyBackendSigner::new(statement_key(), "statement").unwrap(),
        Flaky {
            inner: DirectoryStore::new(&dir),
            down: down.clone(),
        },
    )
    .with_pager(pager.clone());
    let overdue = || w.count("SELECT count(*) FROM event_log WHERE kind = 'statement-overdue'");

    // Due since the asset's first evaluation (t0): within the grace, nothing is paged yet.
    assert!(statements.publish_due(t0()).await.unwrap().is_empty());
    assert!(
        statements
            .publish_due(t0() + Duration::minutes(30))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(overdue().await, 0);
    // Past the grace: paged, once.
    statements
        .publish_due(t0() + Duration::minutes(61))
        .await
        .unwrap();
    assert_eq!(overdue().await, 1);
    assert_eq!(
        pager
            .kinds()
            .iter()
            .filter(|k| *k == "statement-overdue")
            .count(),
        1
    );
    statements
        .publish_due(t0() + Duration::minutes(90))
        .await
        .unwrap();
    assert_eq!(overdue().await, 1);

    // Published once the store is back (with an evaluation recent enough to state); the next
    // falls due a day later and pages again when it is not produced.
    down.store(false, Ordering::SeqCst);
    let at = t0() + Duration::minutes(100);
    // A final head at the clock (block time is t0 plus the block number in seconds).
    read_all(&w.admin, 6_000).await;
    let (check, readings, _) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    assert_eq!(
        check.evaluate(ASSET, at).await.unwrap().outcome,
        Outcome::Holds
    );
    assert_eq!(statements.publish_due(at).await.unwrap().len(), 1);
    down.store(true, Ordering::SeqCst);
    statements
        .publish_due(at + Duration::days(1) + Duration::minutes(30))
        .await
        .unwrap();
    assert_eq!(overdue().await, 1);
    let pages = statements
        .page_overdue(at + Duration::days(1) + Duration::minutes(61))
        .await
        .unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(overdue().await, 2);
    std::fs::remove_dir_all(dir).ok();
}
