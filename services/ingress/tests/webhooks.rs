// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Webhooks to requesters (issue #2): registration, signing, at-least-once delivery with a
//! dead-letter, conditions once per (condition, route), and scoping (`REQ-ST-5`, `REQ-IC-5`,
//! `REQ-ID-4`; PLAN `DEC-15`).
//!
//! The dispatcher reads the sequenced log, so the tests append what the core would: in-place holds
//! as transitions (written as the owner, as the core's `SECURITY DEFINER` path writes them), and a
//! local receiver stands for the requester.

mod common;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use bezent_ingress::webhooks::{
    DEDUPE_HEADER, Dispatcher, SIGNATURE_HEADER, SealingKey, TIMESTAMP_HEADER, Webhooks, signature,
};
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What the receiver saw: path with query, headers, body.
type Seen = Arc<Mutex<Vec<(String, HeaderMap, String)>>>;

/// A local requester endpoint: records every POST and answers with the next queued status, or
/// `default` when the queue is empty.
struct Receiver {
    base: String,
    seen: Seen,
    answers: Arc<Mutex<VecDeque<u16>>>,
}

#[derive(Clone)]
struct ReceiverState {
    seen: Seen,
    answers: Arc<Mutex<VecDeque<u16>>>,
    default: u16,
}

async fn receive(State(s): State<ReceiverState>, request: Request) -> StatusCode {
    let path = request
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_default();
    let headers = request.headers().clone();
    let bytes = axum::body::to_bytes(request.into_body(), 1 << 20)
        .await
        .unwrap();
    s.seen
        .lock()
        .unwrap()
        .push((path, headers, String::from_utf8(bytes.to_vec()).unwrap()));
    let code = s.answers.lock().unwrap().pop_front().unwrap_or(s.default);
    StatusCode::from_u16(code).unwrap()
}

async fn receiver(default: u16) -> Receiver {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let state = ReceiverState {
        seen: Arc::default(),
        answers: Arc::default(),
        default,
    };
    let (seen, answers) = (state.seen.clone(), state.answers.clone());
    let app = Router::new()
        .route("/{*path}", post(receive))
        .with_state(state);
    tokio::spawn(async move { axum::serve(listener, app).await });
    Receiver {
        base,
        seen,
        answers,
    }
}

impl Receiver {
    fn seen(&self) -> Vec<(String, HeaderMap, String)> {
        self.seen.lock().unwrap().clone()
    }
    fn answer(&self, codes: &[u16]) {
        self.answers.lock().unwrap().extend(codes);
    }
}

/// Webhook settings for tests: plain http to the loopback receiver, the harness's only allowlisted
/// host, no back-off, three attempts.
fn hooks(key: &SealingKey) -> Webhooks {
    let mut w = Webhooks::new(key.clone());
    w.allow_hosts = vec!["127.0.0.1".into()];
    w.max_attempts = 3;
    w.backoff_base = Duration::ZERO;
    w.timeout = Duration::from_secs(5);
    w
}

struct Harness {
    ingress: Ingress,
    dispatcher: Dispatcher,
}

async fn harness(pool: &PgPool) -> Harness {
    let key = SealingKey::random();
    let ingress = Ingress::with_webhooks(pool, hooks(&key)).await;
    let dispatcher = Dispatcher::new(dispatcher_pool(pool).await, hooks(&key)).unwrap();
    Harness {
        ingress,
        dispatcher,
    }
}

/// A deposit by `credential` at a fresh arrival in `block`; returns its bridge reference.
async fn deposit(
    h: &Harness,
    pool: &PgPool,
    credential: &str,
    reference: &str,
    block: i64,
) -> String {
    let (status, answer) = h
        .ingress
        .post(
            "/v0/deposits",
            credential,
            &deposit_body(reference, arrival(pool, block, 1_000).await),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    answer["bridge_reference"].as_str().unwrap().to_owned()
}

async fn transfer_id(pool: &PgPool, bridge: &str) -> i64 {
    scalar(
        pool,
        &format!("SELECT id FROM transfers WHERE bridge_reference = '{bridge}'"),
    )
    .await
}

/// Holds (or with `None` releases) the transfer in place, as the core's `hold` does; returns the
/// log position of its entry.
async fn hold(pool: &PgPool, transfer: i64, condition: Option<&str>) -> i64 {
    sqlx::query(
        "INSERT INTO transitions (transfer_id, from_state, to_state, condition) \
         SELECT id, state, state, $2::text::transfer_condition FROM transfers WHERE id = $1",
    )
    .bind(transfer)
    .bind(condition)
    .execute(pool)
    .await
    .expect("hold");
    scalar(pool, "SELECT seq FROM event_log_head").await
}

async fn secret(h: &Harness, credential: &str) -> Vec<u8> {
    let (status, body) = h
        .ingress
        .post("/v0/webhooks/secret", credential, &json!({}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    hex::decode(body["secret"].as_str().unwrap()).unwrap()
}

fn verify(secret: &[u8], headers: &HeaderMap, body: &str) {
    let t: i64 = headers[TIMESTAMP_HEADER].to_str().unwrap().parse().unwrap();
    assert!((chrono::Utc::now().timestamp() - t).abs() < 60);
    assert_eq!(
        headers[SIGNATURE_HEADER].to_str().unwrap(),
        signature(secret, t, body),
        "the signature verifies under the requester's secret"
    );
}

fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_5_a_notification_is_signed_points_at_the_log_and_is_retried_until_acknowledged(
    pool: PgPool,
) {
    let h = harness(&pool).await;
    let rx = receiver(200).await;
    rx.answer(&[500]);
    let secret = secret(&h, ALPHA).await;
    let bridge = deposit(&h, &pool, ALPHA, "ref-1", 1).await;
    let url = format!("{}/cb/ref-1?access_token=tok-0123456789", rx.base);
    let (status, reg) = h
        .ingress
        .put(
            "/v0/webhooks/transfers/by-request/ref-1",
            ALPHA,
            &json!({ "url": url }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{reg}");
    assert_eq!(reg["bridge_reference"], bridge);

    // A change of the transfer's that is not a shared gate: notified to the transfer's URL.
    let seq = hold(
        &pool,
        transfer_id(&pool, &bridge).await,
        Some("CND-AmountMismatch"),
    )
    .await;

    let first = h.dispatcher.step().await.unwrap();
    assert_eq!((first.enqueued, first.retrying, first.delivered), (1, 1, 0));
    let second = h.dispatcher.step().await.unwrap();
    assert_eq!((second.enqueued, second.delivered), (0, 1));

    let seen = rx.seen();
    assert_eq!(seen.len(), 2, "sent until acknowledged (at-least-once)");
    let (path, headers, body) = &seen[1];
    assert_eq!(path, "/cb/ref-1?access_token=tok-0123456789");
    assert_eq!(&seen[0].2, body, "the same bytes on every attempt");
    verify(&secret, headers, body);
    verify(&secret, &seen[0].1, &seen[0].2);

    // Only where to look and how to recognise it: never an outcome.
    let notice: Value = serde_json::from_str(body).unwrap();
    assert_eq!(
        keys(&notice),
        [
            "bridge_reference",
            "dedupe_key",
            "request_reference",
            "seq",
            "type"
        ]
    );
    assert_eq!(notice["type"], "transfer");
    assert_eq!(notice["seq"], seq);
    assert_eq!(notice["dedupe_key"], format!("log-{seq}"));
    assert_eq!(
        headers[DEDUPE_HEADER].to_str().unwrap(),
        format!("log-{seq}")
    );
    assert_eq!(notice["bridge_reference"], bridge);
    assert_eq!(notice["request_reference"], "ref-1");

    // The log is the source of truth: the position the notice names is readable there.
    let (_, page) = h
        .ingress
        .get(&format!("/v0/log?after={}", seq - 1), ALPHA)
        .await;
    assert_eq!(page["entries"][0]["seq"], seq);
    assert_eq!(
        page["entries"][0]["payload"]["condition"],
        "CND-AmountMismatch"
    );

    // Observable, and the URL, which carries a token, is sealed at rest.
    let (_, list) = h.ingress.get("/v0/webhooks/deliveries", ALPHA).await;
    assert_eq!(list["deliveries"][0]["state"], "delivered");
    assert_eq!(list["deliveries"][0]["attempts"], 2);
    assert_eq!(list["deliveries"][0]["last_status"], 200);
    assert!(list["deliveries"][0].get("url").is_none());
    let at_rest: Vec<Vec<u8>> = sqlx::query_scalar(
        "SELECT url_sealed FROM webhook_transfer_urls UNION ALL SELECT sealed FROM webhook_secrets",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    for bytes in at_rest {
        assert!(!bytes.windows(9).any(|w| w == b"tok-01234"));
        assert!(!bytes.windows(secret.len()).any(|w| w == secret.as_slice()));
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_5_a_delivery_is_dead_lettered_after_the_bound_and_the_log_still_has_it(
    pool: PgPool,
) {
    let h = harness(&pool).await;
    let rx = receiver(503).await;
    secret(&h, ALPHA).await;
    let bridge = deposit(&h, &pool, ALPHA, "ref-1", 1).await;
    h.ingress
        .put(
            &format!("/v0/webhooks/transfers/by-bridge/{bridge}"),
            ALPHA,
            &json!({ "url": format!("{}/cb", rx.base) }),
        )
        .await;
    let seq = hold(
        &pool,
        transfer_id(&pool, &bridge).await,
        Some("CND-AmountMismatch"),
    )
    .await;

    for _ in 0..2 {
        assert_eq!(h.dispatcher.step().await.unwrap().retrying, 1);
    }
    assert_eq!(h.dispatcher.step().await.unwrap().dead, 1);
    assert_eq!(h.dispatcher.step().await.unwrap(), Default::default());
    assert_eq!(rx.seen().len(), 3, "no attempt after the bound");

    let (status, dead) = h
        .ingress
        .get("/v0/webhooks/deliveries?state=dead", ALPHA)
        .await;
    assert_eq!(status, StatusCode::OK);
    let d = &dead["deliveries"][0];
    assert_eq!(d["seq"], seq);
    assert_eq!(d["attempts"], 3);
    assert_eq!(d["last_status"], 503);
    assert!(d["finished_at"].is_string());
    let (_, pending) = h
        .ingress
        .get("/v0/webhooks/deliveries?state=pending", ALPHA)
        .await;
    assert_eq!(pending["deliveries"], json!([]));

    // No requester needs one to arrive: the log still has the entry.
    let (_, page) = h
        .ingress
        .get(&format!("/v0/log?after={}", seq - 1), ALPHA)
        .await;
    assert_eq!(page["entries"][0]["seq"], seq);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_5_nothing_is_sent_unsigned(pool: PgPool) {
    let h = harness(&pool).await;
    let rx = receiver(200).await;
    let bridge = deposit(&h, &pool, ALPHA, "ref-1", 1).await;
    h.ingress
        .put(
            "/v0/webhooks/transfers/by-request/ref-1",
            ALPHA,
            &json!({ "url": format!("{}/cb", rx.base) }),
        )
        .await;
    hold(
        &pool,
        transfer_id(&pool, &bridge).await,
        Some("CND-AmountMismatch"),
    )
    .await;

    assert_eq!(h.dispatcher.step().await.unwrap().retrying, 1);
    assert!(rx.seen().is_empty(), "no secret, nothing sent");
    let (_, list) = h.ingress.get("/v0/webhooks/deliveries", ALPHA).await;
    assert_eq!(list["deliveries"][0]["last_error"], "no-secret");

    let secret = secret(&h, ALPHA).await;
    assert_eq!(h.dispatcher.step().await.unwrap().delivered, 1);
    let seen = rx.seen();
    verify(&secret, &seen[0].1, &seen[0].2);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_5_an_entry_is_fanned_out_once_and_delivered_once_by_concurrent_dispatchers(
    pool: PgPool,
) {
    let h = harness(&pool).await;
    let rx = receiver(200).await;
    secret(&h, ALPHA).await;
    let bridge = deposit(&h, &pool, ALPHA, "ref-1", 1).await;
    h.ingress
        .put(
            "/v0/webhooks/transfers/by-request/ref-1",
            ALPHA,
            &json!({ "url": format!("{}/cb", rx.base) }),
        )
        .await;
    let id = transfer_id(&pool, &bridge).await;
    for _ in 0..5 {
        hold(&pool, id, Some("CND-AmountMismatch")).await;
        hold(&pool, id, None).await;
    }

    // Fan-out alone, twice and concurrently: each entry once.
    let (a, b) = tokio::join!(h.dispatcher.step(), h.dispatcher.step());
    assert_eq!(a.unwrap().enqueued + b.unwrap().enqueued, 10);
    assert_eq!(count(&pool, "webhook_deliveries").await, 10);
    while h.dispatcher.step().await.unwrap() != Default::default() {}
    assert_eq!(rx.seen().len(), 10, "each key delivered once");
    let mut keys: Vec<String> = rx
        .seen()
        .iter()
        .map(|(_, h, _)| h[DEDUPE_HEADER].to_str().unwrap().to_owned())
        .collect();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), 10);

    // The cursor never moves back, so nothing is fanned out again by a rewind; and only the
    // dispatcher moves it at all.
    assert!(
        sqlx::query("UPDATE webhook_cursor SET after = after + 1")
            .execute(&service_pool(&pool).await)
            .await
            .is_err()
    );
    let err = sqlx::query("UPDATE webhook_cursor SET after = 0")
        .execute(&dispatcher_pool(&pool).await)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("only moves forward"), "{err}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_5_no_requester_needs_a_notification(pool: PgPool) {
    let h = harness(&pool).await;
    let bridge = deposit(&h, &pool, ALPHA, "ref-1", 1).await;
    let seq = hold(&pool, transfer_id(&pool, &bridge).await, Some("CND-Paused")).await;
    let step = h.dispatcher.step().await.unwrap();
    assert_eq!(step.enqueued, 0, "nothing registered, nothing to send");
    assert!(step.fanned_out >= 1);
    assert_eq!(count(&pool, "webhook_deliveries").await, 0);
    let (_, page) = h
        .ingress
        .get(&format!("/v0/log?after={}", seq - 1), ALPHA)
        .await;
    assert_eq!(page["entries"][0]["seq"], seq);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ic_5_a_condition_is_notified_once_per_route_not_once_per_transfer(pool: PgPool) {
    let h = harness(&pool).await;
    let rx = receiver(200).await;
    let secret = secret(&h, ALPHA).await;
    let (status, _) = h
        .ingress
        .put(
            "/v0/webhooks/conditions",
            ALPHA,
            &json!({ "url": format!("{}/conditions", rx.base) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let mut ids = Vec::new();
    for n in 1..=3 {
        let bridge = deposit(&h, &pool, ALPHA, &format!("ref-{n}"), n).await;
        h.ingress
            .put(
                &format!("/v0/webhooks/transfers/by-bridge/{bridge}"),
                ALPHA,
                &json!({ "url": format!("{}/t/{n}", rx.base) }),
            )
            .await;
        ids.push(transfer_id(&pool, &bridge).await);
    }

    // The route is paused: every transfer on it is held in place.
    let first = hold(&pool, ids[0], Some("CND-Paused")).await;
    hold(&pool, ids[1], Some("CND-Paused")).await;
    hold(&pool, ids[2], Some("CND-Paused")).await;
    h.dispatcher.step().await.unwrap();
    let seen = rx.seen();
    assert_eq!(
        seen.len(),
        1,
        "one notice for the condition, none per transfer"
    );
    let (path, headers, body) = &seen[0];
    assert_eq!(path, "/conditions");
    verify(&secret, headers, body);
    let notice: Value = serde_json::from_str(body).unwrap();
    assert_eq!(
        notice,
        json!({
            "type": "condition",
            "seq": first,
            "dedupe_key": format!("log-{first}-held-CND-Paused"),
            "condition": "CND-Paused",
            "route": { "asset": ASSET, "network": ORIGIN, "kind": "deposit" },
        })
    );

    // Released one by one: one notice when the last is released.
    hold(&pool, ids[0], None).await;
    hold(&pool, ids[1], None).await;
    h.dispatcher.step().await.unwrap();
    assert_eq!(rx.seen().len(), 1);
    let last = hold(&pool, ids[2], None).await;
    h.dispatcher.step().await.unwrap();
    let seen = rx.seen();
    assert_eq!(seen.len(), 2);
    let notice: Value = serde_json::from_str(&seen[1].2).unwrap();
    assert_eq!(
        notice["dedupe_key"],
        format!("log-{last}-released-CND-Paused")
    );
    assert_eq!(notice["seq"], last);

    // Paused again later: a new episode, notified again.
    let again = hold(&pool, ids[1], Some("CND-Paused")).await;
    h.dispatcher.step().await.unwrap();
    let notice: Value = serde_json::from_str(&rx.seen()[2].2).unwrap();
    assert_eq!(notice["dedupe_key"], format!("log-{again}-held-CND-Paused"));

    // A condition of one transfer's own is that transfer's news, not a shared condition.
    hold(&pool, ids[0], Some("CND-AmountMismatch")).await;
    h.dispatcher.step().await.unwrap();
    let seen = rx.seen();
    assert_eq!(seen.len(), 4);
    assert_eq!(seen[3].0, "/t/1");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ic_5_a_condition_reaches_every_requester_granted_on_the_route_with_or_without_a_transfer(
    pool: PgPool,
) {
    let h = harness(&pool).await;
    let alpha = receiver(200).await;
    let beta = receiver(200).await;
    let gamma = receiver(200).await;
    let beta_secret = secret(&h, BETA).await;
    secret(&h, ALPHA).await;
    secret(&h, GAMMA).await;
    for (who, rx) in [(ALPHA, &alpha), (BETA, &beta), (GAMMA, &gamma)] {
        h.ingress
            .put(
                "/v0/webhooks/conditions",
                who,
                &json!({ "url": format!("{}/conditions", rx.base) }),
            )
            .await;
    }
    // Only alpha has transfers on the route; beta is granted deposits and has none; gamma has
    // no grant at all.
    let mut ids = Vec::new();
    for n in 1..=2 {
        let bridge = deposit(&h, &pool, ALPHA, &format!("ref-{n}"), n).await;
        ids.push(transfer_id(&pool, &bridge).await);
    }

    // Starts: once, to every requester granted on the route.
    let first = hold(&pool, ids[0], Some("CND-Paused")).await;
    hold(&pool, ids[1], Some("CND-Paused")).await;
    h.dispatcher.step().await.unwrap();
    assert_eq!(alpha.seen().len(), 1);
    assert_eq!(beta.seen().len(), 1);
    assert_eq!(gamma.seen().len(), 0, "no grant on the route, no notice");
    let (_, headers, body) = &beta.seen()[0];
    verify(&beta_secret, headers, body);
    let notice: Value = serde_json::from_str(body).unwrap();
    assert_eq!(notice["type"], "condition");
    assert_eq!(notice["dedupe_key"], format!("log-{first}-held-CND-Paused"));
    assert_eq!(
        notice["route"],
        json!({ "asset": ASSET, "network": ORIGIN, "kind": "deposit" })
    );

    // Clears: once, when the last is released.
    hold(&pool, ids[0], None).await;
    h.dispatcher.step().await.unwrap();
    assert_eq!(beta.seen().len(), 1);
    let last = hold(&pool, ids[1], None).await;
    h.dispatcher.step().await.unwrap();
    assert_eq!(alpha.seen().len(), 2);
    assert_eq!(beta.seen().len(), 2);
    let notice: Value = serde_json::from_str(&beta.seen()[1].2).unwrap();
    assert_eq!(
        notice["dedupe_key"],
        format!("log-{last}-released-CND-Paused")
    );

    // The core's reserve-short condition names no transfer, only a route of withdrawals: it
    // reaches the requesters granted withdrawals (alpha), and not beta (deposits only).
    for kind in ["condition-raised", "condition-cleared"] {
        sqlx::query("INSERT INTO event_log (kind, transfer_id, payload) VALUES ($1, NULL, $2)")
            .bind(kind)
            .bind(json!({ "condition": "CND-ReserveShort", "asset": ASSET, "network": ORIGIN }))
            .execute(&pool)
            .await
            .unwrap();
        h.dispatcher.step().await.unwrap();
    }
    let seen = alpha.seen();
    assert_eq!(seen.len(), 4);
    let raised: Value = serde_json::from_str(&seen[2].2).unwrap();
    let cleared: Value = serde_json::from_str(&seen[3].2).unwrap();
    assert!(
        raised["dedupe_key"]
            .as_str()
            .unwrap()
            .ends_with("-held-CND-ReserveShort")
    );
    assert!(
        cleared["dedupe_key"]
            .as_str()
            .unwrap()
            .ends_with("-released-CND-ReserveShort")
    );
    assert_eq!(raised["route"]["kind"], "withdrawal");
    assert_eq!(beta.seen().len(), 2);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_id_4_a_requester_registers_only_for_transfers_its_own_requests_made(pool: PgPool) {
    let h = harness(&pool).await;
    let bridge = deposit(&h, &pool, ALPHA, "ref-1", 1).await;
    let body = json!({ "url": "https://beta.example/cb" });

    // Knowing alpha's references is not enough.
    let (status, err) = h
        .ingress
        .put(
            &format!("/v0/webhooks/transfers/by-bridge/{bridge}"),
            BETA,
            &body,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{err}");
    let (status, _) = h
        .ingress
        .put("/v0/webhooks/transfers/by-request/ref-1", BETA, &body)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = h
        .ingress
        .put(
            &format!("/v0/webhooks/transfers/by-bridge/{bridge}"),
            GAMMA,
            &body,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = h
        .ingress
        .call("PUT", "/v0/webhooks/conditions", None, Some(&body))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The store refuses it too, whatever the service sends.
    let service = ingress_pool(&pool).await;
    let id = transfer_id(&pool, &bridge).await;
    let err = sqlx::query(
        "INSERT INTO webhook_transfer_urls \
           (requester, transfer_id, url_nonce, url_sealed, url_digest, after_seq) \
         VALUES ('beta', $1, '\\x000000000000000000000000', \
                 '\\x00000000000000000000000000000000ff', sha256('x'), 0)",
    )
    .bind(id)
    .execute(&service)
    .await
    .unwrap_err();
    assert!(err.to_string().contains("REQ-ID-4"), "{err}");

    // And one requester never sees another's deliveries.
    let rx = receiver(200).await;
    secret(&h, ALPHA).await;
    h.ingress
        .put(
            "/v0/webhooks/transfers/by-request/ref-1",
            ALPHA,
            &json!({ "url": format!("{}/cb", rx.base) }),
        )
        .await;
    hold(&pool, id, Some("CND-AmountMismatch")).await;
    h.dispatcher.step().await.unwrap();
    let (_, mine) = h.ingress.get("/v0/webhooks/deliveries", ALPHA).await;
    assert_eq!(mine["deliveries"].as_array().unwrap().len(), 1);
    let (_, theirs) = h.ingress.get("/v0/webhooks/deliveries", BETA).await;
    assert_eq!(theirs["deliveries"], json!([]));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_id_4_a_requester_may_replace_its_own_callback_url_and_each_change_is_recorded(
    pool: PgPool,
) {
    let h = harness(&pool).await;
    let rx = receiver(200).await;
    secret(&h, ALPHA).await;
    let bridge = deposit(&h, &pool, ALPHA, "ref-1", 1).await;
    let old = json!({ "url": format!("{}/old?access_token=cb_1", rx.base) });
    let (status, first) = h
        .ingress
        .put("/v0/webhooks/transfers/by-request/ref-1", ALPHA, &old)
        .await;
    assert_eq!(status, StatusCode::CREATED);
    // The same URL again, by either reference: nothing new is recorded.
    let (status, again) = h
        .ingress
        .put(
            &format!("/v0/webhooks/transfers/by-bridge/{bridge}"),
            ALPHA,
            &old,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again, first);
    assert_eq!(count(&pool, "webhook_transfer_urls").await, 1);

    // Another URL (Swaps issues a new callback token on each retry): it replaces the first.
    let new = json!({ "url": format!("{}/new?access_token=cb_2", rx.base) });
    let (status, replaced) = h
        .ingress
        .put("/v0/webhooks/transfers/by-request/ref-1", ALPHA, &new)
        .await;
    assert_eq!(status, StatusCode::OK, "{replaced}");
    assert_eq!(count(&pool, "webhook_transfer_urls").await, 2);

    // The change is recorded append-only: who, when, and the hashes of the new and old URL.
    let (actor, new_hash, old_hash, first_hash): (String, Vec<u8>, Vec<u8>, Vec<u8>) =
        sqlx::query_as(
            "SELECT n.requester, n.url_digest, n.previous_digest, o.url_digest \
             FROM webhook_transfer_urls n, webhook_transfer_urls o \
             WHERE n.previous_digest IS NOT NULL AND o.previous_digest IS NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(actor, "alpha");
    assert_eq!(old_hash, first_hash);
    assert_ne!(new_hash, old_hash);
    assert_eq!(new_hash.len(), 32);
    // A keyed digest (PR #55 review S5): not the plain SHA-256 of the URL, so a guess of the
    // token in it cannot be tested offline against the store.
    let plain: Vec<u8> = sqlx::query_scalar("SELECT sha256(convert_to($1, 'UTF8'))")
        .bind(format!("{}/old?access_token=cb_1", rx.base))
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_ne!(first_hash, plain);
    let service = service_pool(&pool).await;
    assert!(
        sqlx::query("UPDATE webhook_transfer_urls SET url_sealed = url_sealed")
            .execute(&service)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM webhook_transfer_urls")
            .execute(&service)
            .await
            .is_err()
    );

    // A delivery goes to the URL current when it is sent.
    let id = transfer_id(&pool, &bridge).await;
    hold(&pool, id, Some("CND-AmountMismatch")).await;
    h.dispatcher.step().await.unwrap();
    let seen = rx.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "/new?access_token=cb_2");

    // Replaced after the fan-out and before the retry: the retry goes to the latest.
    rx.answer(&[500]);
    hold(&pool, id, Some("CND-FeeShort")).await;
    h.dispatcher.step().await.unwrap();
    let newest = json!({ "url": format!("{}/newest", rx.base) });
    h.ingress
        .put("/v0/webhooks/transfers/by-request/ref-1", ALPHA, &newest)
        .await;
    h.dispatcher.step().await.unwrap();
    let seen = rx.seen();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[1].0, "/new?access_token=cb_2");
    assert_eq!(seen[2].0, "/newest");
    assert_eq!(seen[1].2, seen[2].2, "the same bytes");

    // Another requester still cannot set it, or see it (REQ-ID-4).
    let (status, _) = h
        .ingress
        .put(
            &format!("/v0/webhooks/transfers/by-bridge/{bridge}"),
            BETA,
            &json!({ "url": "https://elsewhere.example/cb" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(count(&pool, "webhook_transfer_urls").await, 3);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_5_callback_urls_are_https_and_never_internal_at_registration(pool: PgPool) {
    let key = SealingKey::random();
    let mut strict = hooks(&key);
    strict.allow_hosts.clear();
    let ingress = Ingress::with_webhooks(&pool, strict).await;
    ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("ref-1", arrival(&pool, 1, 1_000).await),
        )
        .await;
    let put = |url: &'static str| {
        let ingress = &ingress;
        async move {
            ingress
                .put(
                    "/v0/webhooks/transfers/by-request/ref-1",
                    ALPHA,
                    &json!({ "url": url }),
                )
                .await
        }
    };
    for bad in [
        "http://127.0.0.1:9/cb",
        "http://swaps.example/cb",
        "ftp://swaps.example/cb",
        "https://user:pw@swaps.example/cb",
        "https://swaps.example/cb#frag",
        "/relative",
        // PR #55 review M1: an IP literal is checked at registration already.
        "https://127.0.0.1/cb",
        "https://10.0.0.1/cb",
        "https://100.64.0.1/cb",
        "https://169.254.169.254/latest/meta-data",
        "https://169.254.170.2/v2/credentials",
        "https://[::1]/cb",
        "https://[fd00:ec2::254]/",
        "https://[::ffff:127.0.0.1]/cb",
        "https://[fe80::1]/cb",
        "https://0.0.0.0/cb",
    ] {
        let (status, err) = put(bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(err["error"], "ERR-Malformed");
    }
    let (status, _) = put("https://swaps.example/cb").await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = put("https://93.184.216.34/cb").await;
    assert_eq!(status, StatusCode::OK);

    // The harness's allowlisted host may take plain http and a loopback address; nothing else
    // may, an allowlisted metadata address is still refused, and bezent's own addresses are
    // refused even when allowlisted.
    let mut lax = hooks(&key);
    lax.allow_hosts = vec!["127.0.0.1".into(), "169.254.169.254".into()];
    lax.deny = vec![
        "127.0.0.1:8080".parse().unwrap(),
        "core.internal".parse().unwrap(),
    ];
    let app = bezent_ingress::App::new(
        ingress_pool(&pool).await,
        bezent_ingress::Limits {
            max_in_flight: 8,
            log_page: 50,
        },
    )
    .with_webhooks(lax);
    let router = bezent_ingress::router(app);
    let call = |url: &'static str| {
        let router = router.clone();
        async move {
            use tower::ServiceExt;
            let request = axum::http::Request::builder()
                .method("PUT")
                .uri("/v0/webhooks/conditions")
                .header("authorization", format!("Bearer {ALPHA}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(json!({ "url": url }).to_string()))
                .unwrap();
            router.oneshot(request).await.unwrap().status()
        }
    };
    assert_eq!(call("http://127.0.0.1:9/cb").await, StatusCode::OK);
    for bad in [
        "http://localhost:9/cb",
        "http://swaps.example/cb",
        "http://169.254.169.254/latest/meta-data",
        "http://127.0.0.1:8080/v0/deposits",
        "https://core.internal/x",
    ] {
        assert_eq!(call(bad).await, StatusCode::BAD_REQUEST, "{bad}");
    }

    // Without webhooks configured, the surface says so.
    let plain = Ingress::unseeded(&pool, 8).await;
    let (status, err) = plain.post("/v0/webhooks/secret", ALPHA, &json!({})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(err["error"], "webhooks-disabled");
}

/// PR #55 review M1: the send-time check is the guard. A name is resolved when the delivery is
/// attempted and every address it resolves to must be admitted; bezent's own addresses are
/// refused even when allowlisted; redirects are not followed; and the listing shows only a
/// coarse class, never an address.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_5_the_send_time_check_refuses_internal_addresses_and_shows_only_a_class(
    pool: PgPool,
) {
    let key = SealingKey::random();
    // Registration as lax as a test store allows; the dispatchers below differ.
    let mut lax = hooks(&key);
    lax.allow_hosts = vec!["127.0.0.1".into(), "localhost".into()];
    let ingress = Ingress::with_webhooks(&pool, lax.clone()).await;
    let h = Harness {
        ingress,
        dispatcher: Dispatcher::new(dispatcher_pool(&pool).await, lax.clone()).unwrap(),
    };
    let rx = receiver(200).await;
    secret(&h, ALPHA).await;
    let port = rx.base.rsplit(':').next().unwrap().to_owned();
    let bridge = deposit(&h, &pool, ALPHA, "ref-1", 1).await;
    let id = transfer_id(&pool, &bridge).await;
    let register = |url: String| {
        let h = &h;
        async move {
            let (status, body) = h
                .ingress
                .put(
                    "/v0/webhooks/transfers/by-request/ref-1",
                    ALPHA,
                    &json!({ "url": url }),
                )
                .await;
            assert!(status.is_success(), "{body}");
        }
    };
    let last = |pool: &PgPool| {
        let pool = pool.clone();
        async move {
            sqlx::query_as::<_, (String, Option<String>, Option<i32>)>(
                "SELECT state, last_error, last_status FROM webhook_deliveries \
                 ORDER BY id DESC LIMIT 1",
            )
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };

    // A name that resolves to loopback, on a dispatcher that does not allowlist it: refused at
    // send time, nothing reaches the receiver.
    let mut strict = lax.clone();
    strict.allow_hosts = vec!["127.0.0.1".into()];
    let guarded = Dispatcher::new(dispatcher_pool(&pool).await, strict).unwrap();
    register(format!("https://localhost:{port}/cb")).await;
    hold(&pool, id, Some("CND-AmountMismatch")).await;
    while guarded.step().await.unwrap() != Default::default() {}
    assert!(
        rx.seen().is_empty(),
        "nothing is sent to an address not admitted"
    );
    let (state, error, status) = last(&pool).await;
    assert_eq!(
        (state.as_str(), error.as_deref(), status),
        ("dead", Some("address-not-allowed"), None)
    );
    // Plain http to a host it does not allowlist: refused as a URL.
    register(format!("http://localhost:{port}/cb")).await;
    hold(&pool, id, Some("CND-FeeShort")).await;
    while guarded.step().await.unwrap() != Default::default() {}
    assert!(rx.seen().is_empty());
    let (_, error, _) = last(&pool).await;
    assert_eq!(error.as_deref(), Some("url-not-allowed"));

    // Allowlisted by name: resolved once, vetted, and the connection pinned to what was vetted.
    hold(&pool, id, Some("CND-AmountMismatch")).await;
    h.dispatcher.step().await.unwrap();
    assert_eq!(rx.seen().len(), 1);
    assert_eq!(rx.seen()[0].0, "/cb");

    // bezent's own address, allowlisted or not: refused.
    let mut own = lax.clone();
    own.deny = vec![format!("127.0.0.1:{port}").parse().unwrap()];
    let denying = Dispatcher::new(dispatcher_pool(&pool).await, own.clone()).unwrap();
    register(format!("http://127.0.0.1:{port}/own")).await;
    hold(&pool, id, Some("CND-FeeShort")).await;
    while denying.step().await.unwrap() != Default::default() {}
    assert_eq!(rx.seen().len(), 1);
    let (state, error, _) = last(&pool).await;
    assert_eq!(
        (state.as_str(), error.as_deref()),
        ("dead", Some("address-not-allowed"))
    );
    // By name too: `localhost` resolves to the denied address.
    register(format!("http://localhost:{port}/own")).await;
    hold(&pool, id, Some("CND-AmountMismatch")).await;
    while denying.step().await.unwrap() != Default::default() {}
    assert_eq!(rx.seen().len(), 1);
    let (_, error, _) = last(&pool).await;
    assert_eq!(error.as_deref(), Some("address-not-allowed"));

    // A redirect is answered, never followed.
    register(format!("http://127.0.0.1:{port}/moved")).await;
    rx.answer(&[307, 307, 307]);
    hold(&pool, id, Some("CND-FeeShort")).await;
    while h.dispatcher.step().await.unwrap() != Default::default() {}
    assert_eq!(rx.seen().len(), 4, "three attempts, each to the URL itself");
    assert!(rx.seen()[1..].iter().all(|(path, _, _)| path == "/moved"));
    let (_, error, status) = last(&pool).await;
    assert_eq!((error.as_deref(), status), (Some("http-status"), Some(307)));

    // S-d (PR #55 re-check): a name that does not resolve shows the same class as a refused
    // address, so the listing is no oracle for which internal names exist.
    register("https://no-such-host.invalid/cb".into()).await;
    hold(&pool, id, Some("CND-AmountMismatch")).await;
    while h.dispatcher.step().await.unwrap() != Default::default() {}
    let (state, error, status) = last(&pool).await;
    assert_eq!(
        (state.as_str(), error.as_deref(), status),
        ("dead", Some("address-not-allowed"), None)
    );

    // Nothing in the listing names an address or carries an error text.
    let (_, list) = h.ingress.get("/v0/webhooks/deliveries", ALPHA).await;
    let text = list.to_string();
    assert!(!text.contains("127.0.0.1") && !text.contains("::1"));
    for d in list["deliveries"].as_array().unwrap() {
        if let Some(e) = d["last_error"].as_str() {
            assert!(
                ["url-not-allowed", "address-not-allowed", "http-status"].contains(&e),
                "{e}"
            );
        }
    }
}

/// PR #55 review M1: a webhook's client resolves nothing itself; it connects only to the
/// addresses vetted and pinned for the attempt.
#[tokio::test]
async fn req_st_5_a_webhook_client_resolves_nothing_it_was_not_given() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let app = Router::new().route("/{*path}", post(|| async { StatusCode::OK }));
        axum::serve(listener, app).await
    });
    let w = hooks(&SealingKey::random());
    let url = format!("http://localhost:{port}/x");
    let err = w.client(None).unwrap().post(&url).send().await.unwrap_err();
    assert!(err.is_connect(), "{err}");
    let pinned = ["127.0.0.1:0".parse().unwrap()];
    let ok = w
        .client(Some(("localhost", &pinned)))
        .unwrap()
        .post(&url)
        .send()
        .await
        .unwrap();
    assert!(ok.status().is_success());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_5_no_function_of_0044_is_public_and_a_finished_delivery_never_changes(
    pool: PgPool,
) {
    let public: Vec<String> = sqlx::query_scalar(
        "SELECT p.oid::regprocedure::text FROM pg_proc p \
         WHERE p.proname IN ('webhook_transfer_urls_guard', 'webhook_cursor_guard', \
             'webhook_deliveries_guard', 'webhook_dispatcher_only') \
           AND has_function_privilege('public', p.oid, 'EXECUTE')",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(public.is_empty(), "PUBLIC may run {public:?}");

    let h = harness(&pool).await;
    let rx = receiver(200).await;
    secret(&h, ALPHA).await;
    let bridge = deposit(&h, &pool, ALPHA, "ref-1", 1).await;
    h.ingress
        .put(
            "/v0/webhooks/transfers/by-request/ref-1",
            ALPHA,
            &json!({ "url": format!("{}/cb", rx.base) }),
        )
        .await;
    hold(
        &pool,
        transfer_id(&pool, &bridge).await,
        Some("CND-AmountMismatch"),
    )
    .await;
    assert_eq!(h.dispatcher.step().await.unwrap().delivered, 1);

    // Not even the dispatcher's own role changes a finished delivery or what one points at.
    for pool in [service_pool(&pool).await, dispatcher_pool(&pool).await] {
        for sql in [
            "UPDATE webhook_deliveries SET state = 'pending', finished_at = NULL",
            "UPDATE webhook_deliveries SET attempts = attempts + 1",
            "UPDATE webhook_deliveries SET seq = seq + 1",
            "DELETE FROM webhook_deliveries",
            "DELETE FROM webhook_secrets",
        ] {
            assert!(sqlx::query(sql).execute(&pool).await.is_err(), "{sql}");
        }
    }
}

/// PR #55 review M2: a service login (the ingress, the core or an endpoint) can neither write a delivery nor read a sealed secret or URL; the store refuses a delivery from
/// any role but the dispatcher's whatever is granted; and a row the dispatcher did not derive
/// from the log is never signed and sent as it stands.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_5_only_the_dispatcher_writes_a_delivery_and_a_forged_one_is_never_sent(
    pool: PgPool,
) {
    let h = harness(&pool).await;
    let rx = receiver(200).await;
    secret(&h, ALPHA).await;
    secret(&h, BETA).await;
    h.ingress
        .put(
            "/v0/webhooks/conditions",
            ALPHA,
            &json!({ "url": format!("{}/conditions", rx.base) }),
        )
        .await;
    let alpha_bridge = deposit(&h, &pool, ALPHA, "ref-a", 1).await;
    h.ingress
        .put(
            "/v0/webhooks/transfers/by-request/ref-a",
            ALPHA,
            &json!({ "url": format!("{}/a?access_token=cb_a", rx.base) }),
        )
        .await;
    let beta_bridge = deposit(&h, &pool, BETA, "ref-b", 2).await;
    let (alpha_id, beta_id) = (
        transfer_id(&pool, &alpha_bridge).await,
        transfer_id(&pool, &beta_bridge).await,
    );
    let own = hold(&pool, alpha_id, Some("CND-FeeShort")).await;
    let theirs = hold(&pool, beta_id, Some("CND-FeeShort")).await;

    // The ingress's login and an endpoint's cannot write a delivery, or read a sealed column.
    let insert = |requester: &str, seq: i64, transfer: i64| {
        format!(
            "INSERT INTO webhook_deliveries (requester, seq, dedupe_key, target, transfer_id) \
             VALUES ('{requester}', {seq}, 'log-{seq}', 'transfer', {transfer})"
        )
    };
    for role in [
        bezent_store::INGRESS_ROLE,
        bezent_store::CORE_ROLE,
        bezent_store::KREIVO_ENDPOINT_ROLE,
        bezent_store::EVM_ENDPOINT_ROLE,
    ] {
        let login = role_pool(&pool, role).await;
        for sql in [
            insert("alpha", theirs, beta_id),
            "UPDATE webhook_deliveries SET state = 'dead', finished_at = now()".into(),
            format!(
                "INSERT INTO webhook_condition_holds VALUES ({alpha_id}, 'CND-Paused', 'x/y/deposit', 1)"
            ),
            "SELECT sealed FROM webhook_secrets".into(),
            "SELECT url_sealed FROM webhook_transfer_urls".into(),
            "SELECT url_sealed FROM webhook_condition_urls".into(),
        ] {
            assert!(
                sqlx::query(&sql).execute(&login).await.is_err(),
                "{role}: {sql}"
            );
        }
    }
    // Whatever is granted, the store refuses a delivery from any login but the dispatcher's
    // (checked on real logins in the store's `webhook_roles` tests).

    // A row carries no body and no URL to sign: only what it points at, and its key is the one
    // derived from that.
    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name::text FROM information_schema.columns \
         WHERE table_name = 'webhook_deliveries'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    for gone in ["body", "url_nonce", "url_sealed"] {
        assert!(!columns.contains(&gone.to_owned()), "{gone}");
    }
    assert!(
        sqlx::query(&format!(
            "INSERT INTO webhook_deliveries (requester, seq, dedupe_key, target, transfer_id) \
             VALUES ('alpha', {own}, 'log-{own}-forged', 'transfer', {alpha_id})"
        ))
        .execute(&pool)
        .await
        .is_err()
    );

    // Rows the dispatcher did not derive from the log (written here as the owner, who may write
    // anything): a condition the entry does not show, alpha pointed at beta's transfer, beta at
    // alpha's. None is sent; each ends dead, refused.
    for sql in [
        format!(
            "INSERT INTO webhook_deliveries (requester, seq, dedupe_key, target, condition, change) \
             VALUES ('alpha', {own}, 'log-{own}-held-CND-Paused', 'condition', 'CND-Paused', 'held')"
        ),
        insert("alpha", theirs, beta_id),
        insert("beta", own, alpha_id),
    ] {
        sqlx::query(&sql).execute(&pool).await.unwrap();
    }
    while h.dispatcher.step().await.unwrap() != Default::default() {}
    let seen = rx.seen();
    assert_eq!(seen.len(), 1, "only alpha's own notice is sent");
    assert_eq!(seen[0].0, "/a?access_token=cb_a");
    let notice: Value = serde_json::from_str(&seen[0].2).unwrap();
    assert_eq!(notice["dedupe_key"], format!("log-{own}"));
    assert_eq!(notice["bridge_reference"], alpha_bridge);
    let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT requester, dedupe_key, state, last_error FROM webhook_deliveries ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let refused: Vec<_> = rows
        .iter()
        .filter(|r| r.3.as_deref() == Some("notice-refused"))
        .map(|r| (r.0.as_str(), r.1.clone(), r.2.as_str()))
        .collect();
    assert_eq!(
        refused,
        [
            ("alpha", format!("log-{own}-held-CND-Paused"), "dead"),
            ("alpha", format!("log-{theirs}"), "dead"),
            ("beta", format!("log-{own}"), "dead"),
        ]
    );
}
