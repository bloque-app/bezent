// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! T-018-05 (F-018, amendment 0029, proposed): the `Directed` filter, pairing, and the kinds
//! `directed_arrival` and `unpaired_direction` (`AC-A6.6`, `INV-10`, `DEC-12`).
//!
//! A `Directed` log of a declared directing contract pairs with the admitted token's `Transfer`
//! right before it, from its sender to the vault, of its amount. The pair is one observation at
//! the `Transfer`'s coordinate and the `Transfer` is not also an `Arrival`. A `Directed` log with
//! no such pair is `UnpairedDirection`. Providers that disagree on a pair leave a gap. An empty
//! `directing` configuration changes nothing.

mod common;

use alloy::primitives::{Address, B256, U256};
use bezent_evm_endpoint::evm;
use bezent_evm_endpoint::observe::Step;
use bezent_evm_endpoint::rpc::mock::MockChain;
use common::{CHAIN_ID, Fixture, depositor, transfer_log, usdc, usdt0, vault};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

type Row = (
    i64,
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    serde_json::Value,
);

/// The declared directing contract.
fn directing() -> Address {
    Address::repeat_byte(0xd1)
}

/// The recipient on the issuing network: a 32-byte account.
fn account() -> B256 {
    B256::repeat_byte(0x5e)
}

fn tx(n: u8) -> B256 {
    B256::repeat_byte(n)
}

#[allow(clippy::too_many_arguments)]
fn directed_log(
    c: &MockChain,
    number: u64,
    log_index: u64,
    tx: B256,
    from: Address,
    sender: Address,
    amount: U256,
    recipient: B256,
) {
    c.log(
        number,
        log_index,
        tx,
        u64::from(tx[0]),
        from,
        vec![evm::DIRECTED_TOPIC, sender.into_word(), recipient],
        amount.to_be_bytes::<32>().to_vec(),
    );
}

/// What was recorded: block, log index, kind, event, token, amount, sender, detail.
async fn rows(f: &Fixture) -> Vec<Row> {
    sqlx::query_as(
        "SELECT block_number, position_index, kind, detail ->> 'event', token, amount::text, \
           sender, detail FROM observations \
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

/// A fixture that declares the directing contract from block 1, and a new block to put logs in.
async fn declared(pool: PgPoolOptions, connect: PgConnectOptions) -> (Fixture, u64) {
    let mut f = Fixture::new(pool, connect, 3).await;
    f.directing = vec![(directing(), 1)];
    let block = f.chain.mine().number;
    (f, block)
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_6_a_pair_is_one_directed_arrival_at_the_transfers_coordinate_and_never_an_arrival(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let (f, block) = declared(pool, connect).await;
    transfer_log(
        &f.chain,
        block,
        4,
        tx(1),
        usdc(),
        depositor(),
        vault(),
        U256::from(700),
    );
    directed_log(
        &f.chain,
        block,
        5,
        tx(1),
        directing(),
        depositor(),
        U256::from(700),
        account(),
    );
    f.advance(2);
    assert!(matches!(caught_up(&f).await, Step::CaughtUp { .. }));
    // A second look records nothing twice (INV-10).
    assert!(matches!(caught_up(&f).await, Step::CaughtUp { .. }));
    let rows = rows(&f).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (n, index, kind, event, token, amount, sender, detail) = &rows[0];
    assert_eq!(u64::try_from(*n).unwrap(), block);
    // The Transfer's coordinate, not the Directed log's.
    assert_eq!(*index, 4);
    assert_eq!(kind, "directed_arrival");
    assert_eq!(event, "DirectedArrival");
    assert_eq!(token.as_deref(), Some(usdc().to_string().as_str()));
    assert_eq!(amount.as_deref(), Some("700"));
    assert_eq!(sender.as_deref(), Some(depositor().to_string().as_str()));
    assert_eq!(detail["direction_log"], 5);
    assert_eq!(detail["directing"], directing().to_checksum(None));
    assert_eq!(detail["recipient"], format!("0x{}", "5e".repeat(32)));
    // The reserve is still the recipient column, as an arrival's.
    let recipient: Option<String> =
        sqlx::query_scalar("SELECT recipient FROM observations WHERE kind = 'directed_arrival'")
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert_eq!(recipient.as_deref(), Some(common::vault_text().as_str()));
    assert_eq!(
        f.count("SELECT count(*) FROM observations WHERE kind = 'arrival'")
            .await,
        0
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_6_two_deposits_in_one_transaction_are_two_pairs(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let (f, block) = declared(pool, connect).await;
    let other = Address::repeat_byte(0xdf);
    let t = tx(2);
    transfer_log(
        &f.chain,
        block,
        0,
        t,
        usdc(),
        depositor(),
        vault(),
        U256::from(5_000_000),
    );
    directed_log(
        &f.chain,
        block,
        1,
        t,
        directing(),
        depositor(),
        U256::from(5_000_000),
        account(),
    );
    transfer_log(
        &f.chain,
        block,
        2,
        t,
        usdc(),
        other,
        vault(),
        U256::from(9_000_000),
    );
    directed_log(
        &f.chain,
        block,
        3,
        t,
        directing(),
        other,
        U256::from(9_000_000),
        B256::repeat_byte(0x6f),
    );
    f.advance(2);
    caught_up(&f).await;
    let rows = rows(&f).await;
    let got: Vec<_> = rows
        .iter()
        .map(|r| (r.1, r.2.as_str(), r.5.as_deref()))
        .collect();
    assert_eq!(
        got,
        [
            (0, "directed_arrival", Some("5000000")),
            (2, "directed_arrival", Some("9000000"))
        ]
    );
    assert_eq!(rows[0].7["direction_log"], 1);
    assert_eq!(rows[1].7["direction_log"], 3);
    assert_eq!(rows[1].7["recipient"], format!("0x{}", "6f".repeat(32)));
}

/// A `Directed` log with no valid pair is `UnpairedDirection`, and the transfer it failed to pair
/// with stays what it is on its own.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_6_a_directed_log_with_no_valid_pair_is_unpaired_and_never_an_arrival(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let (f, block) = declared(pool, connect).await;
    let other = Address::repeat_byte(0xdf);
    let amount = U256::from(700);
    // 1. No transfer at all (index 0 of its transaction).
    directed_log(
        &f.chain,
        block,
        0,
        tx(3),
        directing(),
        depositor(),
        amount,
        account(),
    );
    // 2. A transfer from someone else than the event's sender.
    transfer_log(&f.chain, block, 1, tx(4), usdc(), other, vault(), amount);
    directed_log(
        &f.chain,
        block,
        2,
        tx(4),
        directing(),
        depositor(),
        amount,
        account(),
    );
    // 3. Another amount.
    transfer_log(
        &f.chain,
        block,
        3,
        tx(5),
        usdc(),
        depositor(),
        vault(),
        amount,
    );
    directed_log(
        &f.chain,
        block,
        4,
        tx(5),
        directing(),
        depositor(),
        U256::from(701),
        account(),
    );
    // 4. Not right before it: another log between.
    transfer_log(
        &f.chain,
        block,
        5,
        tx(6),
        usdc(),
        depositor(),
        vault(),
        amount,
    );
    transfer_log(
        &f.chain,
        block,
        6,
        tx(6),
        usdc(),
        depositor(),
        other,
        U256::from(1),
    );
    directed_log(
        &f.chain,
        block,
        7,
        tx(6),
        directing(),
        depositor(),
        amount,
        account(),
    );
    // 5. A transfer to someone else than the vault.
    transfer_log(
        &f.chain,
        block,
        8,
        tx(7),
        usdc(),
        depositor(),
        other,
        amount,
    );
    directed_log(
        &f.chain,
        block,
        9,
        tx(7),
        directing(),
        depositor(),
        amount,
        account(),
    );
    // 6. A token that is not admitted (a convertible, a token of another issuer).
    transfer_log(
        &f.chain,
        block,
        10,
        tx(8),
        usdt0(),
        depositor(),
        vault(),
        amount,
    );
    directed_log(
        &f.chain,
        block,
        11,
        tx(8),
        directing(),
        depositor(),
        amount,
        account(),
    );
    // 7. A transfer of zero.
    transfer_log(
        &f.chain,
        block,
        12,
        tx(9),
        usdc(),
        depositor(),
        vault(),
        U256::ZERO,
    );
    directed_log(
        &f.chain,
        block,
        13,
        tx(9),
        directing(),
        depositor(),
        U256::ZERO,
        account(),
    );
    // 8. The transfer belongs to the log before, a `Directed` of its own: one transfer, one pair.
    transfer_log(
        &f.chain,
        block,
        14,
        tx(10),
        usdc(),
        depositor(),
        vault(),
        amount,
    );
    directed_log(
        &f.chain,
        block,
        15,
        tx(10),
        directing(),
        depositor(),
        amount,
        account(),
    );
    directed_log(
        &f.chain,
        block,
        16,
        tx(10),
        directing(),
        depositor(),
        amount,
        account(),
    );
    // 9. A Directed log that does not decode.
    f.chain.log(
        block,
        17,
        tx(11),
        11,
        directing(),
        vec![evm::DIRECTED_TOPIC, depositor().into_word()],
        vec![0; 32],
    );
    f.advance(2);
    caught_up(&f).await;
    let got: Vec<_> = rows(&f).await.into_iter().map(|r| (r.1, r.3)).collect();
    let want = [
        (0, "UnpairedDirection"),
        (1, "Arrival"),
        (2, "UnpairedDirection"),
        (3, "Arrival"),
        (4, "UnpairedDirection"),
        (5, "Arrival"),
        (7, "UnpairedDirection"),
        (9, "UnpairedDirection"),
        (10, "ConvertibleArrival"),
        (11, "UnpairedDirection"),
        (12, "Arrival"),
        (13, "UnpairedDirection"),
        (14, "DirectedArrival"),
        (16, "UnpairedDirection"),
        (17, "UnpairedDirection"),
    ];
    let want: Vec<_> = want.into_iter().map(|(i, e)| (i, e.to_owned())).collect();
    assert_eq!(got, want);
    // The anomaly's kind is its own, and nothing binds it: no token, no reserve recipient.
    assert_eq!(
        f.count(
            "SELECT count(*) FROM observations WHERE kind = 'unpaired_direction' \
               AND (recipient IS NOT NULL OR token IS NOT NULL)"
        )
        .await,
        0
    );
    assert_eq!(
        f.count("SELECT count(*) FROM observations WHERE kind = 'unpaired_direction'")
            .await,
        9
    );
}

/// `DEC-12`: a pair one provider does not report whole is a gap, and nothing of it is recorded.
/// Provider `a` reports the pair; provider `b` lacks the `Directed` log (or the `Transfer`).
async fn disagree(pool: PgPoolOptions, connect: PgConnectOptions, lacks_transfer: bool) {
    let a = MockChain::new(CHAIN_ID, 3);
    let b = a.fork();
    let h = a.mine();
    b.mine();
    b.replace_with(a.header_at(h.number));
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
    directed_log(
        &a,
        h.number,
        1,
        tx(1),
        directing(),
        depositor(),
        U256::from(5),
        account(),
    );
    if lacks_transfer {
        directed_log(
            &b,
            h.number,
            1,
            tx(1),
            directing(),
            depositor(),
            U256::from(5),
            account(),
        );
    } else {
        transfer_log(
            &b,
            h.number,
            0,
            tx(1),
            usdc(),
            depositor(),
            vault(),
            U256::from(5),
        );
    }
    a.finalize(h.number);
    b.finalize(h.number);
    let mut f = Fixture::with_chains(pool, connect, a, b).await;
    f.directing = vec![(directing(), 1)];
    let Step::Gap { reason, .. } = caught_up(&f).await else {
        panic!("not a gap");
    };
    assert!(reason.contains("DEC-12"), "{reason}");
    assert!(rows(&f).await.is_empty());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_12_a_provider_that_lacks_the_directed_log_leaves_a_gap(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    disagree(pool, connect, false).await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_12_a_provider_that_lacks_the_transfer_leaves_a_gap(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    disagree(pool, connect, true).await;
}

/// Empty configuration changes nothing: a `Directed` log is not read, and the transfer is the
/// `Arrival` it always was.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_6_empty_directing_changes_nothing(pool: PgPoolOptions, connect: PgConnectOptions) {
    let f = Fixture::new(pool, connect, 3).await;
    assert!(f.directing.is_empty());
    let block = f.chain.mine().number;
    transfer_log(
        &f.chain,
        block,
        0,
        tx(1),
        usdc(),
        depositor(),
        vault(),
        U256::from(700),
    );
    directed_log(
        &f.chain,
        block,
        1,
        tx(1),
        directing(),
        depositor(),
        U256::from(700),
        account(),
    );
    f.advance(2);
    caught_up(&f).await;
    let got: Vec<_> = rows(&f).await.into_iter().map(|r| (r.1, r.3)).collect();
    assert_eq!(got, [(0, "Arrival".to_owned())]);
}

/// A `Directed` log of an address that is not declared is not read; one below the declaration's
/// first block counts as nothing, and its transfer is an `Arrival`.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_6_only_a_declared_contract_from_its_first_block_directs(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let mut f = Fixture::new(pool, connect, 3).await;
    let early = f.chain.mine().number;
    let late = f.chain.mine().number;
    f.directing = vec![(directing(), late)];
    let stranger = Address::repeat_byte(0xee);
    for (block, at) in [(early, directing()), (late, stranger)] {
        transfer_log(
            &f.chain,
            block,
            0,
            tx(1),
            usdc(),
            depositor(),
            vault(),
            U256::from(700),
        );
        directed_log(
            &f.chain,
            block,
            1,
            tx(1),
            at,
            depositor(),
            U256::from(700),
            account(),
        );
    }
    f.advance(2);
    caught_up(&f).await;
    let got: Vec<_> = rows(&f).await.into_iter().map(|r| (r.0, r.3)).collect();
    assert_eq!(
        got,
        [
            (i64::try_from(early).unwrap(), "Arrival".to_owned()),
            (i64::try_from(late).unwrap(), "Arrival".to_owned())
        ]
    );
}
