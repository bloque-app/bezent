// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! T-002-02 and T-002-03: requester credentials and grants, and the deposit, withdrawal-extras and
//! refund requests, against PostgreSQL (`REQ-PN-1`, `REQ-PN-2`, `REQ-RQ-1`, `REQ-RQ-3`,
//! `REQ-RQ-4`, `REQ-RQ-6`, `REQ-RQ-7`, `REQ-RQ-9`, `REQ-ID-1`, `REQ-ID-3`, `REQ-ID-4`,
//! `AC-D1.1`, `AC-D1.2`).

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;

fn assert_refused(body: &Value, error: &str) {
    assert_eq!(body["state"], "Refused", "{body}");
    assert_eq!(body["error"], error, "{body}");
}

// --- REQ-PN-1, REQ-PN-2 (T-002-02) ---------------------------------------------------------

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pn_1_unauthenticated_requests_are_refused(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    let body = deposit_body("ref-1", arrival(&pool, 1, 1_000).await);
    for credential in [
        None,
        Some("alpha-k1.wrong-secret"),
        Some("nobody.alpha-secret-0123456789"),
        Some("alpha-k1"),
        Some(""),
    ] {
        let (status, answer) = ingress
            .call("POST", "/v0/deposits", credential, Some(&body))
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{credential:?}");
        assert_eq!(answer["error"], "ERR-NotAuthorised");
        let (status, _) = ingress
            .call("GET", "/v0/transfers/by-request/ref-1", credential, None)
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(count(&pool, "intake").await, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pn_2_requests_outside_the_grant_are_refused(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    let coordinate = arrival(&pool, 1, 1_000).await;

    // gamma holds no grant; beta may request deposits but not refunds or withdrawals' extras.
    let (status, answer) = ingress
        .post(
            "/v0/deposits",
            GAMMA,
            &deposit_body("ref-1", coordinate.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(answer["error"], "ERR-NotAuthorised");
    let refund = json!({
        "request_reference": "ref-2",
        "coordinate": coordinate,
        "destination": { "network": ORIGIN, "address": ORIGIN_ADDRESS },
    });
    let (status, _) = ingress.post("/v0/refunds", BETA, &refund).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let extras = json!({
        "request_reference": "ref-3",
        "redemption_reference": "0x01",
        "expires_at": "2026-12-01T00:00:00Z",
    });
    let (status, _) = ingress.post("/v0/withdrawals/extras", BETA, &extras).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(count(&pool, "intake").await, 0);

    // Within its grant, beta's request is taken in, and recorded as beta's (REQ-PN-1).
    let (status, _) = ingress
        .post("/v0/deposits", BETA, &deposit_body("ref-1", coordinate))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let requester: String = scalar(&pool, "SELECT requester FROM intake").await;
    assert_eq!(requester, "beta");
}

// --- REQ-RQ-1, REQ-RQ-3, REQ-ID-3 ------------------------------------------------------------

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rq_1_a_request_is_taken_in_durably_before_it_is_answered(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    let (status, answer) = ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("ref-1", arrival(&pool, 1, 1_000).await),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["state"], "Accepted");
    assert_eq!(answer["kind"], "deposit");
    assert_eq!(answer["request_reference"], "ref-1");

    // The answer is what the store holds, committed (REQ-RQ-1, REQ-ST-7).
    let bridge_reference = answer["bridge_reference"].as_str().unwrap();
    let (state, answered): (String, String) = sqlx::query_as(
        "SELECT t.state::text, a.bridge_reference FROM intake i \
         JOIN intake_answers a ON a.intake_id = i.id JOIN transfers t ON t.id = a.transfer_id \
         WHERE i.requester = 'alpha' AND i.request_reference = 'ref-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "Accepted");
    assert_eq!(answered, bridge_reference);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_id_3_the_bridge_reference_is_bezents_own(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    let (_, answer) = ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("ref-1", arrival(&pool, 1, 1_000).await),
        )
        .await;
    let bridge_reference = answer["bridge_reference"].as_str().unwrap();
    assert_ne!(bridge_reference, "ref-1");
    // Derived from the coordinate (REQ-IN-3): it does not depend on the requester's reference.
    assert!(bridge_reference.starts_with("bz-d-"), "{bridge_reference}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rq_7_a_request_carrying_an_outcome_is_refused(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    let coordinate = arrival(&pool, 1, 1_000).await;
    for (field, value) in [
        ("amount", json!("999999")),
        ("final", json!(true)),
        ("finality", json!("final")),
        ("status", json!("Completed")),
        ("executed", json!(true)),
    ] {
        let mut body = deposit_body("ref-1", coordinate.clone());
        body[field] = value;
        let (status, answer) = ingress.post("/v0/deposits", ALPHA, &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{field}");
        assert_eq!(answer["error"], "ERR-Malformed");
        assert!(
            answer["detail"].as_str().unwrap().contains(field),
            "{answer}"
        );
    }
    // Inside the coordinate, too.
    let mut body = deposit_body("ref-1", coordinate.clone());
    body["coordinate"]["final"] = json!(true);
    let (status, _) = ingress.post("/v0/deposits", ALPHA, &body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(count(&pool, "intake").await, 0);

    // The amount is the observation's, never the request's.
    let (_, answer) = ingress
        .post("/v0/deposits", ALPHA, &deposit_body("ref-1", coordinate))
        .await;
    assert_eq!(answer["amount"], "1000");
}

/// F-004 review MF1: an arrival at the reserve from an address the operator declared it funds the
/// reserve from is a surplus contribution, not a movement a deposit can bind. The requester gets
/// `ERR-SourceNotFound` ("the coordinate named does not hold a movement bezent can bind"), so no
/// one can have an operator top-up minted to them (`INV-1`, `INV-2`, `REQ-IN-3`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_1_inv_2_req_in_3_mf1_a_deposit_never_binds_an_operator_top_up(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    // Declared in another case: EVM addresses compare without regard to it.
    exec(
        &pool,
        &format!(
            "INSERT INTO surplus_funding_addresses (asset, network, address, reference) \
             VALUES ('{ASSET}', '{ORIGIN}', '0xFUNDER', 'funding-declaration-1')"
        ),
    )
    .await;
    let top_up = arrival(&pool, 5, 5_000).await;
    let (status, answer) = ingress
        .post("/v0/deposits", ALPHA, &deposit_body("ref-1", top_up))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_refused(&answer, "ERR-SourceNotFound");
    let bound: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM transfers WHERE source_network IS NOT NULL AND kind = 'deposit'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(bound, 0);
}

/// The reviewer's probe (SF-A): a declared funder sends 5,000 and a refund request names the
/// coordinate. The same rule as a deposit's: `ERR-SourceNotFound`, nothing bound.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_2_req_rq_6_sf_a_a_refund_never_binds_an_operator_top_up(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    exec(
        &pool,
        &format!(
            "INSERT INTO surplus_funding_addresses (asset, network, address, reference) \
             VALUES ('{ASSET}', '{ORIGIN}', '0xFUNDER', 'funding-declaration-1')"
        ),
    )
    .await;
    let top_up = arrival(&pool, 5, 5_000).await;
    let refund = json!({
        "request_reference": "refund-1",
        "coordinate": top_up,
        "destination": { "network": ORIGIN, "address": ORIGIN_ADDRESS },
    });
    let (status, answer) = ingress.post("/v0/refunds", ALPHA, &refund).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_refused(&answer, "ERR-SourceNotFound");
    let bound: i64 =
        sqlx::query_scalar("SELECT count(*) FROM transfers WHERE source_network IS NOT NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(bound, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rq_3_a_deposit_binds_only_an_observed_arrival_at_a_reserve(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;

    // Nothing observed at the coordinate.
    let (status, answer) = ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("ref-1", coordinate(99)),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_refused(&answer, "ERR-SourceNotFound");
    assert!(
        answer["bridge_reference"]
            .as_str()
            .unwrap()
            .starts_with("bz-r-")
    );

    // A movement that is not to the reserve.
    let elsewhere = observe(&pool, 2, TOKEN, 1_000, ORIGIN_ADDRESS).await;
    let (_, answer) = ingress
        .post("/v0/deposits", ALPHA, &deposit_body("ref-2", elsewhere))
        .await;
    assert_refused(&answer, "ERR-SourceNotFound");

    // An observation withdrawn before finality (REQ-IN-7).
    let withdrawn = observe_seen(
        &pool,
        3,
        Seen {
            outcome: Some("withdrawn"),
            ..arrival_seen(1_000)
        },
    )
    .await;
    let (_, answer) = ingress
        .post("/v0/deposits", ALPHA, &deposit_body("ref-3", withdrawn))
        .await;
    assert_refused(&answer, "ERR-SourceNotFound");

    // A token not admitted: no route covers it.
    let unadmitted = observe(&pool, 4, "not-admitted", 1_000, RESERVE).await;
    let (_, answer) = ingress
        .post("/v0/deposits", ALPHA, &deposit_body("ref-4", unadmitted))
        .await;
    assert_refused(&answer, "ERR-UnknownRoute");

    // None of them made a transfer; each refusal is recorded and logged (REQ-ST-4).
    assert_eq!(count(&pool, "transfers").await, 0);
    assert_eq!(count(&pool, "intake_answers").await, 4);
    let refusals: i64 = scalar(
        &pool,
        "SELECT count(*) FROM event_log WHERE kind = 'refusal'",
    )
    .await;
    assert_eq!(refusals, 4);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_2_a_coordinate_is_consumed_once(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    let coordinate = arrival(&pool, 1, 1_000).await;
    let (_, first) = ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("ref-1", coordinate.clone()),
        )
        .await;
    assert_eq!(first["state"], "Accepted");

    // Another reference, by the same requester or another, cannot bind it again.
    for (credential, reference) in [(ALPHA, "ref-2"), (BETA, "ref-1")] {
        let (status, answer) = ingress
            .post(
                "/v0/deposits",
                credential,
                &deposit_body(reference, coordinate.clone()),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_refused(&answer, "ERR-SourceConsumed");
        assert!(
            answer.get("source").is_none(),
            "a refusal binds nothing: {answer}"
        );
    }
    let bound: i64 = scalar(
        &pool,
        "SELECT count(*) FROM transfers WHERE source_network IS NOT NULL",
    )
    .await;
    assert_eq!(bound, 1);
}

// --- REQ-IN-5: a ledger's contract account is never a destination ----------------------------

/// Alice's account under the Polkadot prefix: the same account as `ISSUING_ADDRESS`, spelled
/// differently.
const ISSUING_ADDRESS_OTHER_PREFIX: &str = "15oF4uVJwmo4TdGW7VfQxNLavjCXviqxT9S1MgbjMNHr6Sp5";
/// Bob's account: well formed, and not declared.
const BOB: &str = "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty";

async fn declare_ledger_account(pool: &PgPool, network: &str, address: &str) {
    common::exec(
        pool,
        &format!(
            "INSERT INTO ledger_accounts (network, address, reason) \
             VALUES ('{network}', '{address}', 'a ledger holds value here')"
        ),
    )
    .await;
}

fn assert_explained(body: &Value) {
    let detail = body["detail"].as_str().expect("the refusal is explained");
    assert!(detail.contains("ledger"), "{body}");
    // No vendor, no product, no network is named.
    for name in ["aledgent", "Kreivo", "kreivo", "Swaps", "Polygon", "Base"] {
        assert!(!detail.contains(name), "{detail}");
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_5_a_deposit_to_a_declared_ledger_account_is_refused(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    declare_ledger_account(&pool, ISSUING, ISSUING_ADDRESS).await;

    // The declared address, and the same account spelled under another prefix.
    for (block, address) in (1..).zip([ISSUING_ADDRESS, ISSUING_ADDRESS_OTHER_PREFIX]) {
        let mut body = deposit_body(&format!("ref-{block}"), arrival(&pool, block, 1_000).await);
        body["destination"] = json!({ "network": ISSUING, "address": address });
        let (_, answer) = ingress.post("/v0/deposits", ALPHA, &body).await;
        assert_refused(&answer, "ERR-InvalidDestination");
        assert_explained(&answer);

        // A repeat answers alike.
        let (_, repeat) = ingress.post("/v0/deposits", ALPHA, &body).await;
        assert_eq!(repeat["detail"], answer["detail"]);
    }

    // Another account on the same network is taken in.
    let mut body = deposit_body("ref-ok", arrival(&pool, 10, 1_000).await);
    body["destination"] = json!({ "network": ISSUING, "address": BOB });
    let (status, answer) = ingress.post("/v0/deposits", ALPHA, &body).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["state"], "Accepted", "{answer}");
    assert!(answer.get("detail").is_none(), "{answer}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_5_a_refund_destination_on_the_list_is_refused(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    declare_ledger_account(&pool, ORIGIN, ORIGIN_ADDRESS).await;

    // The refund destination of a deposit, in another case.
    let mut body = deposit_body("ref-1", arrival(&pool, 1, 1_000).await);
    body["refund_destination"] = json!({
        "network": ORIGIN,
        "address": ORIGIN_ADDRESS.to_ascii_lowercase(),
    });
    let (_, answer) = ingress.post("/v0/deposits", ALPHA, &body).await;
    assert_refused(&answer, "ERR-InvalidDestination");
    assert_explained(&answer);

    // The destination of the refund of an unassociated arrival.
    let refund = json!({
        "request_reference": "refund-1",
        "coordinate": arrival(&pool, 2, MAXIMUM + 1).await,
        "destination": { "network": ORIGIN, "address": ORIGIN_ADDRESS },
    });
    let (_, answer) = ingress.post("/v0/refunds", ALPHA, &refund).await;
    assert_refused(&answer, "ERR-InvalidDestination");
    assert_explained(&answer);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_5_the_list_is_written_by_the_owner_or_the_operator_only(pool: PgPool) {
    let _ = Ingress::new(&pool).await;
    declare_ledger_account(&pool, ISSUING, ISSUING_ADDRESS).await;
    // Recorded with its actor and its reason, and never edited.
    let actor: String = common::scalar(&pool, "SELECT actor FROM ledger_accounts").await;
    assert!(!actor.is_empty());
    for sql in [
        "UPDATE ledger_accounts SET reason = 'x'",
        "DELETE FROM ledger_accounts",
        "TRUNCATE ledger_accounts",
    ] {
        assert!(sqlx::raw_sql(sql).execute(&pool).await.is_err(), "{sql}");
    }
    assert!(
        sqlx::raw_sql(
            "INSERT INTO ledger_accounts (network, address, reason) VALUES ('kreivo', 'x', '')"
        )
        .execute(&pool)
        .await
        .is_err(),
        "a declaration carries its reason"
    );
    // The service role reads the list and cannot write it; the operator appends only.
    let service_writes: bool = common::scalar(
        &pool,
        "SELECT has_table_privilege('bezent_service', 'ledger_accounts', 'INSERT') \
           OR has_table_privilege('bezent_service', 'ledger_accounts', 'UPDATE')",
    )
    .await;
    assert!(!service_writes);
    let operator_appends: bool = common::scalar(
        &pool,
        "SELECT has_column_privilege('bezent_operator', 'ledger_accounts', 'reason', 'INSERT') \
           AND NOT has_table_privilege('bezent_operator', 'ledger_accounts', 'UPDATE')",
    )
    .await;
    assert!(operator_appends);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn err_invalid_destination_is_refused_at_request(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    let cases = [
        // The destination is on the issuing network, in its format.
        (
            "destination",
            json!({ "network": ORIGIN, "address": ORIGIN_ADDRESS }),
        ),
        (
            "destination",
            json!({ "network": ISSUING, "address": "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQX" }),
        ),
        // The refund destination is on the arrival's network (REQ-OT-4), in its format.
        (
            "refund_destination",
            json!({ "network": ISSUING, "address": ISSUING_ADDRESS }),
        ),
        (
            "refund_destination",
            json!({ "network": ORIGIN, "address": "0x1234" }),
        ),
    ];
    for (block, (field, value)) in (1..).zip(cases) {
        let mut body = deposit_body(&format!("ref-{block}"), arrival(&pool, block, 1_000).await);
        body[field] = value;
        let (_, answer) = ingress.post("/v0/deposits", ALPHA, &body).await;
        assert_refused(&answer, "ERR-InvalidDestination");
    }
}

// --- REQ-ID-1: AC-D1.1, AC-D1.2 ---------------------------------------------------------------

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_d1_1_a_repeat_with_the_same_reference_and_body_returns_the_original_status(
    pool: PgPool,
) {
    let ingress = Ingress::new(&pool).await;
    let body = deposit_body("ref-1", arrival(&pool, 1, 1_000).await);
    let (status, original) = ingress.post("/v0/deposits", ALPHA, &body).await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, repeat) = ingress.post("/v0/deposits", ALPHA, &body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(repeat, original);

    // The same body written differently (the hash in capitals) is the same body.
    let mut same = body.clone();
    same["coordinate"]["block_hash"] = json!(
        body["coordinate"]["block_hash"]
            .as_str()
            .unwrap()
            .to_uppercase()
            .replacen("0X", "0x", 1)
    );
    let (status, repeat) = ingress.post("/v0/deposits", ALPHA, &same).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(repeat, original);

    // Neither repeat started a second transfer, nor was recorded again.
    assert_eq!(count(&pool, "intake").await, 1);
    assert_eq!(count(&pool, "transfers").await, 1);

    // A refusal repeats as itself, too.
    let refused = deposit_body("ref-2", coordinate(99));
    let (_, first) = ingress.post("/v0/deposits", ALPHA, &refused).await;
    let (status, again) = ingress.post("/v0/deposits", ALPHA, &refused).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again, first);
    assert_refused(&again, "ERR-SourceNotFound");

    // The original's status is returned as it stands now, not as it was first answered. The
    // endpoint read the final head its arrival's outcome names (the store moves nothing to
    // Source-Final on less, review M1 of PR #56).
    exec(
        &pool,
        "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
           SELECT o.network, x.final_block_number, x.final_block_hash, '2026-10-13T00:01:00Z' \
           FROM observation_outcomes x JOIN observations o ON o.id = x.observation_id \
           WHERE x.outcome = 'final' ON CONFLICT DO NOTHING; \
         UPDATE transfers SET on_chain_reference = '\\x01' WHERE bridge_reference LIKE 'bz-d-%'; \
         INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
             block_hash, block_time) \
           SELECT id, 'Accepted', 'Source-Final', 'origin-a', 40, '\\x02', '2026-10-13T00:01:00Z' \
           FROM transfers WHERE bridge_reference LIKE 'bz-d-%'",
    )
    .await;
    let (status, now) = ingress.post("/v0/deposits", ALPHA, &body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(now["state"], "Source-Final");
    assert_eq!(now["source_final"]["block_number"], 40);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_d1_2_a_repeat_with_a_different_body_is_refused_with_reference_conflict(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    let body = deposit_body("ref-1", arrival(&pool, 1, 1_000).await);
    let (_, original) = ingress.post("/v0/deposits", ALPHA, &body).await;

    let mut different = body.clone();
    different["destination"]["address"] = json!("5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty");
    let (status, answer) = ingress.post("/v0/deposits", ALPHA, &different).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(answer["error"], "ERR-ReferenceConflict");

    // The same reference for another kind of request is a different body too.
    let refund = json!({
        "request_reference": "ref-1",
        "coordinate": body["coordinate"],
        "destination": { "network": ORIGIN, "address": ORIGIN_ADDRESS },
    });
    let (status, answer) = ingress.post("/v0/refunds", ALPHA, &refund).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(answer["error"], "ERR-ReferenceConflict");

    // Neither started a transfer, and the original stands.
    assert_eq!(count(&pool, "intake").await, 1);
    assert_eq!(count(&pool, "transfers").await, 1);
    let (_, status) = ingress.get("/v0/transfers/by-request/ref-1", ALPHA).await;
    assert_eq!(status, original);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_d1_1_concurrent_repeats_start_one_transfer(pool: PgPool) {
    let ingress = std::sync::Arc::new(Ingress::new(&pool).await);
    let body = deposit_body("ref-1", arrival(&pool, 1, 1_000).await);
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let ingress = std::sync::Arc::clone(&ingress);
        let body = body.clone();
        tasks.push(tokio::spawn(async move {
            ingress.post("/v0/deposits", ALPHA, &body).await
        }));
    }
    let mut created = 0;
    let mut answers = Vec::new();
    for t in tasks {
        let (status, answer) = t.await.unwrap();
        assert!(
            status == StatusCode::CREATED || status == StatusCode::OK,
            "{status} {answer}"
        );
        created += usize::from(status == StatusCode::CREATED);
        answers.push(answer);
    }
    assert_eq!(created, 1);
    assert!(answers.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(count(&pool, "transfers").await, 1);
}

// --- REQ-ID-4 --------------------------------------------------------------------------------

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_id_4_one_requester_never_collides_with_anothers_references(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    let (status, a) = ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("shared", arrival(&pool, 1, 1_000).await),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    // The same reference, another body, another requester: no conflict.
    let (status, b) = ingress
        .post(
            "/v0/deposits",
            BETA,
            &deposit_body("shared", arrival(&pool, 2, 2_000).await),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_ne!(a["bridge_reference"], b["bridge_reference"]);

    // Each reads its own under the shared reference.
    let (_, mine) = ingress.get("/v0/transfers/by-request/shared", ALPHA).await;
    assert_eq!(mine["amount"], "1000");
    let (_, theirs) = ingress.get("/v0/transfers/by-request/shared", BETA).await;
    assert_eq!(theirs["amount"], "2000");
}

// --- REQ-RQ-6 --------------------------------------------------------------------------------

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rq_6_a_refund_of_an_unassociated_arrival_waits_for_a_second_approver(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    // Above the route's maximum: it can never become a deposit (REQ-IN-6).
    let coordinate = arrival(&pool, 1, MAXIMUM + 1).await;
    let refund = json!({
        "request_reference": "refund-1",
        "coordinate": coordinate,
        "destination": { "network": ORIGIN, "address": ORIGIN_ADDRESS },
    });
    let (status, answer) = ingress.post("/v0/refunds", ALPHA, &refund).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["kind"], "refund");
    // Not decided: it waits for the operator's second approver.
    assert_eq!(answer["state"], "Received");
    assert_eq!(answer["amount"], (MAXIMUM + 1).to_string());
    assert!(
        answer["bridge_reference"]
            .as_str()
            .unwrap()
            .starts_with("bz-u-")
    );

    // The arrival is now bound to the refund; a deposit cannot take it (INV-2).
    let (_, deposit) = ingress
        .post("/v0/deposits", ALPHA, &deposit_body("dep-1", coordinate))
        .await;
    assert_refused(&deposit, "ERR-SourceConsumed");

    // The destination is on the arrival's network (REQ-OT-4).
    let wrong = json!({
        "request_reference": "refund-2",
        "coordinate": arrival(&pool, 2, 5).await,
        "destination": { "network": ISSUING, "address": ISSUING_ADDRESS },
    });
    let (_, answer) = ingress.post("/v0/refunds", ALPHA, &wrong).await;
    assert_refused(&answer, "ERR-InvalidDestination");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rq_6_a_refund_of_a_token_not_admitted_is_taken_in(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    // An arrival at a reserve of a token no asset admits: it can never become a deposit, and
    // its refund needs no bridged asset or route (REQ-IN-6; amendment 0010, "Not a gap").
    let coordinate = observe(&pool, 1, "not-admitted", 700, RESERVE).await;
    let (_, deposit) = ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("dep-1", coordinate.clone()),
        )
        .await;
    assert_refused(&deposit, "ERR-UnknownRoute");

    let refund = json!({
        "request_reference": "refund-1",
        "coordinate": coordinate,
        "destination": { "network": ORIGIN, "address": ORIGIN_ADDRESS },
    });
    let (status, answer) = ingress.post("/v0/refunds", ALPHA, &refund).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["kind"], "refund");
    assert_eq!(answer["state"], "Received");
    assert_eq!(answer["token"], "not-admitted");
    assert!(answer.get("asset").is_none(), "{answer}");
    assert_eq!(answer["amount"], "700");
    assert!(
        answer["bridge_reference"]
            .as_str()
            .unwrap()
            .starts_with("bz-u-")
    );
    // Recorded without a bridged asset, bound to the arrival (INV-2), and readable as its own.
    let (asset, token): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT asset, token FROM transfers WHERE kind = 'refund'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((asset, token.as_deref()), (None, Some("not-admitted")));
    let (_, read) = ingress
        .get("/v0/transfers/by-request/refund-1", ALPHA)
        .await;
    assert_eq!(read, answer);

    // A second refund of the same arrival is refused, with no transfer of its own: it names no
    // asset (INV-2).
    let again = json!({
        "request_reference": "refund-2",
        "coordinate": coordinate,
        "destination": { "network": ORIGIN, "address": ORIGIN_ADDRESS },
    });
    let (_, answer) = ingress.post("/v0/refunds", ALPHA, &again).await;
    assert_refused(&answer, "ERR-SourceConsumed");
    assert_eq!(count(&pool, "transfers").await, 1);

    // Still only from a reserve: the same token sent anywhere else is not bezent's to release.
    let elsewhere = json!({
        "request_reference": "refund-3",
        "coordinate": observe(&pool, 2, "not-admitted", 700, ORIGIN_ADDRESS).await,
        "destination": { "network": ORIGIN, "address": ORIGIN_ADDRESS },
    });
    let (_, answer) = ingress.post("/v0/refunds", ALPHA, &elsewhere).await;
    assert_refused(&answer, "ERR-SourceNotFound");
}

// --- REQ-RQ-4, REQ-RQ-9 ----------------------------------------------------------------------

/// A withdrawal the core made from a redemption of `asset` at (`ISSUING`, `block`, log 0), with
/// on-chain reference `reference`; the redemption's observation carries `commitment`, if any.
async fn withdrawal(
    pool: &PgPool,
    block: i64,
    asset: &str,
    reference: &str,
    commitment: Option<&str>,
) {
    let detail = match commitment {
        Some(c) => json!({ "commitment": c }),
        None => json!({}),
    };
    sqlx::query(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, token, amount, sender, executed, on_chain_reference, detail) \
         VALUES ($1, $2, '\\xcc', '2026-10-13T00:00:00Z', 'log', 0, $3, 500, $4, TRUE, \
             decode($5, 'hex'), $6)",
    )
    .bind(ISSUING)
    .bind(block)
    .bind(asset)
    .bind(ISSUING_ADDRESS)
    .bind(reference)
    .bind(&detail)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index, on_chain_reference) \
         VALUES ('bz-w-' || $3, 'redemption-' || $3, 'withdrawal', $2, 500, $4, $5, 'Received', \
             $1, $6, '\\xcc', 'log', 0, decode($3, 'hex'))",
    )
    .bind(ISSUING)
    .bind(asset)
    .bind(reference)
    .bind(ORIGIN)
    .bind(ORIGIN_ADDRESS)
    .bind(block)
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rq_4_a_withdrawals_extras_are_added_by_its_redemptions_reference(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    withdrawal(&pool, 1, ASSET, "a1", None).await;

    let extras = json!({
        "request_reference": "x-1",
        "redemption_reference": "0xA1",
        "expires_at": "2026-12-01T00:00:00Z",
    });
    let (status, answer) = ingress.post("/v0/withdrawals/extras", ALPHA, &extras).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["kind"], "withdrawal");
    assert_eq!(answer["bridge_reference"], "bz-w-a1");
    assert_eq!(answer["expires_at"], "2026-12-01T00:00:00Z");

    // Once per withdrawal: a second set of extras is refused, and changes nothing.
    let again = json!({
        "request_reference": "x-2",
        "redemption_reference": "0xa1",
        "expires_at": "2027-01-01T00:00:00Z",
    });
    let (_, answer) = ingress.post("/v0/withdrawals/extras", ALPHA, &again).await;
    assert_refused(&answer, "ERR-ReferenceConflict");
    let (_, status) = ingress.get("/v0/transfers/by-bridge/bz-w-a1", ALPHA).await;
    assert_eq!(status["expires_at"], "2026-12-01T00:00:00Z");

    // No withdrawal for the reference.
    let unknown = json!({
        "request_reference": "x-3",
        "redemption_reference": "0xff",
        "expires_at": "2027-01-01T00:00:00Z",
    });
    let (_, answer) = ingress
        .post("/v0/withdrawals/extras", ALPHA, &unknown)
        .await;
    assert_refused(&answer, "ERR-SourceNotFound");
}

/// Amendment 0011 (`REQ-RQ-4`, `REQ-RF-4`): a withdrawal's refund always goes to its redeemer, so
/// the extras take no refund destination. A request that carries one is malformed and changes
/// nothing.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rq_4_a_withdrawals_extras_take_no_refund_destination(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    withdrawal(&pool, 1, ASSET, "a1", None).await;

    let with_destination = json!({
        "request_reference": "x-1",
        "redemption_reference": "0xa1",
        "expires_at": "2026-12-01T00:00:00Z",
        "refund_destination": { "network": ISSUING, "address": ISSUING_ADDRESS },
    });
    let (status, answer) = ingress
        .post("/v0/withdrawals/extras", ALPHA, &with_destination)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    assert_eq!(answer["error"], "ERR-Malformed");
    assert!(
        answer["detail"]
            .as_str()
            .unwrap()
            .contains("refund_destination"),
        "{answer}"
    );
    assert_eq!(count(&pool, "intake").await, 0);

    // Alone, it names nothing either; and the withdrawal's refund destination stays unset.
    let alone = json!({
        "request_reference": "x-2",
        "redemption_reference": "0xa1",
        "refund_destination": { "network": ISSUING, "address": ISSUING_ADDRESS },
    });
    let (status, _) = ingress.post("/v0/withdrawals/extras", ALPHA, &alone).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        scalar::<bool>(
            &pool,
            "SELECT refund_destination IS NULL AND expires_at IS NULL FROM transfers"
        )
        .await
    );
    let (_, status) = ingress.get("/v0/transfers/by-bridge/bz-w-a1", ALPHA).await;
    assert!(status.get("refund_destination").is_none(), "{status}");
}

/// `ERR-ReferenceConflict` (`REQ-RQ-4`): extras are written once, the first accepted wins, and a
/// later request from another requester with different extras is refused with it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rq_4_err_reference_conflict_a_second_requesters_different_extras(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    exec(
        &pool,
        "INSERT INTO requester_grants (requester, grant_kind) VALUES ('beta', 'withdrawal')",
    )
    .await;
    withdrawal(&pool, 1, ASSET, "a1", None).await;

    let first = json!({
        "request_reference": "x-1",
        "redemption_reference": "0xa1",
        "expires_at": "2026-12-01T00:00:00Z",
    });
    let (status, answer) = ingress.post("/v0/withdrawals/extras", ALPHA, &first).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert!(answer.get("error").is_none(), "{answer}");

    let second = json!({
        "request_reference": "y-1",
        "redemption_reference": "0xa1",
        "expires_at": "2027-06-01T00:00:00Z",
    });
    let (status, answer) = ingress.post("/v0/withdrawals/extras", BETA, &second).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_refused(&answer, "ERR-ReferenceConflict");
    // It reaches nothing of the withdrawal, which keeps the first's expiry.
    assert!(answer.get("expires_at").is_none(), "{answer}");
    let (_, status) = ingress.get("/v0/transfers/by-bridge/bz-w-a1", ALPHA).await;
    assert_eq!(status["expires_at"], "2026-12-01T00:00:00Z");
}

/// `REQ-RQ-4`: for an attested backend only the requester holding the attesting grant may add
/// extras. The others are refused before anything is recorded.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rq_4_extras_of_an_attested_backend_need_the_attesting_grant(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    exec(
        &pool,
        "INSERT INTO requester_grants (requester, grant_kind) VALUES ('beta', 'withdrawal')",
    )
    .await;
    withdrawal(
        &pool,
        1,
        ATTESTED,
        "b1",
        Some(&format!("0x{}", "11".repeat(32))),
    )
    .await;
    let extras = json!({
        "request_reference": "x-1",
        "redemption_reference": "0xb1",
        "expires_at": "2026-12-01T00:00:00Z",
    });
    let (status, answer) = ingress.post("/v0/withdrawals/extras", BETA, &extras).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
    assert_eq!(answer["error"], "ERR-NotAuthorised");
    assert_eq!(count(&pool, "intake").await, 0);

    let (status, answer) = ingress.post("/v0/withdrawals/extras", ALPHA, &extras).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert!(answer.get("error").is_none(), "{answer}");
}

/// `ERR-Malformed` (§10): a request that does not parse, or lacks a field its kind requires.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn err_malformed_an_unparseable_or_incomplete_request(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    withdrawal(&pool, 2, ASSET, "a2", None).await;
    let coordinate = arrival(&pool, 1, 1_000).await;

    let mut no_destination = deposit_body("m-1", coordinate.clone());
    no_destination
        .as_object_mut()
        .unwrap()
        .remove("destination");
    let mut no_coordinate = deposit_body("m-2", coordinate.clone());
    no_coordinate.as_object_mut().unwrap().remove("coordinate");
    let mut bad_hash = deposit_body("m-3", coordinate.clone());
    bad_hash["coordinate"]["block_hash"] = json!("not-hex");
    let mut no_reference = deposit_body("m-4", coordinate.clone());
    no_reference
        .as_object_mut()
        .unwrap()
        .remove("request_reference");
    let mut empty_reference = deposit_body("", coordinate.clone());
    empty_reference["request_reference"] = json!("");
    for (path, body) in [
        ("/v0/deposits", no_destination),
        ("/v0/deposits", no_coordinate),
        ("/v0/deposits", bad_hash),
        ("/v0/deposits", no_reference),
        ("/v0/deposits", empty_reference),
        ("/v0/deposits", json!({})),
        ("/v0/deposits", json!([1, 2])),
        // Incomplete: a refund names a destination.
        (
            "/v0/refunds",
            json!({ "request_reference": "m-5", "coordinate": coordinate }),
        ),
        // Incomplete: extras name an expiry or instructions; the redemption's reference is hex.
        (
            "/v0/withdrawals/extras",
            json!({ "request_reference": "m-6", "redemption_reference": "0xa2" }),
        ),
        (
            "/v0/withdrawals/extras",
            json!({ "request_reference": "m-7", "redemption_reference": "zz",
                "expires_at": "2099-01-01T00:00:00Z" }),
        ),
        (
            "/v0/withdrawals/extras",
            json!({ "request_reference": "m-8", "redemption_reference": "0xa2",
                "expires_at": "tomorrow" }),
        ),
    ] {
        let (status, answer) = ingress.post(path, ALPHA, &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path} {body}: {answer}");
        assert_eq!(answer["error"], "ERR-Malformed", "{path} {body}");
    }

    // Not JSON at all.
    let response = {
        use axum::body::Body;
        use axum::http::{Request, header};
        use tower::ServiceExt;
        let request = Request::builder()
            .method("POST")
            .uri("/v0/deposits")
            .header(header::AUTHORIZATION, format!("Bearer {ALPHA}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{not json"))
            .unwrap();
        bezent_ingress::router(ingress.app.clone())
            .oneshot(request)
            .await
            .unwrap()
    };
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // A malformed request is not taken in: nothing is recorded.
    assert_eq!(count(&pool, "intake").await, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rq_9_err_commitment_mismatch_relayed_instructions_are_accepted_only_when_they_hash_to_the_commitment(
    pool: PgPool,
) {
    let ingress = Ingress::new(&pool).await;
    let instructions = b"\x04bank-account-and-reference";
    let commitment = {
        use blake2::Digest;
        hex::encode(blake2::Blake2b::<blake2::digest::consts::U32>::digest(
            instructions,
        ))
    };
    withdrawal(&pool, 1, ATTESTED, "b1", Some(&format!("0x{commitment}"))).await;
    withdrawal(&pool, 2, ASSET, "b2", None).await;

    // Instructions that do not hash to the commitment.
    let wrong = json!({
        "request_reference": "i-1",
        "redemption_reference": "0xb1",
        "instructions": format!("0x{}", hex::encode(b"\x04someone-else")),
    });
    let (_, answer) = ingress.post("/v0/withdrawals/extras", ALPHA, &wrong).await;
    assert_refused(&answer, "ERR-CommitmentMismatch");

    // A public backend's redemption carries its instructions inline; none are relayed.
    let public = json!({
        "request_reference": "i-2",
        "redemption_reference": "0xb2",
        "instructions": format!("0x{}", hex::encode(instructions)),
    });
    let (_, answer) = ingress.post("/v0/withdrawals/extras", ALPHA, &public).await;
    assert_refused(&answer, "ERR-CommitmentMismatch");
    assert_eq!(count(&pool, "relayed_instructions").await, 0);

    // The instructions that hash to it are accepted and kept with the withdrawal.
    let right = json!({
        "request_reference": "i-3",
        "redemption_reference": "0xb1",
        "instructions": format!("0x{}", hex::encode(instructions)),
    });
    let (status, answer) = ingress.post("/v0/withdrawals/extras", ALPHA, &right).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(answer["bridge_reference"], "bz-w-b1");
    assert!(answer.get("error").is_none(), "{answer}");
    let kept: Vec<u8> = scalar(&pool, "SELECT instructions FROM relayed_instructions").await;
    assert_eq!(kept, instructions);
}

// --- REQ-ID-4, REQ-RQ-4: extras are written once ---------------------------------------------

/// Waits until `n` statements of this database wait on a lock.
async fn until_waiting_on_locks(pool: &PgPool, n: i64) {
    for _ in 0..500 {
        let waiting: i64 = scalar(
            pool,
            "SELECT count(*) FROM pg_stat_activity \
             WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .await;
        if waiting >= n {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("{n} requests never waited on the withdrawal's lock");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_id_4_a_second_requesters_extras_on_one_redemption_are_refused_and_see_nothing(
    pool: PgPool,
) {
    let ingress = std::sync::Arc::new(Ingress::new(&pool).await);
    exec(
        &pool,
        "INSERT INTO requester_grants (requester, grant_kind) VALUES \
           ('beta', 'withdrawal'), ('beta', 'attest')",
    )
    .await;
    let instructions = b"\x04bank-account-and-reference";
    let commitment = {
        use blake2::Digest;
        hex::encode(blake2::Blake2b::<blake2::digest::consts::U32>::digest(
            instructions,
        ))
    };
    withdrawal(&pool, 1, ASSET, "a1", None).await;
    withdrawal(&pool, 2, ATTESTED, "b1", Some(&format!("0x{commitment}"))).await;
    let relayed = format!("0x{}", hex::encode(instructions));

    // The review's reproduction: both withdrawals are held while two requesters send extras for
    // each, then released. Every request is waiting on the row lock when the holder lets go.
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM transfers WHERE kind = 'withdrawal' FOR UPDATE")
        .execute(&mut *holder)
        .await
        .unwrap();
    let requests = [
        (
            ALPHA,
            json!({ "request_reference": "x-a", "redemption_reference": "0xa1",
                "expires_at": "2026-12-01T00:00:00Z" }),
        ),
        (
            BETA,
            json!({ "request_reference": "x-b", "redemption_reference": "0xa1",
                "expires_at": "2027-01-01T00:00:00Z" }),
        ),
        (
            ALPHA,
            json!({ "request_reference": "i-a", "redemption_reference": "0xb1",
                "instructions": relayed }),
        ),
        (
            BETA,
            json!({ "request_reference": "i-b", "redemption_reference": "0xb1",
                "instructions": relayed }),
        ),
    ];
    let mut tasks = Vec::new();
    for (credential, body) in requests.clone() {
        let ingress = std::sync::Arc::clone(&ingress);
        tasks.push(tokio::spawn(async move {
            ingress
                .post("/v0/withdrawals/extras", credential, &body)
                .await
        }));
    }
    until_waiting_on_locks(&pool, 4).await;
    holder.commit().await.unwrap();

    let mut answers = Vec::new();
    for t in tasks {
        let (status, answer) = t.await.unwrap();
        // Never a failure: the loser is a named refusal, not a 500 from the store's backstop.
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        answers.push(answer);
    }
    for (pair, reference) in [([0, 1], "bz-w-a1"), ([2, 3], "bz-w-b1")] {
        let won: Vec<usize> = pair
            .into_iter()
            .filter(|&i| answers[i].get("error").is_none())
            .collect();
        assert_eq!(
            won.len(),
            1,
            "one set of extras per withdrawal: {answers:?}"
        );
        let winner = won[0];
        let loser = if pair[0] == winner { pair[1] } else { pair[0] };
        assert_eq!(answers[winner]["bridge_reference"], reference);

        // The loser is refused, and its answer carries nothing of the withdrawal.
        let lost = &answers[loser];
        assert_refused(lost, "ERR-ReferenceConflict");
        assert_ne!(lost["bridge_reference"], reference);
        for field in [
            "refund_destination",
            "expires_at",
            "asset",
            "amount",
            "source",
        ] {
            assert!(lost.get(field).is_none(), "{field} leaked: {lost}");
        }
        // Nor can it read the withdrawal afterwards; the winner can.
        let path = format!("/v0/transfers/by-bridge/{reference}");
        let (status, _) = ingress.get(&path, requests[loser].0).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = ingress.get(&path, requests[winner].0).await;
        assert_eq!(status, StatusCode::OK);
    }

    // The store holds one answer per withdrawal, and one relay of the instructions.
    let linked: i64 = scalar(
        &pool,
        "SELECT count(*) FROM intake_answers a JOIN transfers t ON t.id = a.transfer_id \
         WHERE t.kind = 'withdrawal'",
    )
    .await;
    assert_eq!(linked, 2);
    assert_eq!(count(&pool, "relayed_instructions").await, 1);
    // Only the winner's field was written on the public withdrawal.
    let expiry: String = scalar(
        &pool,
        "SELECT to_char(expires_at AT TIME ZONE 'UTC', 'YYYY') FROM transfers \
         WHERE bridge_reference = 'bz-w-a1'",
    )
    .await;
    assert!(
        ["2026", "2027"].contains(&expiry.as_str()),
        "one requester wrote the withdrawal: {expiry}"
    );
}

// --- REQ-LM-2 --------------------------------------------------------------------------------

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_lm_2_an_expiry_already_past_is_refused_and_not_recorded(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    withdrawal(&pool, 1, ASSET, "a1", None).await;
    let past = json!({
        "request_reference": "x-1",
        "redemption_reference": "0xa1",
        "expires_at": "2020-01-01T00:00:00Z",
    });
    let (status, answer) = ingress.post("/v0/withdrawals/extras", ALPHA, &past).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    assert_eq!(count(&pool, "intake").await, 0);
    assert!(scalar::<bool>(&pool, "SELECT expires_at IS NULL FROM transfers").await);
}

// --- REQ-RQ-3, REQ-RQ-6, REQ-IN-3, REQ-IN-6, REQ-CV-1, REQ-CV-2, INV-2: what a request may name ---

/// Another reserve on `ORIGIN` (of `bOTHER`): a leg from it is a rebalance.
const OTHER_RESERVE: &str = "0x00000000000000000000000000000000000000bb";

/// A refund request for `coordinate`.
fn refund_body(reference: &str, coordinate: Value) -> Value {
    json!({
        "request_reference": reference,
        "coordinate": coordinate,
        "destination": { "network": ORIGIN, "address": ORIGIN_ADDRESS },
    })
}

/// Observations at the reserve that neither a deposit nor a refund may name.
fn never_bindable() -> Vec<(&'static str, Seen<'static>)> {
    vec![
        (
            "no kind written (M1: fail closed)",
            Seen {
                kind: None,
                ..arrival_seen(1_000)
            },
        ),
        (
            "kind other",
            Seen {
                kind: Some("other"),
                ..arrival_seen(1_000)
            },
        ),
        (
            "a payout leg (it carries the redemption's reference)",
            Seen {
                reference: Some("a1"),
                kind: Some("payout_leg"),
                ..arrival_seen(1_000)
            },
        ),
        (
            "an arrival that carries an on-chain reference",
            Seen {
                reference: Some("a1"),
                ..arrival_seen(1_000)
            },
        ),
        (
            "an outflow",
            Seen {
                kind: Some("outflow"),
                ..arrival_seen(1_000)
            },
        ),
        (
            "a conversion's input",
            Seen {
                kind: Some("conversion_input"),
                ..arrival_seen(1_000)
            },
        ),
        (
            "a self transfer",
            Seen {
                kind: Some("self_transfer"),
                ..arrival_seen(1_000)
            },
        ),
    ]
}

/// M1: an observation without an explicit bindable kind binds nothing, as a deposit or as a
/// refund; the derived test is gone.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m1_inv_2_an_observation_without_a_bindable_kind_binds_nothing(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    for (i, (what, seen)) in never_bindable().into_iter().enumerate() {
        let block = 10 + i64::try_from(i).unwrap();
        let coordinate = observe_seen(&pool, block, seen).await;
        let (status, answer) = ingress
            .post(
                "/v0/deposits",
                ALPHA,
                &deposit_body(&format!("d-{i}"), coordinate.clone()),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{what}: {answer}");
        assert_refused(&answer, "ERR-SourceNotFound");
        let (status, answer) = ingress
            .post(
                "/v0/refunds",
                ALPHA,
                &refund_body(&format!("r-{i}"), coordinate),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{what}: {answer}");
        assert_refused(&answer, "ERR-SourceNotFound");
    }
    assert_eq!(count(&pool, "transfers").await, 0);
}

/// M2: a deposit binds an `arrival` or a `conversion_output` at the reserve (REQ-CV-2); a
/// convertible arrival is never minted against (REQ-CV-1), nor are the other kinds.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m2_req_cv_1_req_cv_2_a_deposit_names_an_arrival_or_a_conversion_output(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    let not_for_deposit = [
        (
            "a convertible arrival (REQ-CV-1)",
            Seen {
                kind: Some("convertible_arrival"),
                ..arrival_seen(1_000)
            },
        ),
        (
            "an arrival of a token that is not admitted",
            Seen {
                kind: Some("other_arrival"),
                ..arrival_seen(1_000)
            },
        ),
        (
            "a leg from another reserve (a rebalance)",
            Seen {
                sender: OTHER_RESERVE,
                ..arrival_seen(1_000)
            },
        ),
    ];
    for (i, (what, seen)) in not_for_deposit.into_iter().enumerate() {
        let block = 10 + i64::try_from(i).unwrap();
        let coordinate = observe_seen(&pool, block, seen).await;
        let (status, answer) = ingress
            .post(
                "/v0/deposits",
                ALPHA,
                &deposit_body(&format!("d-{i}"), coordinate),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{what}: {answer}");
        assert_refused(&answer, "ERR-SourceNotFound");
    }
    assert_eq!(count(&pool, "transfers").await, 0);

    // An arrival and a conversion's output, each final at the reserve, bind.
    for (block, kind) in [(50, "arrival"), (51, "conversion_output")] {
        let coordinate = observe_seen(
            &pool,
            block,
            Seen {
                kind: Some(kind),
                ..arrival_seen(1_000)
            },
        )
        .await;
        let (_, answer) = ingress
            .post(
                "/v0/deposits",
                ALPHA,
                &deposit_body(&format!("d-{kind}"), coordinate),
            )
            .await;
        assert_eq!(answer["state"], "Accepted", "{kind}: {answer}");
    }
}

/// S1: the refund of an unassociated arrival names any arrival at a reserve (REQ-IN-6): a token
/// not admitted, a conversion's output, an amount outside the route's bounds, a venue delivery and
/// a leg from another reserve. Wider than a deposit, but never a convertible arrival (M1).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s1_req_rq_6_req_in_6_a_refund_names_any_unassociated_arrival(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    exec(
        &pool,
        "UPDATE routes SET venue_addresses = '[\"0x00000000000000000000000000000000000000cc\"]'",
    )
    .await;
    let unassociated = [
        (
            "a token not admitted",
            Seen {
                token: "not-admitted",
                kind: Some("other_arrival"),
                ..arrival_seen(700)
            },
        ),
        (
            "a conversion's output",
            Seen {
                kind: Some("conversion_output"),
                ..arrival_seen(700)
            },
        ),
        ("below the route's minimum", arrival_seen(MINIMUM - 1)),
        ("above the route's maximum", arrival_seen(MAXIMUM + 1)),
        (
            "a venue delivery",
            Seen {
                sender: "0x00000000000000000000000000000000000000cc",
                ..arrival_seen(700)
            },
        ),
        (
            "a leg from another reserve",
            Seen {
                sender: OTHER_RESERVE,
                ..arrival_seen(700)
            },
        ),
    ];
    for (i, (what, seen)) in unassociated.into_iter().enumerate() {
        let block = 10 + i64::try_from(i).unwrap();
        let coordinate = observe_seen(&pool, block, seen).await;
        let (status, answer) = ingress
            .post(
                "/v0/refunds",
                ALPHA,
                &refund_body(&format!("r-{i}"), coordinate),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{what}: {answer}");
        assert_eq!(answer["state"], "Received", "{what}: {answer}");
    }
    assert_eq!(count(&pool, "transfers").await, 6);
}

/// S4: a request naming an arrival that is observed but not final is answered as retryable: 503
/// `not-final`, nothing recorded, the request reference not consumed. Sent again once the arrival
/// is final, the same request is taken in.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s4_req_rq_3_req_rq_6_req_id_1_a_request_at_an_arrival_not_final_is_retryable(
    pool: PgPool,
) {
    let ingress = Ingress::new(&pool).await;
    let deposit_at = observe_seen(
        &pool,
        20,
        Seen {
            outcome: None,
            ..arrival_seen(1_000)
        },
    )
    .await;
    let refund_at = observe_seen(
        &pool,
        21,
        Seen {
            token: "not-admitted",
            kind: Some("other_arrival"),
            outcome: None,
            ..arrival_seen(700)
        },
    )
    .await;
    let deposit = deposit_body("d-early", deposit_at);
    let refund = refund_body("r-early", refund_at);

    let (status, answer) = ingress.post("/v0/deposits", ALPHA, &deposit).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{answer}");
    assert_eq!(answer["error"], "not-final", "{answer}");
    let (status, answer) = ingress.post("/v0/refunds", ALPHA, &refund).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{answer}");
    assert_eq!(answer["error"], "not-final", "{answer}");
    assert_eq!(count(&pool, "intake").await, 0);
    assert_eq!(count(&pool, "intake_answers").await, 0);
    assert_eq!(count(&pool, "transfers").await, 0);

    // Both become final; the same requests, by the same references, are taken in.
    exec(
        &pool,
        "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
             final_block_hash) SELECT id, 'final', block_number + 10, '\\x01' FROM observations",
    )
    .await;
    let (status, answer) = ingress.post("/v0/deposits", ALPHA, &deposit).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["state"], "Accepted", "{answer}");
    let (status, answer) = ingress.post("/v0/refunds", ALPHA, &refund).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["state"], "Received", "{answer}");

    // One withdrawn before finality is a permanent refusal (REQ-IN-7).
    let withdrawn = observe_seen(
        &pool,
        30,
        Seen {
            outcome: Some("withdrawn"),
            ..arrival_seen(1_000)
        },
    )
    .await;
    let (status, answer) = ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("d-withdrawn", withdrawn),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_refused(&answer, "ERR-SourceNotFound");
}

// --- REQ-LM-1, CND-CutOff --------------------------------------------------------------------

/// A cut-off holds a transfer (`CND-CutOff`); the core applies it (`services/core/tests/
/// conditions.rs`, `cnd_cutoff_a_step_past_the_route_cut_off_holds_until_it_reopens`). It never
/// refuses a request. With the routes closed almost all day, every day, requests are taken in.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_lm_1_cnd_cutoff_no_request_is_refused_for_a_cut_off(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    let closed = r#"[{"weekdays": [1, 2, 3, 4, 5, 6, 7], "from": "00:00", "to": "23:59"}]"#;
    exec(&pool, &format!("UPDATE routes SET cutoffs = '{closed}'")).await;
    exec(
        &pool,
        &format!(
            "INSERT INTO routes (asset, network, direction, minimum, maximum, window_limit, \
                 window_seconds, capacity, cutoffs, completion_budget_seconds) \
             VALUES ('{ASSET}', '{ORIGIN}', 'withdrawal', 1, 1000000, 2500000, 86400, 100, \
                 '{closed}', 3600)"
        ),
    )
    .await;
    let (_, routes) = ingress.get("/v0/routes", ALPHA).await;
    assert!(
        routes.to_string().contains("23:59"),
        "the cut-off is declared: {routes}"
    );

    let (status, answer) = ingress
        .post(
            "/v0/deposits",
            ALPHA,
            &deposit_body("c-1", arrival(&pool, 1, 1_000).await),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["state"], "Accepted", "{answer}");
    assert!(answer.get("error").is_none(), "{answer}");

    withdrawal(&pool, 2, ASSET, "a1", None).await;
    let extras = json!({
        "request_reference": "c-2",
        "redemption_reference": "0xa1",
        "expires_at": "2099-01-01T00:00:00Z",
    });
    let (status, answer) = ingress.post("/v0/withdrawals/extras", ALPHA, &extras).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert!(answer.get("error").is_none(), "{answer}");
}

/// M1: a refund never names a `convertible_arrival` (INV-2, REQ-CV-1): its value is released only
/// through its conversion. It is answered as no source found, and nothing is recorded.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m1_inv_2_req_cv_1_a_refund_never_names_a_convertible_arrival(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    let coordinate = observe_seen(
        &pool,
        10,
        Seen {
            kind: Some("convertible_arrival"),
            ..arrival_seen(700)
        },
    )
    .await;
    let (status, answer) = ingress
        .post(
            "/v0/refunds",
            ALPHA,
            &refund_body("r-convertible", coordinate),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_refused(&answer, "ERR-SourceNotFound");
    assert_eq!(count(&pool, "transfers").await, 0);
}

/// S1: a conversion's output sent from a venue address (the pool, declared in `venue_addresses`)
/// is still taken in as a deposit (REQ-CV-2); an arrival from that address is a venue delivery.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s1_req_cv_2_a_conversion_output_from_a_venue_address_is_still_a_deposit(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    const POOL: &str = "0x00000000000000000000000000000000000000cc";
    exec(
        &pool,
        &format!("UPDATE routes SET venue_addresses = '[\"{POOL}\"]'"),
    )
    .await;
    let output = observe_seen(
        &pool,
        10,
        Seen {
            sender: POOL,
            kind: Some("conversion_output"),
            ..arrival_seen(1_000)
        },
    )
    .await;
    let (_, answer) = ingress
        .post("/v0/deposits", ALPHA, &deposit_body("d-output", output))
        .await;
    assert_eq!(answer["state"], "Accepted", "{answer}");
    let delivery = observe_seen(
        &pool,
        11,
        Seen {
            sender: POOL,
            kind: Some("arrival"),
            ..arrival_seen(1_000)
        },
    )
    .await;
    let (_, answer) = ingress
        .post("/v0/deposits", ALPHA, &deposit_body("d-delivery", delivery))
        .await;
    assert_refused(&answer, "ERR-SourceNotFound");
}

// --- REQ-IN-5: one canonical form, validated at insert, removed as an entry -------------------

const ALICE_RAW: &str = "0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d";

/// A deposit to `address` on the issuing network, `n` being its own arrival and reference.
async fn deposit_to(ingress: &Ingress, pool: &PgPool, n: i64, address: &str) -> Value {
    let mut body = deposit_body(&format!("ref-{n}"), arrival(pool, n, 1_000).await);
    body["destination"] = json!({ "network": ISSUING, "address": address });
    ingress.post("/v0/deposits", ALPHA, &body).await.1
}

/// A deposit with its refund destination `address` on the origin network.
async fn deposit_refunding_to(ingress: &Ingress, pool: &PgPool, n: i64, address: &str) -> Value {
    let mut body = deposit_body(&format!("ref-{n}"), arrival(pool, n, 1_000).await);
    body["refund_destination"] = json!({ "network": ORIGIN, "address": address });
    ingress.post("/v0/deposits", ALPHA, &body).await.1
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_5_an_entry_in_any_spelling_names_one_account_for_any_spelling_asked(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    // The same account declared as a raw key, under the Polkadot prefix and under prefix 42: one
    // account in the list, whichever way it was written.
    for (n, address) in [ALICE_RAW, ISSUING_ADDRESS_OTHER_PREFIX, ISSUING_ADDRESS]
        .into_iter()
        .enumerate()
    {
        declare_ledger_account(&pool, ISSUING, address).await;
        let accounts: i64 = common::scalar(
            &pool,
            "SELECT count(*) FROM ledger_accounts_listed WHERE network = 'kreivo'",
        )
        .await;
        assert_eq!(accounts, 1, "entry {n}");
    }
    // Asked in each SS58 spelling the ingress takes.
    for (n, asked) in (1..).zip([ISSUING_ADDRESS, ISSUING_ADDRESS_OTHER_PREFIX]) {
        assert_refused(
            &deposit_to(&ingress, &pool, n, asked).await,
            "ERR-InvalidDestination",
        );
    }
    let ok = deposit_to(&ingress, &pool, 10, BOB).await;
    assert_eq!(ok["state"], "Accepted", "{ok}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_5_a_raw_key_entry_is_the_account_of_its_ss58_spelling(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    declare_ledger_account(&pool, ISSUING, ALICE_RAW).await;
    let stored: String =
        common::scalar(&pool, "SELECT encode(account, 'hex') FROM ledger_accounts").await;
    assert_eq!(stored, &ALICE_RAW[2..]);
    for (n, asked) in (1..).zip([ISSUING_ADDRESS, ISSUING_ADDRESS_OTHER_PREFIX]) {
        assert_refused(
            &deposit_to(&ingress, &pool, n, asked).await,
            "ERR-InvalidDestination",
        );
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_5_an_evm_entry_matches_in_any_case(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    // Declared with its EIP-55 checksum; asked in lowercase, in uppercase and as declared.
    declare_ledger_account(&pool, ORIGIN, ORIGIN_ADDRESS).await;
    let upper = format!("0x{}", ORIGIN_ADDRESS[2..].to_ascii_uppercase());
    let asked = [
        ORIGIN_ADDRESS.to_ascii_lowercase(),
        upper,
        ORIGIN_ADDRESS.into(),
    ];
    for (n, address) in (1..).zip(asked) {
        assert_refused(
            &deposit_refunding_to(&ingress, &pool, n, &address).await,
            "ERR-InvalidDestination",
        );
    }
    let other = "0x00000000000000000000000000000000000000aa";
    let ok = deposit_refunding_to(&ingress, &pool, 10, other).await;
    assert_eq!(ok["state"], "Accepted", "{ok}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_5_an_entry_that_does_not_parse_for_its_network_is_refused_at_insert(pool: PgPool) {
    let _ = Ingress::new(&pool).await;
    for (network, address) in [
        // The wrong network's format.
        (ISSUING, ORIGIN_ADDRESS),
        (ORIGIN, ISSUING_ADDRESS),
        // Not an address at all, or truncated.
        (ISSUING, "5Grw"),
        (ISSUING, "0x1234"),
        (ISSUING, "not-base58-0OIl"),
        (ORIGIN, "0x1234"),
        (ORIGIN, "0xzz908400098527886E0F7030069857D2E4169EE7"),
    ] {
        let refused = sqlx::raw_sql(&format!(
            "INSERT INTO ledger_accounts (network, address, reason) \
             VALUES ('{network}', '{address}', 'a typo')"
        ))
        .execute(&pool)
        .await;
        assert!(refused.is_err(), "{network} {address}");
    }
    let rows: i64 = common::scalar(&pool, "SELECT count(*) FROM ledger_accounts").await;
    assert_eq!(rows, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_5_an_entry_the_services_cannot_read_back_refuses_closed(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    // Well formed to the store (the database cannot check an SS58 checksum), wrong to the services:
    // a typo. Nothing fails open: the destinations of the network are refused until it is fixed.
    declare_ledger_account(&pool, ISSUING, &ISSUING_ADDRESS.replace('Q', "R")).await;
    assert_refused(
        &deposit_to(&ingress, &pool, 1, BOB).await,
        "ERR-InvalidDestination",
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_5_an_unlisted_entry_withdraws_an_account_and_the_latest_decides(pool: PgPool) {
    let ingress = Ingress::new(&pool).await;
    declare_ledger_account(&pool, ISSUING, ISSUING_ADDRESS).await;
    assert_refused(
        &deposit_to(&ingress, &pool, 1, ISSUING_ADDRESS).await,
        "ERR-InvalidDestination",
    );

    // Withdrawn by a new entry, under another spelling, with its own reason; nothing is edited.
    common::exec(
        &pool,
        &format!(
            "INSERT INTO ledger_accounts (network, address, kind, reason) \
             VALUES ('{ISSUING}', '{ALICE_RAW}', 'unlisted', 'listed by mistake')"
        ),
    )
    .await;
    let rows: i64 = common::scalar(&pool, "SELECT count(*) FROM ledger_accounts").await;
    let listed: i64 = common::scalar(&pool, "SELECT count(*) FROM ledger_accounts_listed").await;
    assert_eq!((rows, listed), (2, 0));
    let ok = deposit_to(&ingress, &pool, 2, ISSUING_ADDRESS).await;
    assert_eq!(ok["state"], "Accepted", "{ok}");

    // Declared again: the latest entry decides.
    declare_ledger_account(&pool, ISSUING, ISSUING_ADDRESS_OTHER_PREFIX).await;
    assert_refused(
        &deposit_to(&ingress, &pool, 3, ISSUING_ADDRESS).await,
        "ERR-InvalidDestination",
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_5_the_operator_appends_entries_by_column_and_sets_no_account(pool: PgPool) {
    let _ = Ingress::new(&pool).await;
    for (column, expected) in [
        ("network", true),
        ("address", true),
        ("kind", true),
        ("reason", true),
        ("id", false),
        ("account", false),
        ("actor", false),
        ("declared_at", false),
    ] {
        let granted: bool = common::scalar(
            &pool,
            &format!(
                "SELECT has_column_privilege('bezent_operator', 'ledger_accounts', '{column}', 'INSERT')"
            ),
        )
        .await;
        assert_eq!(granted, expected, "{column}");
    }
    // As the operator's role, an entry is canonicalised at insert.
    // SET LOCAL ROLE, in a transaction: the pooled connection never keeps the role (#27).
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(&format!(
        "SET LOCAL ROLE bezent_operator; \
         INSERT INTO ledger_accounts (network, address, reason) \
         VALUES ('{ISSUING}', '{ISSUING_ADDRESS}', 'a ledger holds value here');"
    ))
    .execute(&mut *tx)
    .await
    .expect("the operator appends an entry");
    tx.commit().await.unwrap();
    let account: String =
        common::scalar(&pool, "SELECT encode(account, 'hex') FROM ledger_accounts").await;
    assert_eq!(account, &ALICE_RAW[2..]);
    // No function is executable by PUBLIC.
    let public_exec: bool = common::scalar(
        &pool,
        "SELECT has_function_privilege('public', 'ledger_accounts_canonical()', 'EXECUTE') \
           OR has_function_privilege('public', 'ledger_accounts_base58(text)', 'EXECUTE')",
    )
    .await;
    assert!(!public_exec);
}
