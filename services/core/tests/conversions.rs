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
//! Amendment 0018 as ruled (`d-cv-conversion-link`, A1b: the request comes first). A deposit may
//! name a final convertible arrival; it reaches `Source-Final` holding `CND-AwaitingConversion`
//! and nothing is minted. Its requester then names, by the deposit's request reference, the
//! conversion's output (`POST /v0/deposits/conversion`, `REQ-RQ-4`); the store moves the deposit's
//! source there (migration 0120), the core clears the condition and mints the output's amount.
//! The store's own rules for the move (`INV-2`) are `crates/store/tests/deposit_conversions.rs`.
//! Nothing here is a claim that bezent can tell that the arrival fed that conversion, or that the
//! requester is the one the value belongs to: it cannot (amendment 0018, A2, open).

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::Duration;
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
/// The funder a convertible arrival comes from, declared as the requester's sender (`REQ-PN-3`,
/// amendment 0026): a deposit binds only an arrival from one of its requester's declared senders.
const FUNDER: &str = "0xFunder0000000000000000000000000000000001";
/// The second requester's own sender.
const OTHER_FUNDER: &str = "0xFunder0000000000000000000000000000000002";

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

/// A convertible arrival at the reserve: the convertible, from the requester's funder, in a
/// transaction of its own.
async fn convertible_arrival(pool: &PgPool, block: i64, amount: u128) -> i64 {
    convertible_arrival_from(pool, block, amount, FUNDER).await
}

/// A convertible arrival at the reserve from `sender`, in a transaction of its own.
async fn convertible_arrival_from(pool: &PgPool, block: i64, amount: u128, sender: &str) -> i64 {
    observe(
        pool,
        ORIGIN,
        block,
        0,
        CONVERTIBLE,
        amount,
        Some(sender),
        Some(RESERVE),
        None,
        true,
        detail(
            "ConvertibleArrival",
            &format!("0x{:064x}", 1000 + block),
            json!({}),
        ),
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

/// The second requester of the binding tests, holding the deposit grant only.
const OTHER: &str = "other";
const OTHER_CREDENTIAL: &str = "ot-1.other-secret-0123456789";

/// Grants `requester` the deposit grant under `credential` (`<id>.<secret>`), with `sender` as its
/// one declared sender on the origin network (`REQ-PN-3`).
async fn grant(w: &World, requester: &str, credential: &str, sender: &str) {
    let (id, secret) = credential.split_once('.').unwrap();
    let digest = hex::encode(Sha256::digest(secret.as_bytes()));
    exec(
        &w.admin,
        &format!(
            "INSERT INTO requesters (requester) VALUES ('{requester}'); \
             INSERT INTO requester_grants (requester, grant_kind) VALUES ('{requester}', 'deposit'); \
             INSERT INTO requester_senders (requester, network, address) \
               VALUES ('{requester}', '{ORIGIN}', '{sender}'); \
             INSERT INTO requester_credentials (credential_id, requester, secret_sha256) \
               VALUES ('{id}', '{requester}', '\\x{digest}');"
        ),
    )
    .await;
}

/// Declares `sender` as one of `requester`'s senders on the origin network (`REQ-PN-3`).
async fn declare_sender(w: &World, requester: &str, sender: &str) {
    exec(
        &w.admin,
        &format!(
            "INSERT INTO requester_senders (requester, network, address) \
               VALUES ('{requester}', '{ORIGIN}', '{sender}')"
        ),
    )
    .await;
}

/// `POST` of `body` to `path` under `credential`: the status and the answer.
async fn post(router: &Router, path: &str, credential: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::post(path)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {credential}"))
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

/// `POST /v0/deposits` for `coordinate` under `credential`.
async fn deposit_as(
    router: &Router,
    credential: &str,
    reference: &str,
    coordinate: &Value,
) -> (StatusCode, Value) {
    post(
        router,
        "/v0/deposits",
        credential,
        json!({
            "request_reference": reference,
            "coordinate": coordinate,
            "destination": {"network": ISSUING, "address": DESTINATION},
            "refund_destination": {"network": ORIGIN, "address": "0x52908400098527886E0F7030069857D2E4169EE7"},
        }),
    )
    .await
}

/// `POST /v0/deposits/conversion`: under `credential`, request `reference` names `output` as the
/// conversion output of the deposit requested as `deposit_reference` (`REQ-RQ-4`).
async fn conversion_as(
    router: &Router,
    credential: &str,
    reference: &str,
    deposit_reference: &str,
    output: &Value,
) -> (StatusCode, Value) {
    post(
        router,
        "/v0/deposits/conversion",
        credential,
        json!({
            "request_reference": reference,
            "deposit_reference": deposit_reference,
            "conversion_output": output,
        }),
    )
    .await
}

/// The transfer a request answered, by its bridge reference.
async fn transfer_of(w: &World, answer: &Value) -> i64 {
    scalar(
        &w.admin,
        &format!(
            "SELECT id FROM transfers WHERE bridge_reference = '{}'",
            answer["bridge_reference"]
                .as_str()
                .expect("a bridge reference")
        ),
    )
    .await
}

/// The source block and log index a transfer is bound to, and its amount.
async fn bound_to(w: &World, id: i64) -> (i64, i64, String) {
    sqlx::query_as(
        "SELECT source_block_number, source_position_index, amount::text FROM transfers WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&w.admin)
    .await
    .unwrap()
}

/// A convertible arrival of 5 000 at block 10, final at 14, requested as a deposit (`dep`) by the
/// first requester: through the real ingress, then the core's cycles. The router and the deposit.
async fn awaiting(w: &World) -> (Router, i64) {
    let router = ingress(w).await;
    let arrival = convertible_arrival(&w.admin, 10, 5_000).await;
    decide(&w.admin, arrival, "final", 14).await;
    let (status, answer) = deposit_as(&router, CREDENTIAL, "dep", &coordinate(10, 0)).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["state"], "Accepted", "{answer}");
    let id = transfer_of(w, &answer).await;
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-AwaitingConversion".into()))
    );
    (router, id)
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
             INSERT INTO requester_senders (requester, network, address) \
               VALUES ('{REQUESTER}', '{ORIGIN}', '{FUNDER}'); \
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

/// `AC-A5.1`, `REQ-CV-1`, `INV-6`, through the real ingress to the core, with no transition written
/// by hand: a deposit request naming a final convertible arrival is accepted, the core takes it to
/// `Source-Final` holding `CND-AwaitingConversion`, and nothing is ordered, cycle after cycle. The
/// arrival is the deposit's now, not an unassociated one; and the store refuses, to the core's own
/// login, a mint order written against it by hand (the order guard admits only a token admitted
/// for the asset).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a5_1_req_cv_1_inv_6_a_deposit_naming_a_convertible_arrival_holds_cnd_awaiting_conversion_and_mints_nothing(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (_, id) = awaiting(&w).await;
    head(&w.admin, ISSUING, 100).await;
    for _ in 0..3 {
        w.settle(t0()).await;
    }
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-AwaitingConversion".into()))
    );
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
    assert_eq!(
        w.count("SELECT count(*) FROM unassociated_arrivals").await,
        0
    );

    let core = login_pool(&w.admin, CORE_LOGIN, bezent_store::CORE_ROLE, 1).await;
    // The reference the core would write for it, so the order reaches the order guard itself.
    sqlx::query("UPDATE transfers SET on_chain_reference = '\\x01' WHERE id = $1")
        .bind(id)
        .execute(&core)
        .await
        .expect("the core writes a deposit's reference");
    let refused = sqlx::query(
        "INSERT INTO intents (transfer_id, network, purpose, on_chain_reference, attempt, payload) \
         VALUES ($1, $2, 'mint', '\\x01', 1, $3)",
    )
    .bind(id)
    .bind(ISSUING)
    .bind(json!({"order": {"asset": ISSUING_ASSET, "to": DESTINATION_ACCOUNT}, "amount": "5000"}))
    .execute(&core)
    .await
    .expect_err("a mint against a convertible arrival")
    .to_string();
    assert!(refused.contains("INV-6"), "{refused}");
}

/// The happy path of A1b, `AC-A5.2`, `REQ-CV-2`, `REQ-CV-5`, `REQ-RQ-4`, `INV-2`, `INV-6`: the
/// request first (the deposit holds `CND-AwaitingConversion`), then the output (named by the
/// deposit's request reference; not final: nothing recorded; final: the source moves to it), then
/// the mint, for exactly the output's amount. Both amounts are reported: the deposit's `amount` is
/// the output's, its `converted_arrival` the arrival's.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a5_2_req_cv_2_req_cv_5_req_rq_4_inv_2_inv_6_the_request_comes_first_then_the_output_then_the_mint(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (router, id) = awaiting(&w).await;
    let (input, output) = swap(&w.admin, 12, 5_000, 4_990).await;

    // The output is observed, not final: nothing recorded, the request reference not consumed.
    let (status, answer) =
        conversion_as(&router, CREDENTIAL, "conv", "dep", &coordinate(12, 2)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{answer}");
    assert_eq!(w.count("SELECT count(*) FROM deposit_conversions").await, 0);
    // The swap's input is no output.
    decide(&w.admin, input, "final", 16).await;
    let (_, answer) =
        conversion_as(&router, CREDENTIAL, "conv-in", "dep", &coordinate(12, 1)).await;
    assert_eq!(answer["error"], "ERR-SourceNotFound", "{answer}");

    decide(&w.admin, output, "final", 16).await;
    let (status, answer) =
        conversion_as(&router, CREDENTIAL, "conv", "dep", &coordinate(12, 2)).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["state"], "Source-Final", "{answer}");
    assert_eq!(answer["amount"], "4990", "{answer}");
    assert_eq!(answer["source"]["block_number"], 12, "{answer}");
    assert_eq!(answer["source"]["position"]["index"], 2, "{answer}");
    assert_eq!(answer["converted_arrival"]["amount"], "5000", "{answer}");
    assert_eq!(
        answer["converted_arrival"]["token"], CONVERTIBLE,
        "{answer}"
    );
    assert_eq!(
        answer["converted_arrival"]["coordinate"]["block_number"], 10,
        "{answer}"
    );
    assert_eq!(bound_to(&w, id).await, (12, 2, "4990".into()));
    // A repeat answers alike (REQ-ID-1); the deposit's own request now shows the same.
    let (status, again) =
        conversion_as(&router, CREDENTIAL, "conv", "dep", &coordinate(12, 2)).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["converted_arrival"], answer["converted_arrival"]);

    // The core clears the condition on the output's final point, then orders the mint: the
    // output's amount, to the request's destination, under the reference derived from the output.
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await, ("Source-Final".into(), None));
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
    let reference: Vec<u8> = scalar(
        &w.admin,
        &format!("SELECT on_chain_reference FROM transfers WHERE id = {id}"),
    )
    .await;
    let source = bezent_core_service::refs::Coordinate {
        network: ORIGIN.into(),
        block_number: 12,
        block_hash: hash(ORIGIN, 12),
        position_kind: "log".into(),
        position_index: 2,
    };
    assert_eq!(
        reference,
        bezent_core_service::refs::mint_reference(&source).to_vec()
    );

    // Written once (REQ-RQ-4): another output for the same deposit is a conflict.
    let (_, other_output) = swap(&w.admin, 18, 1, 1).await;
    decide(&w.admin, other_output, "final", 20).await;
    let (_, answer) = conversion_as(&router, CREDENTIAL, "conv-2", "dep", &coordinate(18, 2)).await;
    assert_eq!(answer["error"], "ERR-ReferenceConflict", "{answer}");
}

/// `REQ-PN-3`, `REQ-CV-2`, `REQ-RQ-4`, `INV-6` (amendment 0026 on A1b): the convertible arrival's
/// sender is what is checked, when a deposit binds it. Another requester, holding the deposit grant
/// but not that sender, is refused `ERR-SenderNotDeclared` on the arrival; the requester whose sender
/// it came from binds it, and then names the output, sent from the venue's pool that nobody
/// declared: the output is bound through the deposit, and the mint is the output's.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pn_3_req_cv_2_the_convertible_arrivals_sender_is_checked_and_its_output_binds_through_the_deposit(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let router = ingress(&w).await;
    grant(&w, OTHER, OTHER_CREDENTIAL, OTHER_FUNDER).await;
    let arrival = convertible_arrival(&w.admin, 10, 5_000).await;
    decide(&w.admin, arrival, "final", 14).await;

    let (_, answer) = deposit_as(&router, OTHER_CREDENTIAL, "theirs", &coordinate(10, 0)).await;
    assert_eq!(answer["error"], "ERR-SenderNotDeclared", "{answer}");
    let (_, answer) = deposit_as(&router, CREDENTIAL, "dep", &coordinate(10, 0)).await;
    assert_eq!(answer["state"], "Accepted", "{answer}");
    let id = transfer_of(&w, &answer).await;
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-AwaitingConversion".into()))
    );

    let (_, output) = swap(&w.admin, 12, 5_000, 4_990).await;
    decide(&w.admin, output, "final", 16).await;
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM requester_senders WHERE address = '{POOL}'"
        ))
        .await,
        0,
        "nobody declared the pool"
    );
    let (_, answer) = conversion_as(&router, CREDENTIAL, "conv", "dep", &coordinate(12, 2)).await;
    assert_eq!(answer["state"], "Source-Final", "{answer}");
    assert_eq!(answer["converted_arrival"]["amount"], "5000", "{answer}");
    assert_eq!(bound_to(&w, id).await, (12, 2, "4990".into()));
    w.cycle(t0()).await;
    w.cycle(t0()).await;
    let (_, payload) = order(&w.admin, id, 1).await.expect("the mint order");
    assert_eq!(payload["amount"], "4990", "{payload}");
}

/// `REQ-RQ-4`, `INV-2`: an output is named only by the requester that made the deposit. Another
/// requester, holding the deposit grant, finds no deposit of its own by that reference, and its
/// request is refused with nothing moved. (That it could have bound the same output by a deposit of
/// its own first is the exposure amendment 0018 leaves open; the next test.)
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rq_4_inv_2_an_output_named_by_a_different_requester_is_refused(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (router, id) = awaiting(&w).await;
    grant(&w, OTHER, OTHER_CREDENTIAL, OTHER_FUNDER).await;
    let (_, output) = swap(&w.admin, 12, 5_000, 4_990).await;
    decide(&w.admin, output, "final", 16).await;

    let (status, answer) =
        conversion_as(&router, OTHER_CREDENTIAL, "conv", "dep", &coordinate(12, 2)).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["error"], "ERR-SourceNotFound", "{answer}");
    assert_eq!(bound_to(&w, id).await, (10, 0, "5000".into()));
    assert_eq!(w.count("SELECT count(*) FROM deposit_conversions").await, 0);
    w.settle(t0()).await;
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
}

/// `INV-2`, `REQ-PN-3`: an output already consumed is refused. A second requester can no longer
/// bind the swap's output by a deposit of its own: the output is sent from the venue's pool, which
/// it did not declare (amendment 0026). It can still name it as the output of its own awaiting
/// deposit, of an arrival from its own sender: that the arrival fed that conversion stays the
/// requester's word (amendment 0018, A1b; open in amendment 0026). Then the first requester's
/// conversion request is `ERR-SourceConsumed`, and its deposit still awaits, minting nothing.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_2_req_pn_3_an_output_already_consumed_is_refused(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (router, id) = awaiting(&w).await;
    grant(&w, OTHER, OTHER_CREDENTIAL, OTHER_FUNDER).await;
    let (_, output) = swap(&w.admin, 12, 5_000, 4_990).await;
    decide(&w.admin, output, "final", 16).await;
    let (_, answer) = deposit_as(&router, OTHER_CREDENTIAL, "theirs", &coordinate(12, 2)).await;
    assert_eq!(answer["error"], "ERR-SenderNotDeclared", "{answer}");

    // The second requester's own convertible arrival, and its deposit awaiting a conversion.
    let theirs = convertible_arrival_from(&w.admin, 11, 5_000, OTHER_FUNDER).await;
    decide(&w.admin, theirs, "final", 16).await;
    let (_, answer) = deposit_as(&router, OTHER_CREDENTIAL, "theirs-own", &coordinate(11, 0)).await;
    assert_eq!(answer["state"], "Accepted", "{answer}");
    w.settle(t0()).await;
    let (_, answer) = conversion_as(
        &router,
        OTHER_CREDENTIAL,
        "theirs-conv",
        "theirs-own",
        &coordinate(12, 2),
    )
    .await;
    assert_eq!(answer["state"], "Source-Final", "{answer}");

    let (_, answer) = conversion_as(&router, CREDENTIAL, "conv", "dep", &coordinate(12, 2)).await;
    assert_eq!(answer["error"], "ERR-SourceConsumed", "{answer}");
    assert_eq!(bound_to(&w, id).await, (10, 0, "5000".into()));
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-AwaitingConversion".into()))
    );
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM intents WHERE transfer_id = {id}"
        ))
        .await,
        0
    );
}

/// `INV-2`: a convertible arrival is consumed once. A second deposit naming it is refused while the
/// first awaits its output, and still after the first moved on to the output: the arrival is
/// recorded against the first deposit, never bound again.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_2_an_arrival_named_twice_is_refused(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (router, id) = awaiting(&w).await;
    grant(&w, OTHER, OTHER_CREDENTIAL, OTHER_FUNDER).await;
    let (_, answer) = deposit_as(&router, CREDENTIAL, "dep-again", &coordinate(10, 0)).await;
    assert_eq!(answer["error"], "ERR-SourceConsumed", "{answer}");

    let (_, output) = swap(&w.admin, 12, 5_000, 4_990).await;
    decide(&w.admin, output, "final", 16).await;
    let (_, answer) = conversion_as(&router, CREDENTIAL, "conv", "dep", &coordinate(12, 2)).await;
    assert_eq!(answer["state"], "Source-Final", "{answer}");
    let (_, answer) = deposit_as(&router, CREDENTIAL, "dep-later", &coordinate(10, 0)).await;
    assert_eq!(answer["error"], "ERR-SourceConsumed", "{answer}");
    // Another requester is refused before consumption is looked at: the arrival is not from a
    // sender it declared (REQ-PN-3, amendment 0026).
    let (_, answer) = deposit_as(
        &router,
        OTHER_CREDENTIAL,
        "theirs-later",
        &coordinate(10, 0),
    )
    .await;
    assert_eq!(answer["error"], "ERR-SenderNotDeclared", "{answer}");
    assert_eq!(
        w.count("SELECT count(*) FROM transfers WHERE state <> 'Refused'")
            .await,
        1
    );
    assert_eq!(bound_to(&w, id).await, (12, 2, "4990".into()));
    assert_eq!(
        w.count("SELECT count(*) FROM unassociated_arrivals").await,
        0
    );
}

/// `REQ-CV-6`, `REQ-RF-1`, `REQ-RV-1`, `REQ-CV-2`: a deposit whose conversion's output is not named by
/// its deadline (the route's, from the convertible arrival's finality, at a final block of the issuing
/// network) goes to `Needs-Review`, keeping the condition. Nothing is refunded and nothing ordered.
/// An output named afterwards is recorded (amendment 0018, Open 6, ruled `d-cv-open-choices` (a)):
/// the deposit's source and amount move to it, and the deposit stays in review, still unminted and
/// unrefunded. How the review then ends is the next test.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_cv_6_req_rf_1_past_its_deadline_the_deposit_goes_to_review_never_to_a_refund(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (router, id) = awaiting(&w).await;
    head(&w.admin, ISSUING, 14 + BUDGET).await;
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-AwaitingConversion".into()))
    );
    head(&w.admin, ISSUING, 14 + BUDGET + 1).await;
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Needs-Review".into(), Some("CND-AwaitingConversion".into()))
    );
    assert_eq!(
        w.count("SELECT count(*) FROM transfers WHERE kind = 'refund'")
            .await,
        0
    );
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);

    // Open 6, ruled: the output is named while the deposit is in review. The source and the amount
    // move, both amounts are reported, and the deposit stays where it is.
    let (_, output) = swap(&w.admin, 20, 5_000, 4_990).await;
    decide(&w.admin, output, "final", 22).await;
    let (status, answer) =
        conversion_as(&router, CREDENTIAL, "conv", "dep", &coordinate(20, 2)).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["state"], "Needs-Review", "{answer}");
    assert_eq!(answer["amount"], "4990", "{answer}");
    assert_eq!(answer["converted_arrival"]["amount"], "5000", "{answer}");
    assert_eq!(bound_to(&w, id).await, (20, 2, "4990".into()));
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Needs-Review".into(), Some("CND-AwaitingConversion".into()))
    );
    assert_eq!(
        w.count("SELECT count(*) FROM transfers WHERE kind = 'refund'")
            .await,
        0
    );
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
    // Written once (REQ-RQ-4): a second output for the deposit is a conflict, in review too.
    let (_, other) = swap(&w.admin, 24, 1, 1).await;
    decide(&w.admin, other, "final", 26).await;
    let (_, answer) = conversion_as(&router, CREDENTIAL, "conv-2", "dep", &coordinate(24, 2)).await;
    assert_eq!(answer["error"], "ERR-ReferenceConflict", "{answer}");
}

/// Operator `by` (an index into `World::ops`) records, as their own login, a resolution of transfer
/// `id` into `to` resting on the issuing network's final head at `block` (the head is recorded).
async fn resolution(w: &World, id: i64, to: &str, block: i64, by: usize) -> i64 {
    head(&w.admin, ISSUING, block).await;
    sqlx::query_scalar(
        "INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, \
             block_hash, block_time, justification) \
         VALUES ('review-resolution', $1, $2::transfer_state, $3, $4, $5, $6, 'see the block') \
         RETURNING id",
    )
    .bind(id)
    .bind(to)
    .bind(ISSUING)
    .bind(block)
    .bind(hash(ISSUING, block))
    .bind(t0() + Duration::seconds(block))
    .fetch_one(&w.ops[by])
    .await
    .unwrap()
}

/// A deposit awaiting its output whose deadline passed: in review, holding the condition.
async fn in_review(w: &World) -> (Router, i64) {
    let (router, id) = awaiting(w).await;
    head(&w.admin, ISSUING, 14 + BUDGET + 1).await;
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Needs-Review".into(), Some("CND-AwaitingConversion".into()))
    );
    (router, id)
}

/// `REQ-CV-2`, `REQ-CV-5`, `REQ-RV-2`, `REQ-GV-5`, `INV-2`, `INV-6`, amendment 0018 Open 6 (ruled
/// `d-cv-open-choices` (a)): an output named for a deposit in review is minted exactly once, at the
/// output's amount, after the review ends as every review ends: a resolution into `Delivering` (one
/// ordinary edge from `Source-Final`, the state held before review) recorded by one operator and
/// approved by a second. Naming moves nothing out of review, neither does a resolution before the
/// approval, and one approved before the output was named never runs.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a5_4_req_cv_2_req_rv_2_req_gv_5_inv_6_an_output_named_in_review_mints_once_after_a_two_person_resolution(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (router, id) = in_review(&w).await;

    // A resolution into Delivering while the source is still the convertible arrival, approved by
    // both: nothing is minted against the arrival (INV-6), and the evidence is refused, so it does
    // not run later at an amount its approvers never saw (the store's order guard stands behind
    // it too).
    let early = resolution(&w, id, "Delivering", 7_000, 0).await;
    approve_as(&w, 1, early).await.unwrap();
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM event_log WHERE kind = 'review-evidence-refused' \
               AND payload ->> 'manual_action_id' = '{early}'"
        ))
        .await,
        1
    );

    // One recorded and approved, then the output named before the core looks at it: it was approved
    // with no amount to mint, so it is refused too (REQ-GV-5), whenever the core sees it.
    let racy = resolution(&w, id, "Delivering", 7_050, 0).await;
    approve_as(&w, 1, racy).await.unwrap();
    let (_, output) = swap(&w.admin, 20, 5_000, 4_990).await;
    decide(&w.admin, output, "final", 22).await;
    let (status, answer) =
        conversion_as(&router, CREDENTIAL, "conv", "dep", &coordinate(20, 2)).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(bound_to(&w, id).await, (20, 2, "4990".into()));
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM event_log WHERE kind = 'review-evidence-refused' \
               AND payload ->> 'manual_action_id' = '{racy}'"
        ))
        .await,
        1
    );

    // A new resolution, recorded and not yet approved: nothing moves (REQ-GV-5).
    let action = resolution(&w, id, "Delivering", 7_100, 0).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Needs-Review");
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);

    // Approved by the second operator: the deposit leaves review for Delivering, the condition
    // cleared, and the mint is ordered once, for the output's amount, under the reference derived
    // from the output.
    approve_as(&w, 1, action).await.unwrap();
    w.settle(t0()).await;
    let (_, payload) = order(&w.admin, id, 1).await.expect("the mint order");
    assert_eq!(
        payload,
        json!({"order": {"asset": ISSUING_ASSET, "to": DESTINATION_ACCOUNT}, "amount": "4990"})
    );
    let source = bezent_core_service::refs::Coordinate {
        network: ORIGIN.into(),
        block_number: 20,
        block_hash: hash(ORIGIN, 20),
        position_kind: "log".into(),
        position_index: 2,
    };
    assert_eq!(
        reference_of(&w.admin, id).await,
        bezent_core_service::refs::mint_reference(&source).to_vec()
    );
    assert_eq!(
        w.count("SELECT count(*) FROM intents WHERE payload ? 'order'")
            .await,
        1
    );

    // Prepared and signed: the deposit leaves review for Delivering, the condition cleared, on the
    // recorded action (REQ-RV-2), and the order is still the only one.
    let now = t0();
    w.cycle(now).await;
    let (o, _) = order(&w.admin, id, 1).await.unwrap();
    prepare(&w.admin, o, None).await;
    w.cycle(now).await;
    let (s, _) = signing_intent(&w.admin, id, 1)
        .await
        .expect("a signing intent");
    sign(&w, s).await;
    w.cycle(now).await;
    assert_eq!(w.state(id).await, ("Delivering".into(), None));
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM transitions WHERE transfer_id = {id} \
               AND from_state = 'Needs-Review' AND to_state = 'Delivering' \
               AND manual_action_id = {action}"
        ))
        .await,
        1
    );

    // Minted, included and final: Completed, with one order and one mint, and no way back into
    // review on the arrival's old deadline.
    let reference = reference_of(&w.admin, id).await;
    let minted = observe(
        &w.admin,
        ISSUING,
        40,
        0,
        ISSUING_ASSET,
        4_990,
        None,
        Some(DESTINATION_ACCOUNT),
        Some(&reference),
        true,
        json!({"event": "Minted"}),
    )
    .await;
    w.cycle(now).await;
    assert_eq!(w.state(id).await.0, "Delivered");
    decide(&w.admin, minted, "final", 45).await;
    w.settle(now).await;
    assert_eq!(w.state(id).await, ("Completed".into(), None));
    assert_eq!(
        w.count("SELECT count(*) FROM intents WHERE payload ? 'order'")
            .await,
        1
    );
    assert_eq!(
        w.count("SELECT count(*) FROM transfers WHERE kind = 'refund'")
            .await,
        0
    );
}

/// `REQ-LM-1`, `REQ-LM-2`, `REQ-CV-2`, amendment 0018 Open 3 (ruled `d-cv-open-choices` (b)): the
/// output's amount is checked against the deposit route's limits, not only the convertible arrival's
/// at the deposit request, with the route's own errors and before anything is recorded. Nothing is
/// matched by amount: the output is the one the request names. The route here takes 4 000 to 6 000
/// per transfer and 9 000 per window; another deposit of 4 000 is in the window beside the awaiting
/// 5 000. An output of 3 999 is below the minimum, 6 001 above the maximum, and 5 001 would take the
/// window to 9 001; 5 000 passes, which also shows the deposit's own arrival counted once, not twice
/// (4 000 + 5 000 + 5 000 would be past the window).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a5_5_req_lm_1_req_lm_2_req_cv_2_the_output_is_checked_against_the_routes_limits(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (router, id) = awaiting(&w).await;
    let beside = observe(
        &w.admin,
        ORIGIN,
        30,
        0,
        TOKEN,
        4_000,
        Some(FUNDER),
        Some(RESERVE),
        None,
        true,
        json!({}),
    )
    .await;
    decide(&w.admin, beside, "final", 31).await;
    let (_, answer) = deposit_as(&router, CREDENTIAL, "beside", &coordinate(30, 0)).await;
    assert_eq!(answer["state"], "Accepted", "{answer}");
    exec(
        &w.admin,
        &format!(
            "UPDATE routes SET minimum = 4000, maximum = 6000, window_limit = 9000 \
             WHERE asset = '{ASSET}' AND direction = 'deposit'"
        ),
    )
    .await;

    for (block, amount, error) in [
        (40, 3_999, "ERR-BelowMinimum"),
        (42, 6_001, "ERR-AboveMaximum"),
        (44, 5_001, "ERR-WindowExceeded"),
    ] {
        let (_, output) = swap(&w.admin, block, 5_000, amount).await;
        decide(&w.admin, output, "final", block + 1).await;
        let (status, answer) = conversion_as(
            &router,
            CREDENTIAL,
            &format!("conv-{block}"),
            "dep",
            &coordinate(block, 2),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        assert_eq!(answer["error"], error, "{answer}");
        // Nothing recorded and nothing consumed: the deposit still awaits, on its arrival.
        assert_eq!(w.count("SELECT count(*) FROM deposit_conversions").await, 0);
        assert_eq!(bound_to(&w, id).await, (10, 0, "5000".into()));
    }

    let (_, output) = swap(&w.admin, 46, 5_000, 5_000).await;
    decide(&w.admin, output, "final", 47).await;
    let (status, answer) =
        conversion_as(&router, CREDENTIAL, "conv-ok", "dep", &coordinate(46, 2)).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["state"], "Source-Final", "{answer}");
    assert_eq!(answer["amount"], "5000", "{answer}");
    assert_eq!(bound_to(&w, id).await, (46, 2, "5000".into()));
}

/// `REQ-CV-2`, `REQ-RF-1`, `REQ-RV-2`, amendment 0018 Open 6 (`d-cv-open-choices` (a)): an output is
/// not named for a deposit after a refund of it is recorded or started. A review resolution into
/// `Refunding` is recorded by an operator; until the core refuses its evidence (a deposit sent to
/// review from `Source-Final` has no edge to `Refunding`, so it is refused on the first cycle), naming
/// is refused (`ERR-SourceConsumed`), with nothing recorded and nothing minted. Once the core has
/// refused it, it starts no refund, and the output is named.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a5_4_req_cv_2_req_rf_1_req_rv_2_an_output_is_not_named_after_a_refund_is_started(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (router, id) = in_review(&w).await;
    let (_, output) = swap(&w.admin, 20, 5_000, 4_990).await;
    decide(&w.admin, output, "final", 22).await;

    let refund = resolution(&w, id, "Refunding", 7_000, 0).await;
    let (status, answer) =
        conversion_as(&router, CREDENTIAL, "conv", "dep", &coordinate(20, 2)).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["error"], "ERR-SourceConsumed", "{answer}");
    assert_eq!(w.count("SELECT count(*) FROM deposit_conversions").await, 0);
    assert_eq!(bound_to(&w, id).await, (10, 0, "5000".into()));
    assert_eq!(w.state(id).await.0, "Needs-Review");

    // The core refuses that evidence (there is no such edge): no refund started, so nothing stops
    // the output being named, and a new request for it is not held up by the refused one.
    w.settle(t0()).await;
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM event_log WHERE kind = 'review-evidence-refused' \
               AND payload ->> 'manual_action_id' = '{refund}'"
        ))
        .await,
        1
    );
    assert_eq!(
        w.count("SELECT count(*) FROM transfers WHERE kind = 'refund'")
            .await,
        0
    );
    let (_, answer) = conversion_as(&router, CREDENTIAL, "conv-2", "dep", &coordinate(20, 2)).await;
    assert_eq!(answer["amount"], "4990", "{answer}");
    assert_eq!(bound_to(&w, id).await, (20, 2, "4990".into()));
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
}

/// `REQ-CV-6`: a conversion completes by its deadline when its output's block is at or before it.
/// An output from a block past the deadline, named while the issuing network's final head has not
/// passed the deadline yet, does not clear the condition; once the head passes it, the deposit goes
/// to review with the output as its source, and nothing is minted or refunded.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_cv_6_an_output_after_the_deadline_does_not_complete_the_conversion(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let (router, id) = awaiting(&w).await;
    let late = 14 + BUDGET + 10;
    let (_, output) = swap(&w.admin, late, 5_000, 4_990).await;
    decide(&w.admin, output, "final", late + 2).await;
    let (_, answer) = conversion_as(&router, CREDENTIAL, "conv", "dep", &coordinate(late, 2)).await;
    assert_eq!(answer["state"], "Source-Final", "{answer}");
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-AwaitingConversion".into()))
    );
    head(&w.admin, ISSUING, 14 + BUDGET + 1).await;
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Needs-Review".into(), Some("CND-AwaitingConversion".into()))
    );
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
    assert_eq!(
        w.count("SELECT count(*) FROM transfers WHERE kind = 'refund'")
            .await,
        0
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

/// `AC-A5.2`, `REQ-CV-2`, `REQ-CV-5`, `INV-6`: an output named directly by a deposit request (the
/// path that existed before amendment 0018, which the ruling did not remove) is bound once final
/// and minted for exactly what the swap delivered; the swap's input is refused. Such a deposit
/// records no convertible arrival; the convertible arrival nobody named stays unassociated. Since
/// amendment 0026 the output binds only for a requester that declared its sender, the venue's pool
/// (`REQ-PN-3`; whether the direct path stays is amendment 0018's Open 2): refused until then.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a5_2_req_cv_2_req_cv_5_inv_6_an_output_named_directly_is_bound_and_minted(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let router = ingress(&w).await;
    let arrival = convertible_arrival(&w.admin, 10, 5_000).await;
    let (input, output) = swap(&w.admin, 12, 5_000, 4_990).await;

    // Nothing is final: the output is refused, nothing recorded, the reference not consumed (INV-6).
    let (status, answer) = request_deposit(&router, "r-out", &coordinate(12, 2)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{answer}");
    assert_eq!(w.count("SELECT count(*) FROM transfers").await, 0);

    decide(&w.admin, arrival, "final", 14).await;
    decide(&w.admin, input, "final", 14).await;
    let (_, answer) = request_deposit(&router, "r-in", &coordinate(12, 1)).await;
    assert_eq!(answer["error"], "ERR-SourceNotFound", "{answer}");
    // The convertible arrival no deposit names is held as an unassociated arrival (REQ-CV-1,
    // REQ-IN-6): not absorbed, not dropped, not minted.
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM unassociated_arrivals WHERE observation_id = {arrival}"
        ))
        .await,
        1
    );

    decide(&w.admin, output, "final", 14).await;
    let (_, answer) = request_deposit(&router, "r-out-undeclared", &coordinate(12, 2)).await;
    assert_eq!(answer["error"], "ERR-SenderNotDeclared", "{answer}");
    declare_sender(&w, REQUESTER, POOL).await;
    let (status, answer) = request_deposit(&router, "r-out", &coordinate(12, 2)).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["state"], "Accepted", "{answer}");
    assert!(answer.get("converted_arrival").is_none(), "{answer}");
    let id = transfer_of(&w, &answer).await;
    assert_eq!(bound_to(&w, id).await, (12, 2, "4990".into()));

    w.cycle(t0()).await;
    assert_eq!(w.state(id).await, ("Source-Final".into(), None));
    w.cycle(t0()).await;
    let (_, payload) = order(&w.admin, id, 1).await.expect("the mint order");
    assert_eq!(
        payload,
        json!({"order": {"asset": ISSUING_ASSET, "to": DESTINATION_ACCOUNT}, "amount": "4990"})
    );
    // The coordinate is consumed once (INV-2).
    let (_, answer) = request_deposit(&router, "r-again", &coordinate(12, 2)).await;
    assert_eq!(answer["error"], "ERR-SourceConsumed", "{answer}");
}
