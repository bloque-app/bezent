// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Skipping stretches proven unchanged (`notes/KREIVO-WALK-SKIP.md`; `REQ-OB-3` as amendment 0027
//! drafts it, `AC-D2.3`): a skipping walk records what reading every block records, the
//! contract's every event (`FakeChain::contract_events`) and the blocks from `start_block` to the
//! head as read without a hole, while reading only the blocks near a change.

mod common;

use bezent_kreivo_endpoint::chain::{ChainError, RuntimeVersion};
use bezent_kreivo_endpoint::kreivo::{AssetId, Backend, ContractEvent, Payout};
use bezent_kreivo_endpoint::observe::{self, Step};
use bezent_kreivo_endpoint::skip::{Skip, Subject};
use common::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

const CODE: [u8; 32] = [0xcd; 32];
const RUNG: u64 = 256;

fn redeemed(reference: [u8; 32], amount: u128) -> ContractEvent {
    ContractEvent::Redeemed {
        seq: 0,
        reference,
        redeemer: [7; 32],
        asset: AssetId::Here(101),
        amount,
        backend: Backend::Evm { chain_id: 137 },
        payout: Payout::Address([0xab; 20]),
    }
}

fn skip(f: &Fixture) -> Skip {
    f.chain.set_code_hash(Some(CODE));
    Skip::new(Subject {
        contract: CONTRACT,
        code_hash: CODE,
        app: (5, 0),
        lane: f.lane.account,
        assets: vec![101],
    })
}

/// Steps the follower, skipping or not, until it stops reading.
async fn walk(f: &Fixture, skip: Option<&Skip>) -> Step {
    let follower = observe::Follower {
        rung: RUNG,
        max_range: 64,
        skip,
        ..f.follower()
    };
    loop {
        match follower.step().await.unwrap() {
            Step::Read { .. } => continue,
            other => return other,
        }
    }
}

/// The observations recorded, as (block, event index, seq).
async fn observations(f: &Fixture) -> Vec<(u64, u32, u64)> {
    let rows: Vec<(i64, i64, String)> = sqlx::query_as(
        "SELECT block_number, position_index, detail ->> 'seq' FROM observations \
         WHERE position_kind = 'log' ORDER BY block_number, position_index",
    )
    .fetch_all(&f.admin)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(b, i, s)| (b as u64, i as u32, s.parse().unwrap()))
        .collect()
}

/// The read ranges cover 1 to `head` without a hole or an overlap, and there is no open gap.
async fn read_without_a_hole(f: &Fixture, head: u64) {
    let ranges: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT first_block, last_block FROM observation_ranges WHERE outcome = 'read' ORDER BY id",
    )
    .fetch_all(&f.admin)
    .await
    .unwrap();
    let mut next = 1;
    for (first, last) in ranges {
        assert_eq!(first as u64, next, "a range starts at {first}, not {next}");
        next = last as u64 + 1;
    }
    assert_eq!(next, head + 1);
    assert_eq!(
        f.count("SELECT count(*) FROM observation_gaps WHERE NOT filled")
            .await,
        0
    );
}

/// What reading every block records, and the blocks a skipping walk read the events of after the
/// first rung (which it reads whole: there is no watermark to start from).
async fn same_as_every_block(f: &Fixture, head: u64) -> Vec<u64> {
    assert_eq!(observations(f).await, f.chain.contract_events());
    read_without_a_hole(f, head).await;
    f.chain
        .event_reads()
        .into_iter()
        .filter(|b| *b > RUNG)
        .collect()
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn skip_a_stretch_with_nothing_in_it_is_read_without_reading_its_blocks(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let skip = skip(&f);
    f.chain.advance(2_000);
    assert_eq!(
        walk(&f, Some(&skip)).await,
        Step::CaughtUp { watermark: 2_003 }
    );
    // No block after the first rung had its events read, and the walk to each rung's top jumped
    // by proven hashes: a header for each jump, none per block.
    assert_eq!(same_as_every_block(&f, 2_003).await, Vec::<u64>::new());
    let headers = f
        .chain
        .header_reads()
        .into_iter()
        .filter(|b| *b > RUNG)
        .count();
    assert!(headers < 50, "{headers} headers read above the first rung");
    // Each skipped stretch's last block is anchored, so the proof can be made again.
    assert!(
        f.count("SELECT count(*) FROM observation_walk_anchors WHERE block_number = 2003")
            .await
            == 1
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn skip_an_event_inside_a_stretch_is_found_by_bisection(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let skip = skip(&f);
    // A redemption in block 4 (in the first rung) and one in block 1,500.
    f.chain.emit(redeemed([1; 32], 10));
    f.chain.advance(1_496);
    f.chain.emit(redeemed([2; 32], 20));
    f.chain.advance(500);
    assert_eq!(
        walk(&f, Some(&skip)).await,
        Step::CaughtUp { watermark: 1_999 }
    );
    let reads = same_as_every_block(&f, 1_999).await;
    assert_eq!(observations(&f).await.len(), 2);
    // Only a leaf around block 1,500 was read block by block.
    assert!(reads.contains(&1_500), "{reads:?}");
    assert!(reads.iter().all(|b| b.abs_diff(1_500) <= 16), "{reads:?}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn skip_a_mint_and_a_burn_that_net_outside_the_contract_record_what_every_block_records(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let skip = skip(&f);
    f.chain.set_supply(101, 1_000, 1_000);
    f.chain.advance(1_026);
    // In blocks 1,030 to 1,099 the asset's admin has minted 500 outside the contract; in block
    // 1,100 it burns them again. The walk reads no Assets event, so reading every block records
    // nothing of it either: the supply check at the finalized head is what sees an outside mint
    // that stays (INV-4).
    f.chain.set_supply(101, 1_000, 1_500);
    f.chain.advance(70);
    f.chain.set_supply(101, 1_000, 1_000);
    f.chain.advance(400);
    // And one that does not net: the supply stays changed from block 1,500 on.
    f.chain.set_supply(101, 1_000, 1_200);
    f.chain.advance(100);
    assert_eq!(
        walk(&f, Some(&skip)).await,
        Step::CaughtUp { watermark: 1_599 }
    );
    let reads = same_as_every_block(&f, 1_599).await;
    assert_eq!(observations(&f).await, vec![]);
    // The pair that nets inside one rung (1,024 to 1,280) leaves its ends equal: skipped, like
    // the full walk sees nothing there. The supply that stays changed is narrowed down to the
    // blocks around it, where nothing is found either.
    assert!(reads.contains(&1_500), "{reads:?}");
    assert!(reads.iter().all(|b| b.abs_diff(1_500) <= 16), "{reads:?}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn skip_an_upgrade_inside_a_stretch_is_read_block_by_block_around_it(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let skip = skip(&f);
    f.chain.advance(997);
    // Spec 136 from block 1,001, with a redemption after it; then spec 135, a runtime whose paths
    // to the contract's code were not read, from block 1,601: everything under it is read block by
    // block.
    f.chain.set_version(RuntimeVersion {
        spec_version: 136,
        transaction_version: 13,
    });
    f.chain.advance(300);
    f.chain.emit(redeemed([3; 32], 30));
    f.chain.advance(300);
    f.chain.set_version(RuntimeVersion {
        spec_version: 135,
        transaction_version: 13,
    });
    f.chain.advance(100);
    assert_eq!(
        walk(&f, Some(&skip)).await,
        Step::CaughtUp { watermark: 1_700 }
    );
    let reads = same_as_every_block(&f, 1_700).await;
    assert_eq!(observations(&f).await.len(), 1);
    assert!(
        reads.contains(&1_001) && reads.contains(&1_301),
        "{reads:?}"
    );
    // Under the unread runtime, every block from its first rung's start on was read.
    assert!((1_601..=1_700).all(|b| reads.contains(&b)), "{reads:?}");
    let skipped: Vec<_> = (257..=1_536).filter(|b| !reads.contains(b)).collect();
    assert!(skipped.len() > 1_000, "{} of 1,280 skipped", skipped.len());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn skip_a_publish_in_the_contracts_store_is_read_block_by_block_around_it(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let skip = skip(&f);
    f.chain.advance(800);
    // The app is published again: the instance's code could have been swapped and back.
    f.chain.publish_app();
    f.chain.advance(400);
    assert_eq!(
        walk(&f, Some(&skip)).await,
        Step::CaughtUp { watermark: 1_203 }
    );
    let reads = same_as_every_block(&f, 1_203).await;
    assert!(reads.contains(&804), "{reads:?}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn skip_a_proof_that_does_not_verify_is_a_visible_gap(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let skip = skip(&f);
    f.chain.advance(1_000);
    // The end state of block 512, the second rung's top, does not verify.
    f.chain.fail_end_state_at(
        512,
        ChainError::Invalid("a storage proof does not verify".into()),
    );
    let step = walk(&f, Some(&skip)).await;
    let Step::Gap {
        first,
        last,
        reason,
    } = &step
    else {
        panic!("{step:?}");
    };
    assert_eq!((*first, *last), (257, 512));
    assert!(
        reason.starts_with("skip: ") && reason.contains("does not verify"),
        "{reason}"
    );
    assert_eq!(
        observe::watermark(&f.endpoint, NETWORK).await.unwrap(),
        Some(256)
    );
    assert_eq!(
        f.count("SELECT count(*) FROM observation_gaps WHERE first_block = 257 AND NOT filled")
            .await,
        1
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn skip_an_end_that_cannot_be_read_is_read_block_by_block(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let skip = skip(&f);
    f.chain.emit(redeemed([4; 32], 40));
    f.chain.advance(700);
    f.chain.fail_end_state_at(
        512,
        ChainError::Unavailable("the history source did not answer".into()),
    );
    assert_eq!(
        walk(&f, Some(&skip)).await,
        Step::CaughtUp { watermark: 703 }
    );
    let reads = same_as_every_block(&f, 703).await;
    // Both rungs that end at the block that could not be told were read whole.
    assert_eq!(reads, (257..=703).collect::<Vec<_>>());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn skip_off_reads_every_block_as_before(pool: PgPoolOptions, connect: PgConnectOptions) {
    let f = Fixture::new(pool, connect).await;
    let _ = skip(&f);
    f.chain.emit(redeemed([5; 32], 50));
    f.chain.advance(600);
    assert_eq!(walk(&f, None).await, Step::CaughtUp { watermark: 603 });
    assert_eq!(observations(&f).await, f.chain.contract_events());
    read_without_a_hole(&f, 603).await;
    assert_eq!(f.chain.event_reads(), (1..=603).collect::<Vec<_>>());
    assert!(f.chain.end_reads().is_empty());
}
