// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! T-009-02, T-009-03, T-009-05: the finalized-range follower, gaps and reorgs; arrivals,
//! convertibles and conversions at the vault; payout legs and reference pairing (`REQ-OB-*`,
//! `REQ-IN-*`, `REQ-CV-2`, `REQ-CV-4`, `AC-A2.1`–`AC-A2.3`, `AC-D2.3`, `AC-C1.3`, `INV-10`,
//! `INV-14`; PLAN `DEC-8`, `DEC-12`).

mod common;

use alloy::primitives::{Address, B256, U256};
use bezent_evm_endpoint::evm;
use bezent_evm_endpoint::observe::Step;
use bezent_evm_endpoint::rpc::mock::MockChain;
use common::{
    CHAIN_ID, Fixture, contracts, delivery, depositor, payout_logs, pool_address, transfer_log,
    usdc, usdt0, vault,
};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

type Row = (
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<Vec<u8>>,
    serde_json::Value,
);

async fn observations(f: &Fixture) -> Vec<Row> {
    sqlx::query_as(
        "SELECT block_number, detail ->> 'event', token, amount::text, sender, recipient, \
           on_chain_reference, detail FROM observations \
         WHERE network = 'polygon' AND position_kind = 'log' ORDER BY block_number, position_index",
    )
    .fetch_all(&f.admin)
    .await
    .unwrap()
}

async fn caught_up(f: &Fixture) -> Step {
    loop {
        match f.follower().step().await.unwrap() {
            Step::Read { .. } => continue,
            other => return other,
        }
    }
}

fn tx(n: u8) -> B256 {
    B256::repeat_byte(n)
}

/// An arrival of `amount` USDC from the depositor, in a new block.
fn arrive(c: &MockChain, amount: u64) -> u64 {
    let h = c.mine();
    transfer_log(
        c,
        h.number,
        3,
        tx(h.number as u8),
        usdc(),
        depositor(),
        vault(),
        U256::from(amount),
    );
    h.number
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_10_req_in_1_an_arrival_is_recorded_once_at_its_coordinate_with_its_final_point(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 3).await;
    let block = arrive(&f.chain, 700);
    f.advance(2);
    assert_eq!(caught_up(&f).await, Step::CaughtUp { watermark: 5 });
    // A second look records nothing twice (INV-10).
    assert_eq!(caught_up(&f).await, Step::CaughtUp { watermark: 5 });
    let rows = observations(&f).await;
    assert_eq!(rows.len(), 1);
    let (n, event, token, amount, sender, recipient, reference, _) = &rows[0];
    assert_eq!(u64::try_from(*n).unwrap(), block);
    assert_eq!(event.as_deref(), Some("Arrival"));
    assert_eq!(token.as_deref(), Some(usdc().to_string().as_str()));
    assert_eq!(amount.as_deref(), Some("700"));
    assert_eq!(sender.as_deref(), Some(depositor().to_string().as_str()));
    // The reserve's spelling in the store, so a deposit request binds it (REQ-IN-3).
    assert_eq!(recipient.as_deref(), Some(common::vault_text().as_str()));
    assert!(reference.is_none());
    // Final as observed, at a recorded final head with the same hash (REQ-OB-2).
    let finals = f
        .count(
            "SELECT count(*) FROM observation_outcomes x JOIN observations o ON o.id = x.observation_id \
             JOIN final_heads h ON h.network = o.network AND h.block_number = x.final_block_number \
               AND h.block_hash = x.final_block_hash \
             WHERE x.outcome = 'final' AND x.final_block_number >= o.block_number",
        )
        .await;
    assert_eq!(finals, 1);
    // The coordinate: block, its hash, the log index.
    let index = f
        .count("SELECT position_index FROM observations WHERE position_kind = 'log'")
        .await;
    assert_eq!(index, 3);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a2_1_an_arrival_above_the_final_point_is_not_recorded(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 3).await;
    let block = arrive(&f.chain, 700);
    f.chain.mine();
    // Not final yet: the finalized head is still block 2.
    assert_eq!(caught_up(&f).await, Step::CaughtUp { watermark: 2 });
    assert!(observations(&f).await.is_empty());
    f.chain.finalize(block);
    assert_eq!(caught_up(&f).await, Step::CaughtUp { watermark: block });
    assert_eq!(observations(&f).await.len(), 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a2_3_an_arrival_in_a_block_replaced_before_finality_is_never_recorded(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 3).await;
    let block = arrive(&f.chain, 700);
    caught_up(&f).await;
    // The block is replaced before finality: its arrival is gone.
    f.chain.drop_logs(block);
    f.chain.replace(block);
    f.chain.finalize(block);
    assert_eq!(caught_up(&f).await, Step::CaughtUp { watermark: block });
    assert!(observations(&f).await.is_empty());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ob_2_a_log_with_another_hash_than_the_final_block_is_a_gap(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 3).await;
    let block = arrive(&f.chain, 700);
    f.chain.finalize(block);
    // Both providers serve the log with the hash of a block that is not the final one.
    f.chain.set_log_hash(block, B256::repeat_byte(0xee));
    let Step::Gap { first, reason, .. } = caught_up(&f).await else {
        panic!("not a gap");
    };
    assert_eq!(first, 1);
    assert!(reason.contains("REQ-OB-2"), "{reason}");
    assert!(observations(&f).await.is_empty());
    // The provider recovers: the gap is filled, the arrival recorded at the final hash.
    let good = f.chain.header_at(block).hash;
    f.chain.set_log_hash(block, good);
    assert_eq!(caught_up(&f).await, Step::CaughtUp { watermark: block });
    assert_eq!(observations(&f).await.len(), 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_d2_3_a_range_that_cannot_be_read_is_a_visible_gap_filled_before_moving_on(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 3).await;
    for _ in 0..6 {
        f.chain.mine();
    }
    let block = arrive(&f.chain, 900);
    f.chain.finalize(block);
    f.chain.fail_logs(Some("upstream timeout"));
    let Step::Gap { first, last, .. } = caught_up(&f).await else {
        panic!("not a gap");
    };
    assert_eq!((first, last), (1, 4));
    // The gap is visible and open; nothing past it was read.
    let open = f
        .count("SELECT count(*) FROM observation_gaps WHERE NOT filled")
        .await;
    assert_eq!(open, 1);
    let watermark = f
        .count("SELECT coalesce(max(watermark), 0) FROM observation_watermarks")
        .await;
    assert_eq!(watermark, 0);
    // The same gap is recorded once.
    caught_up(&f).await;
    assert_eq!(
        f.count("SELECT count(*) FROM observation_ranges WHERE outcome = 'gap'")
            .await,
        1
    );
    // Filled from history before anything after it.
    f.chain.fail_logs(None);
    assert_eq!(caught_up(&f).await, Step::CaughtUp { watermark: block });
    assert_eq!(
        f.count("SELECT count(*) FROM observation_gaps WHERE NOT filled")
            .await,
        0
    );
    assert_eq!(observations(&f).await.len(), 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_12_providers_that_disagree_on_a_range_record_a_gap_never_an_arrival(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let a = MockChain::new(CHAIN_ID, 3);
    let b = a.fork();
    // Only provider a reports the arrival; the block hashes agree.
    let h = a.mine();
    b.mine();
    let shared = a.header_at(h.number);
    b.replace_with(shared);
    transfer_log(
        &a,
        h.number,
        0,
        tx(1),
        usdc(),
        depositor(),
        vault(),
        U256::from(5),
    );
    a.finalize(h.number);
    b.finalize(h.number);
    let f = Fixture::with_chains(pool, connect, a, b).await;
    let Step::Gap { reason, .. } = caught_up(&f).await else {
        panic!("not a gap");
    };
    assert!(reason.contains("DEC-12"), "{reason}");
    assert!(observations(&f).await.is_empty());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_cv_4_req_cv_2_convertibles_are_itemised_and_conversions_carry_both_amounts(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 3).await;
    // A USDT0 arrival: itemised, never reserve (REQ-CV-4).
    let h = f.chain.mine();
    transfer_log(
        &f.chain,
        h.number,
        0,
        tx(1),
        usdt0(),
        depositor(),
        vault(),
        U256::from(1_000),
    );
    // Swaps' conversion under its role: the pool pays USDC to the vault, the vault pays the pool
    // USDT0, in one transaction (REQ-CV-2).
    let h2 = f.chain.mine();
    transfer_log(
        &f.chain,
        h2.number,
        4,
        tx(2),
        usdc(),
        pool_address(),
        vault(),
        U256::from(998),
    );
    transfer_log(
        &f.chain,
        h2.number,
        5,
        tx(2),
        usdt0(),
        vault(),
        pool_address(),
        U256::from(1_000),
    );
    f.chain.finalize(h2.number);
    caught_up(&f).await;
    let rows = observations(&f).await;
    let events: Vec<_> = rows.iter().map(|r| r.1.clone().unwrap()).collect();
    assert_eq!(
        events,
        ["ConvertibleArrival", "Conversion", "ConversionInput"]
    );
    let conversion = &rows[1];
    assert_eq!(conversion.3.as_deref(), Some("998"));
    assert_eq!(conversion.7["conversion"]["amount_in"], "1000");
    assert_eq!(conversion.7["conversion"]["token_in"], usdt0().to_string());
    assert_eq!(conversion.7["conversion"]["log_index_in"], 5);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_6_other_arrivals_are_recorded_unassociated_and_never_stop_the_follower(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 3).await;
    let h = f.chain.mine();
    let spam = Address::repeat_byte(0x5a);
    // A token nobody admitted, with an amount past u128, and an ERC-721 transfer to the vault.
    transfer_log(
        &f.chain,
        h.number,
        0,
        tx(1),
        spam,
        depositor(),
        vault(),
        U256::MAX,
    );
    f.chain.log(
        h.number,
        1,
        tx(1),
        1,
        spam,
        vec![
            evm::TRANSFER_TOPIC,
            depositor().into_word(),
            vault().into_word(),
            B256::with_last_byte(7),
        ],
        vec![],
    );
    f.chain.finalize(h.number);
    assert_eq!(
        caught_up(&f).await,
        Step::CaughtUp {
            watermark: h.number
        }
    );
    let rows = observations(&f).await;
    assert_eq!(rows[0].1.as_deref(), Some("OtherArrival"));
    assert_eq!(rows[0].3, None);
    assert_eq!(rows[0].7["amount_text"], U256::MAX.to_string());
    assert_eq!(rows[1].1.as_deref(), Some("UndecodedTransfer"));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_c1_3_inv_14_a_payout_leg_carries_its_reference_token_sender_and_recipient(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 3).await;
    let h = f.chain.mine();
    let d = delivery(B256::repeat_byte(0x77), 250);
    payout_logs(&f.chain, h.number, tx(9), &d);
    f.chain.finalize(h.number);
    caught_up(&f).await;
    let rows = observations(&f).await;
    assert_eq!(
        rows.len(),
        1,
        "the paired reference is the payout's, not its own"
    );
    let (_, event, token, amount, sender, recipient, reference, detail) = &rows[0];
    assert_eq!(event.as_deref(), Some("Payout"));
    assert_eq!(token.as_deref(), Some(usdc().to_string().as_str()));
    assert_eq!(amount.as_deref(), Some("250"));
    assert_eq!(sender.as_deref(), Some(common::vault_text().as_str()));
    assert_eq!(recipient.as_deref(), Some(d.to.to_string().as_str()));
    assert_eq!(reference.as_deref(), Some(d.reference.as_slice()));
    assert_eq!(detail["tx_hash"], serde_json::json!(tx(9)));
    assert_eq!(detail["reference_log_index"], 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn t_009_05_self_zero_bare_emit_and_unpaired_transfers_are_not_payouts(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 3).await;
    let emitter = contracts().emitter;
    let reference = |n: u8| vec![evm::REFERENCE_TOPIC, B256::repeat_byte(n)];
    let h = f.chain.mine();
    let n = h.number;
    // A transfer to the vault itself, then its reference.
    transfer_log(
        &f.chain,
        n,
        0,
        tx(1),
        usdc(),
        vault(),
        vault(),
        U256::from(5),
    );
    f.chain.log(n, 1, tx(1), 1, emitter, reference(1), vec![]);
    // A zero transfer, then its reference.
    transfer_log(
        &f.chain,
        n,
        2,
        tx(2),
        usdc(),
        vault(),
        common::beneficiary(),
        U256::ZERO,
    );
    f.chain.log(n, 3, tx(2), 2, emitter, reference(2), vec![]);
    // A bare emit, by anyone.
    f.chain.log(n, 4, tx(3), 3, emitter, reference(3), vec![]);
    // A transfer whose reference is not the next log of its transaction.
    transfer_log(
        &f.chain,
        n,
        5,
        tx(4),
        usdc(),
        vault(),
        common::beneficiary(),
        U256::from(9),
    );
    f.chain.log(n, 7, tx(4), 4, emitter, reference(4), vec![]);
    // A reference in another transaction right after a transfer.
    transfer_log(
        &f.chain,
        n,
        8,
        tx(5),
        usdc(),
        vault(),
        common::beneficiary(),
        U256::from(9),
    );
    f.chain.log(n, 9, tx(6), 6, emitter, reference(5), vec![]);
    f.chain.finalize(n);
    caught_up(&f).await;
    let events: Vec<_> = observations(&f)
        .await
        .into_iter()
        .map(|r| r.1.unwrap())
        .collect();
    assert!(!events.iter().any(|e| e == "Payout"), "{events:?}");
    // Nothing a deposit could bind: no admitted token received by the reserve (INV-6).
    let bindable = f
        .count(&format!(
            "SELECT count(*) FROM observations WHERE recipient = '{}'",
            common::vault_text()
        ))
        .await;
    assert_eq!(bindable, 0);
    assert_eq!(
        events,
        [
            "SelfTransfer",
            "UnpairedReference",
            "ZeroTransfer",
            "UnpairedReference",
            "UnpairedReference",
            "Outflow",
            "UnpairedReference",
            "Outflow",
            "UnpairedReference",
        ]
    );
}
