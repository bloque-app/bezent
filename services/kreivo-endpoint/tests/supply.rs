// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Issue #42: each bridged asset's supply recorded at each final head the endpoint records, with
//! the on-chain component's count and the asset's issuance, for the reserve check (`REQ-BK-8`,
//! `NFR-5`, `INV-1`, `REQ-OB-5`).

mod common;

use bezent_kreivo_endpoint::supply::{Recorded, Supplies};
use common::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

fn supplies(f: &Fixture) -> Supplies<'_, bezent_kreivo_endpoint::chain::fake::FakeChain> {
    Supplies {
        chain: &f.chain,
        pool: &f.endpoint,
        network: NETWORK,
        contract: CONTRACT,
    }
}

async fn declare(f: &Fixture, asset: &str, issuing: &str) {
    sqlx::query(
        "INSERT INTO assets (asset, precision, assurance_class, issuing_asset) \
         VALUES ($1, 6, 'public', $2)",
    )
    .bind(asset)
    .bind(issuing)
    .execute(&f.admin)
    .await
    .unwrap();
}

type Row = (i64, Option<String>, Option<String>, bool, serde_json::Value);

type SupplyRow = (
    String,
    i64,
    Option<String>,
    Option<String>,
    bool,
    serde_json::Value,
);

async fn readings(f: &Fixture) -> Vec<(String, Row)> {
    let rows: Vec<SupplyRow> = sqlx::query_as(
        "SELECT subject, block_number, amount::text, unknown_reason, mismatch, detail \
             FROM reserve_readings WHERE network = $1 AND kind = 'supply' \
             ORDER BY block_number, subject",
    )
    .bind(NETWORK)
    .fetch_all(&f.admin)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(s, b, a, u, m, d)| (s, (b, a, u, m, d)))
        .collect()
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_8_supply_is_recorded_at_each_final_head_with_the_count_and_the_issuance(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    declare(&f, "bUSDC", "Here(101)").await;
    f.chain.set_supply(101, 500, 500);
    f.chain.advance(1);
    let head = f.chain.finalized_ref();

    let s = supplies(&f);
    assert_eq!(
        s.record().await.unwrap(),
        Recorded::At {
            block: head.number,
            readings: 1
        }
    );
    // The head is a final head of the network, with its block time (REQ-OB-5).
    assert_eq!(
        f.count(&format!(
            "SELECT count(*) FROM final_heads WHERE network = '{NETWORK}' \
               AND block_number = {} AND block_hash = '\\x{}' AND block_time > '2025-01-01'",
            head.number,
            hex::encode(head.hash)
        ))
        .await,
        1
    );
    let got = readings(&f).await;
    assert_eq!(got.len(), 1);
    let (subject, (block, amount, unknown, mismatch, detail)) = &got[0];
    assert_eq!(subject, "Here(101)");
    assert_eq!(*block, i64::try_from(head.number).unwrap());
    assert_eq!(amount.as_deref(), Some("500"));
    assert!(unknown.is_none() && !mismatch);
    assert_eq!(detail["contract_supply"], "500");
    assert_eq!(detail["issuance"], "500");

    // The same head again records nothing; the next head is recorded with what it holds.
    assert_eq!(s.record().await.unwrap(), Recorded::Already(head.number));
    f.chain.set_supply(101, 700, 700);
    f.chain.advance(1);
    assert!(matches!(
        s.record().await.unwrap(),
        Recorded::At { readings: 1, .. }
    ));
    let got = readings(&f).await;
    assert_eq!(got.len(), 2);
    assert_eq!(got[1].1.1.as_deref(), Some("700"));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_4_a_count_that_differs_from_the_issuance_is_recorded_flagged_for_the_core(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    declare(&f, "bUSDC", "Here(101)").await;
    // 100 more exist than the component minted: value made outside it (INV-4).
    f.chain.set_supply(101, 500, 600);
    f.chain.advance(1);
    supplies(&f).record().await.unwrap();

    let got = readings(&f).await;
    let (_, (_, amount, unknown, mismatch, detail)) = &got[0];
    // The issuance is the reading (§5.3: the total on the issuing network); both are kept.
    assert_eq!(amount.as_deref(), Some("600"));
    assert!(unknown.is_none());
    assert!(*mismatch);
    assert_eq!(detail["contract_supply"], "500");
    assert_eq!(detail["issuance"], "600");
    // PR #50 review M1: not a delivery anomaly. The flag is durable with the head, and the core
    // raises key-compromise and pauses the asset from it (services/core/tests/running_core.rs,
    // inv_4_…); the endpoint writes no signal of its own that a stop after commit could lose.
    assert_eq!(
        f.count(
            "SELECT count(*) FROM event_log WHERE kind IN ('delivery-anomaly', 'key-compromise')"
        )
        .await,
        0
    );
    // Flagged at every head where it stands.
    f.chain.advance(1);
    supplies(&f).record().await.unwrap();
    let got = readings(&f).await;
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|(_, (_, _, _, m, _))| *m));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_8_a_supply_that_cannot_be_read_is_unknown_never_a_guess(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    declare(&f, "bUSDC", "Here(101)").await;
    // An asset the chain does not hold.
    declare(&f, "bEURC", "Here(999)").await;
    f.chain.set_supply(101, 5, 5);
    f.chain.advance(1);
    supplies(&f).record().await.unwrap();
    let got = readings(&f).await;
    let missing = got.iter().find(|(s, _)| s == "Here(999)").unwrap();
    assert!(missing.1.1.is_none());
    assert!(
        missing
            .1
            .2
            .as_deref()
            .is_some_and(|r| r.contains("does not exist")),
        "{missing:?}"
    );
    assert!(
        got.iter()
            .any(|(s, r)| s == "Here(101)" && r.1.as_deref() == Some("5"))
    );

    // A chain that cannot be read records nothing, not even the head.
    f.chain.produce();
    f.chain.finalize(f.chain.best());
    f.chain.set_unavailable(true);
    assert!(matches!(
        supplies(&f).record().await.unwrap(),
        Recorded::Unavailable(_)
    ));
    f.chain.set_unavailable(false);
    assert_eq!(
        f.count(&format!(
            "SELECT count(*) FROM final_heads WHERE network = '{NETWORK}'"
        ))
        .await,
        1
    );
}
