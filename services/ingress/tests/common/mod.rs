// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Helpers shared by the ingress tests. Every test runs in a fresh database that `#[sqlx::test]`
//! creates on the server `DATABASE_URL` names, with the store's migrations applied by the
//! migration owner. The ingress itself runs as the service role (`bezent_service`), as deployed
//! (`F-001` NOTES, S1); declarations and observations are written as the owner, as the operator and
//! the endpoints would.

#![allow(dead_code)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use bezent_ingress::{App, Limits, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

/// The issuing network.
pub const ISSUING: &str = "kreivo";
/// The origin network.
pub const ORIGIN: &str = "origin-a";
/// The public bridged asset and its origin token on `ORIGIN`.
pub const ASSET: &str = "bUSDC";
pub const TOKEN: &str = "usdc";
/// A token admitted for an asset that has no route.
pub const ROUTELESS_TOKEN: &str = "routeless";
/// An attested asset (REQ-BE-2).
pub const ATTESTED: &str = "bFIAT";
/// The reserve address of `ASSET` on `ORIGIN`.
pub const RESERVE: &str = "0x00000000000000000000000000000000000000aa";
/// A well-formed address on `ORIGIN`.
pub const ORIGIN_ADDRESS: &str = "0x52908400098527886E0F7030069857D2E4169EE7";
/// A well-formed SS58 address on `ISSUING` (Alice, prefix 42).
pub const ISSUING_ADDRESS: &str = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";

/// Route limits of the seeded deposit route, in the origin token's minor units.
pub const MINIMUM: u64 = 100;
pub const MAXIMUM: u64 = 1_000_000;
pub const WINDOW_LIMIT: u64 = 2_500_000;
pub const CAPACITY: i64 = 100;

/// The credentials of the seeded requesters: `alpha` holds every grant, `beta` only deposits,
/// `gamma` none.
pub const ALPHA: &str = "alpha-k1.alpha-secret-0123456789";
pub const BETA: &str = "beta-k1.beta-secret-0123456789";
pub const GAMMA: &str = "gamma-k1.gamma-secret-0123456789";

/// Runs `sql` as the owner.
pub async fn exec(pool: &PgPool, sql: &str) {
    sqlx::raw_sql(sql)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// Reads one value as the owner.
pub async fn scalar<T>(pool: &PgPool, sql: &str) -> T
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
{
    sqlx::query_scalar(sql)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

fn credential(requester: &str, token: &str) -> String {
    let (id, secret) = token.split_once('.').unwrap();
    let digest = hex::encode(Sha256::digest(secret.as_bytes()));
    format!(
        "INSERT INTO requester_credentials (credential_id, requester, secret_sha256) \
         VALUES ('{id}', '{requester}', '\\x{digest}');"
    )
}

/// The declarations: networks, assets, admitted tokens, the reserve, the deposit route, and the
/// requesters with their grants and credentials.
pub async fn seed(pool: &PgPool) {
    exec(
        pool,
        &format!(
            "INSERT INTO networks (network, role, address_format) VALUES \
               ('{ISSUING}', 'issuing', 'ss58'), ('{ORIGIN}', 'origin', 'evm'); \
             INSERT INTO assets (asset, precision, assurance_class) VALUES \
               ('{ASSET}', 6, 'public'), ('{ATTESTED}', 2, 'attested'), ('bOTHER', 6, 'public'); \
             INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
               ('{ORIGIN}', '{TOKEN}', '{ASSET}', 6), \
               ('{ORIGIN}', '{ROUTELESS_TOKEN}', 'bOTHER', 6); \
             INSERT INTO reserves (asset, network, address) VALUES \
               ('{ASSET}', '{ORIGIN}', '{RESERVE}'), \
               ('bOTHER', '{ORIGIN}', '0x00000000000000000000000000000000000000bb'); \
             INSERT INTO routes (asset, network, direction, minimum, maximum, window_limit, \
                 window_seconds, capacity, cutoffs, completion_budget_seconds) VALUES \
               ('{ASSET}', '{ORIGIN}', 'deposit', {MINIMUM}, {MAXIMUM}, {WINDOW_LIMIT}, 86400, \
                {CAPACITY}, '[]', 3600); \
             INSERT INTO requesters (requester) VALUES ('alpha'), ('beta'), ('gamma'); \
             INSERT INTO requester_grants (requester, grant_kind) VALUES \
               ('alpha', 'deposit'), ('alpha', 'withdrawal'), ('alpha', 'refund'), ('alpha', 'attest'), \
               ('beta', 'deposit'); \
             {} {} {}",
            credential("alpha", ALPHA),
            credential("beta", BETA),
            credential("gamma", GAMMA),
        ),
    )
    .await;
}

/// What an observation at (`ORIGIN`, `block`, log 0) says, besides its coordinate.
pub struct Seen<'a> {
    pub token: &'a str,
    pub amount: u64,
    pub sender: &'a str,
    pub recipient: &'a str,
    /// The on-chain reference the movement carries (hex), if any: a payout leg does.
    pub reference: Option<&'a str>,
    /// The kind the endpoint wrote (`observations.kind`, migration 0034); `None` writes none,
    /// which the store records as `other`.
    pub kind: Option<&'a str>,
    /// `final`, `withdrawn`, or none yet.
    pub outcome: Option<&'a str>,
}

/// Records, as an endpoint would, the observation `seen` at (`ORIGIN`, `block`, log 0), and
/// returns its coordinate as a request carries it.
pub async fn observe_seen(pool: &PgPool, block: i64, seen: Seen<'_>) -> Value {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, token, amount, sender, recipient, executed, on_chain_reference, \
             kind) \
         VALUES ($1, $2, $3, '2026-10-13T00:00:00Z', 'log', 0, $4, $5::numeric, $6, $7, TRUE, \
             decode($8, 'hex'), coalesce($9, 'other')) RETURNING id",
    )
    .bind(ORIGIN)
    .bind(block)
    .bind(block_hash(block))
    .bind(seen.token)
    .bind(seen.amount.to_string())
    .bind(seen.sender)
    .bind(seen.recipient)
    .bind(seen.reference)
    .bind(seen.kind)
    .fetch_one(pool)
    .await
    .expect("observe");
    if let Some(outcome) = seen.outcome {
        sqlx::query(
            "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
                 final_block_hash) VALUES ($1, $2, $3, '\\x01')",
        )
        .bind(id)
        .bind(outcome)
        .bind(block + 10)
        .execute(pool)
        .await
        .expect("outcome");
    }
    coordinate(block)
}

/// A final arrival of `amount` of the origin token at the reserve, from a funder.
pub fn arrival_seen(amount: u64) -> Seen<'static> {
    Seen {
        token: TOKEN,
        amount,
        sender: "0xfunder",
        recipient: RESERVE,
        reference: None,
        kind: Some("arrival"),
        outcome: Some("final"),
    }
}

/// Records an executed, final movement of `amount` of `token` to `recipient` at (`ORIGIN`,
/// `block`, log 0), and returns its coordinate as a request carries it.
pub async fn observe(
    pool: &PgPool,
    block: i64,
    token: &str,
    amount: u64,
    recipient: &str,
) -> Value {
    observe_seen(
        pool,
        block,
        Seen {
            token,
            amount,
            sender: "0xfunder",
            recipient,
            reference: None,
            kind: Some("arrival"),
            outcome: Some("final"),
        },
    )
    .await
}

/// An arrival of `amount` of the origin token at the reserve.
pub async fn arrival(pool: &PgPool, block: i64, amount: u64) -> Value {
    observe(pool, block, TOKEN, amount, RESERVE).await
}

/// The block hash of `block` in these tests.
pub fn block_hash(block: i64) -> Vec<u8> {
    let mut h = vec![0xab];
    h.extend_from_slice(&block.to_be_bytes());
    h
}

/// The coordinate of (`ORIGIN`, `block`, log 0).
pub fn coordinate(block: i64) -> Value {
    json!({
        "network": ORIGIN,
        "block_number": block,
        "block_hash": format!("0x{}", hex::encode(block_hash(block))),
        "position": { "kind": "log", "index": 0 },
    })
}

/// A deposit request binding `coordinate`.
pub fn deposit_body(reference: &str, coordinate: Value) -> Value {
    json!({
        "request_reference": reference,
        "coordinate": coordinate,
        "destination": { "network": ISSUING, "address": ISSUING_ADDRESS },
        "refund_destination": { "network": ORIGIN, "address": ORIGIN_ADDRESS },
    })
}

/// A pool on the same database logged in as the ingress's login (it was the shared service role's
/// before migration 0045).
pub async fn service_pool(pool: &PgPool) -> PgPool {
    ingress_pool(pool).await
}

/// The test-only password of the logins these tests make.
pub const LOGIN_PASSWORD: &str = "test-only-ingress-tests-login";

/// A pool logged in as a login of its own that is a member of `role` and of nothing else, as each
/// service's login is: one pool, one login, never a switched role (#27, #46).
pub async fn role_pool(pool: &PgPool, role: &'static str) -> PgPool {
    bezent_store::login::connect_login(
        pool,
        &format!("ingress_tests_{role}"),
        role,
        LOGIN_PASSWORD,
        PgPoolOptions::new().max_connections(8),
    )
    .await
    .expect("role pool")
}

/// The ingress API's pool: `bezent_ingress`, which alone registers webhook URLs and makes secrets
/// (PR #55 re-check S-c).
pub async fn ingress_pool(pool: &PgPool) -> PgPool {
    role_pool(pool, bezent_store::INGRESS_ROLE).await
}

/// The webhook dispatcher's pool: its own role, `bezent_webhooks` (PR #55 review M2).
pub async fn dispatcher_pool(pool: &PgPool) -> PgPool {
    role_pool(pool, bezent_store::WEBHOOKS_ROLE).await
}

/// The ingress under test, on the service role.
pub struct Ingress {
    pub app: App,
    router: Router,
}

impl Ingress {
    /// Seeds the declarations and builds the ingress.
    pub async fn new(pool: &PgPool) -> Self {
        seed(pool).await;
        Self::unseeded(pool, 64).await
    }

    /// Seeds the declarations and builds the ingress with webhooks (`REQ-ST-5`).
    pub async fn with_webhooks(pool: &PgPool, hooks: bezent_ingress::webhooks::Webhooks) -> Self {
        seed(pool).await;
        let app = App::new(
            ingress_pool(pool).await,
            Limits {
                max_in_flight: 64,
                log_page: 50,
            },
        )
        .with_webhooks(hooks);
        Self {
            router: router(app.clone()),
            app,
        }
    }

    /// PUTs `body` to `path` as `credential`.
    pub async fn put(&self, path: &str, credential: &str, body: &Value) -> (StatusCode, Value) {
        self.call("PUT", path, Some(credential), Some(body)).await
    }

    /// The ingress, with `max_in_flight` intake permits, on a database already seeded.
    pub async fn unseeded(pool: &PgPool, max_in_flight: usize) -> Self {
        let app = App::new(
            ingress_pool(pool).await,
            Limits {
                max_in_flight,
                log_page: 50,
            },
        );
        Self {
            router: router(app.clone()),
            app,
        }
    }

    /// Sends a request; returns the status and the JSON body.
    pub async fn call(
        &self,
        method: &str,
        path: &str,
        credential: Option<&str>,
        body: Option<&Value>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(c) = credential {
            request = request.header(header::AUTHORIZATION, format!("Bearer {c}"));
        }
        let request = match body {
            Some(b) => request
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(b).unwrap())),
            None => request.body(Body::empty()),
        }
        .unwrap();
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()))
        };
        (status, value)
    }

    /// POSTs `body` to `path` as `credential`.
    pub async fn post(&self, path: &str, credential: &str, body: &Value) -> (StatusCode, Value) {
        self.call("POST", path, Some(credential), Some(body)).await
    }

    /// GETs `path` as `credential`.
    pub async fn get(&self, path: &str, credential: &str) -> (StatusCode, Value) {
        self.call("GET", path, Some(credential), None).await
    }
}

/// Counts the rows of `table`, as the owner.
pub async fn count(pool: &PgPool, table: &str) -> i64 {
    scalar(pool, &format!("SELECT count(*) FROM {table}")).await
}
