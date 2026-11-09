// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-018, T-018-06 (migration 0144, amendment 0029 A2, A3): what the requests' surface does with a
//! directed arrival and with a directed deposit, which the core makes from its own observation and
//! which has no request and so no answer (`REQ-IN-10`, `REQ-RQ-3`, `REQ-RQ-6`, `REQ-ST-1`,
//! `REQ-ST-5`, `AC-A6.2`, `INV-2`).
//!
//! The store's own rules for it are `crates/store/tests/directed_deposits.rs`. Here the ingress is
//! the requester's way in, and every reader of a deposit's answer (`by-request`, `by-bridge`,
//! `by-coordinate`, the log, the callback registration and the webhooks' fan-out) is asked about a
//! deposit that has none.

mod common;

use axum::http::StatusCode;
use bezent_ingress::webhooks::{Dispatcher, SealingKey, Webhooks};
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::time::Duration;

/// The recipient the directing code recorded: a 32-byte account on the issuing network, in hex.
const RECIPIENT: &str = "0x8eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a48";
const DIRECTING: &str = "0xD1Ec7ed00000000000000000000000000000d1ec";

fn assert_refused(body: &Value, error: &str) {
    assert_eq!(body["state"], "Refused", "{body}");
    assert_eq!(body["error"], error, "{body}");
}

/// What the EVM endpoint records for a directed arrival at (`ORIGIN`, `block`, log 0): final, at the
/// reserve, with the issuing network's recipient and the refund address in its detail. Returns the
/// coordinate as a request carries it.
async fn directed(pool: &PgPool, block: i64, amount: u64) -> Value {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, token, amount, sender, recipient, executed, kind, detail) \
         VALUES ($1, $2, $3, '2026-10-13T00:00:00Z', 'log', 0, $4, $5::numeric, $6, $7, TRUE, \
             'directed_arrival', jsonb_build_object('event', 'DirectedArrival', \
               'directing', $8::text, 'direction_log', 1, 'recipient', $9::text, \
               'refund', $10::text)) RETURNING id",
    )
    .bind(ORIGIN)
    .bind(block)
    .bind(block_hash(block))
    .bind(TOKEN)
    .bind(amount.to_string())
    .bind(FUNDER)
    .bind(RESERVE)
    .bind(DIRECTING)
    .bind(RECIPIENT)
    .bind(ORIGIN_ADDRESS)
    .fetch_one(pool)
    .await
    .expect("observe a directed arrival");
    exec(
        pool,
        &format!(
            "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
               VALUES ('{ORIGIN}', {block} + 10, '\\x01', '2026-10-13T00:00:00Z') \
               ON CONFLICT DO NOTHING; \
             INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
               final_block_hash) VALUES ({id}, 'final', {block} + 10, '\\x01')"
        ),
    )
    .await;
    coordinate(block)
}

/// The core's call, as the core's login. Returns the deposit's bridge reference.
async fn core_deposit(core: &PgPool, block: i64) -> String {
    let id: Option<i64> = sqlx::query_scalar("SELECT directed_deposit($1, $2, $3, 'log', 0)")
        .bind(ORIGIN)
        .bind(block)
        .bind(block_hash(block))
        .fetch_one(core)
        .await
        .expect("the core makes the deposit");
    sqlx::query_scalar("SELECT bridge_reference FROM transfers WHERE id = $1")
        .bind(id.expect("a deposit"))
        .fetch_one(core)
        .await
        .unwrap()
}

async fn core_pool(pool: &PgPool) -> PgPool {
    role_pool(pool, bezent_store::CORE_ROLE).await
}

fn refund_body(reference: &str, coordinate: Value) -> Value {
    json!({
        "request_reference": reference,
        "coordinate": coordinate,
        "destination": { "network": ORIGIN, "address": ORIGIN_ADDRESS },
    })
}

fn coordinate_query(block: i64) -> String {
    let c = coordinate(block);
    format!(
        "/v0/transfers/by-coordinate?network={}&block_number={}&block_hash={}&position_kind=log&position_index=0",
        c["network"].as_str().unwrap(),
        block,
        c["block_hash"].as_str().unwrap(),
    )
}

/// `AC-A6.2`, `REQ-IN-10`, `REQ-RQ-3`, `REQ-RQ-6`: a deposit request, or a refund request, by any
/// requester naming a directed arrival's coordinate is `ERR-SourceNotFound` and binds nothing:
/// before the arrival has its deposit and after. And it leaves nothing in the way: the core's
/// deposit for the same coordinate is made afterwards, with no request's reference holding the slot.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_2_a_request_naming_a_directed_arrival_is_err_source_not_found_and_binds_nothing(
    pool: PgPool,
) {
    let ingress = Ingress::new(&pool).await;
    let core = core_pool(&pool).await;
    let c = directed(&pool, 1, 5_000).await;

    for (credential, reference) in [(ALPHA, "ref-a"), (BETA, "ref-b")] {
        let (status, answer) = ingress
            .post(
                "/v0/deposits",
                credential,
                &deposit_body(reference, c.clone()),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        assert_refused(&answer, "ERR-SourceNotFound");
    }
    let (status, answer) = ingress
        .post("/v0/refunds", ALPHA, &refund_body("refund-a", c.clone()))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_refused(&answer, "ERR-SourceNotFound");
    assert_eq!(count(&pool, "transfers").await, 0, "binds nothing");

    // The core's deposit is made at the same coordinate; it is that deposit's alone.
    let bridge = core_deposit(&core, 1).await;
    assert_eq!(count(&pool, "transfers").await, 1);
    assert_eq!(
        count(&pool, "intake_answers WHERE transfer_id IS NOT NULL").await,
        0
    );

    // And once it is bound, a request names it as it did before: not found, not consumed.
    let (_, answer) = ingress
        .post("/v0/deposits", ALPHA, &deposit_body("ref-late", c.clone()))
        .await;
    assert_refused(&answer, "ERR-SourceNotFound");
    let (_, answer) = ingress
        .post("/v0/refunds", ALPHA, &refund_body("refund-late", c))
        .await;
    assert_refused(&answer, "ERR-SourceNotFound");
    assert_eq!(count(&pool, "transfers").await, 1);
    let still: String = scalar(&pool, "SELECT bridge_reference FROM transfers").await;
    assert_eq!(still, bridge);
}

/// `REQ-ST-1`, `REQ-ID-4`, `AC-A6.2`: a directed deposit has no answer, so no requester reads it,
/// and no reader fails on it. `by-request`, `by-bridge` and `by-coordinate` answer as for any
/// transfer a requester has no part in; the log holds none of its entries; a callback is not
/// registered for it, whichever of its references is named. A requester's own deposit beside it is
/// read as before.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_1_no_reader_fails_on_a_deposit_that_has_no_answer(pool: PgPool) {
    let hooks = {
        let mut w = Webhooks::new(SealingKey::random());
        w.allow_hosts = vec!["127.0.0.1".into()];
        w
    };
    let ingress = Ingress::with_webhooks(&pool, hooks).await;
    let core = core_pool(&pool).await;
    let own = arrival(&pool, 1, 1_000).await;
    let (status, mine) = ingress
        .post("/v0/deposits", ALPHA, &deposit_body("ref-own", own))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{mine}");
    directed(&pool, 2, 5_000).await;
    let bridge = core_deposit(&core, 2).await;
    let request_reference: String = scalar(
        &pool,
        &format!("SELECT request_reference FROM transfers WHERE bridge_reference = '{bridge}'"),
    )
    .await;

    for credential in [ALPHA, BETA] {
        let (status, _) = ingress
            .get(&format!("/v0/transfers/by-bridge/{bridge}"), credential)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "by-bridge");
        let (status, _) = ingress
            .get(
                &format!("/v0/transfers/by-request/{request_reference}"),
                credential,
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "by-request");
        let (status, by_coordinate) = ingress.get(&coordinate_query(2), credential).await;
        assert_eq!(status, StatusCode::OK, "{by_coordinate}");
        assert_eq!(by_coordinate["transfers"], json!([]), "by-coordinate");
    }
    // The requester's own deposit is read as it was.
    let (status, by_request) = ingress.get("/v0/transfers/by-request/ref-own", ALPHA).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(by_request, mine);

    // The log: the requester's entries, and never the directed deposit's.
    let directed_id: i64 = scalar(
        &pool,
        &format!("SELECT id FROM transfers WHERE bridge_reference = '{bridge}'"),
    )
    .await;
    let (status, log) = ingress.get("/v0/log?after=0&limit=50", ALPHA).await;
    assert_eq!(status, StatusCode::OK, "{log}");
    let entries = log["entries"].as_array().unwrap();
    assert!(!entries.is_empty());
    for e in entries {
        assert_ne!(e["payload"]["bridge_reference"], json!(bridge), "{e}");
    }
    assert!(
        count(
            &pool,
            &format!("event_log WHERE transfer_id = {directed_id}")
        )
        .await
            > 0,
        "the directed deposit's creation is in the sequenced log"
    );

    // No callback is registered for it, by any of its references (REQ-ID-4).
    for path in [
        format!("/v0/webhooks/transfers/by-bridge/{bridge}"),
        format!("/v0/webhooks/transfers/by-request/{request_reference}"),
    ] {
        let (status, body) = ingress
            .put(&path, ALPHA, &json!({ "url": "http://127.0.0.1:1/cb" }))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
    }
    assert_eq!(count(&pool, "webhook_transfer_urls").await, 0);
}

/// `REQ-ST-5`, `REQ-ID-4`: the dispatcher fans the log out to the requesters whose request made or
/// reached a transfer. A directed deposit has none, so a hold of it, a shared gate (`CND-Paused`) and
/// its release among them, enqueues nothing and fails nothing; a requester's own transfer beside it
/// is notified as before.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_5_the_dispatcher_fans_a_directed_deposits_entries_out_to_nobody(pool: PgPool) {
    let key = SealingKey::random();
    let hooks = || {
        let mut w = Webhooks::new(key.clone());
        w.allow_hosts = vec!["127.0.0.1".into()];
        w.max_attempts = 1;
        w.backoff_base = Duration::ZERO;
        w.timeout = Duration::from_secs(2);
        w
    };
    let ingress = Ingress::with_webhooks(&pool, hooks()).await;
    let dispatcher = Dispatcher::new(dispatcher_pool(&pool).await, hooks()).unwrap();
    let core = core_pool(&pool).await;
    let (status, _) = ingress.post("/v0/webhooks/secret", ALPHA, &json!({})).await;
    assert_eq!(status, StatusCode::CREATED);

    // Alpha's own deposit, with a callback; the directed deposit beside it, on the same route.
    let own = arrival(&pool, 1, 1_000).await;
    let (status, _) = ingress
        .post("/v0/deposits", ALPHA, &deposit_body("ref-own", own))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = ingress
        .put(
            "/v0/webhooks/transfers/by-request/ref-own",
            ALPHA,
            &json!({ "url": "http://127.0.0.1:1/cb" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    directed(&pool, 2, 5_000).await;
    let bridge = core_deposit(&core, 2).await;
    let id: i64 = scalar(
        &pool,
        &format!("SELECT id FROM transfers WHERE bridge_reference = '{bridge}'"),
    )
    .await;

    // Everything written so far is read: nothing to alpha's callback about the directed deposit.
    let step = dispatcher.step().await.expect("the fan-out reads it");
    assert!(step.fanned_out > 0, "{step:?}");
    let before = count(&pool, "webhook_deliveries WHERE transfer_id IS NOT NULL").await;

    // Holds of the directed deposit: a transfer's own condition, and a shared gate, in place.
    for condition in [Some("CND-AmountMismatch"), None, Some("CND-Paused"), None] {
        sqlx::query(
            "INSERT INTO transitions (transfer_id, from_state, to_state, condition) \
             SELECT id, state, state, $2::text::transfer_condition FROM transfers WHERE id = $1",
        )
        .bind(id)
        .bind(condition)
        .execute(&pool)
        .await
        .expect("hold the directed deposit in place");
        let step = dispatcher.step().await.expect("the fan-out reads the hold");
        assert!(step.fanned_out > 0, "{step:?}");
    }
    assert_eq!(
        count(&pool, "webhook_deliveries WHERE transfer_id IS NOT NULL").await,
        before,
        "no delivery names a transfer nobody's request made or reached"
    );
    assert_eq!(
        count(&pool, "webhook_deliveries WHERE state = 'dead'").await,
        0
    );
}
