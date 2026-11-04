// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-010 · Swaps' conversions on the vault: what the core does with a convertible arrival and with a
//! conversion's output (`US-A5`, `AC-A5.1`, `AC-A5.2`; `REQ-CV-1`, `REQ-CV-2`, `REQ-CV-5`, `INV-6`).
//! Each test is at the service layer, where SPEC §9 puts `INV-6`: the core drives the real store under
//! its own role. The store's refusals are `crates/store/tests/arrival_kind.rs`; the statement's
//! itemised holdings (`AC-A5.3`) are `statement.rs`; the on-chain role (`REQ-CV-7`) is
//! `contracts/evm/test/fork/Conversion.t.sol`; a real swap observed at the vault is
//! `services/evm-endpoint/tests/fork.rs`.
//!
//! What is not here: the record of the convertible arrival with both amounts, and the condition
//! `CND-AwaitingConversion` held by an arrival. SPEC §5.10 does not say which arrival a deposit
//! records or what holds the condition; `amendments/0018-conversion-record.md` proposes how, and
//! nothing is built on it until it is ruled.

mod common;

use common::*;
use serde_json::json;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// The convertible token the vault admits (USDT0 on Polygon, `DEC-13`); not an admitted origin token.
const CONVERTIBLE: &str = "0xc2132d05d31c914a87c6611c10748aeb04b58e8f";
/// The declared pool the swap goes through.
const POOL: &str = "0x31083a78e11b18e450fd139f9abea98cd53181b7";

/// A deposit request binding the observation at (`ORIGIN`, `block`, log 0) for `amount`, taken in
/// as `deposit` does, but returning the store's refusal instead of panicking on it.
async fn try_deposit(pool: &sqlx::PgPool, block: i64, amount: u128) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, refund_destination_network, refund_destination, \
             source_network, source_block_number, source_block_hash, source_position_kind, \
             source_position_index, state) \
         VALUES ('bz-d-' || $1, 'req-' || $1, 'deposit', $2, $3::numeric, $4, $5, $6, $7, $6, $1, \
             $8, 'log', 0, 'Received') RETURNING id",
    )
    .bind(block)
    .bind(ASSET)
    .bind(amount.to_string())
    .bind(ISSUING)
    .bind(DESTINATION)
    .bind(ORIGIN)
    .bind(BENEFICIARY)
    .bind(hash(ORIGIN, block))
    .fetch_one(pool)
    .await
}

/// The conversion's output as the endpoint records it: the origin token from the pool, in the
/// transaction where the vault paid the pool its convertible (`REQ-CV-2`), both amounts in the detail.
async fn conversion_output(pool: &sqlx::PgPool, block: i64, input: u128, output: u128) -> i64 {
    observe(
        pool,
        ORIGIN,
        block,
        0,
        TOKEN,
        output,
        Some(POOL),
        Some(RESERVE),
        None,
        true,
        json!({"event": "Conversion", "conversion": {
            "pool": POOL, "token_in": CONVERTIBLE, "amount_in": input.to_string(),
            "log_index_in": 1}}),
    )
    .await
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a5_1_req_cv_1_a_convertible_arrival_is_not_minted_against_and_stays_held(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    // A convertible arrives at the reserve and is final.
    let a = observe(
        &w.admin,
        ORIGIN,
        10,
        0,
        CONVERTIBLE,
        5_000,
        Some("0xfunder"),
        Some(RESERVE),
        None,
        true,
        json!({"event": "ConvertibleArrival"}),
    )
    .await;
    decide(&w.admin, a, "final", 12).await;
    let kind: String = scalar(
        &w.admin,
        &format!("SELECT kind FROM observations WHERE id = {a}"),
    )
    .await;
    assert_eq!(kind, "convertible_arrival");

    // No deposit may name it: the store refuses the request (REQ-CV-1), so no transfer exists.
    let err = try_deposit(&w.admin, 10, 5_000)
        .await
        .expect_err("a deposit naming a convertible arrival");
    assert!(err.to_string().contains("REQ-CV-1"), "{err}");

    // Cycles with the arrival final mint nothing: no transfer, no order, no intent.
    w.settle(t0()).await;
    head(&w.admin, ISSUING, 100).await;
    w.settle(t0() + chrono::Duration::days(3)).await;
    assert_eq!(w.count("SELECT count(*) FROM transfers").await, 0);
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);

    // It is held, not absorbed or dropped (REQ-IN-6): the operator's view of unbound arrivals lists
    // it. The condition it holds there is `CND-Unassociated`; `CND-AwaitingConversion` on an arrival
    // is what amendment 0018 proposes (SPEC §5.10 gives a transfer no arrival to hold it for).
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM unassociated_arrivals WHERE observation_id = {a}"
        ))
        .await,
        1
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a5_2_req_cv_2_req_cv_5_inv_6_a_deposit_is_minted_for_what_the_swap_delivered_once_final(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    // The swap spent 5 000 of the convertible and delivered 4 990 of the origin token, a price
    // and a slippage that are the requester's (REQ-CV-5).
    let output = conversion_output(&w.admin, 10, 5_000, 4_990).await;

    // The deposit names the output's coordinate. It cannot claim the input's amount or a quoted
    // one: a transfer's amount is its bound observation's (REQ-CV-5, REQ-RQ-7).
    let err = try_deposit(&w.admin, 10, 5_000)
        .await
        .expect_err("a deposit for more than the output delivered");
    assert!(err.to_string().contains("REQ-CV-2"), "{err}");
    let id = try_deposit(&w.admin, 10, 4_990)
        .await
        .expect("a deposit for the output's amount");
    exec(
        &w.admin,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state) \
             VALUES ({id}, 'Received', 'Accepted')"
        ),
    )
    .await;

    // The output is not final: nothing moves (INV-6).
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Accepted");
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);

    // Final: the deposit reaches Source-Final and the mint is ordered for the output, to the
    // request's destination, under the arrival reference derived from the output's coordinate.
    decide(&w.admin, output, "final", 12).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Source-Final");
    w.cycle(t0()).await;
    let (_, payload) = order(&w.admin, id, 1).await.expect("the mint order");
    assert_eq!(
        payload,
        json!({"order": {"asset": ISSUING_ASSET, "to": DESTINATION_ACCOUNT}, "amount": "4990"})
    );
    assert_eq!(
        w.count("SELECT count(*) FROM intents WHERE payload ? 'order'")
            .await,
        1
    );
}
