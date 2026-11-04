// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `M-2` (issue #30): precision scaling at the rounding boundaries, with bBTC as declared in the
//! plan, 8 decimals on the issuing network against BTCB's 18, a ratio of 10^10 (`REQ-AS-4`,
//! `REQ-AS-9`, `REQ-AS-10`, `INV-11`, `INV-17`). The core's mint, the book's remainder and the
//! payout order are three separate pieces of integer arithmetic; each case here drives all three
//! through the store and checks they agree to the unit, and that nothing is lost or created:
//! `minted * ratio + remainder = arrival`, `remainder < ratio`, and the reserve equation holds.
//! Amounts are integers throughout; there is no floating-point number in this file.

mod common;

use chrono::Duration;
use common::reserve::*;
use common::*;
use serde_json::json;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// BTCB's precision less bBTC's.
const RATIO: u128 = 10_000_000_000;
const BBTC_RESERVE: &str = "0xbb";

async fn declare_bbtc(w: &World) {
    exec(
        &w.admin,
        &format!(
            "INSERT INTO assets (asset, precision, assurance_class, issuing_asset) \
               VALUES ('bBTC', 8, 'public', 'Here(103)'); \
             INSERT INTO admitted_tokens (network, token, asset, precision) \
               VALUES ('{ORIGIN}', 'btcb', 'bBTC', 18); \
             INSERT INTO reserves (asset, network, address) VALUES ('bBTC', '{ORIGIN}', '{BBTC_RESERVE}'); \
             INSERT INTO routes (asset, network, direction, minimum, maximum, window_limit, \
                 window_seconds, capacity, cutoffs, completion_budget_seconds) VALUES \
               ('bBTC', '{ORIGIN}', 'deposit', 1, 100000000000000000000000, \
                  100000000000000000000000, 86400, 100, '[]', {BUDGET}), \
               ('bBTC', '{ORIGIN}', 'withdrawal', 1, 100000000000000000000000, \
                  100000000000000000000000, 86400, 100, '[]', {BUDGET});"
        ),
    )
    .await;
}

/// A bBTC deposit of `arrival` BTCB minor units at `block`, through to `Completed`. Returns the
/// transfer, the amount the core ordered minted, and the bridge reference.
async fn deposit_bbtc(w: &World, block: i64, arrival: u128) -> (i64, u128, String) {
    let a = observe(
        &w.admin,
        ORIGIN,
        block,
        0,
        "btcb",
        arrival,
        Some("0xfunder"),
        Some(BBTC_RESERVE),
        None,
        true,
        json!({}),
    )
    .await;
    let bridge = format!("bz-d-{block}");
    let id: i64 = scalar(
        &w.admin,
        &format!(
            "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                 destination_network, destination, refund_destination_network, refund_destination, \
                 source_network, source_block_number, source_block_hash, source_position_kind, \
                 source_position_index, state) \
             VALUES ('{bridge}', 'req-{block}', 'deposit', 'bBTC', {arrival}, '{ISSUING}', \
                 '{DESTINATION}', '{ORIGIN}', '{BENEFICIARY}', '{ORIGIN}', {block}, '\\x{}', 'log', 0, \
                 'Received') RETURNING id",
            hex::encode(hash(ORIGIN, block))
        ),
    )
    .await;
    exec(
        &w.admin,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state) VALUES ({id}, 'Received', 'Accepted')"
        ),
    )
    .await;
    decide(&w.admin, a, "final", block + 2).await;
    let now = t0() + Duration::minutes(5);
    w.cycle(now).await;
    deliver(w, id, 1, now).await;
    let (_, payload) = order(&w.admin, id, 1)
        .await
        .unwrap_or_else(|| panic!("no mint was ordered for an arrival of {arrival}"));
    let ordered: u128 = payload["amount"].as_str().unwrap().parse().unwrap();
    let minted = observe(
        &w.admin,
        ISSUING,
        block + 20,
        0,
        "Here(103)",
        ordered,
        None,
        Some(DESTINATION_ACCOUNT),
        Some(&reference_of(&w.admin, id).await),
        true,
        json!({"event": "Minted"}),
    )
    .await;
    decide(&w.admin, minted, "final", block + 22).await;
    w.settle(now).await;
    (id, ordered, bridge)
}

/// A deterministic sequence (xorshift64*): the cases that are not boundaries. Seeded, so a failure
/// names its arrival and reruns the same.
struct Seq(u64);
impl Seq {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// A value below 10^22 (a hundred thousand BTC in BTCB units), at least one ratio.
    fn arrival(&mut self) -> u128 {
        let wide = (u128::from(self.next()) << 64) | u128::from(self.next());
        RATIO + wide % (10_000_000_000_000_000_000_000 - RATIO)
    }
}

/// `REQ-AS-9`: for each arrival, the mint is the arrival divided by the ratio, rounded down; the
/// remainder, under one minor unit of the asset, is the deposit's surplus; nothing else moves.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_as_9_a_bbtc_deposit_mints_the_floor_and_books_the_exact_remainder(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let w = World::new(options, connect).await;
    declare_bbtc(&w).await;
    // The rounding boundaries around one and two minor units of bBTC, then a plain amount with a
    // remainder of its own, one with none, the largest the route takes, and seeded ones.
    let mut arrivals: Vec<u128> = vec![
        RATIO,
        RATIO + 1,
        2 * RATIO - 1,
        2 * RATIO,
        2 * RATIO + 1,
        123_456_789_012_345_678,
        // Just under one BTC: sixteen-plus significant digits whose quotient a rounding division
        // carries up (the store's guard did, until migration 0056).
        999_999_999_999_999_999,
        5_000_000_000_000_000_000,
        5_000_000_000_000_000_007,
        9_999_999_999_999_999_999_999,
        10_000_000_000_000_000_000_000 - 1,
    ];
    let mut seq = Seq(0x9E37_79B9_7F4A_7C15);
    arrivals.extend((0..6).map(|_| seq.arrival()));

    let (mut arrived, mut supply, mut surplus) = (0u128, 0u128, 0u128);
    for (i, arrival) in arrivals.iter().copied().enumerate() {
        let block = 100 * (i as i64 + 1);
        let (id, ordered, bridge) = deposit_bbtc(&w, block, arrival).await;
        let (floor, remainder) = (arrival / RATIO, arrival % RATIO);
        assert_eq!(ordered, floor, "the mint of {arrival}");
        assert_eq!(w.state(id).await.0, "Completed", "{arrival}");
        // Exactly the remainder is this deposit's surplus, keyed by its reference.
        assert_eq!(
            balance(&w.admin, "bBTC", "surplus", Some(&bridge)).await,
            -i128::try_from(remainder).unwrap(),
            "the surplus of {arrival}"
        );
        assert!(remainder < RATIO);
        assert_eq!(floor * RATIO + remainder, arrival);
        arrived += arrival;
        supply += floor * RATIO;
        surplus += remainder;
    }
    // Totals, in the book unit (BTCB minor units): the reserve is everything that arrived, supply
    // is what was minted, surplus is what was left; the reserve equation holds to the unit.
    let book_of = |account: &'static str| balance(&w.admin, "bBTC", account, None);
    assert_eq!(book_of("reserve").await, i128::try_from(arrived).unwrap());
    assert_eq!(book_of("supply").await, -i128::try_from(supply).unwrap());
    assert_eq!(book_of("surplus").await, -i128::try_from(surplus).unwrap());
    assert_eq!(arrived, supply + surplus);
}

/// `REQ-AS-10`: a redemption of bBTC is a withdrawal of exactly the redeemed amount times the
/// ratio, and the payout is ordered for exactly that. A payout never has a remainder.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_as_10_a_bbtc_payout_is_exactly_the_redeemed_amount_times_the_ratio(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let w = World::new(options, connect).await;
    declare_bbtc(&w).await;
    // A reserve to pay from: a final arrival of 10 BTC, minted in full.
    let (_, _, _) = deposit_bbtc(&w, 10, 10 * 100_000_000 * RATIO).await;
    let redeemed_amounts: [u128; 5] = [1, 2, 99_999_999, 100_000_000, 123_456_789];
    for (i, redeemed) in redeemed_amounts.iter().copied().enumerate() {
        let block = 1_000 + 100 * i as i64;
        let reference = [i as u8 + 1; 32];
        let r = observe(
            &w.admin,
            ISSUING,
            block,
            0,
            "Here(103)",
            redeemed,
            Some(REDEEMER),
            None,
            Some(&reference),
            true,
            json!({"event": "Redeemed", "backend": {"kind": "evm", "chain_id": 80002},
                   "payout": {"address": BENEFICIARY}}),
        )
        .await;
        decide(&w.admin, r, "final", block + 2).await;
        w.cycle(t0()).await;
        let id: i64 = scalar(
            &w.admin,
            &format!(
                "SELECT id FROM transfers WHERE kind = 'withdrawal' AND on_chain_reference = '\\x{}'",
                hex::encode(reference)
            ),
        )
        .await;
        // The withdrawal is in the book unit.
        let amount: String = scalar(
            &w.admin,
            &format!("SELECT amount::text FROM transfers WHERE id = {id}"),
        )
        .await;
        assert_eq!(
            amount.parse::<u128>().unwrap(),
            redeemed * RATIO,
            "{redeemed}"
        );
        // The order is recorded by the next cycle; signing it is the endpoint's and the signer's, and
        // the signer's checks of the bytes are not what this test is about.
        w.cycle(t0()).await;
        let (_, payload) = order(&w.admin, id, 1)
            .await
            .unwrap_or_else(|| panic!("no payout was ordered for a redemption of {redeemed}"));
        assert_eq!(
            payload["amount"].as_str().unwrap().parse::<u128>().unwrap(),
            redeemed * RATIO,
            "the payout of {redeemed}"
        );
    }
}
