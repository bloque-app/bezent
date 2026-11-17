// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-018, T-018-10 (amendment 0029, Open 8; migration 0146): the public status route, at the service
//! layer (SPEC §9: the ingress over the store). With no credential, on its own router and its own
//! login (`bezent_public`), it answers the state, the named conditions and the coordinates of a
//! directed deposit by its arrival's coordinate, and of a withdrawal by its redemption's on-chain
//! reference, and nothing a requester supplied (`REQ-ST-8`); a requester's deposit, by its coordinate,
//! answers exactly as an unknown coordinate does (`REQ-ID-4`); it serves nothing else; and every
//! lookup is by index (rate-limit-friendly, amendment 0029 Open 8).

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use bezent_ingress::public::{LOOKUP, PublicApp, public_router};
use common::*;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

/// The recipient the directing code recorded: a 32-byte account on the issuing network, in hex.
const RECIPIENT: &str = "0x8eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a48";
const DIRECTING: &str = "0xD1Ec7ed00000000000000000000000000000d1ec";
const FUNDER: &str = "0xf00d00000000000000000000000000000000f00d";
/// A redemption's reference and its redeemer, written in two SS58 prefixes (42 and Kreivo's 2):
/// one account. Their bridge reference is the core's vector (`apps/bridge/test/vectors/core-refs.json`).
const REDEMPTION: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";
const REDEEMER_42: &str = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";
const REDEEMER_2: &str = "HNZata7iMYWmk5RvZRTiAsSDhV8366zq2YGb3tLH5Upf74F";
const WITHDRAWAL_REFERENCE: &str = "bz-w-2bf980c6dbcf6d12bdf619c9bf5b468ab3ae442d";

/// The public route, as `bezent_public`'s own login, answering at most `in_flight` at once.
async fn public(pool: &PgPool, in_flight: usize) -> Router {
    let p = role_pool(pool, bezent_store::PUBLIC_ROLE).await;
    public_router(PublicApp::new(p, in_flight))
}

/// GETs `path` with no credential; returns the status, the headers' CORS and cache values, and
/// the body.
async fn get(router: &Router, path: &str) -> (StatusCode, Value) {
    let (status, _, body) = get_with_headers(router, path).await;
    (status, body)
}

async fn get_with_headers(
    router: &Router,
    path: &str,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header(header::ACCEPT, "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, headers, body)
}

fn deposit_path(block: i64) -> String {
    format!(
        "/v0/public/deposits/by-coordinate?network={ORIGIN}&block_number={block}&block_hash=0x{}&position_kind=log&position_index=0",
        hex::encode(block_hash(block))
    )
}

fn withdrawal_path(redeemer: &str, reference: &str) -> String {
    format!("/v0/public/withdrawals/by-redemption?redeemer={redeemer}&reference={reference}")
}

/// What the EVM endpoint records for a final directed arrival at (`ORIGIN`, `block`, log 0), through
/// [`DIRECTING`], which is declared (`REQ-AS-11`; once).
async fn directed(pool: &PgPool, block: i64, amount: u64) {
    exec(
        pool,
        &format!(
            "INSERT INTO directing_code (asset, network, address, token, minimum, maximum, \
                 from_block, reference) \
             VALUES ('{ASSET}', '{ORIGIN}', '{DIRECTING}', '{TOKEN}', 1, 1000000000, 0, \
                 'test declaration') ON CONFLICT DO NOTHING"
        ),
    )
    .await;
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
}

/// The core's deposit of the directed arrival at `block`, as the core's login: (id, bridge reference).
async fn core_deposit(core: &PgPool, block: i64) -> (i64, String) {
    let id: Option<i64> = sqlx::query_scalar("SELECT directed_deposit($1, $2, $3, 'log', 0)")
        .bind(ORIGIN)
        .bind(block)
        .bind(block_hash(block))
        .fetch_one(core)
        .await
        .expect("the core makes the deposit");
    let id = id.expect("a deposit");
    let reference = sqlx::query_scalar("SELECT bridge_reference FROM transfers WHERE id = $1")
        .bind(id)
        .fetch_one(core)
        .await
        .unwrap();
    (id, reference)
}

/// A withdrawal the core made from the redemption (`REDEEMER_42`, `REDEMPTION`) on the issuing
/// network, at block `block` (not final yet), to a payout address.
async fn withdrawal(pool: &PgPool, block: i64) -> i64 {
    sqlx::query(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, token, amount, sender, executed, on_chain_reference, kind, detail) \
         VALUES ($1, $2, '\\xcc', '2026-10-13T00:00:00Z', 'log', 0, $3, 500, $4, TRUE, \
             decode($5, 'hex'), 'redemption', '{}')",
    )
    .bind(ISSUING)
    .bind(block)
    .bind(ASSET)
    .bind(REDEEMER_42)
    .bind(REDEMPTION.trim_start_matches("0x"))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query_scalar(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index, on_chain_reference, \
             redeemer) \
         VALUES ($1, 'redemption:' || $1, 'withdrawal', $2, 500, $3, $4, 'Received', \
             $5, $6, '\\xcc', 'log', 0, decode($7, 'hex'), $8) RETURNING id",
    )
    .bind(WITHDRAWAL_REFERENCE)
    .bind(ASSET)
    .bind(ORIGIN)
    .bind(ORIGIN_ADDRESS)
    .bind(ISSUING)
    .bind(block)
    .bind(REDEMPTION.trim_start_matches("0x"))
    .bind(REDEEMER_42)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Every string anywhere in `v`, and every key.
fn words(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| words(x, out)),
        Value::Object(o) => {
            for (k, x) in o {
                out.push(k.clone());
                words(x, out);
            }
        }
        _ => {}
    }
}

/// `REQ-ST-8`, `REQ-ST-1`, `REQ-ST-3`: a directed deposit is read by anyone, with no credential, by
/// its arrival's coordinate: its state, the condition it holds and the coordinate bezent holds,
/// final as observed, under its bridge reference; and nothing about its destination, its refund
/// address, its amount, or any request (it has none).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_8_a_directed_deposit_is_read_by_its_arrivals_coordinate_with_no_credential(
    pool: PgPool,
) {
    seed(&pool).await;
    let core = role_pool(&pool, bezent_store::CORE_ROLE).await;
    let route = public(&pool, 8).await;
    directed(&pool, 3, 5_000).await;
    let (status, answer) = get(&route, &deposit_path(3)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "no deposit yet: {answer}");

    let (id, reference) = core_deposit(&core, 3).await;
    let (status, headers, answer) = get_with_headers(&route, &deposit_path(3)).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["kind"], "directed_deposit");
    assert_eq!(answer["bridge_reference"], reference.as_str());
    assert_eq!(answer["state"], "Source-Final");
    assert_eq!(answer["conditions"], json!([]));
    assert_eq!(
        answer["coordinates"],
        json!([{
            "network": ORIGIN, "role": "arrival", "block_number": 3,
            "block_hash": format!("0x{}", hex::encode(block_hash(3))),
            "position_kind": "log", "position_index": 0, "final": true,
        }])
    );
    assert_eq!(
        answer["transfers"],
        json!([{"role": "deposit", "bridge_reference": reference, "state": "Source-Final"}])
    );
    assert!(answer["updated_at"].is_string());
    // Public data: any origin reads it, a front may cache it briefly.
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    assert!(
        headers[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("max-age")
    );
    // Only the keys REQ-ST-8 allows: nothing the depositor or a requester wrote, and no amount.
    let mut keys: Vec<&str> = answer
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "bridge_reference",
            "conditions",
            "coordinates",
            "kind",
            "state",
            "transfers",
            "updated_at"
        ]
    );
    let mut all = Vec::new();
    words(&answer, &mut all);
    for absent in [
        RECIPIENT,
        ORIGIN_ADDRESS,
        FUNDER,
        DIRECTING,
        "5000",
        "directed:",
    ] {
        assert!(
            !all.iter().any(|w| w.contains(absent)),
            "{absent} in {answer}"
        );
    }

    // A condition it holds is named, and the answer follows it.
    exec(
        &core,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state, condition) \
             VALUES ({id}, 'Source-Final', 'Source-Final', 'CND-DepositWindow')"
        ),
    )
    .await;
    let (status, answer) = get(&route, &deposit_path(3)).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["conditions"], json!(["CND-DepositWindow"]));
    assert_eq!(answer["transfers"][0]["condition"], "CND-DepositWindow");
}

/// `REQ-ID-4`, `REQ-ST-8`: a deposit a requester's request bound, by its arrival's coordinate,
/// answers "not found" with exactly the body of a coordinate bezent never saw: the route says
/// nothing about what any requester asked for. The authenticated surface still answers its own
/// requester.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_id_4_a_requester_bound_transfers_coordinate_answers_not_found_as_an_unknown_one(
    pool: PgPool,
) {
    let ingress = Ingress::new(&pool).await;
    let route = public(&pool, 8).await;
    let c = arrival(&pool, 5, 5_000).await;
    let (status, bound) = ingress
        .post("/v0/deposits", ALPHA, &deposit_body("alpha-ref-1", c))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{bound}");
    assert!(
        bound["bridge_reference"]
            .as_str()
            .unwrap()
            .starts_with("bz-d-")
    );
    assert_eq!(
        count(&pool, "transfers").await,
        1,
        "the request bound the arrival"
    );

    let (status, known) = get(&route, &deposit_path(5)).await;
    let (unknown_status, unknown) = get(&route, &deposit_path(77)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{known}");
    assert_eq!(unknown_status, StatusCode::NOT_FOUND);
    assert_eq!(known, unknown, "indistinguishable from unknown");
    assert_eq!(known["error"], "not-found");
    // Its withdrawal route does not answer for it either.
    let (status, _) = get(&route, &withdrawal_path(REDEEMER_42, REDEMPTION)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // And the bound deposit's coordinate is the requester's to read, authenticated.
    let (status, list) = ingress
        .get(
            &format!(
                "/v0/transfers/by-coordinate?network={ORIGIN}&block_number=5&block_hash=0x{}&position_kind=log&position_index=0",
                hex::encode(block_hash(5))
            ),
            ALPHA,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["transfers"][0]["request_reference"], "alpha-ref-1");
}

/// `REQ-ST-8`, `REQ-OT-2`: a withdrawal is read by its redemption's on-chain reference (the redeemer,
/// in any SS58 prefix or as hex, and its reference): its state and the redemption's coordinate,
/// not final yet; and nothing a requester added to it, not its extras' request reference nor their
/// expiry, nor its payout address or amount.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_8_a_withdrawal_is_read_by_its_redemption_and_carries_nothing_a_requester_added(
    pool: PgPool,
) {
    let ingress = Ingress::new(&pool).await;
    let route = public(&pool, 8).await;
    withdrawal(&pool, 9).await;
    let extras = json!({
        "request_reference": "alpha-extras-1",
        "redemption_reference": REDEMPTION,
        "expires_at": "2026-12-01T00:00:00Z",
    });
    let (status, answer) = ingress.post("/v0/withdrawals/extras", ALPHA, &extras).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["bridge_reference"], WITHDRAWAL_REFERENCE);

    let account = "0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d";
    let mut answers = Vec::new();
    for redeemer in [REDEEMER_42, REDEEMER_2, account] {
        let (status, answer) = get(&route, &withdrawal_path(redeemer, REDEMPTION)).await;
        assert_eq!(status, StatusCode::OK, "{redeemer}: {answer}");
        answers.push(answer);
    }
    assert!(answers.windows(2).all(|w| w[0] == w[1]), "one account");
    let answer = &answers[0];
    assert_eq!(answer["kind"], "withdrawal");
    assert_eq!(answer["bridge_reference"], WITHDRAWAL_REFERENCE);
    assert_eq!(answer["state"], "Received");
    assert_eq!(
        answer["coordinates"],
        json!([{
            "network": ISSUING, "role": "redemption", "block_number": 9, "block_hash": "0xcc",
            "position_kind": "log", "position_index": 0, "final": false,
        }])
    );
    let mut all = Vec::new();
    words(answer, &mut all);
    for absent in [
        "alpha-extras-1",
        "2026-12-01",
        "expires_at",
        "alpha",
        ORIGIN_ADDRESS,
        "redemption:",
        "500",
    ] {
        assert!(
            !all.iter().any(|w| w.contains(absent)),
            "{absent} in {answer}"
        );
    }

    // Another reference, or another redeemer, is no withdrawal; a malformed one is refused by name.
    let other = format!("0x{}", "11".repeat(32));
    assert_eq!(
        get(&route, &withdrawal_path(REDEEMER_42, &other)).await.0,
        StatusCode::NOT_FOUND
    );
    let (status, body) = get(
        &route,
        &withdrawal_path(
            "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQZ",
            REDEMPTION,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a bad checksum: {body}");
    assert_eq!(body["error"], "ERR-Malformed");
    let (status, _) = get(&route, &withdrawal_path(REDEEMER_42, "0x00")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Nor is a directed deposit's coordinate answered on the withdrawal's route, or the reverse.
    let (status, _) = get(&route, &deposit_path(9)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// `REQ-ST-8`, `REQ-PN-1`: the public router serves its two queries and its health, and nothing of
/// the request, query or webhook surfaces; and its login reads the one view, no table.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_8_the_public_route_serves_nothing_else_and_its_login_reads_no_table(pool: PgPool) {
    seed(&pool).await;
    let route = public(&pool, 8).await;
    let (status, health) = get(&route, "/health").await;
    assert_eq!((status, health), (StatusCode::OK, json!({"status": "ok"})));
    for path in [
        "/v0/transfers/by-request/alpha-ref-1",
        "/v0/transfers/by-coordinate?network=origin-a&block_number=1&block_hash=0x01&position_kind=log&position_index=0",
        "/v0/routes",
        "/v0/log?after=0",
        "/v0/reserves",
    ] {
        assert_eq!(get(&route, path).await.0, StatusCode::NOT_FOUND, "{path}");
    }
    let posted = route
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v0/deposits")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        matches!(
            posted.status(),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ),
        "{}",
        posted.status()
    );
    // A malformed coordinate is refused by name; an unknown parameter too.
    let (status, body) = get(
        &route,
        "/v0/public/deposits/by-coordinate?network=origin-a&block_number=1&block_hash=zz&position_kind=log&position_index=0",
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("ERR-Malformed"))
    );
    let (status, _) = get(&route, &format!("{}&request_reference=x", deposit_path(1))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // The login: its own role's grants exactly (#46), and no table.
    let login = role_pool(&pool, bezent_store::PUBLIC_ROLE).await;
    bezent_store::login::check_login(&login, bezent_store::PUBLIC_ROLE)
        .await
        .expect("the public route's login passes its check");
    for table in [
        "transfers",
        "intake",
        "intake_answers",
        "observations",
        "requesters",
        "event_log",
    ] {
        let err = sqlx::query(&format!("SELECT 1 FROM {table} LIMIT 1"))
            .execute(&login)
            .await
            .expect_err(table);
        assert!(
            err.to_string().contains("permission denied"),
            "{table}: {err}"
        );
    }
    // And the ingress's check refuses it: it is no other service's login.
    assert!(
        bezent_store::login::check_login(&login, bezent_store::INGRESS_ROLE)
            .await
            .is_err()
    );
}

/// `REQ-ST-8` (amendment 0029, Open 8: rate-limit friendly): every lookup the route makes is by
/// index, keyed by the bridge reference its query derives. On a store of some thousands of transfers
/// and observations, its statistics taken, the plan of either lookup scans no table and starts each
/// branch of the view at `transfers`' unique bridge reference, finding a source's observation by its
/// coordinate. And beyond its bound on requests at once it answers `ERR-Overloaded`, retryable, at
/// once.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_8_the_route_reads_by_index_only_and_refuses_beyond_its_bound(pool: PgPool) {
    seed(&pool).await;
    let core = role_pool(&pool, bezent_store::CORE_ROLE).await;
    directed(&pool, 3, 5_000).await;
    core_deposit(&core, 3).await;
    withdrawal(&pool, 9).await;
    exec(
        &pool,
        &format!(
            "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
                 position_index, token, amount, sender, recipient, executed, kind) \
             SELECT '{ORIGIN}', g, '\\xab'::bytea || int8send(g), '2026-10-13T00:00:00Z', 'log', 0, \
                 '{TOKEN}', 100, '0xfunder', '{RESERVE}', TRUE, 'arrival' \
             FROM generate_series(1000, 3999) g; \
             INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
                 position_index, token, amount, sender, executed, on_chain_reference, kind) \
             SELECT '{ISSUING}', g, '\\xcc', '2026-10-13T00:00:00Z', 'log', 0, '{ASSET}', 500, \
                 '{REDEEMER_42}', TRUE, int8send(g), 'redemption' \
             FROM generate_series(1000, 3999) g; \
             INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                 destination_network, destination, state, source_network, source_block_number, \
                 source_block_hash, source_position_kind, source_position_index, \
                 on_chain_reference, redeemer) \
             SELECT 'bz-w-bulk-' || g, 'redemption:bulk-' || g, 'withdrawal', '{ASSET}', 500, \
                 '{ORIGIN}', '{ORIGIN_ADDRESS}', 'Received', '{ISSUING}', g, '\\xcc', 'log', 0, \
                 int8send(g), '{REDEEMER_42}' \
             FROM generate_series(1000, 3999) g; \
             ANALYZE"
        ),
    )
    .await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL enable_seqscan = off")
        .execute(&mut *tx)
        .await
        .unwrap();
    for subject in ["directed_deposit", "withdrawal"] {
        let plan: Vec<String> = sqlx::query_scalar(&format!("EXPLAIN {LOOKUP}"))
            .bind(vec!["bz-d-x".to_owned(), WITHDRAWAL_REFERENCE.to_owned()])
            .bind(subject)
            .fetch_all(&mut *tx)
            .await
            .unwrap();
        let plan = plan.join("\n");
        assert!(!plan.contains("Seq Scan"), "{subject}:\n{plan}");
        assert!(
            !plan.contains("Index Cond: (refund_of IS NULL)"),
            "{subject}: a branch walks every transfer:\n{plan}"
        );
        assert!(
            plan.matches("transfers_bridge_reference_key").count() >= 2,
            "{subject}: each branch starts at the bridge reference:\n{plan}"
        );
        assert!(
            !plan.contains("observations_recipient"),
            "{subject}: a source is found by its coordinate:\n{plan}"
        );
    }
    tx.rollback().await.unwrap();

    // No permit left: refused at once, with a retry hint, and the store is not asked; `/health`
    // takes a permit too (review L2 of PR #139).
    let p = role_pool(&pool, bezent_store::PUBLIC_ROLE).await;
    let app = PublicApp::new(p, 1);
    let held = app.clone();
    let route = public_router(app);
    let _permit = held.hold_for_tests();
    let (status, headers, body) = get_with_headers(&route, &deposit_path(3)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"], "ERR-Overloaded");
    assert_eq!(headers[header::RETRY_AFTER], "1");
    let (status, body) = get(&route, "/health").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"], "ERR-Overloaded");
}

/// `REQ-ST-8`, `REQ-LM-4` (review M1 of PR #139): an unauthenticated caller cannot make the route
/// work on its input. A redeemer longer than an issuing-network account can be (49 base58
/// characters, 66 as `0x` hex), a reference longer than 66, and a URI longer than the route reads,
/// are refused at once, before any decoding and with no permit taken: 64 thousand characters of
/// base58 took some two seconds of a worker to decode before.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_8_an_overlong_query_is_refused_at_once_before_any_decoding(pool: PgPool) {
    seed(&pool).await;
    let p = role_pool(&pool, bezent_store::PUBLIC_ROLE).await;
    let app = PublicApp::new(p, 1);
    let held = app.clone();
    let route = public_router(app);
    // Every permit is held: what is refused here is refused before one is asked for.
    let _permit = held.hold_for_tests();
    let long_base58 = "5".repeat(1500);
    let long_hex = format!("0x{}", "ab".repeat(700));
    for (what, path, status) in [
        (
            "a long base58 redeemer",
            withdrawal_path(&long_base58, REDEMPTION),
            StatusCode::BAD_REQUEST,
        ),
        (
            "a redeemer one character too long",
            withdrawal_path(&format!("{REDEEMER_42}5"), REDEMPTION),
            StatusCode::BAD_REQUEST,
        ),
        (
            "a long hex redeemer",
            withdrawal_path(&long_hex, REDEMPTION),
            StatusCode::BAD_REQUEST,
        ),
        (
            "a long reference",
            withdrawal_path(REDEEMER_42, &long_hex),
            StatusCode::BAD_REQUEST,
        ),
        (
            "a URI longer than the route reads",
            withdrawal_path(&"5".repeat(64_000), REDEMPTION),
            StatusCode::URI_TOO_LONG,
        ),
    ] {
        let started = std::time::Instant::now();
        let (got, body) = get(&route, &path).await;
        assert_eq!(got, status, "{what}: {body}");
        assert_eq!(body["error"], "ERR-Malformed", "{what}");
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "{what}: {:?}",
            started.elapsed()
        );
    }
    // The longest SS58 an account takes, and 0x and 64 hex digits, are still read (here, not
    // found: the permit is held, so they reach the bound instead).
    let (status, _) = get(&route, &withdrawal_path(REDEEMER_42, REDEMPTION)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

/// The public listener, served as the ingress serves it, on `pool`'s route, with `bounds`: its
/// address, the stop sender and the task.
async fn listen(
    pool: &PgPool,
    bounds: bezent_ingress::public::ListenerBounds,
) -> (
    std::net::SocketAddr,
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let p = role_pool(pool, bezent_store::PUBLIC_ROLE).await;
    let route = bezent_ingress::public::public_router_with(
        PublicApp::new(p, 8),
        std::time::Duration::from_secs(5),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let served = tokio::spawn(bezent_ingress::public::serve(
        listener, route, bounds, stop_rx,
    ));
    (address, stop_tx, served)
}

/// Stops the listener and waits for it.
async fn stop_listening(
    stop: tokio::sync::watch::Sender<bool>,
    served: tokio::task::JoinHandle<std::io::Result<()>>,
) {
    stop.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(15), served)
        .await
        .expect("the listener stops")
        .unwrap()
        .unwrap();
}

/// Everything `stream` sends until it closes, or for at most `within`.
async fn read_all(stream: &mut tokio::net::TcpStream, within: std::time::Duration) -> String {
    use tokio::io::AsyncReadExt;
    let mut out = Vec::new();
    let _ = tokio::time::timeout(within, stream.read_to_end(&mut out)).await;
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether `stream` is closed by the other end within `within`: it reads to its end in time.
async fn closed_within(stream: &mut tokio::net::TcpStream, within: std::time::Duration) -> bool {
    use tokio::io::AsyncReadExt;
    let mut sink = Vec::new();
    matches!(
        tokio::time::timeout(within, stream.read_to_end(&mut sink)).await,
        Ok(Ok(_)) | Ok(Err(_))
    )
}

/// Review M1 and L3 of PR #139 and its second fix round: the public listener, served as the ingress
/// serves it, bounds what a connection can hold. A request head larger than it reads is refused
/// (431); a head that does not arrive within the header-read timeout closes the connection; a
/// connection beyond the bound is answered `ERR-Overloaded` at once and closed, not queued, and is
/// served once one closes.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_8_the_public_listener_bounds_heads_slow_clients_and_connections(pool: PgPool) {
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    seed(&pool).await;
    let (address, stop, served) = listen(
        &pool,
        bezent_ingress::public::ListenerBounds {
            header_read_timeout: Duration::from_millis(300),
            idle_timeout: Duration::from_secs(60),
            connection_timeout: Duration::from_secs(30),
            max_connections: 2,
        },
    )
    .await;

    // A healthy request is answered.
    let mut ok = tokio::net::TcpStream::connect(address).await.unwrap();
    ok.write_all(b"GET /health HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n")
        .await
        .unwrap();
    let got = read_all(&mut ok, Duration::from_secs(10)).await;
    assert!(got.starts_with("HTTP/1.1 200"), "{got}");

    // A head larger than the listener reads: 431, the connection closed.
    let mut big = tokio::net::TcpStream::connect(address).await.unwrap();
    let filler = "a".repeat(bezent_ingress::public::MAX_HEAD_BYTES * 2);
    big.write_all(
        format!("GET /health HTTP/1.1\r\nhost: x\r\nx-filler: {filler}\r\n\r\n").as_bytes(),
    )
    .await
    .ok();
    let got = read_all(&mut big, Duration::from_secs(10)).await;
    assert!(got.starts_with("HTTP/1.1 431"), "{got}");

    // A head that never finishes: closed after the header-read timeout, not held.
    let mut slow = tokio::net::TcpStream::connect(address).await.unwrap();
    slow.write_all(b"GET /health HTTP/1.1\r\nhost: x\r\n")
        .await
        .unwrap();
    let started = std::time::Instant::now();
    assert!(
        closed_within(&mut slow, Duration::from_secs(10)).await,
        "the slow head was held"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the slow head was held for {:?}",
        started.elapsed()
    );

    // Two connections held open (the bound): a third is refused at once, ERR-Overloaded with
    // Retry-After, and closed; once one of the two closes, a new one is served.
    let mut first = tokio::net::TcpStream::connect(address).await.unwrap();
    first
        .write_all(b"GET /health HTTP/1.1\r\nhost: x\r\n\r\n")
        .await
        .unwrap();
    let mut second = tokio::net::TcpStream::connect(address).await.unwrap();
    second
        .write_all(b"GET /health HTTP/1.1\r\nhost: x\r\n\r\n")
        .await
        .unwrap();
    let mut buffer = [0u8; 1024];
    for s in [&mut first, &mut second] {
        let n = tokio::time::timeout(Duration::from_secs(10), s.read(&mut buffer))
            .await
            .expect("an answer")
            .unwrap();
        assert!(buffer[..n].starts_with(b"HTTP/1.1 200"));
    }
    let mut third = tokio::net::TcpStream::connect(address).await.unwrap();
    third
        .write_all(b"GET /health HTTP/1.1\r\nhost: x\r\n\r\n")
        .await
        .unwrap();
    let got = read_all(&mut third, Duration::from_secs(10)).await;
    assert!(got.starts_with("HTTP/1.1 503"), "{got}");
    assert!(got.to_ascii_lowercase().contains("retry-after: 1"), "{got}");
    assert!(got.contains(r#""error":"ERR-Overloaded""#), "{got}");
    drop(first);
    let mut fourth = None;
    for _ in 0..50 {
        let mut s = tokio::net::TcpStream::connect(address).await.unwrap();
        s.write_all(b"GET /health HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        let got = read_all(&mut s, Duration::from_secs(10)).await;
        if got.starts_with("HTTP/1.1 200") {
            fourth = Some(got);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(fourth.is_some(), "served once one closed");
    drop(second);
    stop_listening(stop, served).await;
}

/// Second fix round of PR #139: a client that pipelines requests and never reads the answers cannot
/// hold its connection past the connection timeout (hyper has no write timeout of its own), so a
/// well-behaved client is served, here on a listener of one connection, within that timeout and its
/// grace. The answers' bytes outgrow both sockets' buffers, so the server's write blocks.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_8_a_client_that_never_reads_its_answers_is_dropped_at_the_connection_timeout(
    pool: PgPool,
) {
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;
    seed(&pool).await;
    let timeout = Duration::from_secs(2);
    let (address, stop, served) = listen(
        &pool,
        bezent_ingress::public::ListenerBounds {
            header_read_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(60),
            connection_timeout: timeout,
            max_connections: 1,
        },
    )
    .await;
    // The slow reader: a small receive buffer, many pipelined requests (each answered 404, with the
    // route's headers), and never a read.
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    let slow = socket.connect(address).await.unwrap();
    let (_unread, mut writes) = slow.into_split();
    let pipelined = tokio::spawn(async move {
        let one = b"GET /nothing-here HTTP/1.1\r\nhost: x\r\n\r\n";
        for _ in 0..200_000 {
            if writes.write_all(one).await.is_err() {
                return;
            }
        }
    });
    // The slow reader holds the one connection: a client is refused while it does.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = std::time::Instant::now();
    let mut held = false;
    let mut answered = None;
    while started.elapsed() < Duration::from_secs(20) {
        let mut s = tokio::net::TcpStream::connect(address).await.unwrap();
        s.write_all(b"GET /health HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        let got = read_all(&mut s, Duration::from_secs(5)).await;
        if got.starts_with("HTTP/1.1 200") {
            answered = Some(started.elapsed());
            break;
        }
        held = true;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    pipelined.abort();
    assert!(held, "the slow reader never held the connection");
    let answered = answered.expect("the well-behaved client was never served");
    assert!(
        answered < timeout + bezent_ingress::public::CONNECTION_GRACE + Duration::from_secs(5),
        "served only after {answered:?}"
    );
    stop_listening(stop, served).await;
}

/// Second fix round of PR #139 (the review's passing cases kept): a request whose body trickles in
/// does not hold its connection past the header-read timeout of what follows it, and an idle
/// keep-alive connection is kept past the header-read timeout (a front's pool) and closed at the idle
/// timeout.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_8_a_trickled_body_and_an_idle_keep_alive_release_their_connection(pool: PgPool) {
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    seed(&pool).await;
    let (address, stop, served) = listen(
        &pool,
        bezent_ingress::public::ListenerBounds {
            header_read_timeout: Duration::from_millis(500),
            idle_timeout: Duration::from_secs(3),
            connection_timeout: Duration::from_secs(30),
            max_connections: 4,
        },
    )
    .await;

    // A body announced and trickled a byte at a time: the connection is released.
    let mut trickle = tokio::net::TcpStream::connect(address).await.unwrap();
    trickle
        .write_all(b"GET /health HTTP/1.1\r\nhost: x\r\ncontent-length: 100000\r\n\r\n")
        .await
        .unwrap();
    let (mut reads, mut writes) = trickle.into_split();
    let dribble = tokio::spawn(async move {
        for _ in 0..100_000 {
            if writes.write_all(b"a").await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
    let mut sink = Vec::new();
    let released =
        tokio::time::timeout(Duration::from_secs(20), reads.read_to_end(&mut sink)).await;
    dribble.abort();
    assert!(released.is_ok(), "the trickled body held its connection");

    // An idle keep-alive: open past the header-read timeout, served again, then closed when idle.
    let mut kept = tokio::net::TcpStream::connect(address).await.unwrap();
    let mut buffer = [0u8; 1024];
    for _ in 0..2 {
        kept.write_all(b"GET /health HTTP/1.1\r\nhost: x\r\n\r\n")
            .await
            .unwrap();
        let n = tokio::time::timeout(Duration::from_secs(10), kept.read(&mut buffer))
            .await
            .expect("an answer")
            .unwrap();
        assert!(buffer[..n].starts_with(b"HTTP/1.1 200"), "kept alive");
        // Idle for twice the header-read timeout, well within the idle one.
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }
    let idle = std::time::Instant::now();
    assert!(
        closed_within(&mut kept, Duration::from_secs(20)).await,
        "an idle connection was never closed"
    );
    assert!(
        idle.elapsed() >= Duration::from_secs(1),
        "closed after {:?}, before its idle timeout",
        idle.elapsed()
    );
    stop_listening(stop, served).await;
}

/// Review M1 of PR #139: a request the route takes longer than its request timeout to answer is
/// answered 503, retryable, and its work dropped. A pool with no connection free stands in for a
/// slow store.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_8_a_request_past_its_timeout_answers_503(pool: PgPool) {
    use std::time::Duration;
    seed(&pool).await;
    let p = role_pool(&pool, bezent_store::PUBLIC_ROLE).await;
    // Every connection of the route's pool busy (8, `role_pool`'s): its next query waits for one
    // past the timeout.
    let mut busy = Vec::new();
    for _ in 0..8 {
        busy.push(p.acquire().await.unwrap());
    }
    let route = bezent_ingress::public::public_router_with(
        PublicApp::new(p, 8),
        Duration::from_millis(100),
    );
    let started = std::time::Instant::now();
    let (status, headers, body) = get_with_headers(&route, "/health").await;
    drop(busy);
    assert!(
        matches!(status, StatusCode::SERVICE_UNAVAILABLE),
        "{status}"
    );
    assert_eq!(headers[header::RETRY_AFTER], "1");
    // Second fix round of PR #139: ERR-Overloaded's own body, not an empty one.
    assert_eq!(body["error"], "ERR-Overloaded", "{body}");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
}
