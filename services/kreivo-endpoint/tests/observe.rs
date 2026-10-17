// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! T-007-02: the finalized head followed in contiguous ranges, the contract's events recorded as
//! observations by coordinate, and a range that cannot be read made a visible gap that is filled
//! before anything after it (`REQ-OB-1` to `REQ-OB-5`, `AC-D2.3`, `INV-10`, `REQ-RQ-9`).

mod common;

use bezent_kreivo_endpoint::chain::BlockRef;
use bezent_kreivo_endpoint::kreivo::{AssetId, Backend, ContractEvent, Payout};
use bezent_kreivo_endpoint::observe::{self, NewObservation, Step};
use common::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

fn redeemed(reference: [u8; 32], redeemer: [u8; 32], amount: u128) -> ContractEvent {
    ContractEvent::Redeemed {
        seq: 0,
        reference,
        redeemer,
        asset: AssetId::Here(101),
        amount,
        backend: Backend::Evm { chain_id: 137 },
        payout: Payout::Address([0xab; 20]),
    }
}

/// Steps the follower until it stops reading.
async fn follow(f: &Fixture) -> Step {
    loop {
        match f.follower().step().await.unwrap() {
            Step::Read { .. } => continue,
            other => return other,
        }
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_d2_3_a_block_that_cannot_be_read_is_a_visible_gap_filled_before_anything_after_it(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    // Block 4: a redemption. Block 6: another. Block 5 cannot be read.
    f.chain.emit(redeemed([1; 32], [7; 32], 10));
    f.chain.produce();
    f.chain.produce();
    f.chain.emit(redeemed([2; 32], [7; 32], 20));
    f.chain.advance(4);
    f.chain.fail_reads_at(5);

    let step = follow(&f).await;
    let Step::Gap { first, reason, .. } = &step else {
        panic!("{step:?}");
    };
    assert_eq!(*first, 5);
    assert!(reason.contains("block 5"), "{reason}");
    // The gap is visible, open, and the watermark stops before it: nothing after it is read.
    assert_eq!(
        f.count("SELECT count(*) FROM observation_gaps WHERE first_block = 5 AND NOT filled")
            .await,
        1
    );
    assert_eq!(
        observe::watermark(&f.endpoint, NETWORK).await.unwrap(),
        Some(4)
    );
    assert_eq!(f.count("SELECT count(*) FROM observations").await, 1);
    assert_eq!(
        f.count("SELECT count(*) FROM observations WHERE block_number > 4")
            .await,
        0
    );
    // Another step records no second copy of the same open gap.
    follow(&f).await;
    assert_eq!(f.count("SELECT count(*) FROM observation_gaps").await, 1);

    // The block can be read again: the gap is filled from history, then the rest is read.
    f.chain.heal(5);
    let step = follow(&f).await;
    assert_eq!(
        step,
        Step::CaughtUp {
            watermark: f.chain.finalized_ref().number
        }
    );
    assert_eq!(
        f.count("SELECT count(*) FROM observation_gaps WHERE filled")
            .await,
        1
    );
    let seqs: Vec<(i64, String)> = sqlx::query_as(
        "SELECT block_number, detail ->> 'seq' FROM observations ORDER BY block_number",
    )
    .fetch_all(&f.admin)
    .await
    .unwrap();
    assert_eq!(seqs, vec![(4, "0".into()), (6, "1".into())]);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_d2_3_a_jump_in_the_contract_event_sequence_is_a_gap(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    f.chain.emit(redeemed([1; 32], [7; 32], 10));
    f.chain.produce();
    // An event the endpoint never sees: the next one skips a sequence number.
    f.chain.skip_seq();
    f.chain.emit(redeemed([2; 32], [7; 32], 10));
    f.chain.advance(1);

    let step = follow(&f).await;
    let Step::Gap { first, reason, .. } = step else {
        panic!("{step:?}");
    };
    assert_eq!(first, 5);
    assert!(
        reason.contains("AC-D2.3") && reason.contains("from 1 to 2"),
        "{reason}"
    );
    assert_eq!(f.count("SELECT count(*) FROM observations").await, 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ob_3_the_store_refuses_a_range_past_the_watermark(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let head = f.chain.finalized_ref();
    observe::record_read(&f.endpoint, NETWORK, &CONTRACT, 1, 2, &head, &[])
        .await
        .unwrap();
    // Block 3 was not read: a range from block 4 would move past it.
    let err = observe::record_read(&f.endpoint, NETWORK, &CONTRACT, 4, 4, &head, &[])
        .await
        .unwrap_err();
    let db = err.as_database_error().expect("a store refusal");
    assert_eq!(db.code().as_deref(), Some(bezent_store::REFUSAL_SQLSTATE));
    assert!(db.message().contains("REQ-OB-3"), "{}", db.message());
    // Overlapping and adjacent ranges are fine.
    observe::record_read(&f.endpoint, NETWORK, &CONTRACT, 2, 3, &head, &[])
        .await
        .unwrap();
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_10_an_observation_is_recorded_once_per_coordinate(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let head = f.chain.finalized_ref();
    let block = f.chain.block(2);
    let o = NewObservation {
        block,
        time_ms: 1_760_000_012_000,
        index: 3,
        event: ContractEvent::Minted {
            seq: 0,
            reference: [5; 32],
            asset: AssetId::Here(101),
            to: [6; 32],
            amount: 1_000_000,
        },
    };
    let first = observe::record_read(
        &f.endpoint,
        NETWORK,
        &CONTRACT,
        1,
        2,
        &head,
        std::slice::from_ref(&o),
    )
    .await
    .unwrap();
    let again = observe::record_read(&f.endpoint, NETWORK, &CONTRACT, 1, 2, &head, &[o])
        .await
        .unwrap();
    assert_eq!((first, again), (1, 0));
    assert_eq!(f.count("SELECT count(*) FROM observations").await, 1);
    assert_eq!(
        f.count("SELECT count(*) FROM observation_outcomes").await,
        1
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rq_9_a_redemption_is_observed_at_its_coordinate_with_what_the_core_needs(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    f.chain.emit(redeemed([9; 32], [7; 32], 2_500_000));
    f.chain.advance(1);
    assert!(matches!(follow(&f).await, Step::CaughtUp { .. }));

    type Row = (
        String,
        i64,
        Vec<u8>,
        String,
        i64,
        Option<String>,
        String,
        Option<String>,
        Option<Vec<u8>>,
        bool,
        String,
    );
    let row: Row = sqlx::query_as(
        "SELECT network, block_number, block_hash, position_kind, position_index, token, \
           amount::text, sender, on_chain_reference, executed, detail::text FROM observations",
    )
    .fetch_one(&f.admin)
    .await
    .unwrap();
    let block: BlockRef = f.chain.block(4);
    assert_eq!(row.0, NETWORK);
    assert_eq!((row.1, row.2.as_slice()), (4, block.hash.as_slice()));
    assert_eq!((row.3.as_str(), row.4), ("log", 0));
    assert_eq!(row.5.as_deref(), Some("Here(101)"));
    assert_eq!(row.6, "2500000");
    assert_eq!(row.7, Some(format!("0x{}", hex::encode([7; 32]))));
    assert_eq!(row.8, Some(vec![9; 32]));
    assert!(row.9);
    let detail: serde_json::Value = serde_json::from_str(&row.10).unwrap();
    assert_eq!(detail["event"], "Redeemed");
    assert_eq!(
        detail["backend"],
        serde_json::json!({"kind": "evm", "chain_id": 137})
    );
    assert_eq!(
        detail["payout"],
        serde_json::json!({"address": format!("0x{}", hex::encode([0xab; 20]))})
    );
    // Read at a final block, under the finalized head (REQ-OB-2).
    let (outcome, at): (String, i64) =
        sqlx::query_as("SELECT outcome, final_block_number FROM observation_outcomes")
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert_eq!(outcome, "final");
    assert!(at >= 4);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ob_5_nothing_above_the_finalized_head_is_read(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    f.chain.emit(redeemed([1; 32], [7; 32], 10));
    // Produced, not final.
    f.chain.produce();
    f.chain.produce();
    assert_eq!(follow(&f).await, Step::CaughtUp { watermark: 3 });
    assert_eq!(f.count("SELECT count(*) FROM observations").await, 0);
    f.chain.finalize(5);
    follow(&f).await;
    assert_eq!(f.count("SELECT count(*) FROM observations").await, 1);
}
