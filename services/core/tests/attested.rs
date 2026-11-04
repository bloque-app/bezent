// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `F-011`, end to end: an attesting party drives bezent through the real request surface, and the
//! real core makes of it what `DEC-14` says (`REQ-BE-4`, `REQ-BE-5`, `REQ-BE-6`, `REQ-RQ-9`,
//! `REQ-BK-1`, `REQ-BK-9`; T-011-04 to T-011-09). The Swaps module stays a draft by ruling
//! (`RISK-8`), so this suite is what sends its requests: the ingress router in process, as the
//! ingress's login, beside the core as the core's. Only the issuing network is faked: its
//! observations are written as its endpoint would, and its supply is read from them.

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use bezent_core::attested::report_message;
use bezent_core_service::reserve::check::{AttestedSnapshots, Outcome, ReserveCheck};
use bezent_core_service::reserve::readings::{Readings, Recorded};
use bezent_core_service::reserve::statement::{DirectoryStore, KeyBackendSigner, Statements};
use bezent_ingress::{App, Limits, router};
use bezent_signer::keys::{FileBackend, SigningEngine};
use chrono::Duration;
use common::reserve::*;
use common::*;
use ed25519_dalek::{Signer as _, SigningKey};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tower::ServiceExt;

const RAIL: &str = "fiat-test";
const COPM: &str = "COPM";
const COP: &str = "cop";
const ISSUED: &str = "Here(100)";
const ACCOUNTS: &str = "swaps-accounts";
const TREASURY: &str = "swaps-treasury";
const PARTY: &str = "Swaps";
const REQUESTER: &str = "swaps-test";
const CREDENTIAL: &str = "swaps-k1.swaps-secret-0123456789";
const FLOOR: u128 = 500;
/// The fiat routes' completion budget in most of these tests: the party's reports run on the store's
/// clock and the issuing network's on the tests' fixed one, so a short budget is always past. A test
/// that wants a deadline past sets it.
const FIAT_BUDGET: i64 = 10 * 365 * 86_400;
/// The party's test key: signs nothing but these tests' reports.
const KEY_SEED: [u8; 32] = [0x5a; 32];

struct Fiat {
    w: World,
    router: Router,
    check: ReserveCheck<FakeReadings, FakePager>,
    readings: FakeReadings,
    /// How far the issuing network has been read.
    read_to: std::sync::atomic::AtomicI64,
}

impl Fiat {
    async fn new(options: PgPoolOptions, connect: PgConnectOptions) -> Self {
        let w = World::new(options, connect).await;
        let public = hex::encode(SigningKey::from_bytes(&KEY_SEED).verifying_key().to_bytes());
        let secret = hex::encode(Sha256::digest(
            CREDENTIAL.split_once('.').unwrap().1.as_bytes(),
        ));
        exec(
            &w.admin,
            &format!(
                "INSERT INTO networks (network, role, address_format, backend, \
                     reserve_point_lag_seconds) \
                   VALUES ('{RAIL}', 'origin', 'opaque', 'attested:7', 3600); \
                 INSERT INTO assets (asset, precision, assurance_class, issuing_asset, \
                     surplus_floor, surplus_alert) \
                   VALUES ('{COPM}', 2, 'attested', '{ISSUED}', {FLOOR}, {alert}); \
                 INSERT INTO admitted_tokens (network, token, asset, precision) \
                   VALUES ('{RAIL}', '{COP}', '{COPM}', 2); \
                 INSERT INTO reserves (asset, network, address) \
                   VALUES ('{COPM}', '{RAIL}', '{ACCOUNTS}'); \
                 INSERT INTO routes (asset, network, direction, minimum, maximum, window_limit, \
                     window_seconds, capacity, cutoffs, completion_budget_seconds) VALUES \
                   ('{COPM}', '{RAIL}', 'deposit', 1, 1000000000, 10000000000, 86400, 100, '[]', {FIAT_BUDGET}), \
                   ('{COPM}', '{RAIL}', 'withdrawal', 1, 1000000000, 10000000000, 86400, 100, '[]', {FIAT_BUDGET}); \
                 INSERT INTO surplus_funding_addresses (asset, network, address, reference, from_block) \
                   VALUES ('{COPM}', '{RAIL}', '{TREASURY}', 'treasury', 0); \
                 INSERT INTO requesters (requester) VALUES ('{REQUESTER}'); \
                 INSERT INTO requester_grants (requester, grant_kind) VALUES \
                   ('{REQUESTER}', 'deposit'), ('{REQUESTER}', 'withdrawal'), \
                   ('{REQUESTER}', 'attest'), ('{REQUESTER}', 'refund'); \
                 INSERT INTO requester_credentials (credential_id, requester, secret_sha256) \
                   VALUES ('{id}', '{REQUESTER}', '\\x{secret}'); \
                 INSERT INTO attesting_parties (network, requester, party) \
                   VALUES ('{RAIL}', '{REQUESTER}', '{PARTY}'); \
                 INSERT INTO attesting_keys (network, public_key) \
                   VALUES ('{RAIL}', '\\x{public}');",
                alert = FLOOR + 1,
                id = CREDENTIAL.split_once('.').unwrap().0,
            ),
        )
        .await;
        let ingress_pool = login_pool(
            &w.admin,
            "core_tests_ingress_login",
            bezent_store::INGRESS_ROLE,
            4,
        )
        .await;
        let app = App::new(
            ingress_pool,
            Limits {
                max_in_flight: 16,
                log_page: 50,
            },
        );
        let (check, readings, _) = checker(&w);
        Self {
            w,
            router: router(app),
            check,
            readings,
            read_to: 300.into(),
        }
    }

    async fn post(&self, path: &str, body: &Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {CREDENTIAL}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap();
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// Signs and sends a report of `kind`.
    async fn report(&self, id: &str, kind: &str, fields: Value) -> (StatusCode, Value) {
        let mut document = json!({"version": 1, "report_id": id, "network": RAIL, "kind": kind,
                                  "asset": COPM, "token": COP});
        document
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let report = document.to_string();
        let key = SigningKey::from_bytes(&KEY_SEED);
        let signature = key.sign(&report_message(report.as_bytes()));
        self.post(
            "/v0/reports",
            &json!({"report": report,
                    "signature": format!("0x{}", hex::encode(signature.to_bytes())),
                    "public_key": format!("0x{}", hex::encode(key.verifying_key().to_bytes()))}),
        )
        .await
    }

    /// A report that must be taken in.
    async fn reported(&self, id: &str, kind: &str, fields: Value) -> Value {
        let (status, answer) = self.report(id, kind, fields).await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        assert_eq!(answer["state"], "Recorded", "{answer}");
        answer
    }

    /// The operator's own money into the reserve, and a snapshot of it (`REQ-BK-5`).
    async fn funded(&self, amount: u128) {
        self.reported(
            "fund-1",
            "arrival",
            json!({"amount": amount.to_string(), "position": "acct-1", "funder": TREASURY}),
        )
        .await;
        self.snapshot("snap-0", "fund-1", amount).await;
        self.w.settle(now()).await;
    }

    /// A snapshot: `amount` in the one account, reflecting reports through `through`.
    async fn snapshot(&self, id: &str, through: &str, amount: u128) {
        self.reported(
            id,
            "positions",
            json!({"positions": [{"position": "acct-1", "amount": amount.to_string()}],
                   "through": through}),
        )
        .await;
    }

    /// A deposit of an `arrival` report, through to `Accepted`; returns the transfer.
    async fn deposited(&self, id: &str, amount: u128) -> i64 {
        let report = self
            .reported(
                id,
                "arrival",
                json!({"amount": amount.to_string(), "position": "acct-1",
                       "ids": {"order": format!("ord-{id}")}}),
            )
            .await;
        let body = json!({
            "request_reference": format!("dep-{id}"),
            "coordinate": report["coordinate"],
            "destination": { "network": ISSUING, "address": DESTINATION },
            "refund_destination": { "network": RAIL, "address": "the-payer" },
        });
        // Not recorded yet: not final, nothing recorded, the reference unspent (REQ-RQ-3).
        let (status, answer) = self.post("/v0/deposits", &body).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{answer}");
        assert_eq!(answer["error"], "not-final");
        self.w.cycle(now()).await;
        let (status, answer) = self.post("/v0/deposits", &body).await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        assert_eq!(answer["state"], "Accepted", "{answer}");
        scalar(
            &self.w.admin,
            &format!(
                "SELECT id FROM transfers WHERE bridge_reference = '{}'",
                answer["bridge_reference"].as_str().unwrap()
            ),
        )
        .await
    }

    /// The Kreivo endpoint's half of mint `id`, to `Completed`.
    async fn minted(&self, id: i64, block: i64, amount: u128) {
        deliver(&self.w, id, 1, now()).await;
        let o = observe(
            &self.w.admin,
            ISSUING,
            block,
            0,
            ISSUED,
            amount,
            None,
            Some(DESTINATION_ACCOUNT),
            Some(&reference_of(&self.w.admin, id).await),
            true,
            json!({"event": "Minted"}),
        )
        .await;
        decide(&self.w.admin, o, "final", block + 2).await;
        self.w.settle(now()).await;
        assert_eq!(self.w.state(id).await, ("Completed".into(), None));
    }

    /// One evaluation of the check, on what the core recorded of the party's reports (`Recorded`,
    /// the running core's), with supply as the issuing network's observations add up.
    async fn evaluated(&self) -> bezent_core_service::reserve::check::Evaluation {
        let snapshot = Recorded::new(self.w.core.pool().clone())
            .attested(COPM, RAIL)
            .await
            .unwrap();
        self.readings
            .chain()
            .attested
            .insert((COPM.into(), RAIL.into()), snapshot);
        let last = self
            .read_to
            .fetch_add(10, std::sync::atomic::Ordering::Relaxed);
        read_through(&self.w.admin, ISSUING, last).await;
        sync_chain(&self.w.admin, &self.readings).await;
        self.check.evaluate(COPM, t0()).await.unwrap()
    }

    async fn holds(&self) -> bezent_core_service::reserve::check::Evaluation {
        let e = self.evaluated().await;
        assert_eq!(
            e.outcome,
            Outcome::Holds,
            "{:?} {:?}",
            e.differences,
            e.reason
        );
        assert_eq!(e.chain, e.book);
        e
    }

    async fn orders(&self, id: i64) -> i64 {
        self.w
            .count(&format!(
                "SELECT count(*) FROM intents WHERE transfer_id = {id} AND payload ? 'order'"
            ))
            .await
    }
}

fn now() -> chrono::DateTime<chrono::Utc> {
    t0() + Duration::minutes(5)
}

/// `AC`-level: `F-011` done-when, the M-3 exit criterion: "a reversal is exercised end to end".
///
/// An arrival is reported, a deposit binds it, it is minted. A reversal is reported: the reserve
/// falls, surplus takes it and falls under its floor, supply and the holder are untouched, and the
/// check still holds. The next deposit holds `CND-Paused` with nothing ordered. The restoration
/// releases it, and it is minted.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_be_6_a_reversal_end_to_end_through_the_request_surface_and_the_core(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fiat::new(options, connect).await;
    f.funded(1_000).await;

    // The mint.
    let first = f.deposited("pay-1", 5_000).await;
    f.w.cycle(now()).await;
    assert_eq!(f.w.state(first).await.0, "Source-Final");
    f.minted(first, 100, 5_000).await;
    let e = f.holds().await;
    assert_eq!(e.chain.reserve[RAIL], 6_000);
    assert_eq!(e.chain.supply, 5_000);
    assert_eq!(e.book.surplus, 1_000);
    assert!(
        !e.alerts.iter().any(|a| a.contains("reversal")),
        "{:?}",
        e.alerts
    );

    // The reversal: reported, recorded with the party's own identifiers, charged to surplus.
    f.reported(
        "rev-1",
        "reversal",
        json!({"amount": "800", "position": "acct-1", "reverses": "pay-1",
               "ids": {"chargeback": "cb-77"}}),
    )
    .await;
    f.w.cycle(now()).await;
    let detail: Value = scalar(
        &f.w.admin,
        &format!(
            "SELECT detail FROM observations \
             WHERE network = '{RAIL}' AND detail ->> 'report_id' = 'rev-1'"
        ),
    )
    .await;
    assert_eq!(detail["party"], PARTY);
    assert_eq!(detail["ids"]["chargeback"], "cb-77");
    assert_eq!(detail["reverses"], "pay-1");
    let e = f.holds().await;
    assert_eq!(e.chain.reserve[RAIL], 5_200);
    assert_eq!(e.book.surplus, 200);
    // Nothing clawed back: supply as it was, no destroy, no order touching the holder.
    assert_eq!(e.chain.supply, 5_000);
    assert_eq!(f.w.state(first).await, ("Completed".into(), None));
    assert_eq!(f.orders(first).await, 1);
    for alert in ["surplus-below-floor", "mints-paused-reversal-below-floor"] {
        assert!(
            e.alerts.contains(&alert.to_owned()),
            "{alert}: {:?}",
            e.alerts
        );
    }

    // A new deposit is taken in, final at its source, and not minted while the surplus is under.
    let second = f.deposited("pay-2", 300).await;
    f.w.settle(now()).await;
    assert_eq!(
        f.w.state(second).await,
        ("Source-Final".into(), Some("CND-Paused".into()))
    );
    assert_eq!(f.orders(second).await, 0);
    let e = f.holds().await;
    assert_eq!(e.chain.supply, 5_000);
    assert_eq!(e.chain.reserve[RAIL], 5_500);
    assert_eq!(e.chain.owed_mint.len(), 1);

    // The restoration: the reserve back, in full, from the party that reported the reversal.
    let (_, wrong) = f
        .report(
            "rest-0",
            "restoration",
            json!({"amount": "799", "position": "acct-1", "restores": "rev-1"}),
        )
        .await;
    assert_eq!(wrong["error"], "ERR-SourceMismatch", "{wrong}");
    f.reported(
        "rest-1",
        "restoration",
        json!({"amount": "800", "position": "acct-1", "restores": "rev-1"}),
    )
    .await;
    f.w.settle(now()).await;
    let e = f.holds().await;
    assert_eq!(e.book.surplus, 1_000);
    assert_eq!(e.chain.reserve[RAIL], 6_300);
    assert!(
        !e.alerts.iter().any(|a| a.contains("reversal")),
        "{:?}",
        e.alerts
    );

    // Released: the held mint goes on, and the check holds with it minted.
    assert_eq!(f.w.state(second).await.1, None);
    f.minted(second, 150, 300).await;
    let e = f.holds().await;
    assert_eq!(e.chain.supply, 5_300);
    assert!(e.chain.owed_mint.is_empty());

    // A snapshot that takes in everything reported: the equation holds on it alone.
    f.snapshot("snap-1", "rest-1", 6_300).await;
    f.w.settle(now()).await;
    let e = f.holds().await;
    assert_eq!(e.chain.reserve[RAIL], 6_300);
}

/// `REQ-BE-4`, `REQ-BE-5`, the M-3 exit criterion: a report becomes the observation, with the party's
/// ids, final at once at a coordinate of its own; and that observation, not the row, is what the
/// rest of bezent reads.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_be_5_a_report_becomes_the_observation_with_the_partys_own_identifiers(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fiat::new(options, connect).await;
    let report = f
        .reported(
            "pay-1",
            "arrival",
            json!({"amount": "500", "position": "acct-1",
                   "ids": {"order": "ord-1", "rail": "rail-9"},
                   "occurred_at": "2026-11-03T10:00:00Z"}),
        )
        .await;
    assert_eq!(
        f.w.count("SELECT count(*) FROM observations WHERE network = 'fiat-test'")
            .await,
        0,
        "the ingress makes no observation"
    );
    f.w.cycle(now()).await;
    type Seen = (i64, Vec<u8>, String, String, String, bool, Value, String);
    let (block, hash, token, amount, recipient, executed, detail, outcome): Seen = sqlx::query_as(
        "SELECT o.block_number, o.block_hash, o.token, o.amount::text, o.recipient, \
                    o.executed, o.detail, x.outcome \
             FROM observations o JOIN observation_outcomes x ON x.observation_id = o.id \
             WHERE o.network = $1",
    )
    .bind(RAIL)
    .fetch_one(&f.w.admin)
    .await
    .unwrap();
    assert_eq!(block, 1);
    assert_eq!(
        format!("0x{}", hex::encode(&hash)),
        report["coordinate"]["block_hash"].as_str().unwrap()
    );
    assert_eq!(
        (token.as_str(), amount.as_str(), recipient.as_str()),
        (COP, "500", ACCOUNTS)
    );
    assert!(executed);
    assert_eq!(outcome, "final");
    assert_eq!(detail["event"], "Arrival");
    assert_eq!(detail["party"], PARTY);
    assert_eq!(detail["report_id"], "pay-1");
    assert_eq!(detail["ids"]["rail"], "rail-9");
    assert_eq!(detail["occurred_at"], "2026-11-03T10:00:00Z");
    // A final head at its block, the report's own (REQ-OB-7); and in the book, owed to mint.
    assert_eq!(
        f.w.count("SELECT count(*) FROM final_heads WHERE network = 'fiat-test'")
            .await,
        1
    );
    assert_eq!(balance(&f.w.admin, COPM, "reserve", None).await, 500);
    // Once: another cycle records nothing new.
    f.w.settle(now()).await;
    assert_eq!(
        f.w.count("SELECT count(*) FROM observations WHERE network = 'fiat-test'")
            .await,
        1
    );
    assert_eq!(
        f.w.count("SELECT count(*) FROM event_log WHERE kind = 'attested-report-recorded'")
            .await,
        1
    );
}

/// `DEC-14`, F-011 NOTES 2: the core checks the signature itself. A row a compromised ingress wrote
/// without the party's key never becomes an observation: it pages, once, and holds its network's
/// queue, so nothing later reaches the book around it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_14_a_forged_report_row_never_becomes_an_observation_and_holds_the_queue(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fiat::new(options, connect).await;
    let body = json!({"version": 1, "report_id": "forged", "network": RAIL, "kind": "arrival",
                      "asset": COPM, "token": COP, "amount": "1000000", "position": "acct-1"});
    let bytes = body.to_string().into_bytes();
    let public = SigningKey::from_bytes(&KEY_SEED).verifying_key().to_bytes();
    let intake: i64 = sqlx::query_scalar(
        "INSERT INTO intake (request_reference, requester, kind, body, body_digest) \
         VALUES ('forged', $1, 'report', '{}', '\\x01') RETURNING id",
    )
    .bind(REQUESTER)
    .fetch_one(&f.w.admin)
    .await
    .unwrap();
    // The declared key's name, and a signature it never made.
    sqlx::query(
        "INSERT INTO attested_reports (network, seq, intake_id, requester, party, report_id, \
             kind, asset, body, signed_bytes, signature, public_key, digest) \
         VALUES ($1, 1, $2, $3, $4, 'forged', 'arrival', $5, $6, $7, $8, $9, $10)",
    )
    .bind(RAIL)
    .bind(intake)
    .bind(REQUESTER)
    .bind(PARTY)
    .bind(COPM)
    .bind(&body)
    .bind(&bytes)
    .bind(&[7u8; 64][..])
    .bind(&public[..])
    .bind(&bezent_core::attested::report_digest(&bytes)[..])
    .execute(&f.w.admin)
    .await
    .unwrap();
    // A good report behind it.
    f.reported(
        "pay-1",
        "arrival",
        json!({"amount": "500", "position": "acct-1"}),
    )
    .await;
    f.w.settle(now()).await;
    f.w.settle(now()).await;
    assert_eq!(
        f.w.count("SELECT count(*) FROM observations WHERE network = 'fiat-test'")
            .await,
        0
    );
    assert_eq!(balance(&f.w.admin, COPM, "reserve", None).await, 0);
    let pages: Vec<Value> =
        sqlx::query_scalar("SELECT payload FROM event_log WHERE kind = 'attested-report-refused'")
            .fetch_all(&f.w.admin)
            .await
            .unwrap();
    assert_eq!(pages.len(), 1, "{pages:?}");
    assert_eq!(pages[0]["report_id"], "forged");
    assert!(
        pages[0]["reason"].as_str().unwrap().contains("signature"),
        "{pages:?}"
    );
}

/// A redemption as the Kreivo endpoint records it (`payout_json`, `backend_json`).
async fn attested_redemption(
    pool: &sqlx::PgPool,
    block: i64,
    reference: [u8; 32],
    commitment: &str,
) {
    let o = observe(
        pool,
        ISSUING,
        block,
        0,
        ISSUED,
        700,
        Some(REDEEMER),
        None,
        Some(&reference),
        true,
        json!({"event": "Redeemed", "backend": {"kind": "attested", "id": 7},
               "payout": {"commitment": commitment}}),
    )
    .await;
    decide(pool, o, "final", block + 2).await;
}

/// `REQ-RQ-9`, the M-3 exit criterion "relayed instructions match their commitment", from a
/// redemption shaped as the endpoint records it: a withdrawal whose destination is the commitment,
/// never an address; instructions refused unless they hash to it; nothing ever ordered or signed;
/// the payout report posted against the reserve, once, for the withdrawal's amount; and the check
/// holds throughout.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rq_9_relayed_instructions_match_their_commitment_and_nothing_is_ordered(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fiat::new(options, connect).await;
    f.funded(1_000).await;
    let first = f.deposited("pay-1", 700).await;
    f.w.cycle(now()).await;
    f.minted(first, 100, 700).await;

    // SCALE-encoded by the party; bezent never decodes them. BLAKE2b-256 is the commitment.
    let instructions = b"\x10bank\x24acct-0042";
    let commitment = {
        use blake2::Digest as _;
        format!(
            "0x{}",
            hex::encode(blake2::Blake2b::<blake2::digest::consts::U32>::digest(
                instructions
            ))
        )
    };
    attested_redemption(&f.w.admin, 200, [9; 32], &commitment).await;
    f.w.settle(now()).await;
    let (id, state, destination, bridge): (i64, String, String, String) = sqlx::query_as(
        "SELECT id, state::text, destination, bridge_reference FROM transfers \
         WHERE kind = 'withdrawal'",
    )
    .fetch_one(&f.w.admin)
    .await
    .unwrap();
    assert_eq!(state, "Source-Final");
    assert_eq!(destination, format!("commitment:{commitment}"));
    let reference = format!("0x{}", hex::encode([9u8; 32]));
    let e = f.holds().await;
    assert_eq!(e.chain.owed_payout.values().sum::<i128>(), 700);

    // A payout before the instructions: the party has not been told where to pay.
    let payout = |id: &str, amount: &str| {
        (
            id.to_owned(),
            json!({"amount": amount, "position": "acct-1", "bridge_reference": bridge.clone()}),
        )
    };
    let (i, fields) = payout("out-0", "700");
    let (_, answer) = f.report(&i, "payout", fields).await;
    assert_eq!(answer["error"], "ERR-SourceNotFound", "{answer}");

    // Instructions that miss their commitment are refused.
    let (status, answer) = f
        .post(
            "/v0/withdrawals/extras",
            &json!({"request_reference": "x-1", "redemption_reference": reference,
                    "instructions": format!("0x{}", hex::encode(b"someone-else"))}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(answer["error"], "ERR-CommitmentMismatch", "{answer}");
    assert_eq!(
        f.w.count("SELECT count(*) FROM relayed_instructions").await,
        0
    );
    // The ones that hash to it are accepted.
    let (status, answer) = f
        .post(
            "/v0/withdrawals/extras",
            &json!({"request_reference": "x-2", "redemption_reference": reference,
                    "instructions": format!("0x{}", hex::encode(instructions))}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert!(answer.get("error").is_none(), "{answer}");
    assert_eq!(
        f.w.count("SELECT count(*) FROM relayed_instructions").await,
        1
    );

    // A payout of another amount is refused (REQ-TR-5); the right one is taken, once.
    let (i, fields) = payout("out-1", "699");
    let (_, answer) = f.report(&i, "payout", fields).await;
    assert_eq!(answer["error"], "ERR-SourceMismatch", "{answer}");
    let (i, fields) = payout("out-2", "700");
    f.reported(&i, "payout", fields).await;
    let (i, fields) = payout("out-3", "700");
    let (_, answer) = f.report(&i, "payout", fields).await;
    assert_eq!(answer["error"], "ERR-SourceConsumed", "{answer}");
    f.w.settle(now()).await;

    // Paid: out of the reserve and off owed payout, by the leg's own reference.
    let e = f.holds().await;
    assert!(e.chain.owed_payout.is_empty(), "{:?}", e.chain.owed_payout);
    assert_eq!(e.chain.reserve[RAIL], 1_000);
    assert_eq!(e.chain.supply, 0);
    assert_eq!(e.book.surplus, 1_000);

    // S2 of the review: nothing was ever ordered. The withdrawal completed on the report, by the
    // working assumption of amendment 0016 A1. No order can be recorded there, whoever writes it.
    assert_eq!(f.orders(id).await, 0);
    assert_eq!(f.w.state(id).await, ("Completed".into(), None));
    // Included and final are its milestones, resting on the report's own coordinate (REQ-TR-2).
    let (included, finished): (String, String) = sqlx::query_as(
        "SELECT destination_included_network, destination_final_network FROM transfers \
         WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&f.w.admin)
    .await
    .unwrap();
    assert_eq!((included.as_str(), finished.as_str()), (RAIL, RAIL));
    assert_eq!(
        f.w.count(&format!(
            "SELECT count(*) FROM transitions WHERE transfer_id = {id} \
             AND to_state = 'Delivering' AND attempt = 1"
        ))
        .await,
        1
    );
    assert_eq!(
        f.w.count("SELECT count(*) FROM intents WHERE network = 'fiat-test'")
            .await,
        0
    );
    let refused = sqlx::query(
        "INSERT INTO intents (transfer_id, network, purpose, on_chain_reference, reference_scope, \
             attempt, payload) \
         SELECT id, destination_network, 'payout', on_chain_reference, redeemer, 1, \
             jsonb_build_object('order', jsonb_build_object('to', destination), 'amount', '700') \
         FROM transfers WHERE id = $1",
    )
    .bind(id)
    .execute(f.w.core.pool())
    .await
    .expect_err("an order on an attested network");
    assert_eq!(
        refused
            .as_database_error()
            .and_then(|d| d.code())
            .as_deref(),
        Some("BZ001"),
        "{refused}"
    );
}

/// `REQ-OT-5`, F-011 NOTES 10: the check before a payout must not count the payout twice. The last
/// holder redeems nearly all of the reserve; once the party reports it paid, the reserve is small and
/// the withdrawal, which holds at `Source-Final`, raises no `CND-ReserveShort`.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ot_5_a_fiat_withdrawal_reported_paid_holds_no_reserve_short(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fiat::new(options, connect).await;
    f.funded(10).await;
    let first = f.deposited("pay-1", 700).await;
    f.w.cycle(now()).await;
    f.minted(first, 100, 700).await;

    let instructions = b"\x10bank\x24acct-0042";
    let commitment = {
        use blake2::Digest as _;
        format!(
            "0x{}",
            hex::encode(blake2::Blake2b::<blake2::digest::consts::U32>::digest(
                instructions
            ))
        )
    };
    attested_redemption(&f.w.admin, 200, [9; 32], &commitment).await;
    f.w.settle(now()).await;
    let (status, answer) = f
        .post(
            "/v0/withdrawals/extras",
            &json!({"request_reference": "x-1",
                    "redemption_reference": format!("0x{}", hex::encode([9u8; 32])),
                    "instructions": format!("0x{}", hex::encode(instructions))}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    let bridge = answer["bridge_reference"].as_str().unwrap();
    f.reported(
        "out-1",
        "payout",
        json!({"amount": "700", "position": "acct-1", "bridge_reference": bridge}),
    )
    .await;
    f.w.settle(now()).await;
    f.w.settle(now()).await;

    let id: i64 = scalar(
        &f.w.admin,
        "SELECT id FROM transfers WHERE kind = 'withdrawal'",
    )
    .await;
    assert_eq!(f.w.state(id).await, ("Completed".into(), None));
    assert_eq!(
        f.w.count(
            "SELECT count(*) FROM event_log WHERE kind = 'condition-raised' \
             AND payload ->> 'condition' = 'CND-ReserveShort'"
        )
        .await,
        0
    );
    assert_eq!(f.holds().await.chain.reserve[RAIL], 10);
}

/// A commitment and the instructions behind it.
fn committed() -> (&'static [u8], String) {
    let instructions: &[u8] = b"\x10bank\x24acct-0042";
    use blake2::Digest as _;
    let commitment = format!(
        "0x{}",
        hex::encode(blake2::Blake2b::<blake2::digest::consts::U32>::digest(
            instructions
        ))
    );
    (instructions, commitment)
}

/// The world up to a fiat withdrawal of 700 handed to the party: funded, deposited, minted, redeemed,
/// the instructions relayed, attempt 1 entered. Returns its id and bridge reference.
async fn handed_over(f: &Fiat) -> (i64, String) {
    f.funded(1_000).await;
    let first = f.deposited("pay-1", 700).await;
    f.w.cycle(now()).await;
    f.minted(first, 100, 700).await;
    let (instructions, commitment) = committed();
    attested_redemption(&f.w.admin, 200, [9; 32], &commitment).await;
    f.w.settle(now()).await;
    let (status, answer) = f
        .post(
            "/v0/withdrawals/extras",
            &json!({"request_reference": "x-1",
                    "redemption_reference": format!("0x{}", hex::encode([9u8; 32])),
                    "instructions": format!("0x{}", hex::encode(instructions))}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    let bridge = answer["bridge_reference"].as_str().unwrap().to_owned();
    f.w.cycle(now()).await;
    let id: i64 = scalar(
        &f.w.admin,
        "SELECT id FROM transfers WHERE kind = 'withdrawal'",
    )
    .await;
    assert_eq!(f.w.state(id).await, ("Delivering".into(), None));
    assert_eq!(f.orders(id).await, 0);
    (id, bridge)
}

/// `REQ-OT-6`, `REQ-RF-1`, `INV-8`, amendment 0016 A1 (a working assumption): a fiat payout the party
/// reports not executed, past the route's deadline, ends in one refund mint to the redeemer, on the
/// real signing path. Nothing left the reserve, supply comes back, and the check holds.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ot_6_a_fiat_payout_reported_not_executed_ends_in_one_refund_mint(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fiat::new(options, connect).await;
    let (id, bridge) = handed_over(&f).await;
    exec(
        &f.w.admin,
        "UPDATE routes SET completion_budget_seconds = 3600 WHERE asset = 'COPM'",
    )
    .await;

    // An attempt that is not the one it is at is refused.
    let (_, wrong) = f
        .report(
            "no-0",
            "payout-not-executed",
            json!({"bridge_reference": bridge, "attempt": 2}),
        )
        .await;
    assert_eq!(wrong["error"], "ERR-SourceMismatch", "{wrong}");
    f.reported(
        "no-1",
        "payout-not-executed",
        json!({"bridge_reference": bridge, "attempt": 1, "ids": {"rail": "rejected-7"}}),
    )
    .await;
    // Nor can it now be reported paid: one outcome per attempt (INV-8).
    let (_, late) = f
        .report(
            "out-1",
            "payout",
            json!({"amount": "700", "position": "acct-1", "bridge_reference": bridge}),
        )
        .await;
    assert_eq!(late["error"], "ERR-SourceConsumed", "{late}");

    f.w.cycle(now()).await;
    assert_eq!(f.w.state(id).await.0, "Refunding");
    let refund: i64 = scalar(
        &f.w.admin,
        &format!("SELECT id FROM transfers WHERE refund_of = {id}"),
    )
    .await;
    // The refund mint is an ordinary one, signed by the delivery key on the issuing network.
    f.w.settle(now()).await;
    deliver(&f.w, refund, 1, now()).await;
    assert_eq!(f.w.state(refund).await.0, "Delivering");
    let minted = observe(
        &f.w.admin,
        ISSUING,
        300,
        0,
        ISSUED,
        700,
        None,
        Some(REDEEMER),
        Some(&[9; 32]),
        true,
        json!({"event": "RefundMinted"}),
    )
    .await;
    decide(&f.w.admin, minted, "final", 302).await;
    f.w.settle(now()).await;
    assert_eq!(f.w.state(refund).await.0, "Completed");
    assert_eq!(f.w.state(id).await.0, "Refunded");
    assert_eq!(
        f.w.count(&format!(
            "SELECT count(*) FROM transfers WHERE refund_of = {id}"
        ))
        .await,
        1
    );
    assert_eq!(f.orders(id).await, 0);

    let e = f.holds().await;
    assert_eq!(e.chain.reserve[RAIL], 1_700, "nothing left the reserve");
    assert_eq!(e.chain.supply, 700, "the redeemer holds it again");
    assert!(e.chain.owed_payout.is_empty());
    assert_eq!(e.book.surplus, 1_000);
}

/// `REQ-RF-5`, a working assumption of amendment 0016 A1: a payout reported not executed before the
/// deadline gets a new attempt, resting on that report, and completes on its payout.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_5_a_fiat_payout_not_executed_within_its_deadline_gets_a_new_attempt(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fiat::new(options, connect).await;
    let (id, bridge) = handed_over(&f).await;
    f.reported(
        "no-1",
        "payout-not-executed",
        json!({"bridge_reference": bridge, "attempt": 1}),
    )
    .await;
    f.w.cycle(now()).await;
    assert_eq!(f.w.state(id).await.0, "Delivering");
    let attempts: Vec<i32> = sqlx::query_scalar(
        "SELECT attempt FROM transitions WHERE transfer_id = $1 AND attempt IS NOT NULL \
         ORDER BY id",
    )
    .bind(id)
    .fetch_all(&f.w.admin)
    .await
    .unwrap();
    assert_eq!(attempts, [1, 2]);
    assert_eq!(
        f.w.count(&format!(
            "SELECT count(*) FROM transfers WHERE refund_of = {id}"
        ))
        .await,
        0
    );
    f.reported(
        "out-2",
        "payout",
        json!({"amount": "700", "position": "acct-1", "bridge_reference": bridge}),
    )
    .await;
    f.w.settle(now()).await;
    assert_eq!(f.w.state(id).await, ("Completed".into(), None));
    let e = f.holds().await;
    assert_eq!(e.chain.reserve[RAIL], 1_000);
    assert!(e.chain.owed_payout.is_empty());
    // Paid, it cannot be reported not executed afterwards: that would be a reversal.
    let (_, late) = f
        .report(
            "no-2",
            "payout-not-executed",
            json!({"bridge_reference": bridge, "attempt": 2}),
        )
        .await;
    assert_eq!(late["error"], "ERR-SourceConsumed", "{late}");
}

/// `REQ-RV-1`, `REQ-RF-1`: no refund on a timer. A fiat payout nobody reports goes to review at its
/// deadline, with nothing refunded; a report of non-execution is evidence for that review, and the
/// review still ends only by its resolution.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_1_a_fiat_payout_unreported_past_its_deadline_goes_to_review_and_is_not_refunded(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fiat::new(options, connect).await;
    let (id, bridge) = handed_over(&f).await;
    exec(
        &f.w.admin,
        "UPDATE routes SET completion_budget_seconds = 3600 WHERE asset = 'COPM'",
    )
    .await;
    f.w.settle(now()).await;
    assert_eq!(f.w.state(id).await.0, "Needs-Review");
    f.reported(
        "no-1",
        "payout-not-executed",
        json!({"bridge_reference": bridge, "attempt": 1}),
    )
    .await;
    f.w.settle(now()).await;
    assert_eq!(f.w.state(id).await.0, "Needs-Review");
    assert_eq!(
        f.w.count(&format!(
            "SELECT count(*) FROM transfers WHERE refund_of = {id}"
        ))
        .await,
        0
    );
    assert_eq!(
        f.w.count(&format!(
            "SELECT count(*) FROM event_log WHERE kind = 'review-evidence' AND transfer_id = {id} \
             AND payload ->> 'kind' = 'not-executed'"
        ))
        .await,
        1
    );
}

/// `REQ-RQ-9`, F-011 NOTES 10: what the store holds as relayed is checked again before anything is
/// handed over: a row that misses the commitment pages once, and the store's own rule stands
/// beside it. And a withdrawal with no instructions waits, past any deadline, with nothing moved.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rq_9_a_relayed_row_that_misses_the_commitment_hands_nothing_over(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fiat::new(options, connect).await;
    f.funded(1_000).await;
    let first = f.deposited("pay-1", 700).await;
    f.w.cycle(now()).await;
    f.minted(first, 100, 700).await;
    let (_, commitment) = committed();
    attested_redemption(&f.w.admin, 200, [9; 32], &commitment).await;
    f.w.settle(now()).await;
    let id: i64 = scalar(
        &f.w.admin,
        "SELECT id FROM transfers WHERE kind = 'withdrawal'",
    )
    .await;
    assert_eq!(f.w.state(id).await.0, "Source-Final");

    // No instructions: the store refuses the hand-over, even from the core's own login.
    let refused = sqlx::query(
        "INSERT INTO transitions (transfer_id, from_state, to_state, attempt) \
         VALUES ($1, 'Source-Final', 'Delivering', 1)",
    )
    .bind(id)
    .execute(f.w.core.pool())
    .await
    .expect_err("Delivering with no instructions");
    assert_eq!(
        refused
            .as_database_error()
            .and_then(|d| d.code())
            .as_deref(),
        Some("BZ001"),
        "{refused}"
    );

    // A row a compromised ingress wrote, under the right commitment, over other bytes.
    exec(
        &f.w.admin,
        &format!(
            "INSERT INTO intake (request_reference, requester, kind, body, body_digest) \
               VALUES ('forged', '{REQUESTER}', 'withdrawal', '{{}}', '\\x01'); \
             INSERT INTO relayed_instructions (transfer_id, intake_id, instructions, commitment) \
               SELECT {id}, i.id, '\\x01', decode('{}', 'hex') FROM intake i \
               WHERE i.request_reference = 'forged';",
            commitment.strip_prefix("0x").unwrap()
        ),
    )
    .await;
    f.w.settle(now()).await;
    f.w.settle(now()).await;
    assert_eq!(f.w.state(id).await.0, "Source-Final");
    assert_eq!(
        f.w.count(&format!(
            "SELECT count(*) FROM event_log WHERE kind = 'attested-instructions-refused' \
             AND transfer_id = {id}"
        ))
        .await,
        1
    );
}

/// `REQ-RQ-6`, `REQ-IN-6`, `REQ-BE-5`: the refund of an unassociated arrival on an attested network:
/// released by a second approver, handed to the party with nothing ordered, completed by its report,
/// and off the book's owed mints. Amendment 0016 A1, a working assumption.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rq_6_the_refund_of_an_unassociated_fiat_arrival_completes_on_its_report(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fiat::new(options, connect).await;
    f.funded(1_000).await;
    let report = f
        .reported(
            "stray-1",
            "arrival",
            json!({"amount": "400", "position": "acct-1"}),
        )
        .await;
    f.w.settle(now()).await;
    assert_eq!(f.holds().await.chain.owed_mint.len(), 1);

    let (status, answer) = f
        .post(
            "/v0/refunds",
            &json!({"request_reference": "ref-1", "coordinate": report["coordinate"],
                    "destination": {"network": RAIL, "address": "the-payer"}}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["state"], "Received", "{answer}");
    let bridge = answer["bridge_reference"].as_str().unwrap().to_owned();
    let id: i64 = scalar(
        &f.w.admin,
        &format!("SELECT id FROM transfers WHERE bridge_reference = '{bridge}'"),
    )
    .await;
    // Not released: not payable, and not moved.
    let payout = json!({"amount": "400", "position": "acct-1", "bridge_reference": bridge});
    let (status, answer) = f.report("out-0", "payout", payout.clone()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{answer}");
    let action: i64 = scalar(
        &f.w.ops[0],
        &format!(
            "INSERT INTO manual_actions (kind, transfer_id, justification) \
             VALUES ('release-unassociated', {id}, 'the payer asked') RETURNING id"
        ),
    )
    .await;
    f.w.settle(now()).await;
    assert_eq!(f.w.state(id).await.0, "Received");
    approve_as(&f.w, 1, action).await.unwrap();
    f.w.settle(now()).await;
    assert_eq!(f.w.state(id).await, ("Delivering".into(), None));
    assert_eq!(f.orders(id).await, 0);

    f.reported("out-1", "payout", payout).await;
    f.w.settle(now()).await;
    assert_eq!(f.w.state(id).await, ("Completed".into(), None));
    let e = f.holds().await;
    assert!(e.chain.owed_mint.is_empty(), "{:?}", e.chain.owed_mint);
    assert_eq!(e.chain.reserve[RAIL], 1_000);
    assert_eq!(e.book.surplus, 1_000);
}

/// Amendment 0016 A3, a working assumption: an arrival reversed and not restored binds nothing, and a
/// deposit bound before the reversal holds unminted. The surplus is ample here, so it is this rule
/// that holds it and not `REQ-BE-6`'s floor. The restoration releases both.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_be_6_an_arrival_reversed_before_its_mint_binds_nothing_and_holds_its_deposit(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fiat::new(options, connect).await;
    f.funded(100_000).await;

    // Reversed before anything binds it: neither a deposit nor a refund may.
    let unbound = f
        .reported(
            "pay-1",
            "arrival",
            json!({"amount": "500", "position": "acct-1"}),
        )
        .await;
    f.reported(
        "rev-1",
        "reversal",
        json!({"amount": "200", "position": "acct-1", "reverses": "pay-1"}),
    )
    .await;
    f.w.settle(now()).await;
    let deposit = |reference: &str| {
        json!({"request_reference": reference, "coordinate": unbound["coordinate"],
               "destination": {"network": ISSUING, "address": DESTINATION},
               "refund_destination": {"network": RAIL, "address": "the-payer"}})
    };
    let (status, answer) = f.post("/v0/deposits", &deposit("dep-1")).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["error"], "ERR-SourceConsumed", "{answer}");
    let (_, answer) = f
        .post(
            "/v0/refunds",
            &json!({"request_reference": "ref-1", "coordinate": unbound["coordinate"],
                    "destination": {"network": RAIL, "address": "the-payer"}}),
        )
        .await;
    assert_eq!(answer["error"], "ERR-SourceConsumed", "{answer}");

    // Bound first, reversed before its mint is signed: it holds, with no new order.
    let held = f.deposited("pay-2", 300).await;
    f.w.cycle(now()).await;
    assert_eq!(f.w.state(held).await.0, "Source-Final");
    f.reported(
        "rev-2",
        "reversal",
        json!({"amount": "300", "position": "acct-1", "reverses": "pay-2"}),
    )
    .await;
    f.w.settle(now()).await;
    assert_eq!(
        f.w.state(held).await,
        ("Source-Final".into(), Some("CND-Paused".into()))
    );
    let e = f.holds().await;
    assert_eq!(e.chain.supply, 0);
    assert!(
        !e.alerts
            .contains(&"mints-paused-reversal-below-floor".to_owned()),
        "the surplus is above its floor: {:?}",
        e.alerts
    );

    // Restored: the first arrival binds, and the held deposit goes on to its mint.
    for (id, reversal, amount) in [("rest-1", "rev-1", "200"), ("rest-2", "rev-2", "300")] {
        f.reported(
            id,
            "restoration",
            json!({"amount": amount, "position": "acct-1", "restores": reversal}),
        )
        .await;
    }
    let (status, answer) = f.post("/v0/deposits", &deposit("dep-2")).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["state"], "Accepted", "{answer}");
    f.w.cycle(now()).await;
    assert_eq!(f.w.state(held).await.1, None);
    f.minted(held, 100, 300).await;
    let e = f.holds().await;
    assert_eq!(e.chain.supply, 300);
}

/// F-011 NOTES 7, the owner's question 4, a working assumption: the snapshot's limits are
/// configuration. Past its interval an alert and nothing else; past its maximum age the asset's routes
/// pause and a human is paged.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_1_a_stale_snapshot_alerts_past_its_interval_and_pauses_past_its_bound(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fiat::new(options, connect).await;
    f.funded(1_000).await;
    f.holds().await;
    let snapshot = Recorded::new(f.w.core.pool().clone())
        .attested(COPM, RAIL)
        .await
        .unwrap();
    f.readings
        .chain()
        .attested
        .insert((COPM.into(), RAIL.into()), snapshot.clone());
    let check = |max_age: i64, interval: i64| {
        ReserveCheck::new(
            f.w.core.pool().clone(),
            f.readings.clone(),
            FakePager::default(),
        )
        .with_attested_snapshots(AttestedSnapshots {
            max_age: Duration::seconds(max_age),
            interval: Duration::seconds(interval),
        })
    };
    let at = |minutes: i64| snapshot.reported_at + Duration::minutes(minutes);
    read_through(&f.w.admin, ISSUING, (at(30) - t0()).num_seconds()).await;

    // Within the interval: nothing.
    let e = check(3_600, 600).evaluate(COPM, at(5)).await.unwrap();
    assert_eq!(e.outcome, Outcome::Holds, "{:?}", e.differences);
    let overdue = format!("attested-snapshot-overdue:{RAIL}");
    assert!(!e.alerts.contains(&overdue), "{:?}", e.alerts);
    // Past it: an alert, and it still holds.
    let e = check(3_600, 600).evaluate(COPM, at(15)).await.unwrap();
    assert_eq!(e.outcome, Outcome::Holds, "{:?}", e.differences);
    assert!(e.alerts.contains(&overdue), "{:?}", e.alerts);
    assert_eq!(
        f.w.count("SELECT count(*) FROM pauses WHERE asset = 'COPM'")
            .await,
        0
    );
    // Past a shorter bound, as the configuration says: paused.
    let e = check(1_200, 600).evaluate(COPM, at(30)).await.unwrap();
    assert_eq!(e.outcome, Outcome::Diverges, "{:?}", e.differences);
    assert!(
        e.differences.iter().any(|d| d.term == "final_head"),
        "{:?}",
        e.differences
    );
    assert_eq!(
        f.w.count("SELECT count(*) FROM pauses WHERE asset = 'COPM' AND cause = 'REQ-BK-1'")
            .await,
        1
    );
}

/// `REQ-RQ-9`: a redemption without a whole commitment makes no withdrawal to the backend.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rq_9_a_redemption_with_no_whole_commitment_makes_no_withdrawal(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fiat::new(options, connect).await;
    attested_redemption(&f.w.admin, 200, [9; 32], "0x5e7c").await;
    f.w.settle(now()).await;
    assert_eq!(
        f.w.count("SELECT count(*) FROM transfers WHERE kind = 'withdrawal'")
            .await,
        0
    );
}

/// `REQ-BK-1`, `REQ-BK-8`: the check is not a formality here. A snapshot that disagrees with what the
/// party reported, to the smallest unit, diverges and pauses the asset's routes.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_1_a_snapshot_that_disagrees_with_the_reports_diverges_and_pauses(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fiat::new(options, connect).await;
    f.funded(1_000).await;
    f.holds().await;
    f.snapshot("snap-1", "fund-1", 999).await;
    f.w.settle(now()).await;
    let e = f.evaluated().await;
    assert_eq!(e.outcome, Outcome::Diverges, "{:?}", e.reason);
    assert!(
        e.differences.iter().any(|d| d.term == "reserve"),
        "{:?}",
        e.differences
    );
    assert_eq!(
        f.w.count("SELECT count(*) FROM pauses WHERE asset = 'COPM' AND cause = 'REQ-BK-1'")
            .await,
        1
    );
}

/// F-011 NOTES 6, 7: an arrival reported after the snapshot is no divergence, whatever the mint
/// rests on; before any snapshot the evaluation is incomplete, never a reserve of zero; and a
/// snapshot past the network's bound does not hold.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_be_4_the_snapshot_rolls_forward_and_may_not_be_missing_or_stale(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fiat::new(options, connect).await;
    // None yet.
    let missing = Recorded::new(f.w.core.pool().clone())
        .attested(COPM, RAIL)
        .await
        .expect_err("no report yet");
    assert!(missing.0.contains("no report"), "{missing}");

    f.funded(1_000).await;
    // A mint resting on the issuing network, of an arrival after the snapshot.
    let id = f.deposited("pay-1", 5_000).await;
    f.w.cycle(now()).await;
    f.minted(id, 100, 5_000).await;
    let e = f.holds().await;
    assert_eq!(e.chain.reserve[RAIL], 6_000);
    assert_eq!(e.points[RAIL].block_number, 3);

    // Past the bound (3600 s here): the point cannot hold.
    let later = chrono::Utc::now() + Duration::hours(2);
    read_through(&f.w.admin, ISSUING, (later - t0()).num_seconds()).await;
    let e = f.check.evaluate(COPM, later).await.unwrap();
    assert_eq!(e.outcome, Outcome::Diverges, "{e:?}");
    assert!(
        e.differences
            .iter()
            .any(|d| d.term == "final_head" && d.item.as_deref().is_some_and(|i| i.contains(RAIL))),
        "{:?}",
        e.differences
    );
}

/// `REQ-BE-3`, `REQ-BE-4`, `REQ-BK-9`, the M-3 exit criterion: the statement says the reserve is
/// attested and names the party; it signs and verifies as any other.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_be_4_the_statement_says_the_reserve_is_attested_and_names_the_party(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fiat::new(options, connect).await;
    f.funded(1_000).await;
    f.holds().await;

    let seed: [u8; 32] = Sha256::digest(b"bezent attested statement test key").into();
    let backend = FileBackend::parse(
        json!({"comment": "TEST KEY", "environment": "test",
               "keys": [{"id": "statement", "scheme": "ed25519",
                         "seed": format!("0x{}", hex::encode(seed))}]})
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    let public = backend.public_key("statement").unwrap();
    let dir = std::env::temp_dir().join(format!("bz-attested-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let statements = Statements::new(
        f.w.core.pool().clone(),
        KeyBackendSigner::new(backend, "statement").unwrap(),
        DirectoryStore::new(&dir),
    );
    let p = statements.publish(COPM, t0()).await.unwrap();
    let signature = bezent_core_service::reserve::statement::StatementStore::get(
        statements.store(),
        &p.signature_name,
    )
    .await
    .unwrap()
    .unwrap();
    let doc =
        bezent_core_service::reserve::statement::verify(&p.bytes, &signature, &public).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(doc["backend"]["assurance_class"], "attested");
    assert_eq!(doc["backend"]["attested_by"], json!([PARTY]));
    assert_eq!(doc["backend"]["networks"][0]["backend"], "attested:7");
    let said = doc["reserve_attested"].as_str().unwrap();
    assert!(said.contains("is attested"), "{said}");
    assert!(said.contains(PARTY), "{said}");
    assert_eq!(
        doc["rest_on_reports"],
        json!(["reserve", "owed_mint", "owed_payout", "surplus"])
    );
    assert_eq!(
        doc["recomputable_from_public_data"],
        json!(["supply", "in_transit"])
    );
    assert_eq!(doc["terms"]["reserve"][0]["attested_by"], PARTY);
    assert_eq!(doc["terms"]["reserve"][0]["report"], "snap-0");
}
