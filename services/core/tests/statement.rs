// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-004 T-004-05: the daily signed statement (`REQ-PR-1`, `REQ-PR-2`, `REQ-BE-3`, `REQ-BK-9`,
//! `REQ-CV-4`, `AC-E1.1`, `AC-E1.2`, `AC-A5.3`). The statement key is a test file key behind the
//! signer's key trait; the store is a directory.

mod common;

use std::collections::BTreeSet;

use bezent_core_service::refs::{self, Coordinate};
use bezent_core_service::reserve::book::{self, Cost};
use bezent_core_service::reserve::check::Outcome;
use bezent_core_service::reserve::coordinate_key;
use bezent_core_service::reserve::readings::Holding;
use bezent_core_service::reserve::s3::fake::FakeBucket;
use bezent_core_service::reserve::s3::{Backoff, S3Store};
use bezent_core_service::reserve::statement::{
    DirectoryStore, KeyBackendSigner, StatementError, StatementStore, Statements, StoreError,
    VerifyError, stuck_page_window, verify, verify_chain,
};
use bezent_signer::keys::{FileBackend, KeyBackend, KeyError};
use chrono::Duration;
use common::reserve::*;
use common::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

fn key_file(scheme: &str) -> FileBackend {
    labelled_key(scheme, "statement")
}

fn labelled_key(scheme: &str, label: &str) -> FileBackend {
    let seed: [u8; 32] = Sha256::digest(format!("bezent {label} test key {scheme}")).into();
    FileBackend::parse(
        json!({"comment": "TEST KEY", "environment": "test",
               "keys": [{"id": "statement", "scheme": scheme,
                         "seed": format!("0x{}", hex::encode(seed))}]})
        .to_string()
        .as_bytes(),
    )
    .unwrap()
}

fn store_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bz-statements-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn rand_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

type Publisher = Statements<KeyBackendSigner<FileBackend>, DirectoryStore>;

fn publisher(w: &World, dir: &std::path::Path) -> (Publisher, Vec<u8>) {
    let backend = key_file("ed25519");
    let public = backend.public_key("statement").unwrap();
    (
        Statements::new(
            w.core.pool().clone(),
            KeyBackendSigner::new(backend, "statement").unwrap(),
            DirectoryStore::new(dir),
        ),
        public,
    )
}

/// A world with every kind of term: the operator's funding, a minted deposit, an arrival not yet
/// minted, a redemption not yet paid, and a convertible holding at the reserve. The check holds.
async fn stated_world(w: &World) -> FakeReadings {
    fund(w, 5, 1_000_000).await;
    minted_deposit(w, 10, 5_000).await;
    final_arrival(&w.admin, 60, 0, 300).await;
    redeemed(w, 70, [5; 32], 700).await;
    read_all(&w.admin, 200).await;
    let (check, readings, _) = checker(w);
    sync_chain(&w.admin, &readings).await;
    readings.chain().convertible.insert(
        RESERVE.into(),
        vec![Holding {
            token: "usdt0".into(),
            amount: "4200".into(),
        }],
    );
    let e = check.evaluate(ASSET, t0()).await.unwrap();
    assert_eq!(e.outcome, Outcome::Holds, "{:?}", e.differences);
    readings
}

/// The check evaluates again at `when`, as it does every minute; a statement states an evaluation
/// no older than fifteen minutes (review S7).
async fn evaluated_at(w: &World, when: chrono::DateTime<chrono::Utc>) {
    // The networks are read up to `when`: a final head older than the bound by the clock is a
    // stall (review MF2).
    read_all(&w.admin, (when - t0()).num_seconds().max(200)).await;
    let (check, readings, _) = checker(w);
    sync_chain(&w.admin, &readings).await;
    let e = check.evaluate(ASSET, when).await.unwrap();
    assert_eq!(e.outcome, Outcome::Holds, "{:?}", e.differences);
}

async fn read(store: &DirectoryStore, name: &str) -> Vec<u8> {
    store.get(name).await.unwrap().expect(name)
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pr_1_req_be_3_req_bk_9_the_statement_verifies_and_names_everything_it_must(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    stated_world(&w).await;
    let dir = store_dir();
    let (statements, public) = publisher(&w, &dir);
    let published = statements.publish_due(t0()).await.unwrap();
    assert_eq!(published.len(), 1);
    let p = &published[0];
    let bytes = read(statements.store(), &p.name).await;
    let signature = read(statements.store(), &p.signature_name).await;
    assert_eq!(bytes, p.bytes);
    // Signed by the statement key, over the exact bytes published.
    let doc = verify(&bytes, &signature, &public).unwrap();
    let mut tampered = bytes.clone();
    tampered[10] ^= 1;
    assert_eq!(
        verify(&tampered, &signature, &public),
        Err(VerifyError::BadSignature)
    );
    let other = labelled_key("ed25519", "another")
        .public_key("statement")
        .unwrap();
    assert_eq!(
        verify(&bytes, &signature, &other),
        Err(VerifyError::BadSignature)
    );

    // REQ-PR-1: a signed claim, not a proof of solvency; REQ-BK-9: the book is not provable.
    assert!(
        doc["claim"]
            .as_str()
            .unwrap()
            .contains("not a proof of solvency")
    );
    assert!(
        doc["reserve_book"]
            .as_str()
            .unwrap()
            .contains("not provable")
    );
    // REQ-BE-3: the backend and its assurance class.
    assert_eq!(doc["backend"]["assurance_class"], "public");
    assert_eq!(doc["backend"]["networks"][0]["backend"], "evm:80002");
    assert_eq!(doc["published_addresses"][0]["address"], RESERVE);
    // Every term, each naming the final block it rests on.
    let terms = &doc["terms"];
    assert_eq!(terms["reserve"][0]["final_block"]["block_number"], 200);
    assert_eq!(terms["reserve"][0]["final_block"]["network"], ORIGIN);
    assert_eq!(terms["supply"]["final_block"]["network"], ISSUING);
    assert_eq!(terms["supply"]["amount"], "4300");
    assert_eq!(terms["in_transit"]["amount"], "0");
    assert_eq!(terms["owed_mint"]["amount"], "300");
    assert_eq!(terms["owed_payout"]["amount"], "700");
    assert_eq!(terms["surplus"]["amount"], "1000000");
    assert_eq!(terms["surplus"]["book_amount"], "1000000");
    // REQ-BK-5, REQ-BK-6 (review S1): the funding address is published, and the contribution
    // from it is listed with its coordinate and amount.
    assert_eq!(doc["operator_funding_addresses"][0]["address"], FUNDING);
    assert_eq!(doc["operator_funding_addresses"][0]["funds"], RESERVE);
    assert_eq!(terms["surplus"]["contributions"][0]["amount"], "1000000");
    assert_eq!(terms["surplus"]["contributions"][0]["from"], FUNDING);
    assert_eq!(doc["final_blocks"][ORIGIN]["block_number"], 200);
    assert_eq!(doc["final_blocks"][ISSUING]["block_number"], 200);
    // REQ-BK-9: which terms anyone can recompute, which rest on reports.
    assert_eq!(
        doc["recomputable_from_public_data"],
        json!([
            "reserve",
            "in_transit",
            "supply",
            "owed_mint",
            "owed_payout",
            "surplus"
        ])
    );
    assert_eq!(doc["rest_on_reports"], json!([]));
    // REQ-CV-4, AC-A5.3: unconverted holdings itemised, outside the equation.
    assert_eq!(doc["unconverted_holdings"]["items"][0]["token"], "usdt0");
    assert_eq!(doc["unconverted_holdings"]["items"][0]["amount"], "4200");
    assert!(
        doc["unconverted_holdings"]["note"]
            .as_str()
            .unwrap()
            .contains("outside the reserve equation")
    );
    assert_eq!(doc["sequence"], 1);
    assert_eq!(doc["previous_sha256"], Value::Null);
    assert_eq!(
        doc["signing_key"]["public_key"],
        format!("0x{}", hex::encode(&public))
    );
    std::fs::remove_dir_all(dir).ok();
}

fn parse_coordinate(key: &str) -> Coordinate {
    let parts: Vec<&str> = key.split(':').collect();
    Coordinate {
        network: parts[0].into(),
        block_number: parts[1].parse().unwrap(),
        block_hash: hex::decode(parts[2].trim_start_matches("0x")).unwrap(),
        position_kind: parts[3].into(),
        position_index: parts[4].parse().unwrap(),
    }
}

/// A final, executed event the chain shows at or before `block`, with `reference`.
async fn shown(pool: &PgPool, network: &str, event: &str, reference: &[u8], block: i64) -> bool {
    scalar(
        pool,
        &format!(
            "SELECT EXISTS (SELECT 1 FROM observations o JOIN observation_outcomes x \
               ON x.observation_id = o.id AND x.outcome = 'final' \
             WHERE o.executed AND o.network = '{network}' AND o.detail ->> 'event' = '{event}' \
               AND o.on_chain_reference = '\\x{}' AND o.block_number <= {block})",
            hex::encode(reference)
        ),
    )
    .await
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_e1_1_a_verifier_recomputes_every_term_from_public_data_and_the_published_addresses(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let chain = stated_world(&w).await;
    let dir = store_dir();
    let (statements, public) = publisher(&w, &dir);
    let p = statements.publish(ASSET, t0()).await.unwrap();
    let doc = verify(
        &read(statements.store(), &p.name).await,
        &read(statements.store(), &p.signature_name).await,
        &public,
    )
    .unwrap();

    // The verifier: the published addresses and the statement, and public data only: each
    // network's balances and supply at the named final block (the readings), and its events (the
    // observations, as anyone reads them from the network). Nothing from bezent's book.
    let n = |v: &Value| v.as_str().unwrap().parse::<i128>().unwrap();
    let addresses: BTreeSet<String> = doc["published_addresses"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["address"].as_str().unwrap().to_owned())
        .collect();
    let mut reserve = 0;
    for r in doc["terms"]["reserve"].as_array().unwrap() {
        let address = r["address"].as_str().unwrap();
        assert!(addresses.contains(address));
        let balance =
            chain.chain().reserve[&(r["token"].as_str().unwrap().to_owned(), address.to_owned())];
        assert_eq!(balance as i128, n(&r["amount"]));
        reserve += balance as i128;
    }
    let supply = &doc["terms"]["supply"];
    let issued = chain.chain().supply[supply["issuing_asset"].as_str().unwrap()];
    assert_eq!(issued as i128, n(&supply["amount"]));
    let issuing_block = doc["final_blocks"][ISSUING]["block_number"]
        .as_i64()
        .unwrap();
    let origin_block = doc["final_blocks"][ORIGIN]["block_number"]
        .as_i64()
        .unwrap();

    // Owed mints: every final arrival at a published address, at or before the named block, that
    // the chain shows neither minted nor refunded, and that the statement does not name as the
    // operator's contribution.
    let contributions: BTreeSet<String> = doc["terms"]["surplus"]["contributions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["coordinate"].as_str().unwrap().to_owned())
        .collect();
    let arrivals: Vec<(i64, i64, String)> = sqlx::query_as(&format!(
        "SELECT o.block_number, o.position_index, o.amount::text FROM observations o \
         JOIN observation_outcomes x ON x.observation_id = o.id AND x.outcome = 'final' \
         WHERE o.executed AND o.network = '{ORIGIN}' AND o.recipient = '{RESERVE}' \
           AND o.token = '{TOKEN}' AND o.block_number <= {origin_block} ORDER BY o.id"
    ))
    .fetch_all(&w.admin)
    .await
    .unwrap();
    let mut owed_mint = BTreeSet::new();
    for (block, index, amount) in arrivals {
        let c = Coordinate {
            network: ORIGIN.into(),
            block_number: block,
            block_hash: hash(ORIGIN, block),
            position_kind: "log".into(),
            position_index: index,
        };
        let key = bezent_core_service::reserve::coordinate_key(&c);
        if contributions.contains(&key)
            || shown(
                &w.admin,
                ISSUING,
                "Minted",
                &refs::mint_reference(&c),
                issuing_block,
            )
            .await
            || shown(
                &w.admin,
                ORIGIN,
                "Payout",
                &refs::unassociated_refund_reference(&c),
                origin_block,
            )
            .await
        {
            continue;
        }
        owed_mint.insert((key, amount));
    }
    let stated: BTreeSet<(String, String)> = doc["terms"]["owed_mint"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            let c = parse_coordinate(i["coordinate"].as_str().unwrap());
            assert_eq!(c.network, ORIGIN);
            (
                i["item"].as_str().unwrap().to_owned(),
                i["amount"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(stated, owed_mint);

    // Owed payouts: every final redemption the chain shows neither paid out nor refund-minted.
    let mut owed_payout = BTreeSet::new();
    for i in doc["terms"]["owed_payout"]["items"].as_array().unwrap() {
        let c = parse_coordinate(i["coordinate"].as_str().unwrap());
        let (sender, reference, amount): (String, Vec<u8>, String) = sqlx::query_as(
            "SELECT o.sender, o.on_chain_reference, o.amount::text FROM observations o \
             JOIN observation_outcomes x ON x.observation_id = o.id AND x.outcome = 'final' \
             WHERE o.network = $1 AND o.block_number = $2 AND o.position_index = $3 \
               AND o.detail ->> 'event' = 'Redeemed'",
        )
        .bind(&c.network)
        .bind(c.block_number)
        .bind(c.position_index)
        .fetch_one(&w.admin)
        .await
        .unwrap();
        let item = refs::withdrawal(&sender, &reference);
        assert_eq!(i["item"], item);
        assert!(
            !shown(
                &w.admin,
                ORIGIN,
                "Payout",
                &refs::payout_reference(&item),
                origin_block
            )
            .await
        );
        assert_eq!(i["amount"], amount);
        owed_payout.insert(item);
    }
    let redemptions: i64 = scalar(
        &w.admin,
        &format!(
            "SELECT count(*) FROM observations o JOIN observation_outcomes x \
               ON x.observation_id = o.id AND x.outcome = 'final' \
             WHERE o.detail ->> 'event' = 'Redeemed' AND o.block_number <= {issuing_block}"
        ),
    )
    .await;
    assert_eq!(redemptions as usize, owed_payout.len());

    // Surplus is the equation's balance.
    let in_transit = n(&doc["terms"]["in_transit"]["amount"]);
    let surplus = reserve + in_transit
        - n(&supply["amount"])
        - n(&doc["terms"]["owed_mint"]["amount"])
        - n(&doc["terms"]["owed_payout"]["amount"]);
    assert_eq!(surplus, n(&doc["terms"]["surplus"]["amount"]));
    std::fs::remove_dir_all(dir).ok();
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_e1_2_req_pr_2_a_revision_of_an_earlier_statement_is_detectable(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    stated_world(&w).await;
    let dir = store_dir();
    let (statements, _) = publisher(&w, &dir);
    let first = statements.publish(ASSET, t0()).await.unwrap();
    evaluated_at(&w, t0() + Duration::days(1)).await;
    let second = statements
        .publish(ASSET, t0() + Duration::days(1))
        .await
        .unwrap();
    assert_eq!(second.sequence, 2);
    let doc: Value = serde_json::from_slice(&second.bytes).unwrap();
    assert_eq!(
        doc["previous_sha256"],
        format!("0x{}", hex::encode(first.sha256))
    );
    verify_chain(&[first.bytes.clone(), second.bytes.clone()]).unwrap();
    // The store refuses to overwrite a statement.
    let mut revised: Value = serde_json::from_slice(&first.bytes).unwrap();
    revised["terms"]["surplus"]["amount"] = json!("2000000");
    let revised = serde_json::to_vec_pretty(&revised).unwrap();
    assert_eq!(
        statements.store().put(&first.name, &revised).await,
        Err(StoreError::Exists(first.name.clone()))
    );
    // A revision made behind the store's back is detected from the later statement alone.
    std::fs::write(dir.join(&first.name), &revised).unwrap();
    let held = read(statements.store(), &first.name).await;
    assert_eq!(
        verify_chain(&[held, read(statements.store(), &second.name).await]),
        Err(VerifyError::Revised { sequence: 2 })
    );
    // And a missing statement too.
    assert!(verify_chain(std::slice::from_ref(&second.bytes)).is_err());
    std::fs::remove_dir_all(dir).ok();
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pr_1_each_asset_is_stated_at_least_daily(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    stated_world(&w).await;
    let dir = store_dir();
    let (statements, _) = publisher(&w, &dir);
    assert_eq!(statements.publish_due(t0()).await.unwrap().len(), 1);
    assert_eq!(
        statements
            .publish_due(t0() + Duration::hours(23))
            .await
            .unwrap()
            .len(),
        0
    );
    evaluated_at(&w, t0() + Duration::hours(24)).await;
    let next = statements
        .publish_due(t0() + Duration::hours(24))
        .await
        .unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].sequence, 2);
    assert_eq!(w.count("SELECT count(*) FROM reserve_statements").await, 2);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn req_pr_1_the_statement_key_is_ed25519_and_no_other_key_signs_a_statement() {
    let refused = KeyBackendSigner::new(key_file("sr25519"), "statement");
    assert!(matches!(refused, Err(KeyError::Refused(_))));
    assert!(KeyBackendSigner::new(key_file("ed25519"), "statement").is_ok());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_4_req_bk_5_the_statement_lists_every_contribution_withdrawal_and_cost(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // PLAN DEC-16 (2026-10-14): the statement lists surplus contributions and withdrawals beside
    // the terms; review S2: and every other movement of surplus, network costs among them.
    let w = World::new(options, connect).await;
    stated_world(&w).await;
    let out = observe(
        &w.admin,
        ORIGIN,
        80,
        5,
        TOKEN,
        400,
        Some(RESERVE),
        Some(FUNDING),
        None,
        true,
        json!({}),
    )
    .await;
    decide(&w.admin, out, "final", 82).await;
    let mut conn = w.core.pool().acquire().await.unwrap();
    book::surplus_withdrawal(&mut conn, ASSET, out, "withdrawal-1")
        .await
        .unwrap();
    book::cost(
        &mut conn,
        ASSET,
        Cost::Network,
        ORIGIN,
        25,
        "gas:0xabc",
        None,
    )
    .await
    .unwrap();
    let (check, readings, _) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    *readings
        .chain()
        .reserve
        .get_mut(&(TOKEN.to_owned(), RESERVE.to_owned()))
        .unwrap() -= 25;
    let when = t0() + Duration::minutes(1);
    let e = check.evaluate(ASSET, when).await.unwrap();
    assert_eq!(e.outcome, Outcome::Holds, "{:?}", e.differences);
    let dir = store_dir();
    let (statements, public) = publisher(&w, &dir);
    let p = statements.publish(ASSET, when).await.unwrap();
    let doc = verify(
        &p.bytes,
        &read(statements.store(), &p.signature_name).await,
        &public,
    )
    .unwrap();
    let surplus = &doc["terms"]["surplus"];
    assert_eq!(surplus["amount"], (1_000_000 - 400 - 25).to_string());
    let movements: Vec<(String, String, String)> = surplus["movements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["kind"].as_str().unwrap().to_owned(),
                m["reference"].as_str().unwrap().to_owned(),
                m["amount"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let contribution = coordinate_key(&Coordinate {
        network: ORIGIN.into(),
        block_number: 5,
        block_hash: hash(ORIGIN, 5),
        position_kind: "log".into(),
        position_index: 7,
    });
    assert_eq!(
        movements,
        [
            ("surplus-contribution", contribution.as_str(), "1000000"),
            ("surplus-withdrawal", "withdrawal-1", "-400"),
            ("network-cost", "gas:0xabc", "-25"),
        ]
        .map(|(k, r, a)| (k.to_owned(), r.to_owned(), a.to_owned()))
        .to_vec()
    );
    std::fs::remove_dir_all(dir).ok();
}

/// A store that fails the first signature write, as a store that drops between the two writes.
#[derive(Clone)]
struct CutShort {
    inner: DirectoryStore,
    failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl StatementStore for CutShort {
    async fn put(&self, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
        if name.ends_with(".sig") && !self.failed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err(StoreError::Io("the store went away".into()));
        }
        self.inner.put(name, bytes).await
    }

    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.get(name).await
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pr_2_a_publish_cut_short_is_adopted_and_the_chain_goes_on(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // Review S7: the document reached the store, its signature did not, the record rolled back.
    let w = World::new(options, connect).await;
    stated_world(&w).await;
    let dir = store_dir();
    let backend = key_file("ed25519");
    let public = backend.public_key("statement").unwrap();
    let statements = Statements::new(
        w.core.pool().clone(),
        KeyBackendSigner::new(backend, "statement").unwrap(),
        CutShort {
            inner: DirectoryStore::new(&dir),
            failed: Default::default(),
        },
    );
    assert!(statements.publish_due(t0()).await.unwrap().is_empty());
    assert_eq!(w.count("SELECT count(*) FROM reserve_statements").await, 0);
    // An hour later, on a newer evaluation: the stored statement is adopted, not replaced.
    let later = t0() + Duration::hours(1);
    evaluated_at(&w, later).await;
    let adopted = statements.publish_due(later).await.unwrap();
    assert_eq!(adopted.len(), 1);
    assert_eq!(adopted[0].sequence, 1);
    let held = statements
        .store()
        .get(&adopted[0].name)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(held, adopted[0].bytes);
    let signature = statements
        .store()
        .get(&adopted[0].signature_name)
        .await
        .unwrap()
        .unwrap();
    let doc = verify(&held, &signature, &public).unwrap();
    assert_eq!(doc["issued_at"], t0().to_rfc3339());
    // And the chain goes on the next day.
    let next_day = t0() + Duration::days(1);
    evaluated_at(&w, next_day).await;
    let next = statements.publish_due(next_day).await.unwrap();
    assert_eq!(next[0].sequence, 2);
    verify_chain(&[held, next[0].bytes.clone()]).unwrap();
    std::fs::remove_dir_all(dir).ok();
}

/// Review SF4: whoever can write to the store under the next statement's name, without the key,
/// never gets a document signed. One that names the right asset, sequence, predecessor and key but
/// is not the statement bezent re-derives from the evaluation it names is refused, nothing is
/// signed or recorded, and a human is paged (`REQ-PR-1`, `REQ-PR-2`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pr_1_req_pr_2_sf4_a_document_planted_in_the_store_is_never_signed(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    stated_world(&w).await;
    let dir = store_dir();
    let (statements, public) = publisher(&w, &dir);
    let pager = FakePager::default();
    let statements = statements.with_pager(pager.clone());
    let evaluation: i64 = scalar(&w.admin, "SELECT max(id) FROM reserve_evaluations").await;
    let name = format!("{ASSET}/{:010}.json", 1);
    let planted = serde_json::to_vec_pretty(&json!({
        "statement": "bezent reserve statement",
        "version": 1,
        "asset": ASSET,
        "sequence": 1,
        "previous_sha256": null,
        "issued_at": t0().to_rfc3339(),
        "claim": "the reserve holds everything, trust us",
        "terms": {"surplus": {"amount": "999999999999"}},
        "evaluation": {"id": evaluation},
        "signing_key": {"scheme": "ed25519", "public_key": format!("0x{}", hex::encode(&public))},
    }))
    .unwrap();
    statements.store().put(&name, &planted).await.unwrap();
    let e = statements
        .publish(ASSET, t0())
        .await
        .expect_err("signed a planted document");
    assert!(e.to_string().contains("re-derives"), "{e}");
    assert!(
        statements
            .store()
            .get(&format!("{name}.sig"))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(w.count("SELECT count(*) FROM reserve_statements").await, 0);
    assert_eq!(pager.kinds(), ["statement-stuck"]);
    std::fs::remove_dir_all(dir).ok();
}

/// PR #50 re-check S-3: a chain that stays stuck is paged once per statement cycle
/// ([`stuck_page_window`]), not on every core cycle (every 5 s); once the window has passed it is
/// paged again (`REQ-PR-2`, `REQ-IC-2`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s_3_req_pr_2_a_stuck_chain_is_paged_once_per_window_not_every_cycle(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    stated_world(&w).await;
    let dir = store_dir();
    let (statements, public) = publisher(&w, &dir);
    let pager = FakePager::default();
    let statements = statements.with_pager(pager.clone());
    let evaluation: i64 = scalar(&w.admin, "SELECT max(id) FROM reserve_evaluations").await;
    let planted = serde_json::to_vec_pretty(&json!({
        "statement": "bezent reserve statement",
        "asset": ASSET,
        "sequence": 1,
        "previous_sha256": null,
        "issued_at": t0().to_rfc3339(),
        "evaluation": {"id": evaluation},
        "signing_key": {"scheme": "ed25519", "public_key": format!("0x{}", hex::encode(&public))},
    }))
    .unwrap();
    statements
        .store()
        .put(&format!("{ASSET}/{:010}.json", 1), &planted)
        .await
        .unwrap();
    let stuck = |pager: &FakePager| {
        pager
            .kinds()
            .iter()
            .filter(|k| *k == "statement-stuck")
            .count()
    };
    // Every 5 s, then ten minutes on: stuck each time, paged once.
    for seconds in [0, 5, 10, 600] {
        let e = statements
            .publish(ASSET, t0() + Duration::seconds(seconds))
            .await
            .unwrap_err();
        assert!(matches!(e, StatementError::Stuck(_)), "{e}");
    }
    assert_eq!(stuck(&pager), 1);
    assert_eq!(
        w.count("SELECT count(*) FROM event_log WHERE kind = 'statement-stuck'")
            .await,
        1
    );
    // A statement cycle later, still stuck: paged again.
    let later = t0() + stuck_page_window() + Duration::minutes(1);
    evaluated_at(&w, later).await;
    assert!(matches!(
        statements.publish(ASSET, later).await.unwrap_err(),
        StatementError::Stuck(_)
    ));
    assert_eq!(stuck(&pager), 2);
    std::fs::remove_dir_all(dir).ok();
}

/// Review SF5: the statement's movements of surplus are cut at the evaluation it states. A cost
/// booked after the evaluation (and before the publish) is not listed, so the movements add up to
/// the book's surplus the statement shows (`REQ-PR-1`, `REQ-BK-4`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pr_1_req_bk_4_sf5_the_movements_are_cut_at_the_evaluation(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    stated_world(&w).await;
    // Booked after the evaluation the statement states.
    let mut conn = w.core.pool().acquire().await.unwrap();
    book::cost(
        &mut conn,
        ASSET,
        Cost::Network,
        ORIGIN,
        25,
        "gas:after-the-evaluation",
        None,
    )
    .await
    .unwrap();
    drop(conn);
    let dir = store_dir();
    let (statements, public) = publisher(&w, &dir);
    let p = statements.publish(ASSET, t0()).await.unwrap();
    let doc = verify(
        &p.bytes,
        &read(statements.store(), &p.signature_name).await,
        &public,
    )
    .unwrap();
    let surplus = &doc["terms"]["surplus"];
    let movements = surplus["movements"].as_array().unwrap();
    assert!(
        movements
            .iter()
            .all(|m| m["reference"] != "gas:after-the-evaluation"),
        "{movements:?}"
    );
    let sum: i128 = movements
        .iter()
        .map(|m| m["amount"].as_str().unwrap().parse::<i128>().unwrap())
        .sum();
    assert_eq!(sum.to_string(), surplus["book_amount"].as_str().unwrap());
    std::fs::remove_dir_all(dir).ok();
}

/// The S3 sink over a fake bucket (issue #58), with a backoff short enough for a test.
fn bucket_store(bucket: &FakeBucket) -> S3Store<FakeBucket> {
    let config = toml::from_str(
        "bucket = \"bezent-statements\"\nprefix = \"statements\"\nregion = \"us-east-2\"\n",
    )
    .unwrap();
    S3Store::new(bucket.clone(), &config).with_backoff(Backoff {
        attempts: 2,
        first_delay: std::time::Duration::from_millis(1),
    })
}

fn bucket_publisher(
    w: &World,
    bucket: &FakeBucket,
) -> (
    Statements<KeyBackendSigner<FileBackend>, S3Store<FakeBucket>>,
    Vec<u8>,
) {
    let backend = key_file("ed25519");
    let public = backend.public_key("statement").unwrap();
    (
        Statements::new(
            w.core.pool().clone(),
            KeyBackendSigner::new(backend, "statement").unwrap(),
            bucket_store(bucket),
        ),
        public,
    )
}

/// Issue #58, `REQ-PR-2`: the publication is recorded (key, ETag, version id) only after the
/// bucket took both objects; a bucket that refuses leaves nothing recorded, pages once, and the
/// same signed statement is written when the bucket answers again.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_58_req_pr_2_publication_is_recorded_only_after_the_bucket_took_the_statement(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    stated_world(&w).await;
    let bucket = FakeBucket::default();
    let (statements, public) = bucket_publisher(&w, &bucket);
    let pager = FakePager::default();
    let statements = statements.with_pager(pager.clone());

    // The bucket refuses every write: nothing is recorded, a human is paged, no statement exists.
    bucket.state().forbidden = true;
    assert!(statements.publish_due(t0()).await.unwrap().is_empty());
    assert_eq!(w.count("SELECT count(*) FROM reserve_statements").await, 0);
    assert_eq!(
        w.count("SELECT count(*) FROM reserve_statement_objects")
            .await,
        0
    );
    assert!(bucket.state().objects.is_empty());
    assert!(
        pager
            .kinds()
            .contains(&"statement-publish-failed".to_owned()),
        "{:?}",
        pager.kinds()
    );
    // Not paged again on the next cycle, inside the window.
    let before = pager.kinds().len();
    assert!(
        statements
            .publish_due(t0() + Duration::seconds(5))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(pager.kinds().len(), before);

    // The bucket answers again: the statement is written, then recorded with its receipts.
    bucket.state().forbidden = false;
    let published = statements
        .publish_due(t0() + Duration::seconds(10))
        .await
        .unwrap();
    assert_eq!(published.len(), 1);
    let p = &published[0];
    let key = "statements/2026/10/13/statement-".to_owned() + ASSET + "-2026-10-13.json";
    assert_eq!(p.receipt.location, key);
    assert_eq!(p.signature_receipt.location, format!("{key}.sig"));
    assert_eq!(
        w.count("SELECT count(*) FROM reserve_statement_objects")
            .await,
        2
    );
    let rows: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT name, location, etag, version_id FROM reserve_statement_objects ORDER BY name",
    )
    .fetch_all(&w.admin)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    for (_, location, etag, version) in &rows {
        assert!(bucket.state().objects.contains_key(location));
        assert!(etag.is_some() && version.is_some());
    }
    // What the bucket holds is exactly what the core signed.
    let (bytes, signature) = {
        let st = bucket.state();
        (
            st.objects[&key].0.clone(),
            st.objects[&format!("{key}.sig")].0.clone(),
        )
    };
    assert_eq!(bytes, p.bytes);
    verify(&bytes, &signature, &public).unwrap();
    // Append-only.
    assert!(
        sqlx::query("UPDATE reserve_statement_objects SET etag = 'x'")
            .execute(&w.admin)
            .await
            .is_err()
    );
}

/// Issue #58: the chain goes on in the bucket, a day a statement, and each is recorded.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_58_the_chain_goes_on_in_the_bucket_one_object_pair_a_day(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    stated_world(&w).await;
    let bucket = FakeBucket::default();
    let (statements, _) = bucket_publisher(&w, &bucket);
    let first = statements.publish_due(t0()).await.unwrap();
    let next_day = t0() + Duration::days(1);
    evaluated_at(&w, next_day).await;
    let second = statements.publish_due(next_day).await.unwrap();
    assert_eq!((first[0].sequence, second[0].sequence), (1, 2));
    verify_chain(&[first[0].bytes.clone(), second[0].bytes.clone()]).unwrap();
    assert_eq!(bucket.state().objects.len(), 4);
    assert!(second[0].receipt.location.contains("2026/10/14/"));
    assert_eq!(
        w.count("SELECT count(*) FROM reserve_statement_objects")
            .await,
        4
    );
    // A third attempt on the same day is not due, and writes nothing.
    assert!(
        statements
            .publish_due(next_day + Duration::hours(1))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(bucket.state().objects.len(), 4);
}
