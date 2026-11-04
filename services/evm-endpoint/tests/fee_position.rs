// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #14: the delivery account's native balance, its fee position, is recorded at each final
//! head the endpoint records, from both providers; providers that disagree, or one that cannot
//! answer, make it unknown, and an unknown fee position never stops the head or the reserve
//! readings (`REQ-ST-6`, `AC-H3.1`, `REQ-OB-5`, `REQ-NX-3`; PLAN `DEC-12`, `DEC-18`).

mod common;

use alloy::primitives::U256;
use bezent_evm_endpoint::fee;
use common::{Fixture, NETWORK};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

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

async fn head_count(f: &Fixture) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM final_heads WHERE network = $1")
        .bind(NETWORK)
        .fetch_one(&f.admin)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_h3_1_req_st_6_the_delivery_accounts_balance_is_recorded_at_the_final_head_and_observable(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    // Before the endpoint has read anything the position is unknown, not zero.
    assert_eq!(observed(&f, "fee_position_amount").await, Some(None));

    // 2^53 + 1 wei: a float cannot hold it, the store does.
    let wei = U256::from(9_007_199_254_740_993u64);
    f.chain.set_balance(f.lane.address, wei);
    f.follower().step().await.unwrap();

    let (head_number, head_hash): (i64, Vec<u8>) = sqlx::query_as(
        "SELECT block_number, block_hash FROM final_heads WHERE network = $1 \
         ORDER BY block_number DESC LIMIT 1",
    )
    .bind(NETWORK)
    .fetch_one(&f.admin)
    .await
    .unwrap();
    let got = fee_rows(&f).await;
    assert_eq!(got.len(), 1, "{got:?}");
    let (block, hash, holder, amount, unknown, detail) = &got[0];
    // At the final head, with its hash, for the lane's address.
    assert_eq!((*block, hash), (head_number, &head_hash));
    assert_eq!(holder, &f.lane.address.to_checksum(None));
    assert_eq!(amount.as_deref(), Some("9007199254740993"));
    assert!(unknown.is_none());
    assert_eq!(
        detail["providers"][0]["answer"]["balance"],
        "9007199254740993"
    );
    assert_eq!(
        detail["providers"][1]["answer"]["balance"],
        "9007199254740993"
    );

    // The core's login reads it, exactly, through the observables.
    assert_eq!(
        observed(&f, "fee_position_amount").await,
        Some(Some("9007199254740993".to_owned()))
    );
    assert!(
        observed(&f, "fee_position_age_seconds")
            .await
            .unwrap()
            .is_some()
    );

    // A head already recorded is not read again; the next head is, and the latest counts.
    f.follower().step().await.unwrap();
    assert_eq!(fee_rows(&f).await.len(), 1);
    f.chain.set_balance(f.lane.address, U256::from(42u64));
    f.advance(1);
    f.follower().step().await.unwrap();
    assert_eq!(fee_rows(&f).await.len(), 2);
    assert_eq!(
        observed(&f, "fee_position_amount").await,
        Some(Some("42".to_owned()))
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_nx_3_providers_that_disagree_make_the_fee_position_unknown_and_the_head_is_still_recorded(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let chain = bezent_evm_endpoint::rpc::mock::MockChain::new(common::CHAIN_ID, 5);
    let other = chain.fork();
    let f = Fixture::with_chains(pool, connect, chain, other).await;
    f.chain.set_balance(f.lane.address, U256::from(1_000u64));
    f.chain_b.set_balance(f.lane.address, U256::from(1_001u64));
    f.follower().step().await.unwrap();

    let got = fee_rows(&f).await;
    assert_eq!(got.len(), 1);
    let (_, _, _, amount, unknown, detail) = &got[0];
    // Never the higher, never the lower, never zero: unknown, with both answers kept.
    assert!(amount.is_none());
    let unknown = unknown.as_deref().unwrap();
    assert!(
        unknown.contains("DEC-12") && unknown.contains("1000") && unknown.contains("1001"),
        "{unknown}"
    );
    assert_eq!(detail["providers"][0]["answer"]["balance"], "1000");
    assert_eq!(detail["providers"][1]["answer"]["balance"], "1001");
    assert_eq!(observed(&f, "fee_position_amount").await, Some(None));

    // The unknown fee position did not cost the head, nor the reserve readings the check needs.
    assert!(head_count(&f).await >= 1);
    let reserve: i64 =
        sqlx::query_scalar("SELECT count(*) FROM reserve_readings WHERE network = $1")
            .bind(NETWORK)
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert!(reserve > 0, "the reserve readings are recorded regardless");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_nx_3_a_provider_that_cannot_answer_a_balance_makes_it_unknown_without_stopping_the_follower(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let chain = bezent_evm_endpoint::rpc::mock::MockChain::new(common::CHAIN_ID, 5);
    let other = chain.fork();
    let f = Fixture::with_chains(pool, connect, chain, other).await;
    f.chain_b.fail_balance(Some("rate limited"));
    f.follower().step().await.unwrap();

    let got = fee_rows(&f).await;
    assert_eq!(got.len(), 1);
    assert!(got[0].3.is_none());
    assert!(
        got[0]
            .4
            .as_deref()
            .is_some_and(|u| u.contains("unavailable") && u.contains("rate limited")),
        "{got:?}"
    );
    assert_eq!(observed(&f, "fee_position_amount").await, Some(None));
    // The follower went on to read the blocks: the head is recorded and nothing is a gap.
    assert!(head_count(&f).await >= 1);
    let gaps: i64 = sqlx::query_scalar("SELECT count(*) FROM observation_gaps WHERE network = $1")
        .bind(NETWORK)
        .fetch_one(&f.admin)
        .await
        .unwrap();
    assert_eq!(gaps, 0);

    // Once the provider answers, the next head's reading is known again.
    f.chain_b.fail_balance(None);
    // The next block, as both providers show it (forks mine blocks of their own).
    let next = f.chain.mine();
    f.chain_b.replace_with(next);
    f.chain.finalize(next.number);
    f.chain_b.finalize(next.number);
    f.follower().step().await.unwrap();
    let got = fee_rows(&f).await;
    assert_eq!(got.len(), 2);
    assert!(got[1].3.is_some() && got[1].4.is_none(), "{got:?}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_nx_3_a_balance_above_the_stores_domain_is_unknown_not_clipped(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    f.chain.set_balance(f.lane.address, U256::MAX);
    let r = fee::read(&f.pair, f.lane.address, 4).await;
    assert!(r.amount.is_none());
    assert!(
        r.unknown_reason
            .as_deref()
            .is_some_and(|u| u.contains("above the store"))
    );
}

async fn reserve_count(f: &Fixture) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM reserve_readings WHERE network = $1")
        .bind(NETWORK)
        .fetch_one(&f.admin)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_6_a_fee_position_that_cannot_be_written_does_not_stop_the_head_or_the_reserve(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    // An endpoint rolled out before the grant (or the table) exists: its INSERT is refused.
    sqlx::query("REVOKE INSERT ON fee_position_readings FROM bezent_evm_endpoint")
        .execute(&f.admin)
        .await
        .unwrap();

    f.follower()
        .step()
        .await
        .expect("a fee position that cannot be written is not the step's failure");
    assert!(head_count(&f).await >= 1, "the head is recorded");
    assert!(
        reserve_count(&f).await > 0,
        "the reserve readings are recorded"
    );
    assert!(fee_rows(&f).await.is_empty());
    // The follower advanced: the next head is taken up, again without error.
    f.advance(1);
    f.follower().step().await.unwrap();
    assert!(head_count(&f).await >= 2);

    // Nothing was ever written, so the observable stays unknown with no age, which the alarm on a
    // missing value and on a growing age both read.
    assert_eq!(observed(&f, "fee_position_amount").await, Some(None));
    assert_eq!(observed(&f, "fee_position_age_seconds").await, Some(None));

    // Once it can be written again, the next head's reading is recorded.
    sqlx::query("GRANT INSERT ON fee_position_readings TO bezent_evm_endpoint")
        .execute(&f.admin)
        .await
        .unwrap();
    f.advance(1);
    f.follower().step().await.unwrap();
    assert_eq!(fee_rows(&f).await.len(), 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_6_a_hanging_balance_read_is_unknown_and_the_step_finishes(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let chain = bezent_evm_endpoint::rpc::mock::MockChain::new(common::CHAIN_ID, 5);
    let other = chain.fork();
    let f = Fixture::with_chains(pool, connect, chain, other).await;
    f.chain_b.hang_balance(true);

    // The step finishes, bounded by the read's timeout plus slack, not by the provider.
    let bound = fee::READ_TIMEOUT + std::time::Duration::from_secs(20);
    tokio::time::timeout(bound, f.follower().step())
        .await
        .expect("the step does not wait on a provider that never answers a balance")
        .unwrap();

    let got = fee_rows(&f).await;
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].3.is_none());
    assert!(
        got[0]
            .4
            .as_deref()
            .is_some_and(|u| u.contains("unavailable")),
        "{got:?}"
    );
    assert_eq!(observed(&f, "fee_position_amount").await, Some(None));
    assert!(head_count(&f).await >= 1);
    assert!(reserve_count(&f).await > 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_6_the_fee_position_is_read_at_the_heads_block_from_both_providers(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let chain = bezent_evm_endpoint::rpc::mock::MockChain::new(common::CHAIN_ID, 5);
    let other = chain.fork();
    let f = Fixture::with_chains(pool, connect, chain, other).await;
    let before = (
        f.chain.balance_reads().len(),
        f.chain_b.balance_reads().len(),
    );
    f.follower().step().await.unwrap();

    let head: i64 =
        sqlx::query_scalar("SELECT max(block_number) FROM final_heads WHERE network = $1")
            .bind(NETWORK)
            .fetch_one(&f.admin)
            .await
            .unwrap();
    let at = bezent_evm_endpoint::rpc::At::Number(head as u64);
    // The reserve readings ask balances too; the fee read is the lane's, and every balance read
    // of the step is pinned to the head, never to "latest" or "finalized".
    for reads in [f.chain.balance_reads(), f.chain_b.balance_reads()] {
        assert!(reads.len() > before.0.min(before.1), "{reads:?}");
        assert!(reads.iter().all(|a| *a == at), "{reads:?} against {at:?}");
    }
}

#[tokio::test]
async fn req_st_6_a_provider_that_never_answers_is_unknown_within_the_bound() {
    use bezent_evm_endpoint::pair::Pair;
    use bezent_evm_endpoint::rpc::mock::{MockChain, MockRpc};
    let chain = MockChain::new(common::CHAIN_ID, 5);
    chain.hang_balance(true);
    let other = MockChain::new(common::CHAIN_ID, 5);
    let pair = Pair {
        a: MockRpc::new("a", &chain),
        b: MockRpc::new("b", &other),
    };
    let holder = alloy::primitives::Address::repeat_byte(7);
    let r = fee::read_within(&pair, holder, 4, std::time::Duration::from_millis(50)).await;
    assert!(r.amount.is_none());
    assert!(
        r.unknown_reason
            .as_deref()
            .is_some_and(|u| u.contains("unavailable"))
    );
}
