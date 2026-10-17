// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! T-002-04: the route table, its limits, and backpressure (`REQ-LM-1`, `REQ-LM-2`, `REQ-LM-4`,
//! §8.4; `AC-G1.1`, `AC-G1.2`, `AC-G1.3`).

mod common;

use axum::http::{StatusCode, header};
use common::*;
use serde_json::Value;
use sqlx::PgPool;

fn assert_refused(body: &Value, error: &str) {
    assert_eq!(body["state"], "Refused", "{body}");
    assert_eq!(body["error"], error, "{body}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_g1_1_the_route_table_declares_limits_and_current_capacity(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("ref-1", arrival(&pool, 1, 4_000).await),
        )
        .await;

    let (status, table) = ingress.get("/v0/routes", BETA).await;
    assert_eq!(status, StatusCode::OK);
    let routes = table["routes"].as_array().unwrap();
    assert_eq!(
        routes.len(),
        1,
        "a route not declared does not exist (REQ-AS-5)"
    );
    let r = &routes[0];
    assert_eq!(r["asset"], ASSET);
    assert_eq!(r["network"], ORIGIN);
    assert_eq!(r["direction"], "deposit");
    assert_eq!(r["minimum"], MINIMUM.to_string());
    assert_eq!(r["maximum"], MAXIMUM.to_string());
    assert_eq!(r["window"]["limit"], WINDOW_LIMIT.to_string());
    assert_eq!(r["window"]["seconds"], 86_400);
    assert_eq!(r["window"]["used"], "4000");
    assert_eq!(r["window"]["remaining"], (WINDOW_LIMIT - 4_000).to_string());
    assert_eq!(r["capacity"]["declared"], CAPACITY);
    assert_eq!(r["capacity"]["in_flight"], 1);
    assert_eq!(r["capacity"]["available"], CAPACITY - 1);
    assert_eq!(r["cutoffs"], serde_json::json!([]));
    assert_eq!(r["completion_budget_seconds"], 3_600);
    assert_eq!(r["paused"], false);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_g1_2_a_request_outside_the_limits_is_refused_with_a_named_error(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;

    let (_, answer) = ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("below", arrival(&pool, 1, MINIMUM - 1).await),
        )
        .await;
    assert_refused(&answer, "ERR-BelowMinimum");

    let (_, answer) = ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("above", arrival(&pool, 2, MAXIMUM + 1).await),
        )
        .await;
    assert_refused(&answer, "ERR-AboveMaximum");

    // The window: two at the maximum fit, a third would exceed it.
    for (block, reference) in [(3, "w-1"), (4, "w-2")] {
        let (_, answer) = ingress
            .post(
                "/v0/deposits",
                ALPHA,
                &deposit_body(reference, arrival(&pool, block, MAXIMUM).await),
            )
            .await;
        assert_eq!(answer["state"], "Accepted", "{answer}");
    }
    let (_, answer) = ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("w-3", arrival(&pool, 5, MAXIMUM).await),
        )
        .await;
    assert_refused(&answer, "ERR-WindowExceeded");

    // A token admitted for an asset with no route.
    let routeless = observe(
        &pool,
        6,
        ROUTELESS_TOKEN,
        1_000,
        "0x00000000000000000000000000000000000000bb",
    )
    .await;
    let (_, answer) = ingress
        .post("/v0/deposits", ALPHA, &deposit_body("no-route", routeless))
        .await;
    assert_refused(&answer, "ERR-UnknownRoute");

    // A pause of the asset (REQ-GV-1).
    exec(
        &pool,
        &format!("INSERT INTO pauses (asset) VALUES ('{ASSET}')"),
    )
    .await;
    let (_, answer) = ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("paused", arrival(&pool, 7, 1_000).await),
        )
        .await;
    assert_refused(&answer, "ERR-RoutePaused");
    let (_, table) = ingress.get("/v0/routes", ALPHA).await;
    assert_eq!(table["routes"][0]["paused"], true);

    // Each refusal moved nothing: no refused transfer binds its arrival (INV-2), and each one's
    // arrival can still be bound once the condition clears.
    let bound_refused: i64 = scalar(
        &pool,
        "SELECT count(*) FROM transfers WHERE state = 'Refused' AND source_network IS NOT NULL",
    )
    .await;
    assert_eq!(bound_refused, 0);
    exec(&pool, "DELETE FROM pauses").await;
    let (_, answer) = ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("unpaused", coordinate(7)),
        )
        .await;
    assert_eq!(answer["state"], "Accepted", "{answer}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_g1_3_when_bezent_cannot_keep_up_new_requests_are_refused_with_overloaded(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    let body = deposit_body("ref-1", arrival(&pool, 1, 1_000).await);

    // Every intake permit is taken: the request is refused, retryably, and nothing is recorded.
    let permits = ingress.app.intake_permits();
    let held = permits
        .clone()
        .acquire_many_owned(u32::try_from(permits.available_permits()).unwrap())
        .await
        .unwrap();
    let (status, answer) = ingress
        .call("POST", "/v0/deposits", Some(ALPHA), Some(&body))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(answer["error"], "ERR-Overloaded");
    assert_eq!(count(&pool, "intake").await, 0);
    drop(held);

    // Retried with the same reference, it is taken in.
    let (status, answer) = ingress.post("/v0/deposits", ALPHA, &body).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(answer["state"], "Accepted");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_g1_3_a_route_at_capacity_refuses_with_overloaded_and_records_nothing(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    exec(&pool, "UPDATE routes SET capacity = 1").await;
    let (_, first) = ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("ref-1", arrival(&pool, 1, 1_000).await),
        )
        .await;
    assert_eq!(first["state"], "Accepted");

    let second = deposit_body("ref-2", arrival(&pool, 2, 1_000).await);
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/v0/deposits")
        .header(header::AUTHORIZATION, format!("Bearer {ALPHA}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(serde_json::to_vec(&second).unwrap()))
        .unwrap();
    let response = {
        use tower::ServiceExt;
        bezent_ingress::router(ingress.app.clone())
            .oneshot(request)
            .await
            .unwrap()
    };
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()[header::RETRY_AFTER], "1");
    let (_, table) = ingress.get("/v0/routes", ALPHA).await;
    assert_eq!(table["routes"][0]["capacity"]["available"], 0);
    assert_eq!(count(&pool, "intake").await, 1);

    // The first transfer ends (its observation withdrawn): capacity returns, and the same
    // request is taken in.
    exec(
        &pool,
        "INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
             block_hash, block_time) \
         SELECT id, 'Accepted', 'Withdrawn', 'origin-a', 5, '\\x05', '2026-10-13T00:00:00Z' \
         FROM transfers",
    )
    .await;
    let (status, answer) = ingress.post("/v0/deposits", ALPHA, &second).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(answer["state"], "Accepted");
}
