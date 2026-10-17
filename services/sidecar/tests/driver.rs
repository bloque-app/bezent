// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The chain-event driver (PLAN `DEC-19`, `pl-pr-e2e` ruling, 2026-10-15): one test per call, the
//! deposit and withdrawal walk-throughs of `deploy/e2e/README.md`, and the refusals that keep it
//! test only.
//!
//! Every test runs in a fresh database that `#[sqlx::test]` creates on the server `DATABASE_URL`
//! names, with bezent's migrations and the sidecar's declarations. The pieces are the sidecar's
//! own: the ingress's router, the core's cycle, the published test signer over its real link, and
//! the driver, whose two pools log in as test logins holding `bezent_kreivo_endpoint` and
//! `bezent_evm_endpoint` only. The core and the ingress each act as their own role for a pool's
//! whole life (one pool, one role, #27).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use bezent_core_service::reserve::check::{LogPager, Outcome, ReserveCheck};
use bezent_core_service::reserve::readings::Recorded;
use bezent_core_service::{Core, Settings};
use bezent_sidecar::driver::{self, Driver, SignerLink};
use bezent_sidecar::setup::{
    ASSET, ISSUING, ISSUING_ASSET, ORIGIN, RESERVE, TOKEN, seed, signer_config,
};
use bezent_signer::link::{self, Callers};
use bezent_signer::log::Log;
use bezent_signer::signer::Signer;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tower::ServiceExt;

const DRIVER_TOKEN: &str = "driver-token-for-tests";
const LINK_TOKEN: &str = "signer-link-token-for-tests";
const CREDENTIAL: &str = "ci-1.ci-swaps-secret-0123456789";
const DESTINATION: &str = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";
const REFUND_TO: &str = "0x1111111111111111111111111111111111111111";
const BENEFICIARY: &str = "0x2222222222222222222222222222222222222222";

const KREIVO_LOGIN: &str = "sidecar_driver_kreivo_for_tests";
const EVM_LOGIN: &str = "sidecar_driver_evm_for_tests";
const CORE_LOGIN: &str = "sidecar_tests_core_login";
const INGRESS_LOGIN: &str = "sidecar_tests_ingress_login";
const SIGNER_LOGIN: &str = "sidecar_tests_signer_login";
const PASSWORD: &str = "test-only-driver-login";

fn temp_dir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bzs-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn exec(pool: &PgPool, sql: &str) {
    sqlx::raw_sql(sql).execute(pool).await.unwrap();
}

/// A pool logged in as `name`, a login of the server that is a member of `role` and of nothing
/// else (#46), created once per server: one pool, one login, never a switched role (#27).
async fn login(admin: &PgPool, name: &str, role: &str) -> PgPool {
    bezent_store::login::connect_login(
        admin,
        name,
        role,
        PASSWORD,
        PgPoolOptions::new().max_connections(3),
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"))
}

struct World {
    admin: PgPool,
    core: Core,
    ingress: Router,
    driver: Arc<Driver>,
    http: Router,
    _dir: PathBuf,
}

impl World {
    async fn new(options: PgPoolOptions, connect: PgConnectOptions) -> Self {
        let admin = bezent_store::login::guard(options)
            .connect_with(connect.clone())
            .await
            .unwrap();
        seed(
            &admin,
            &[(
                "swaps".to_owned(),
                "ci-1".to_owned(),
                "ci-swaps-secret-0123456789".to_owned(),
            )],
        )
        .await
        .unwrap();
        let core = Core::new(
            login(&admin, CORE_LOGIN, bezent_store::CORE_ROLE).await,
            Settings::default(),
        );
        let ingress = bezent_ingress::router(bezent_ingress::App::new(
            login(&admin, INGRESS_LOGIN, bezent_store::INGRESS_ROLE).await,
            bezent_ingress::Limits {
                max_in_flight: 16,
                log_page: 100,
            },
        ));

        // The sidecar's signer: the published test keys, over its link.
        let dir = temp_dir();
        let socket = dir.join("signer.sock");
        let (config, backend) = signer_config(LINK_TOKEN).unwrap();
        let policy = config.policy(&backend).unwrap();
        let signer = Arc::new(Signer::new(
            login(&admin, SIGNER_LOGIN, bezent_store::SIGNER_ROLE).await,
            policy,
            backend,
            Log::capture().0,
        ));
        let listener = link::bind(&socket).unwrap();
        tokio::spawn(link::serve(
            listener,
            Arc::new(Callers::sharing_the_signer_uid_in_tests(&config.callers)),
            signer,
        ));

        let driver = Arc::new(
            Driver::new(
                login(&admin, KREIVO_LOGIN, bezent_store::KREIVO_ENDPOINT_ROLE).await,
                login(&admin, EVM_LOGIN, bezent_store::EVM_ENDPOINT_ROLE).await,
                login(&admin, CORE_LOGIN, bezent_store::CORE_ROLE).await,
                DRIVER_TOKEN,
                SignerLink {
                    socket,
                    token: LINK_TOKEN.to_owned(),
                    uid: bezent_signer::process::euid(),
                },
            )
            .unwrap(),
        );
        Self {
            admin,
            core,
            ingress,
            http: driver.clone().router(),
            driver,
            _dir: dir,
        }
    }

    /// A driver call, as a harness makes it.
    async fn call(&self, path: &str, body: Value) -> (StatusCode, Value) {
        send(&self.http, path, Some(DRIVER_TOKEN), body).await
    }

    async fn ok(&self, path: &str, body: Value) -> Value {
        let (status, v) = self.call(path, body).await;
        assert_eq!(status, StatusCode::OK, "{path}: {v}");
        v
    }

    /// A deposit request at `coordinate`, as Swaps sends it.
    async fn deposit(&self, reference: &str, coordinate: &Value) -> (StatusCode, Value) {
        send(
            &self.ingress,
            "/v0/deposits",
            Some(CREDENTIAL),
            json!({
                "request_reference": reference,
                "coordinate": coordinate,
                "destination": {"network": ISSUING, "address": DESTINATION},
                "refund_destination": {"network": ORIGIN, "address": REFUND_TO},
            }),
        )
        .await
    }

    /// The core's cycle and the stand-in endpoints, `n` rounds.
    async fn turn(&self, n: usize) {
        for _ in 0..n {
            self.core.cycle(chrono::Utc::now()).await.unwrap();
            self.driver.tend().await.unwrap();
        }
    }

    async fn state(&self, bridge_reference: &str) -> (String, Option<String>) {
        sqlx::query_as("SELECT state, condition FROM transfers WHERE bridge_reference = $1")
            .bind(bridge_reference)
            .fetch_one(&self.admin)
            .await
            .unwrap()
    }

    /// Turns until the transfer is in `state`, or fails.
    async fn until(&self, bridge_reference: &str, state: &str) {
        for _ in 0..12 {
            if self.state(bridge_reference).await.0 == state {
                return;
            }
            self.turn(1).await;
        }
        panic!(
            "{bridge_reference} never reached {state}: {:?}",
            self.state(bridge_reference).await
        );
    }

    /// A final arrival of `amount`, deposited: the transfer's bridge reference.
    async fn deposited(&self, reference: &str, amount: u128) -> String {
        let a = self
            .ok("/driver/v0/arrivals", json!({"amount": amount.to_string()}))
            .await;
        self.ok("/driver/v0/finality", json!({"network": ORIGIN}))
            .await;
        let (status, d) = self.deposit(reference, &a["coordinate"]).await;
        assert_eq!(status, StatusCode::CREATED, "{d}");
        d["bridge_reference"].as_str().unwrap().to_owned()
    }

    /// A deposit carried to `Completed` through the driver.
    async fn completed_deposit(&self, reference: &str, amount: u128) -> String {
        let bridge = self.deposited(reference, amount).await;
        self.until(&bridge, "Delivering").await;
        self.ok(
            "/driver/v0/deliveries/include",
            json!({"bridge_reference": bridge}),
        )
        .await;
        self.ok("/driver/v0/finality", json!({"network": ISSUING}))
            .await;
        self.until(&bridge, "Completed").await;
        bridge
    }
}

async fn send(
    router: &Router,
    path: &str,
    bearer: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut request = Request::post(path).header(header::CONTENT_TYPE, "application/json");
    if let Some(b) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {b}"));
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes))),
    )
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

// -------------------------------------------------------------------------------------------
// One test per call.

/// `POST /driver/v0/arrivals`: an arrival at the reserve, kind `arrival`, observed and open; the
/// ingress answers `not-final` until its finality.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_19_arrival_is_an_open_observation_at_the_reserve(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let a = w
        .ok("/driver/v0/arrivals", json!({"amount": "1000000"}))
        .await;
    assert_eq!(a["kind"], "arrival");
    assert_eq!(a["recipient"], RESERVE);
    assert_eq!(a["coordinate"]["network"], ORIGIN);
    assert_eq!(
        a["coordinate"]["position"],
        json!({"kind": "log", "index": 0})
    );
    let row: (String, String, String, String, bool) = sqlx::query_as(
        "SELECT o.kind, o.token, o.amount::text, o.recipient, \
                EXISTS (SELECT 1 FROM observation_outcomes x WHERE x.observation_id = o.id) \
         FROM observations o WHERE o.network = $1",
    )
    .bind(ORIGIN)
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            "arrival".into(),
            TOKEN.into(),
            "1000000".into(),
            RESERVE.into(),
            false
        )
    );
    let (status, body) = w.deposit("early", &a["coordinate"]).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");

    // A token that is not admitted arrives as other_arrival, which no deposit binds.
    let other = w
        .ok(
            "/driver/v0/arrivals",
            json!({"amount": "5", "token": "not-admitted"}),
        )
        .await;
    assert_eq!(other["kind"], "other_arrival");
    // Not an amount: nothing written.
    let (status, _) = w
        .call("/driver/v0/arrivals", json!({"amount": "1.5"}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        count(&w.admin, "SELECT count(*) FROM observations").await,
        2
    );
}

/// `POST /driver/v0/finality`: the tip becomes a final head, read through, and open observations
/// are decided at it; a coordinate may be withdrawn instead.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_19_finality_decides_at_a_recorded_final_head(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let a = w
        .ok("/driver/v0/arrivals", json!({"amount": "1000000"}))
        .await;
    let f = w
        .ok("/driver/v0/finality", json!({"network": ORIGIN}))
        .await;
    assert_eq!(f["decided"][0]["coordinate"], a["coordinate"]);
    assert_eq!(f["decided"][0]["outcome"], "final");
    let head = f["final_head"]["block_number"].as_i64().unwrap();
    // The outcome rests on a recorded final head, and the network is read up to it.
    assert_eq!(
        count(
            &w.admin,
            &format!(
                "SELECT count(*) FROM observation_outcomes x JOIN final_heads h \
                   ON h.network = '{ORIGIN}' AND h.block_number = x.final_block_number \
                  AND h.block_hash = x.final_block_hash WHERE x.outcome = 'final'"
            )
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &w.admin,
            &format!("SELECT watermark FROM observation_watermarks WHERE network = '{ORIGIN}'")
        )
        .await,
        head
    );
    // Now the deposit is taken in.
    let (status, d) = w.deposit("after-final", &a["coordinate"]).await;
    assert_eq!(status, StatusCode::CREATED, "{d}");

    // Withdrawn: one coordinate, its block reorganised away.
    let b = w.ok("/driver/v0/arrivals", json!({"amount": "7"})).await;
    let f = w
        .ok(
            "/driver/v0/finality",
            json!({"network": ORIGIN, "coordinate": b["coordinate"], "outcome": "withdrawn"}),
        )
        .await;
    assert_eq!(f["decided"][0]["outcome"], "withdrawn");
    // Decided once.
    let (status, _) = w
        .call(
            "/driver/v0/finality",
            json!({"network": ORIGIN, "coordinate": b["coordinate"]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    // Withdrawn needs its coordinate.
    let (status, _) = w
        .call(
            "/driver/v0/finality",
            json!({"network": ORIGIN, "outcome": "withdrawn"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// `POST /driver/v0/redemptions`: a redemption on the issuing network; once final, the core
/// makes the withdrawal under the bridge reference the call answered.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_19_redemption_once_final_becomes_a_withdrawal(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let r = w
        .ok(
            "/driver/v0/redemptions",
            json!({"redeemer": DESTINATION, "amount": "400000", "payout_address": BENEFICIARY}),
        )
        .await;
    assert_eq!(r["kind"], "redemption");
    assert_eq!(r["asset"], ISSUING_ASSET);
    let bridge = r["bridge_reference"].as_str().unwrap();
    let row: (String, String, Value) =
        sqlx::query_as("SELECT kind, sender, detail FROM observations WHERE network = $1")
            .bind(ISSUING)
            .fetch_one(&w.admin)
            .await
            .unwrap();
    assert_eq!(row.0, "redemption");
    assert_eq!(Some(row.1.as_str()), r["redeemer"].as_str());
    assert_eq!(row.2["backend"], json!({"kind": "evm", "chain_id": 80002}));
    assert_eq!(row.2["payout"]["address"], BENEFICIARY);

    w.ok("/driver/v0/finality", json!({"network": ISSUING}))
        .await;
    w.turn(1).await;
    let (kind, destination, amount): (String, String, String) = sqlx::query_as(
        "SELECT kind, destination, amount::text FROM transfers WHERE bridge_reference = $1",
    )
    .bind(bridge)
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(
        (kind.as_str(), destination.as_str(), amount.as_str()),
        ("withdrawal", BENEFICIARY, "400000")
    );
    // A redeemer that is no address: refused, nothing written.
    let (status, _) = w
        .call(
            "/driver/v0/redemptions",
            json!({"redeemer": "nobody", "amount": "1", "payout_address": BENEFICIARY}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// `POST /driver/v0/deliveries/include` (and the deposit walk-through): the mint the core
/// ordered, prepared and signed by the stand-in endpoints, included and then final: Completed,
/// the leg the full amount, to the destination (`REQ-TR-5`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_19_include_confirms_the_delivery_the_core_ordered(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let bridge = w.deposited("dep-1", 1_000_000).await;
    // Nothing signed yet: not ready.
    let (status, v) = w
        .call(
            "/driver/v0/deliveries/include",
            json!({"bridge_reference": bridge}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"], "not-ready");

    w.until(&bridge, "Delivering").await;
    let i = w
        .ok(
            "/driver/v0/deliveries/include",
            json!({"bridge_reference": bridge}),
        )
        .await;
    assert_eq!(i["kind"], "mint");
    assert_eq!(i["amount"], "1000000");
    w.until(&bridge, "Delivered").await;
    // Included once.
    let (status, _) = w
        .call(
            "/driver/v0/deliveries/include",
            json!({"bridge_reference": bridge}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    w.ok("/driver/v0/finality", json!({"network": ISSUING}))
        .await;
    w.until(&bridge, "Completed").await;
    // The signed bytes the stand-in recorded are the signer's, over the prepared bytes.
    assert_eq!(
        count(
            &w.admin,
            "SELECT count(*) FROM signed_transactions st JOIN signatures s ON s.intent_id = st.intent_id"
        )
        .await,
        1
    );
}

/// `include` with another amount: the chain moved something else, and the core sends the
/// transfer to review, holding `CND-AmountMismatch` (`REQ-TR-5`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_19_include_of_another_amount_goes_to_review(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let bridge = w.deposited("dep-short", 1_000_000).await;
    w.until(&bridge, "Delivering").await;
    w.ok(
        "/driver/v0/deliveries/include",
        json!({"bridge_reference": bridge, "amount": "999999"}),
    )
    .await;
    w.until(&bridge, "Needs-Review").await;
    assert_eq!(
        w.state(&bridge).await.1.as_deref(),
        Some("CND-AmountMismatch")
    );
}

/// `POST /driver/v0/deliveries/not-executed`: the attempt is proven not executed at a final head
/// past its window, and the core makes attempt 2, which the driver then includes.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_19_not_executed_proves_the_attempt_and_the_core_retries(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let bridge = w.deposited("dep-retry", 500_000).await;
    w.until(&bridge, "Delivering").await;
    let p = w
        .ok(
            "/driver/v0/deliveries/not-executed",
            json!({"bridge_reference": bridge}),
        )
        .await;
    assert_eq!(p["proofs"][0]["kind"], "nonce-unconsumed");
    assert!(
        p["final_head"]["block_number"].as_i64().unwrap()
            >= p["proofs"][0]["valid_until_block"].as_i64().unwrap()
    );
    // Proven once: nothing left to prove.
    let (status, _) = w
        .call(
            "/driver/v0/deliveries/not-executed",
            json!({"bridge_reference": bridge}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    // Attempt 2, ordered, prepared and signed.
    for _ in 0..12 {
        w.turn(1).await;
        let signed = count(
            &w.admin,
            "SELECT count(*) FROM signed_transactions st JOIN intents i ON i.id = st.intent_id \
             WHERE i.attempt = 2",
        )
        .await;
        if signed == 1 {
            break;
        }
    }
    w.until(&bridge, "Delivering").await;
    w.ok(
        "/driver/v0/deliveries/include",
        json!({"bridge_reference": bridge}),
    )
    .await;
    w.ok("/driver/v0/finality", json!({"network": ISSUING}))
        .await;
    w.until(&bridge, "Completed").await;
}

/// The test chain never shows a double delivery: an attempt `include` confirmed is refused by
/// `not-executed`, and an attempt proven not executed is refused by `include`; nothing is written.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_19_an_attempt_is_included_or_not_executed_never_both(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    // Included, then not-executed: refused.
    let bridge = w.deposited("dep-both-a", 500_000).await;
    w.until(&bridge, "Delivering").await;
    w.ok(
        "/driver/v0/deliveries/include",
        json!({"bridge_reference": bridge}),
    )
    .await;
    let proofs = count(&w.admin, "SELECT count(*) FROM non_execution_proofs").await;
    let (status, v) = w
        .call(
            "/driver/v0/deliveries/not-executed",
            json!({"bridge_reference": bridge}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(
        count(&w.admin, "SELECT count(*) FROM non_execution_proofs").await,
        proofs
    );
    // Not-executed, then include: refused, and no mint is observed.
    let bridge = w.deposited("dep-both-b", 400_000).await;
    w.until(&bridge, "Delivering").await;
    w.ok(
        "/driver/v0/deliveries/not-executed",
        json!({"bridge_reference": bridge}),
    )
    .await;
    let observed = count(
        &w.admin,
        "SELECT count(*) FROM observations WHERE kind = 'mint'",
    )
    .await;
    let (status, _) = w
        .call(
            "/driver/v0/deliveries/include",
            json!({"bridge_reference": bridge}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        count(
            &w.admin,
            "SELECT count(*) FROM observations WHERE kind = 'mint'"
        )
        .await,
        observed
    );
}

/// `POST /driver/v0/dry-run-failures`: the next preparation's dry run fails with the network's
/// refusal, and the transfer holds the condition it names (`CND-DestinationRefused`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_19_a_failed_dry_run_holds_its_condition(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    w.ok(
        "/driver/v0/dry-run-failures",
        json!({"network": ISSUING, "reason": "Kreivo: Assets.CannotCreate"}),
    )
    .await;
    let bridge = w.deposited("dep-refused", 1_000_000).await;
    for _ in 0..6 {
        w.turn(1).await;
        if w.state(&bridge).await.1.is_some() {
            break;
        }
    }
    assert_eq!(
        w.state(&bridge).await,
        (
            "Source-Final".to_owned(),
            Some("CND-DestinationRefused".to_owned())
        )
    );
    let (status, _) = w
        .call(
            "/driver/v0/dry-run-failures",
            json!({"network": "polygon", "reason": "x"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// `POST /driver/v0/readings`: supply and the reserve's balance at the point the check reads, as
/// the driver's final observations add up there, so the reserve check holds; a reading set by
/// hand makes it diverge.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_19_readings_let_the_reserve_check_hold(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    // Nothing final yet: not ready.
    let (status, _) = w.call("/driver/v0/readings", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);

    w.completed_deposit("dep-held", 1_000_000).await;
    // A second arrival, final and not deposited: owed mint.
    w.ok("/driver/v0/arrivals", json!({"amount": "250"})).await;
    w.ok("/driver/v0/finality", json!({"network": ORIGIN}))
        .await;
    w.turn(1).await;
    let r = w.ok("/driver/v0/readings", json!({})).await;
    let readings = r["readings"].as_array().unwrap();
    let supply = readings.iter().find(|x| x["kind"] == "supply").unwrap();
    let reserve = readings.iter().find(|x| x["kind"] == "reserve").unwrap();
    assert_eq!(supply["subject"], ISSUING_ASSET);
    assert_eq!(supply["amount"], "1000000");
    assert_eq!(reserve["subject"], TOKEN);
    assert_eq!(reserve["holder"], RESERVE);
    assert_eq!(reserve["amount"], "1000250");

    let pool = w.core.pool().clone();
    let check = ReserveCheck::new(pool.clone(), Recorded::new(pool), LogPager);
    let e = check.evaluate(ASSET, chrono::Utc::now()).await.unwrap();
    assert_eq!(
        e.outcome,
        Outcome::Holds,
        "{:?} {:?}",
        e.reason,
        e.differences
    );

    // A reading given by hand, at a new point: the check diverges.
    w.ok("/driver/v0/finality", json!({"network": ORIGIN}))
        .await;
    w.ok("/driver/v0/arrivals", json!({"amount": "1"})).await;
    w.ok("/driver/v0/finality", json!({"network": ORIGIN}))
        .await;
    w.turn(1).await;
    w.ok(
        "/driver/v0/readings",
        json!({"set": [{"subject": TOKEN, "holder": RESERVE, "amount": "5"}]}),
    )
    .await;
    let e = check
        .evaluate(ASSET, chrono::Utc::now() + chrono::Duration::minutes(1))
        .await
        .unwrap();
    assert_eq!(e.outcome, Outcome::Diverges, "{:?}", e.reason);
}

// -------------------------------------------------------------------------------------------
// The withdrawal walk-through.

/// A deposit, then a redemption paid out on the origin network through the driver: Completed,
/// the payout the full amount, to the beneficiary.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_19_withdrawal_walk_through(options: PgPoolOptions, connect: PgConnectOptions) {
    let w = World::new(options, connect).await;
    w.completed_deposit("dep-for-withdrawal", 1_000_000).await;
    let r = w
        .ok(
            "/driver/v0/redemptions",
            json!({"redeemer": DESTINATION, "amount": "400000", "payout_address": BENEFICIARY}),
        )
        .await;
    let bridge = r["bridge_reference"].as_str().unwrap();
    w.ok("/driver/v0/finality", json!({"network": ISSUING}))
        .await;
    w.turn(1).await;
    w.until(bridge, "Delivering").await;
    let i = w
        .ok(
            "/driver/v0/deliveries/include",
            json!({"bridge_reference": bridge}),
        )
        .await;
    assert_eq!(i["kind"], "payout_leg");
    assert_eq!(i["recipient"], BENEFICIARY);
    assert_eq!(i["amount"], "400000");
    // The signed payout is an EIP-1559 transaction on the origin network's chain id.
    let bytes: Vec<u8> = sqlx::query_scalar(
        "SELECT st.bytes FROM signed_transactions st JOIN intents i ON i.id = st.intent_id \
         WHERE i.purpose = 'payout'",
    )
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(bytes.first(), Some(&2));
    w.ok("/driver/v0/finality", json!({"network": ORIGIN}))
        .await;
    w.until(bridge, "Completed").await;
}

// -------------------------------------------------------------------------------------------
// Test only.

/// The driver is absent when a production network is declared: not bound at all on such a store,
/// and a bound driver answers `404 absent` and writes nothing once one appears (`REQ-KY-6`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ky_6_the_driver_is_absent_when_a_production_network_is_declared(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    // Bound on a store of test networks.
    let bound = driver::start(w.driver.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    drop(bound);

    // A production network: never bound, nothing listens.
    exec(
        &w.admin,
        "INSERT INTO networks (network, role, address_format) VALUES ('polygon', 'origin', 'evm')",
    )
    .await;
    let listen: std::net::SocketAddr = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap()
    };
    let refused = driver::start(w.driver.clone(), listen).await.unwrap_err();
    assert!(refused.contains("polygon"), "{refused}");
    assert!(tokio::net::TcpStream::connect(listen).await.is_err());

    // And a bound driver, asked anything, is absent and writes nothing.
    for (path, body) in [
        ("/driver/v0/arrivals", json!({"amount": "1"})),
        (
            "/driver/v0/redemptions",
            json!({"redeemer": DESTINATION, "amount": "1", "payout_address": BENEFICIARY}),
        ),
        ("/driver/v0/finality", json!({"network": ORIGIN})),
        ("/driver/v0/readings", json!({})),
        (
            "/driver/v0/deliveries/include",
            json!({"bridge_reference": "x"}),
        ),
        (
            "/driver/v0/deliveries/not-executed",
            json!({"bridge_reference": "x"}),
        ),
        (
            "/driver/v0/dry-run-failures",
            json!({"network": ORIGIN, "reason": "x"}),
        ),
    ] {
        let (status, v) = w.call(path, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {v}");
        assert_eq!(v["error"], "absent");
    }
    assert!(w.driver.tend().await.is_err());
    assert_eq!(
        count(&w.admin, "SELECT count(*) FROM observations").await,
        0
    );
    assert_eq!(count(&w.admin, "SELECT count(*) FROM final_heads").await, 0);
}

/// Every call needs the driver's token.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_19_every_call_needs_the_driver_token(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    for bearer in [None, Some("not-the-driver-token"), Some(CREDENTIAL)] {
        let (status, _) = send(
            &w.http,
            "/driver/v0/arrivals",
            bearer,
            json!({"amount": "1"}),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        count(&w.admin, "SELECT count(*) FROM observations").await,
        0
    );
    // A short token is refused outright.
    assert!(
        Driver::new(
            w.core.pool().clone(),
            w.core.pool().clone(),
            w.core.pool().clone(),
            "short",
            SignerLink {
                socket: "/nonexistent".into(),
                token: String::new(),
                uid: 0,
            },
        )
        .is_err()
    );
}
