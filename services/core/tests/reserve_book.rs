// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-004 T-004-01: the reserve book. One test per movement the core records, each named after the
//! requirement or invariant it verifies (`REQ-BK-7`, `INV-17`, `REQ-AS-9`, `REQ-BK-4`, `REQ-BK-5`,
//! `REQ-CV-2`, `AC-E2.2`). Balances are read as the book holds them: debits minus credits, so a
//! credit-normal term (supply, owed mint, owed payout, surplus) reads negative.

mod common;

use bezent_core_service::refs::{self, Coordinate};
use bezent_core_service::reserve::book::{self, BookError, Cost, Side};
use bezent_core_service::reserve::coordinate_key;
use chrono::Duration;
use common::reserve::*;
use common::*;
use serde_json::json;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

fn at(network: &str, block: i64, index: i64) -> Coordinate {
    Coordinate {
        network: network.into(),
        block_number: block,
        block_hash: hash(network, block),
        position_kind: "log".into(),
        position_index: index,
    }
}

async fn post(w: &World) -> usize {
    book::post_pending(w.core.pool()).await.unwrap()
}

async fn entries(w: &World, kind: &str) -> i64 {
    w.count(&format!(
        "SELECT count(*) FROM book_entries WHERE kind = '{kind}'"
    ))
    .await
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_7_a_final_arrival_is_reserve_and_owed_mint_bound_to_a_deposit_or_not(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    // §5.3: OwedMint is every final arrival not yet minted, whether bound to a deposit or not.
    final_arrival(&w.admin, 10, 0, 5_000).await;
    // Not final yet (INV-16): not booked.
    arrival(&w.admin, 30, 2_000).await;
    // Not an admitted origin token (REQ-AS-7, REQ-CV-4): never reserve.
    let other = observe(
        &w.admin,
        ORIGIN,
        31,
        0,
        "usdt0",
        9_000,
        None,
        Some(RESERVE),
        None,
        true,
        json!({}),
    )
    .await;
    decide(&w.admin, other, "final", 33).await;
    assert_eq!(post(&w).await, 1);
    let item = coordinate_key(&at(ORIGIN, 10, 0));
    assert_eq!(balance(&w.admin, ASSET, "reserve", None).await, 5_000);
    assert_eq!(
        balance(&w.admin, ASSET, "owed-mint", Some(&item)).await,
        -5_000
    );
    let (kind, network, block): (String, String, i64) =
        sqlx::query_as("SELECT kind, network, block_number FROM book_entries WHERE reference = $1")
            .bind(&item)
            .fetch_one(&w.admin)
            .await
            .unwrap();
    // It rests on the arrival's own block, so the book can be read as of a final point.
    assert_eq!(
        (kind.as_str(), network.as_str(), block),
        ("arrival", ORIGIN, 10)
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_7_a_mint_moves_the_arrival_from_owed_mint_to_supply(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let id = minted_deposit(&w, 10, 5_000).await;
    assert_eq!(w.state(id).await.0, "Completed");
    let item = coordinate_key(&at(ORIGIN, 10, 0));
    assert_eq!(balance(&w.admin, ASSET, "owed-mint", Some(&item)).await, 0);
    assert_eq!(balance(&w.admin, ASSET, "supply", None).await, -5_000);
    assert_eq!(balance(&w.admin, ASSET, "reserve", None).await, 5_000);
    assert_eq!(balance(&w.admin, ASSET, "surplus", None).await, 0);
    let transfer: i64 = scalar(
        &w.admin,
        "SELECT transfer_id FROM book_entries WHERE kind = 'mint'",
    )
    .await;
    assert_eq!(transfer, id);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_as_9_a_bbtc_remainder_lands_in_surplus_with_the_deposits_reference(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    // bBTC: 6 decimals on the issuing network, its origin token 8; the ratio is 100.
    exec(
        &w.admin,
        &format!(
            "INSERT INTO assets (asset, precision, assurance_class, issuing_asset) \
               VALUES ('bBTC', 6, 'public', 'Here(103)'); \
             INSERT INTO admitted_tokens (network, token, asset, precision) \
               VALUES ('{ORIGIN}', 'wbtc', 'bBTC', 8); \
             INSERT INTO reserves (asset, network, address) VALUES ('bBTC', '{ORIGIN}', '0xbb');"
        ),
    )
    .await;
    let a = observe(
        &w.admin,
        ORIGIN,
        10,
        0,
        "wbtc",
        123_456_789,
        Some("0xfunder"),
        Some("0xbb"),
        None,
        true,
        json!({}),
    )
    .await;
    let id: i64 = scalar(
        &w.admin,
        &format!(
            "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                 destination_network, destination, refund_destination_network, refund_destination, \
                 source_network, source_block_number, source_block_hash, source_position_kind, \
                 source_position_index, state) \
             VALUES ('bz-d-btc', 'req-btc', 'deposit', 'bBTC', 123456789, '{ISSUING}', \
                 '{DESTINATION}', '{ORIGIN}', '{BENEFICIARY}', '{ORIGIN}', 10, '\\x{}', 'log', 0, \
                 'Received') RETURNING id",
            hex::encode(hash(ORIGIN, 10))
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
    decide(&w.admin, a, "final", 12).await;
    let now = t0() + Duration::minutes(5);
    w.cycle(now).await;
    deliver(&w, id, 1, now).await;
    // The mint is the arrival divided by the ratio, rounded down (REQ-AS-9).
    let (_, payload) = order(&w.admin, id, 1).await.unwrap();
    assert_eq!(payload["amount"], "1234567");
    let minted = observe(
        &w.admin,
        ISSUING,
        30,
        0,
        "Here(103)",
        1_234_567,
        None,
        Some(DESTINATION_ACCOUNT),
        Some(&reference_of(&w.admin, id).await),
        true,
        json!({"event": "Minted"}),
    )
    .await;
    decide(&w.admin, minted, "final", 32).await;
    w.settle(now).await;
    assert_eq!(w.state(id).await.0, "Completed");
    let item = coordinate_key(&at(ORIGIN, 10, 0));
    // The arrival leaves owed mint in full; 89 sats, under one minor unit of bBTC, are surplus
    // with the deposit's reference: never minted, dropped or returned.
    assert_eq!(balance(&w.admin, "bBTC", "owed-mint", Some(&item)).await, 0);
    assert_eq!(
        balance(&w.admin, "bBTC", "supply", None).await,
        -123_456_700
    );
    assert_eq!(
        balance(&w.admin, "bBTC", "surplus", Some("bz-d-btc")).await,
        -89
    );
    assert_eq!(
        balance(&w.admin, "bBTC", "reserve", None).await,
        123_456_789
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_7_a_redemption_is_owed_payout_until_its_payout_leaves_the_reserve(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000_000).await;
    let id = redeemed(&w, 20, [2; 32], 700).await;
    let item: String = scalar(
        &w.admin,
        &format!("SELECT bridge_reference FROM transfers WHERE id = {id}"),
    )
    .await;
    assert_eq!(item, refs::withdrawal(REDEEMER, &[2; 32]));
    // Destroyed at redemption (REQ-OT-6): supply falls, the payout is owed.
    assert_eq!(balance(&w.admin, ASSET, "supply", None).await, 700);
    assert_eq!(
        balance(&w.admin, ASSET, "owed-payout", Some(&item)).await,
        -700
    );
    deliver(&w, id, 1, t0()).await;
    paid_out(&w, id, 50, 700).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Completed");
    assert_eq!(
        balance(&w.admin, ASSET, "owed-payout", Some(&item)).await,
        0
    );
    assert_eq!(
        balance(&w.admin, ASSET, "reserve", None).await,
        1_000_000 - 700
    );
    assert_eq!(entries(&w, "payout").await, 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_7_a_refund_mint_returns_the_owed_payout_to_supply(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let r = redemption(&w.admin, 20, &[3; 32], 700).await;
    decide(&w.admin, r, "final", 22).await;
    let refunded = observe(
        &w.admin,
        ISSUING,
        40,
        0,
        ISSUING_ASSET,
        700,
        None,
        Some(REDEEMER),
        Some(&[3; 32]),
        true,
        json!({"event": "RefundMinted"}),
    )
    .await;
    decide(&w.admin, refunded, "final", 42).await;
    assert_eq!(post(&w).await, 2);
    let item = refs::withdrawal(REDEEMER, &[3; 32]);
    assert_eq!(
        balance(&w.admin, ASSET, "owed-payout", Some(&item)).await,
        0
    );
    assert_eq!(balance(&w.admin, ASSET, "supply", None).await, 0);
    assert_eq!(entries(&w, "refund-mint").await, 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_7_the_refund_of_an_unassociated_arrival_leaves_owed_mint_and_the_reserve(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let now = t0() + Duration::minutes(5);
    final_arrival(&w.admin, 10, 0, 5_000).await;
    w.settle(now).await;
    let item = coordinate_key(&at(ORIGIN, 10, 0));
    assert_eq!(
        balance(&w.admin, ASSET, "owed-mint", Some(&item)).await,
        -5_000
    );
    // Its refund, released by two operators (REQ-IN-6), as the F-003 test takes it in.
    let refund: i64 = scalar(
        &w.admin,
        &format!(
            "INSERT INTO transfers (bridge_reference, request_reference, kind, token, amount, \
                 destination_network, destination, state, source_network, source_block_number, \
                 source_block_hash, source_position_kind, source_position_index) \
             VALUES ('bz-u-1', 'r-1', 'refund', '{TOKEN}', 5000, '{ORIGIN}', '{BENEFICIARY}', \
                 'Received', '{ORIGIN}', 10, '\\x{}', 'log', 0) RETURNING id",
            hex::encode(hash(ORIGIN, 10))
        ),
    )
    .await;
    let action: i64 = scalar(
        &w.ops[0],
        &format!(
            "INSERT INTO manual_actions (kind, transfer_id, justification) \
             VALUES ('release-unassociated', {refund}, 'the funder asked') RETURNING id"
        ),
    )
    .await;
    approve_as(&w, 1, action).await.unwrap();
    w.cycle(now).await;
    w.cycle(now).await;
    assert_eq!(w.state(refund).await.0, "Source-Final");
    deliver(&w, refund, 1, now).await;
    let reference = reference_of(&w.admin, refund).await;
    assert_eq!(
        reference,
        refs::unassociated_refund_reference(&at(ORIGIN, 10, 0)).to_vec()
    );
    let leg = observe(
        &w.admin,
        ORIGIN,
        60,
        1,
        TOKEN,
        5_000,
        Some(RESERVE),
        Some(BENEFICIARY),
        Some(&reference),
        true,
        json!({"event": "Payout"}),
    )
    .await;
    decide(&w.admin, leg, "final", 62).await;
    w.settle(now).await;
    assert_eq!(balance(&w.admin, ASSET, "owed-mint", Some(&item)).await, 0);
    assert_eq!(balance(&w.admin, ASSET, "reserve", None).await, 0);
    assert_eq!(entries(&w, "unassociated-refund").await, 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_5_surplus_moves_only_by_a_recorded_contribution_or_withdrawal(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000).await;
    assert_eq!(balance(&w.admin, ASSET, "surplus", None).await, -1_000);
    assert_eq!(balance(&w.admin, ASSET, "owed-mint", None).await, 0);
    assert_eq!(balance(&w.admin, ASSET, "reserve", None).await, 1_000);
    let mut conn = w.core.pool().acquire().await.unwrap();

    // The contribution is the chain's: an arrival from the declared funding address, posted as
    // such, with its coordinate as its reference (review S1).
    assert_eq!(entries(&w, "surplus-contribution").await, 1);
    assert_eq!(entries(&w, "arrival").await, 0);
    // An arrival from any other address is owed mint, bound to a deposit or not (§5.3): no one
    // can record it as surplus.
    final_arrival(&w.admin, 10, 0, 300).await;
    post(&w).await;
    let item = coordinate_key(&at(ORIGIN, 10, 0));
    assert_eq!(
        balance(&w.admin, ASSET, "owed-mint", Some(&item)).await,
        -300
    );
    assert_eq!(balance(&w.admin, ASSET, "surplus", None).await, -1_000);

    // The operator takes 400 of its own out of the reserve address, with a reference.
    let out = observe(
        &w.admin,
        ORIGIN,
        20,
        5,
        TOKEN,
        400,
        Some(RESERVE),
        Some("0xoperator"),
        None,
        true,
        json!({}),
    )
    .await;
    decide(&w.admin, out, "final", 22).await;
    book::surplus_withdrawal(&mut conn, ASSET, out, "withdrawal-1")
        .await
        .unwrap();
    assert_eq!(balance(&w.admin, ASSET, "surplus", None).await, -600);
    assert_eq!(
        balance(&w.admin, ASSET, "reserve", None).await,
        1_000 + 300 - 400
    );
    // Once only (INV-17).
    let twice = book::surplus_withdrawal(&mut conn, ASSET, out, "withdrawal-2").await;
    assert!(matches!(twice, Err(BookError::Refused(_))), "{twice:?}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_e2_2_a_network_cost_or_venue_fee_is_charged_to_surplus_and_no_delivery_is_reduced(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 10_000).await;
    let mut conn = w.core.pool().acquire().await.unwrap();
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
    book::cost(
        &mut conn,
        ASSET,
        Cost::Venue,
        ORIGIN,
        5,
        "venue:fee-1",
        None,
    )
    .await
    .unwrap();
    // REQ-BK-4: against surplus, each with its reference.
    assert_eq!(balance(&w.admin, ASSET, "surplus", None).await, -9_970);
    assert_eq!(balance(&w.admin, ASSET, "reserve", None).await, 9_970);
    let references: Vec<String> = sqlx::query_scalar(
        "SELECT reference FROM book_entries WHERE kind IN ('network-cost', 'venue-fee') ORDER BY id",
    )
    .fetch_all(&w.admin)
    .await
    .unwrap();
    assert_eq!(references, ["gas:0xabc", "venue:fee-1"]);
    // REQ-TR-5: the payout is the redeemed amount, in full.
    let id = redeemed(&w, 20, [4; 32], 700).await;
    deliver(&w, id, 1, t0()).await;
    let (_, payload) = order(&w.admin, id, 1).await.unwrap();
    assert_eq!(payload["amount"], "700");
    paid_out(&w, id, 50, 700).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Completed");
    assert_eq!(balance(&w.admin, ASSET, "surplus", None).await, -9_970);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_17_each_movement_is_booked_once_and_a_correction_is_a_new_entry(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    final_arrival(&w.admin, 10, 0, 5_000).await;
    assert_eq!(post(&w).await, 1);
    // Every cycle posts what is pending; nothing twice.
    assert_eq!(post(&w).await, 0);
    w.settle(t0()).await;
    assert_eq!(entries(&w, "arrival").await, 1);
    // The store refuses a second entry for the same movement.
    let item = coordinate_key(&at(ORIGIN, 10, 0));
    let twice = sqlx::query("INSERT INTO book_entries (reference, kind) VALUES ($1, 'arrival')")
        .bind(&item)
        .execute(&w.admin)
        .await;
    assert!(twice.is_err());
    // A correction names the entry it corrects and changes nothing of it.
    let original: i64 = scalar(
        &w.admin,
        "SELECT id FROM book_entries WHERE kind = 'arrival'",
    )
    .await;
    let mut conn = w.core.pool().acquire().await.unwrap();
    let correction = book::correction(
        &mut conn,
        original,
        ASSET,
        "an example correction",
        vec![
            book::posting("owed-mint", None, Side::Debit, 1, Some(&item)),
            book::posting("surplus", None, Side::Credit, 1, None),
        ],
    )
    .await
    .unwrap();
    let corrects: i64 = scalar(
        &w.admin,
        &format!("SELECT corrects FROM book_entries WHERE id = {correction}"),
    )
    .await;
    assert_eq!(corrects, original);
    assert_eq!(
        balance(&w.admin, ASSET, "owed-mint", Some(&item)).await,
        -4_999
    );
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM book_postings WHERE entry_id = {original}"
        ))
        .await,
        2
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_cv_2_a_conversions_output_is_booked_and_its_input_is_not(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    // The requester's conversion, under its role: the convertible input is not reserve
    // (REQ-AS-7); the output, landing in the reserve, is an arrival like any other.
    let input = observe(
        &w.admin,
        ORIGIN,
        10,
        0,
        "usdt0",
        1_000,
        Some("0xdepositor"),
        Some(RESERVE),
        None,
        true,
        json!({}),
    )
    .await;
    decide(&w.admin, input, "final", 12).await;
    let output = observe(
        &w.admin,
        ORIGIN,
        20,
        1,
        TOKEN,
        998,
        Some("0xvenue"),
        Some(RESERVE),
        None,
        true,
        json!({"event": "Conversion", "input": {"token": "usdt0", "amount": "1000"}}),
    )
    .await;
    decide(&w.admin, output, "final", 22).await;
    assert_eq!(post(&w).await, 1);
    assert_eq!(entries(&w, "conversion").await, 1);
    let item = coordinate_key(&at(ORIGIN, 20, 1));
    assert_eq!(
        balance(&w.admin, ASSET, "owed-mint", Some(&item)).await,
        -998
    );
    assert_eq!(balance(&w.admin, ASSET, "reserve", None).await, 998);
}
