// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #14: the delivery account's fee position on the issuing network is recorded at each final head the
//! endpoint records, as its free balance less its frozen balance, and read through `ops_observables()`
//! (`REQ-ST-6`, `AC-H3.1`, `REQ-OB-5`, `REQ-NX-3`; PLAN `DEC-18`; amendment 0025, `d-fee-position-scope`). A read that
//! fails or hangs is unknown, never zero, and never stops the head or the supply readings.

mod common;

use std::time::Duration;

use bezent_kreivo_endpoint::chain::Balances;
use bezent_kreivo_endpoint::fee;
use bezent_kreivo_endpoint::supply::{Recorded, Supplies};
use common::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

fn supplies(f: &Fixture) -> Supplies<'_, bezent_kreivo_endpoint::chain::fake::FakeChain> {
    Supplies {
        chain: &f.chain,
        pool: &f.endpoint,
        network: NETWORK,
        contract: CONTRACT,
        delivery: f.lane.account,
    }
}

async fn declare(f: &Fixture) {
    sqlx::query(
        "INSERT INTO assets (asset, precision, assurance_class, issuing_asset) \
         VALUES ('bUSDC', 6, 'public', 'Here(101)')",
    )
    .execute(&f.admin)
    .await
    .unwrap();
    f.chain.set_supply(101, 500, 500);
}

type Row = (
    i64,
    Vec<u8>,
    String,
    Option<String>,
    Option<String>,
    serde_json::Value,
);

async fn fee_rows(f: &Fixture) -> Vec<Row> {
    sqlx::query_as(
        "SELECT block_number, block_hash, holder, amount::text, unknown_reason, detail \
         FROM fee_position_readings WHERE network = $1 ORDER BY block_number",
    )
    .bind(NETWORK)
    .fetch_all(&f.admin)
    .await
    .unwrap()
}

/// What the core's own login reads of the network's fee position, as text.
async fn observed(f: &Fixture, metric: &str) -> Option<Option<String>> {
    let rows: Vec<(Option<String>,)> = sqlx::query_as(
        "SELECT value::text FROM ops_observables() WHERE metric = $1 AND network = $2",
    )
    .bind(metric)
    .bind(NETWORK)
    .fetch_all(&f.core)
    .await
    .unwrap();
    rows.into_iter().next().map(|r| r.0)
}

async fn supply_count(f: &Fixture) -> i64 {
    f.count(&format!(
        "SELECT count(*) FROM reserve_readings WHERE network = '{NETWORK}' AND kind = 'supply'"
    ))
    .await
}

async fn head_count(f: &Fixture) -> i64 {
    f.count(&format!(
        "SELECT count(*) FROM final_heads WHERE network = '{NETWORK}'"
    ))
    .await
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_h3_1_req_st_6_the_fee_position_is_free_less_frozen_at_the_final_head_with_the_supply(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    declare(&f).await;
    // A declared network with no reading is a row with no value: unknown, not absent, not zero.
    assert_eq!(observed(&f, "fee_position_amount").await, Some(None));

    // Free 1,000 of which 250 is frozen, and 300 reserved (a storage deposit): the position is what can pay.
    f.chain.set_balances(
        f.lane.account,
        Balances {
            free: 1_000,
            reserved: 300,
            frozen: 250,
        },
    );
    f.chain.advance(1);
    let head = f.chain.finalized_ref();
    assert!(matches!(
        supplies(&f).record().await.unwrap(),
        Recorded::At { .. }
    ));

    let rows = fee_rows(&f).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (block, hash, holder, amount, unknown, detail) = &rows[0];
    // At the same final head as the supply reading (REQ-OB-5), recorded with it.
    assert_eq!(*block, i64::try_from(head.number).unwrap());
    assert_eq!(hash.as_slice(), head.hash.as_slice());
    assert_eq!(supply_count(&f).await, 1);
    assert_eq!(
        f.count(&format!(
            "SELECT count(*) FROM reserve_readings WHERE network = '{NETWORK}' AND kind = 'supply' \
               AND block_number = {block} AND block_hash = '\\x{}'",
            hex::encode(head.hash)
        ))
        .await,
        1
    );
    assert_eq!(amount.as_deref(), Some("750"), "free less frozen");
    assert!(unknown.is_none());
    assert_eq!(holder, &format!("0x{}", hex::encode(f.lane.account)));
    for (k, v) in [("free", "1000"), ("reserved", "300"), ("frozen", "250")] {
        assert_eq!(detail[k], v, "{detail}");
    }
    // The core and the operators read it, exact, through the observable; the row of the other
    // network's kind (a holder) never reaches it.
    assert_eq!(
        observed(&f, "fee_position_amount").await,
        Some(Some("750".to_owned()))
    );
    assert!(
        observed(&f, "fee_position_age_seconds")
            .await
            .unwrap()
            .is_some()
    );
    let leaked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ops_observables() o WHERE o::text LIKE '%' || $1 || '%'",
    )
    .bind(hex::encode(f.lane.account))
    .fetch_one(&f.core)
    .await
    .unwrap();
    assert_eq!(leaked, 0, "no address reaches the observables");

    // The same head again records nothing more; the next head is read at its own block.
    assert_eq!(
        supplies(&f).record().await.unwrap(),
        Recorded::Already(head.number)
    );
    f.chain.set_balances(
        f.lane.account,
        Balances {
            free: 40,
            reserved: 300,
            frozen: 0,
        },
    );
    f.chain.advance(1);
    supplies(&f).record().await.unwrap();
    let rows = fee_rows(&f).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].3.as_deref(), Some("40"));
    assert_eq!(
        observed(&f, "fee_position_amount").await,
        Some(Some("40".to_owned())),
        "the latest reading counts"
    );
    // Read at the recorded final head and at no other block.
    let at: Vec<u64> = f.chain.balance_reads();
    assert_eq!(at.len(), 2, "{at:?}");
    assert_eq!(at[0], head.number);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_6_frozen_above_free_is_a_position_of_zero_and_an_unlisted_account_holds_nothing(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    declare(&f).await;
    f.chain.set_balances(
        f.lane.account,
        Balances {
            free: 100,
            reserved: 0,
            frozen: 400,
        },
    );
    f.chain.advance(1);
    supplies(&f).record().await.unwrap();
    let rows = fee_rows(&f).await;
    // Saturated at zero, a known reading, with both figures kept.
    assert_eq!(rows[0].3.as_deref(), Some("0"));
    assert!(rows[0].4.is_none());
    assert_eq!(rows[0].5["free"], "100");
    assert_eq!(rows[0].5["frozen"], "400");

    // The chain proves an account it holds no entry for has nothing: a known zero, which a floor pages on, and
    // not an unknown reading.
    let head = f.chain.finalized_ref();
    let r = fee::read(&f.chain, &[0x99; 32], &head).await;
    assert_eq!(r.amount, Some(0));
    assert!(r.unknown_reason.is_none());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_nx_3_a_balance_that_cannot_be_read_is_unknown_never_zero_and_never_the_last_value(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    declare(&f).await;
    f.chain.set_balances(
        f.lane.account,
        Balances {
            free: 900,
            reserved: 0,
            frozen: 0,
        },
    );
    f.chain.advance(1);
    supplies(&f).record().await.unwrap();
    assert_eq!(
        observed(&f, "fee_position_amount").await,
        Some(Some("900".to_owned()))
    );

    // Only the balance read fails; every other read answers. The head and the supply are recorded; the fee
    // position is a reading that is unknown with its reason.
    f.chain.set_balance_fault(true, false);
    f.chain.advance(1);
    assert!(matches!(
        supplies(&f).record().await.unwrap(),
        Recorded::At { .. }
    ));
    assert_eq!(head_count(&f).await, 2);
    assert_eq!(supply_count(&f).await, 2);
    let rows = fee_rows(&f).await;
    assert_eq!(rows.len(), 2);
    assert!(rows[1].3.is_none(), "unknown, not zero: {rows:?}");
    assert!(
        rows[1]
            .4
            .as_deref()
            .is_some_and(|r| r.contains("unavailable")),
        "{rows:?}"
    );
    // The earlier amount is not carried forward and is not zero.
    assert_eq!(observed(&f, "fee_position_amount").await, Some(None));
    assert!(
        observed(&f, "fee_position_age_seconds")
            .await
            .unwrap()
            .is_some(),
        "the age of the last attempt is still known"
    );

    // And it heals at the next head.
    f.chain.set_balance_fault(false, false);
    f.chain.advance(1);
    supplies(&f).record().await.unwrap();
    assert_eq!(
        observed(&f, "fee_position_amount").await,
        Some(Some("900".to_owned()))
    );
}

#[tokio::test]
async fn req_st_6_a_balance_read_that_never_answers_is_unknown_within_its_bound() {
    use bezent_kreivo_endpoint::chain::fake::FakeChain;
    let chain = FakeChain::new(GENESIS, CONTRACT);
    chain.advance(1);
    chain.set_balance_fault(false, true);
    let head = chain.finalized_ref();
    let started = std::time::Instant::now();
    let r = fee::read_within(&chain, &[7; 32], &head, Duration::from_millis(100)).await;
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(r.amount.is_none());
    assert!(
        r.unknown_reason
            .as_deref()
            .is_some_and(|u| u.contains("unavailable") && u.contains("not answered")),
        "{r:?}"
    );
    assert_eq!(r.holder, format!("0x{}", hex::encode([7u8; 32])));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_6_a_hanging_balance_does_not_keep_the_head_or_the_supply_from_being_recorded(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    declare(&f).await;
    f.chain.set_balance_fault(false, true);
    f.chain.advance(1);
    // The pass is bounded by the read's timeout plus slack, not by the chain.
    let bound = fee::READ_TIMEOUT + Duration::from_secs(20);
    let recorded = tokio::time::timeout(bound, supplies(&f).record())
        .await
        .expect("the pass does not wait on a balance that never answers")
        .unwrap();
    assert!(matches!(recorded, Recorded::At { .. }));
    assert_eq!(head_count(&f).await, 1);
    assert_eq!(supply_count(&f).await, 1);
    let rows = fee_rows(&f).await;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].3.is_none() && rows[0].4.is_some());
    assert_eq!(observed(&f, "fee_position_amount").await, Some(None));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_6_a_fee_position_that_cannot_be_written_does_not_stop_the_head_or_the_supply(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    declare(&f).await;
    // An endpoint rolled out before the grant exists: its INSERT is refused.
    sqlx::query("REVOKE INSERT ON fee_position_readings FROM bezent_kreivo_endpoint")
        .execute(&f.admin)
        .await
        .unwrap();
    f.chain.advance(1);
    assert!(matches!(
        supplies(&f)
            .record()
            .await
            .expect("a fee position that cannot be written is not the step's failure"),
        Recorded::At { .. }
    ));
    assert_eq!(head_count(&f).await, 1, "the head is recorded");
    assert_eq!(supply_count(&f).await, 1, "the supply is recorded");
    assert!(fee_rows(&f).await.is_empty());
    // Nothing was ever written, so the observable stays unknown with no age: the alarm's two signals.
    assert_eq!(observed(&f, "fee_position_amount").await, Some(None));
    assert_eq!(observed(&f, "fee_position_age_seconds").await, Some(None));

    // Once it can be written again, the next head's reading is recorded.
    sqlx::query("GRANT INSERT ON fee_position_readings TO bezent_kreivo_endpoint")
        .execute(&f.admin)
        .await
        .unwrap();
    f.chain.advance(1);
    supplies(&f).record().await.unwrap();
    assert_eq!(fee_rows(&f).await.len(), 1);
    assert_eq!(head_count(&f).await, 2);
}
