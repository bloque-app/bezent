// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! T-002-05: status by the three keys, reserves and the reserve equation, and the sequenced log
//! from a position (`REQ-RQ-8`, `REQ-ST-2`, `REQ-ST-4`, `REQ-ID-4`; `AC-H1.1`, `AC-H1.2`,
//! `AC-H1.3`).

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::Value;
use sqlx::PgPool;

fn coordinate_query(block: i64) -> String {
    let c = coordinate(block);
    format!(
        "/v0/transfers/by-coordinate?network={}&block_number={}&block_hash={}&position_kind=log&position_index=0",
        c["network"].as_str().unwrap(),
        block,
        c["block_hash"].as_str().unwrap(),
    )
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_h1_1_status_is_returned_by_request_reference_bridge_reference_and_coordinate(
    pool: PgPool,
) {
    let ingress = Ingress::new(&pool).await;
    let (_, answer) = ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("ref-1", arrival(&pool, 1, 1_000).await),
        )
        .await;
    let bridge_reference = answer["bridge_reference"].as_str().unwrap().to_owned();

    let (status, by_request) = ingress.get("/v0/transfers/by-request/ref-1", ALPHA).await;
    assert_eq!(status, StatusCode::OK);
    let (status, by_bridge) = ingress
        .get(
            &format!("/v0/transfers/by-bridge/{bridge_reference}"),
            ALPHA,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, by_coordinate) = ingress.get(&coordinate_query(1), ALPHA).await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(by_request, answer);
    assert_eq!(by_bridge, answer);
    assert_eq!(by_coordinate["transfers"], serde_json::json!([answer]));
    // The status carries the coordinate it rests on, not a verdict (REQ-ST-3).
    assert_eq!(answer["source"]["block_number"], 1);
    assert_eq!(answer["source"]["position"]["kind"], "log");
    assert!(answer.get("source_final").is_none());

    // A refusal is found by its references too.
    let (_, refused) = ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("ref-2", coordinate(99)),
        )
        .await;
    let (_, by_bridge) = ingress
        .get(
            &format!(
                "/v0/transfers/by-bridge/{}",
                refused["bridge_reference"].as_str().unwrap()
            ),
            ALPHA,
        )
        .await;
    assert_eq!(by_bridge, refused);

    // Unknown keys read as not found.
    let (status, _) = ingress.get("/v0/transfers/by-request/nope", ALPHA).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = ingress.get("/v0/transfers/by-bridge/nope", ALPHA).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_id_4_one_requester_cannot_read_anothers_transfers(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    let (_, answer) = ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("ref-1", arrival(&pool, 1, 1_000).await),
        )
        .await;
    let (_, refused) = ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("ref-2", coordinate(99)),
        )
        .await;

    for path in [
        "/v0/transfers/by-request/ref-1".to_owned(),
        format!(
            "/v0/transfers/by-bridge/{}",
            answer["bridge_reference"].as_str().unwrap()
        ),
        format!(
            "/v0/transfers/by-bridge/{}",
            refused["bridge_reference"].as_str().unwrap()
        ),
    ] {
        let (status, _) = ingress.get(&path, BETA).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
    let (_, by_coordinate) = ingress.get(&coordinate_query(1), BETA).await;
    assert_eq!(by_coordinate["transfers"], serde_json::json!([]));
    let (_, log) = ingress.get("/v0/log?after=0", BETA).await;
    assert_eq!(log["entries"], serde_json::json!([]));
    assert!(log["head"].as_i64().unwrap() > 0);
}

/// Every entry of `requester`'s log from position 0, read a page at a time.
async fn read_all(ingress: &Ingress, credential: &str, page: i64) -> Vec<Value> {
    let mut after = 0;
    let mut entries = Vec::new();
    loop {
        let (status, log) = ingress
            .get(&format!("/v0/log?after={after}&limit={page}"), credential)
            .await;
        assert_eq!(status, StatusCode::OK, "{log}");
        entries.extend(log["entries"].as_array().unwrap().iter().cloned());
        let next = log["next_after"].as_i64().unwrap();
        if next == log["head"].as_i64().unwrap() {
            return entries;
        }
        assert!(next > after, "the read moved forward");
        after = next;
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_h1_2_from_its_last_position_a_requester_reads_everything_since(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    // alpha: an accepted deposit (created, then Received -> Accepted), a refused one, and a bare
    // refusal; beta: one of its own, interleaved.
    ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("a-1", arrival(&pool, 1, 1_000).await),
        )
        .await;
    ingress
        .post(
            "/v0/deposits",
            BETA,
            &deposit_body("b-1", arrival(&pool, 2, 1_000).await),
        )
        .await;
    ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("a-2", arrival(&pool, 3, 1).await),
        )
        .await;
    ingress
        .post("/v0/deposits", ALPHA, &deposit_body("a-3", coordinate(99)))
        .await;

    let all = read_all(&ingress, ALPHA, 1_000).await;
    let kinds: Vec<&str> = all.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        [
            "transfer",
            "transition",
            "transfer",
            "transition",
            "refusal"
        ]
    );
    // In order, and every position between two of alpha's that is not beta's is alpha's: nothing
    // of alpha's is skipped (REQ-ST-4).
    let seqs: Vec<i64> = all.iter().map(|e| e["seq"].as_i64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]));
    let total: i64 = scalar(&pool, "SELECT count(*) FROM event_log").await;
    assert_eq!(total, 7, "alpha's five and beta's two");
    assert_eq!(all[1]["payload"]["to_state"], "Accepted");
    assert_eq!(all[3]["payload"]["to_state"], "Refused");
    assert_eq!(all[3]["payload"]["error"], "ERR-BelowMinimum");
    assert_eq!(all[4]["payload"]["error"], "ERR-SourceNotFound");

    // From a later position, exactly what came after it; read in pages of one, the same.
    let after = seqs[2];
    let (_, since) = ingress.get(&format!("/v0/log?after={after}"), ALPHA).await;
    assert_eq!(since["entries"].as_array().unwrap(), &all[3..].to_vec());
    assert_eq!(read_all(&ingress, ALPHA, 1).await, all);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_h1_3_an_entry_read_twice_is_deduplicated_by_its_stable_key(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("a-1", arrival(&pool, 1, 1_000).await),
        )
        .await;
    let (_, first) = ingress.get("/v0/log?after=0", ALPHA).await;
    ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("a-2", arrival(&pool, 2, 1_000).await),
        )
        .await;
    // Delivered again (an overlapping read): the position is the key, and the same position is
    // the same entry.
    let (_, second) = ingress.get("/v0/log?after=0", ALPHA).await;
    let first = first["entries"].as_array().unwrap();
    let second = second["entries"].as_array().unwrap();
    assert_eq!(&second[..first.len()], first.as_slice());
    let mut keys: Vec<i64> = first
        .iter()
        .chain(second)
        .map(|e| e["seq"].as_i64().unwrap())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), second.len());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rq_8_reserve_per_asset_and_network_and_the_latest_equation(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    let (status, reserves) = ingress.get("/v0/reserves", BETA).await;
    assert_eq!(status, StatusCode::OK);
    // One per declared reserve, ordered by asset: bOTHER, then bUSDC.
    assert_eq!(reserves["reserves"].as_array().unwrap().len(), 2);
    assert_eq!(reserves["reserves"][1]["asset"], ASSET);
    assert_eq!(reserves["reserves"][1]["network"], ORIGIN);
    assert_eq!(reserves["reserves"][1]["address"], RESERVE);
    assert_eq!(reserves["reserves"][1]["balance"], "0");

    // As the reserve book holds it.
    exec(
        &pool,
        &format!(
            "WITH e AS (INSERT INTO book_entries (reference) VALUES ('bz-d-1') RETURNING id) \
             INSERT INTO book_postings (entry_id, asset, account, network, side, amount) \
               SELECT id, '{ASSET}', 'reserve', '{ORIGIN}', 'debit', 340282366920938463463374607431768211455 FROM e \
               UNION ALL SELECT id, '{ASSET}', 'owed-mint', NULL, 'credit', 340282366920938463463374607431768211455 FROM e"
        ),
    )
    .await;
    let (_, reserves) = ingress.get("/v0/reserves", BETA).await;
    assert_eq!(
        reserves["reserves"][1]["balance"], "340282366920938463463374607431768211455",
        "exact to the unit (INV-11)"
    );

    // No evaluation is stored before the reserve check exists (F-004).
    let (status, equation) = ingress.get("/v0/reserve-equation", BETA).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(equation["latest"], Value::Null);
}
