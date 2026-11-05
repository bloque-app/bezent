// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-010 · Swaps' conversions on the vault: what the ingress and the core do with a convertible
//! arrival and with a conversion's output (`US-A5`, `AC-A5.1`, `AC-A5.2`; `REQ-CV-1`, `REQ-CV-2`,
//! `REQ-CV-5`, `INV-6`).
//!
//! Two layers (SPEC §9). The store's own refusals are the first two tests, over the real store.
//! The seam is the third: the deposit request goes in through the real ingress router on the
//! ingress's own login (as `services/sidecar/tests/driver.rs` and the kreivo-endpoint's
//! `delivery.rs` compose services), the core cycles on its own login, and the mint order is read
//! from the store. Only the declarations and the observations are written as the owner, as the
//! operator and the EVM endpoint would, in the shape the endpoint records them
//! (`services/evm-endpoint/tests/fork.rs`, `fork_ac_a5_1_ac_a5_2_…`; `observe.rs`). The statement's
//! itemised holdings (`AC-A5.3`) are `statement.rs`; the on-chain role (`REQ-CV-7`) is
//! `contracts/evm/test/fork/Conversion.t.sol`.
//!
//! What is not here: the record of the convertible arrival with both amounts, and the condition
//! `CND-AwaitingConversion` held by an arrival. SPEC §5.10 does not say which arrival a deposit
//! records or what holds the condition; `amendments/0018-conversion-record.md` proposes how, and
//! nothing is built on it until it is ruled. Nor is anything here a claim that the binding rules
//! tie a conversion's output, or any arrival, to a requester: they do not (amendment 0018, A2).

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::*;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tower::ServiceExt;

/// The convertible token the vault admits (USDT0 on Polygon, `DEC-13`); not an admitted origin
/// token. Spelled as the endpoint spells a token it has no declared text for: checksummed.
const CONVERTIBLE: &str = "0xc2132D05D31c914a87C6611C10748AEb04B58e8F";
/// The declared pool the swap goes through, spelled the same way.
const POOL: &str = "0x31083a78E11B18e450fd139F9ABEa98CD53181B7";

/// The ingress's login in these tests: a member of `bezent_ingress` only. Created once per server.
const INGRESS_LOGIN: &str = "core_tests_ingress_login";
/// The requester that sends the deposit requests, and its credential (`<id>.<secret>`).
const REQUESTER: &str = "swaps";
const CREDENTIAL: &str = "sw-1.swaps-secret-0123456789";

/// A deposit request binding the observation at (`ORIGIN`, `block`, log `index`) for `amount`, written
/// straight into the transfers as the migration owner, returning the store's refusal instead of
/// panicking on it. This is the store layer only (the first two tests); the seam goes through the
/// ingress.
async fn try_deposit(
    pool: &sqlx::PgPool,
    block: i64,
    index: i64,
    amount: u128,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, refund_destination_network, refund_destination, \
             source_network, source_block_number, source_block_hash, source_position_kind, \
             source_position_index, state) \
         VALUES ('bz-d-' || $1, 'req-' || $1, 'deposit', $2, $3::numeric, $4, $5, $6, $7, $6, $1, \
             $8, 'log', $9, 'Received') RETURNING id",
    )
    .bind(block)
    .bind(ASSET)
    .bind(amount.to_string())
    .bind(ISSUING)
    .bind(DESTINATION)
    .bind(ORIGIN)
    .bind(BENEFICIARY)
    .bind(hash(ORIGIN, block))
    .bind(index)
    .fetch_one(pool)
    .await
}

/// What the EVM endpoint records for one transaction's log (`observe.rs`): the transaction's hash
/// and index, the emitter, and the event name, then whatever the event adds.
fn detail(event: &str, tx: &str, extra: Value) -> Value {
    let mut d = json!({"event": event, "tx_hash": tx, "tx_index": 3, "emitter": CONVERTIBLE});
    if let Value::Object(more) = extra {
        d.as_object_mut().unwrap().extend(more);
    }
    d
}

/// A convertible arrival at the reserve: the convertible, from a funder, in a transaction of its own.
async fn convertible_arrival(pool: &PgPool, block: i64, amount: u128) -> i64 {
    observe(
        pool,
        ORIGIN,
        block,
        0,
        CONVERTIBLE,
        amount,
        Some("0xFunder0000000000000000000000000000000001"),
        Some(RESERVE),
        None,
        true,
        detail("ConvertibleArrival", &format!("0x{:064x}", 1), json!({})),
    )
    .await
}

/// A swap through the vault's role, as the endpoint records it: in one transaction the vault pays
/// the declared pool `input` of the convertible (log 1, `ConversionInput`) and the pool pays the
/// vault `output` of the origin token (log 2, `Conversion`), the input and both amounts in the
/// output's detail. Returns the input's and the output's observation ids.
async fn swap(pool: &PgPool, block: i64, input: u128, output: u128) -> (i64, i64) {
    let tx = format!("0x{:064x}", 2);
    let i = observe(
        pool,
        ORIGIN,
        block,
        1,
        CONVERTIBLE,
        input,
        Some(RESERVE),
        Some(POOL),
        None,
        true,
        detail("ConversionInput", &tx, json!({})),
    )
    .await;
    let o = observe(
        pool,
        ORIGIN,
        block,
        2,
        TOKEN,
        output,
        Some(POOL),
        Some(RESERVE),
        None,
        true,
        detail(
            "Conversion",
            &tx,
            json!({"conversion": {
                "pool": POOL, "token_in": CONVERTIBLE, "amount_in": input.to_string(),
                "log_index_in": 1}}),
        ),
    )
    .await;
    (i, o)
}

/// The coordinate of (`ORIGIN`, `block`, log `index`) as a request carries it.
fn coordinate(block: i64, index: i64) -> Value {
    json!({
        "network": ORIGIN,
        "block_number": block,
        "block_hash": format!("0x{}", hex::encode(hash(ORIGIN, block))),
        "position": {"kind": "log", "index": index},
    })
}

/// The real ingress, on its own login, and a requester holding the deposit grant.
async fn ingress(w: &World) -> Router {
    let digest = hex::encode(Sha256::digest(
        CREDENTIAL.split_once('.').unwrap().1.as_bytes(),
    ));
    exec(
        &w.admin,
        &format!(
            "INSERT INTO requesters (requester) VALUES ('{REQUESTER}'); \
             INSERT INTO requester_grants (requester, grant_kind) VALUES ('{REQUESTER}', 'deposit'); \
             INSERT INTO requester_credentials (credential_id, requester, secret_sha256) \
               VALUES ('{}', '{REQUESTER}', '\\x{digest}');",
            CREDENTIAL.split_once('.').unwrap().0
        ),
    )
    .await;
    bezent_ingress::router(bezent_ingress::App::new(
        login_pool(&w.admin, INGRESS_LOGIN, bezent_store::INGRESS_ROLE, 4).await,
        bezent_ingress::Limits {
            max_in_flight: 8,
            log_page: 100,
        },
    ))
}

/// `POST /v0/deposits` for `coordinate`, as the requester sends it.
async fn request_deposit(
    router: &Router,
    reference: &str,
    coordinate: &Value,
) -> (StatusCode, Value) {
    let body = json!({
        "request_reference": reference,
        "coordinate": coordinate,
        "destination": {"network": ISSUING, "address": DESTINATION},
        "refund_destination": {"network": ORIGIN, "address": "0x52908400098527886E0F7030069857D2E4169EE7"},
    });
    let response = router
        .clone()
        .oneshot(
            Request::post("/v0/deposits")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {CREDENTIAL}"))
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let answer =
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes)));
    (status, answer)
}

/// Store layer: a transfer never names a convertible arrival (`REQ-CV-1`), and the arrival stays
/// where `REQ-IN-6` puts an arrival nothing can bind. That a deposit *request* for it is refused
/// by the ingress, with nothing minted, is the seam test below.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_cv_1_the_store_refuses_a_transfer_naming_a_convertible_arrival(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let a = convertible_arrival(&w.admin, 10, 5_000).await;
    decide(&w.admin, a, "final", 12).await;
    let kind: String = scalar(
        &w.admin,
        &format!("SELECT kind FROM observations WHERE id = {a}"),
    )
    .await;
    assert_eq!(kind, "convertible_arrival");

    let err = try_deposit(&w.admin, 10, 0, 5_000)
        .await
        .expect_err("a deposit naming a convertible arrival");
    assert!(err.to_string().contains("REQ-CV-1"), "{err}");

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

/// Store layer: a transfer's amount is its bound observation's (`REQ-CV-2`, `REQ-CV-5`): the
/// store refuses a deposit of the conversion's input or of a quoted amount against the output.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_cv_2_req_cv_5_the_store_refuses_a_deposit_of_any_amount_but_the_outputs(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (_, output) = swap(&w.admin, 10, 5_000, 4_990).await;
    decide(&w.admin, output, "final", 12).await;
    let err = try_deposit(&w.admin, 10, 2, 5_000)
        .await
        .expect_err("a deposit for the input's amount");
    assert!(err.to_string().contains("REQ-CV-2"), "{err}");
    try_deposit(&w.admin, 10, 2, 4_990)
        .await
        .expect("a deposit for the output's amount");
}

/// The seam, `AC-A5.1` and `AC-A5.2` through the real ingress to the mint: a convertible arrival
/// is not minted against and a request naming it is refused; a conversion's input is refused; the
/// output is refused until final, then bound, and minted for exactly what the swap delivered.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a5_1_ac_a5_2_req_cv_1_req_cv_2_req_cv_5_inv_6_the_ingress_binds_the_output_and_the_core_mints_it(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let router = ingress(&w).await;

    // 5 000 of the convertible arrive (block 10); the swap spends 5 000 of it and delivers 4 990
    // of the origin token (block 12), a price and a slippage that are the requester's (REQ-CV-5).
    let arrival = convertible_arrival(&w.admin, 10, 5_000).await;
    let (input, output) = swap(&w.admin, 12, 5_000, 4_990).await;

    // Nothing is final: the output is refused, nothing recorded, the reference not consumed (INV-6).
    let (status, answer) = request_deposit(&router, "r-out", &coordinate(12, 2)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{answer}");
    assert_eq!(w.count("SELECT count(*) FROM transfers").await, 0);

    // Everything final. AC-A5.1: a request naming the convertible arrival is refused; it is not
    // minted against and stays held.
    decide(&w.admin, arrival, "final", 14).await;
    decide(&w.admin, input, "final", 14).await;
    let (status, answer) = request_deposit(&router, "r-conv", &coordinate(10, 0)).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["state"], "Refused", "{answer}");
    assert_eq!(answer["error"], "ERR-SourceNotFound", "{answer}");
    // Nor the conversion's input leg.
    let (_, answer) = request_deposit(&router, "r-in", &coordinate(12, 1)).await;
    assert_eq!(answer["error"], "ERR-SourceNotFound", "{answer}");
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM unassociated_arrivals WHERE observation_id = {arrival}"
        ))
        .await,
        1
    );
    w.settle(t0()).await;
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);

    // The output is observed, not final yet at its own block: still nothing (INV-6).
    let (status, _) = request_deposit(&router, "r-out", &coordinate(12, 2)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(w.count("SELECT count(*) FROM transfers").await, 0);

    // Final: the request is accepted, for the output's amount and nothing else.
    decide(&w.admin, output, "final", 14).await;
    let (status, answer) = request_deposit(&router, "r-out", &coordinate(12, 2)).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["state"], "Accepted", "{answer}");
    let bridge = answer["bridge_reference"].as_str().unwrap().to_owned();
    let (id, amount): (i64, String) =
        sqlx::query_as("SELECT id, amount::text FROM transfers WHERE bridge_reference = $1")
            .bind(&bridge)
            .fetch_one(&w.admin)
            .await
            .unwrap();
    assert_eq!(amount, "4990", "the output's, not the input's 5000");

    // The core takes it to Source-Final and orders the mint: the output's amount, to the request's
    // destination, under the arrival reference derived from the output's coordinate.
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

    // The coordinate is consumed once (INV-2): a second request for the same output is refused.
    let (_, answer) = request_deposit(&router, "r-again", &coordinate(12, 2)).await;
    assert_eq!(answer["error"], "ERR-SourceConsumed", "{answer}");
}
