// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! T-011-03: an attested backend's reports at the request surface (`REQ-BE-4`, `REQ-BE-5`,
//! `REQ-BE-6`, `REQ-RQ-4`, `REQ-ID-1`, `REQ-PN-2`; PLAN `DEC-14`). What the core makes of them is in
//! `services/core/tests/attested.rs`.

mod common;

use axum::http::StatusCode;
use bezent_core::attested::{report_digest, report_message};
use common::*;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use sqlx::PgPool;

/// The envelope of `document`, signed with the party's key.
fn signed(document: &Value) -> Value {
    signed_with(document, &SigningKey::from_bytes(&PARTY_KEY_SEED))
}

fn signed_with(document: &Value, key: &SigningKey) -> Value {
    let report = document.to_string();
    let signature = key.sign(&report_message(report.as_bytes()));
    json!({
        "report": report,
        "signature": format!("0x{}", hex::encode(signature.to_bytes())),
        "public_key": format!("0x{}", hex::encode(key.verifying_key().to_bytes())),
    })
}

/// A report of `kind` with `fields`, under the party's identifier `id`.
fn document(id: &str, kind: &str, fields: Value) -> Value {
    let mut d = json!({
        "version": 1, "report_id": id, "network": ATTESTED_NETWORK, "kind": kind,
        "asset": ATTESTED, "token": ATTESTED_TOKEN,
    });
    d.as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    d
}

fn arrival_report(id: &str, amount: &str) -> Value {
    document(
        id,
        "arrival",
        json!({"amount": amount, "position": "acct-1",
               "ids": {"order": "ord-1", "rail": "rail-9"},
               "occurred_at": "2026-11-03T10:00:00Z"}),
    )
}

async fn world(pool: &PgPool) -> Ingress {
    let ingress = Ingress::new(pool).await;
    attested_backend(pool).await;
    ingress
}

const REPORTS: &str = "/v0/reports";

/// `F-011` done-when: a report without the attesting grant is refused, with nothing recorded.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pn_2_a_report_without_the_attesting_grant_is_refused(pool: PgPool) {
    let ingress = world(&pool).await;
    let (status, answer) = ingress
        .post(REPORTS, BETA, &signed(&arrival_report("r-1", "500")))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
    assert_eq!(answer["error"], "ERR-NotAuthorised");
    assert_eq!(count(&pool, "intake").await, 0);
    assert_eq!(count(&pool, "attested_reports").await, 0);
}

/// `REQ-RQ-4`: "that backend's attesting grant": another requester holding the grant is refused,
/// and so is the party on a backend that is not its own.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rq_4_the_attesting_grant_is_only_for_the_backend_the_requester_is_declared_for(
    pool: PgPool,
) {
    let ingress = world(&pool).await;
    exec(
        &pool,
        "INSERT INTO requester_grants (requester, grant_kind) VALUES ('beta', 'attest'); \
         INSERT INTO networks (network, role, address_format, backend) \
           VALUES ('other-rail', 'origin', 'opaque', 'attested:2'); \
         INSERT INTO attesting_parties (network, requester, party) \
           VALUES ('other-rail', 'beta', 'Another');",
    )
    .await;
    let (status, answer) = ingress
        .post(REPORTS, BETA, &signed(&arrival_report("r-1", "500")))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
    let mut other = arrival_report("r-2", "500");
    other["network"] = json!("other-rail");
    let (status, answer) = ingress.post(REPORTS, ALPHA, &signed(&other)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
    let mut undeclared = arrival_report("r-3", "500");
    undeclared["network"] = json!(ORIGIN);
    let (status, answer) = ingress.post(REPORTS, ALPHA, &signed(&undeclared)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
    assert_eq!(count(&pool, "intake").await, 0);
}

/// `DEC-14`: signed reports. A key not declared for the backend, a signature over other bytes, or
/// one from the declared key's impostor is refused, with nothing recorded.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_14_a_report_whose_signature_does_not_verify_is_refused(pool: PgPool) {
    let ingress = world(&pool).await;
    let report = arrival_report("r-1", "500");
    // Another key, whole.
    let impostor = signed_with(&report, &SigningKey::from_bytes(&[0x11; 32]));
    let (status, answer) = ingress.post(REPORTS, ALPHA, &impostor).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
    // The declared key's name on another key's signature.
    let mut forged = signed(&report);
    forged["signature"] = impostor["signature"].clone();
    let (status, answer) = ingress.post(REPORTS, ALPHA, &forged).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
    // The party's signature, over bytes that were changed after signing.
    let mut changed = signed(&report);
    changed["report"] = json!(changed["report"].as_str().unwrap().replace("500", "501"));
    let (status, answer) = ingress.post(REPORTS, ALPHA, &changed).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
    assert_eq!(count(&pool, "intake").await, 0);
    assert_eq!(count(&pool, "attested_reports").await, 0);
}

/// `REQ-BE-4`: recorded with the party's own identifiers, the bytes as signed, and the coordinate
/// the observation will have. It moves nothing, and is not yet an observation (the core's).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_be_4_a_report_is_recorded_as_signed_under_the_partys_own_identifiers(pool: PgPool) {
    let ingress = world(&pool).await;
    let envelope = signed(&arrival_report("r-1", "500"));
    let (status, answer) = ingress.post(REPORTS, ALPHA, &envelope).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(answer["state"], "Recorded");
    assert_eq!(answer["report_id"], "r-1");
    let bytes = envelope["report"].as_str().unwrap().as_bytes();
    let digest = format!("0x{}", hex::encode(report_digest(bytes)));
    assert_eq!(
        answer["coordinate"],
        json!({"network": ATTESTED_NETWORK, "block_number": 1, "block_hash": digest,
               "position": {"kind": "log", "index": 0}})
    );
    let (party, id, kept, order): (String, String, Vec<u8>, Option<String>) = sqlx::query_as(
        "SELECT party, report_id, signed_bytes, body -> 'ids' ->> 'order' FROM attested_reports",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (party.as_str(), id.as_str(), order.as_deref()),
        (PARTY, "r-1", Some("ord-1"))
    );
    assert_eq!(kept, bytes);
    assert_eq!(count(&pool, "observations").await, 0);

    // The next report takes the next number on the backend.
    let (_, next) = ingress
        .post(REPORTS, ALPHA, &signed(&arrival_report("r-2", "700")))
        .await;
    assert_eq!(next["coordinate"]["block_number"], 2);
}

/// `REQ-ID-1`: the party's identifier is the request reference.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_id_1_a_repeat_answers_with_the_original_and_other_bytes_conflict(pool: PgPool) {
    let ingress = world(&pool).await;
    let envelope = signed(&arrival_report("r-1", "500"));
    let (_, first) = ingress.post(REPORTS, ALPHA, &envelope).await;
    let (status, repeat) = ingress.post(REPORTS, ALPHA, &envelope).await;
    assert_eq!(status, StatusCode::OK, "{repeat}");
    assert_eq!(repeat, first);
    assert_eq!(count(&pool, "attested_reports").await, 1);
    let (status, answer) = ingress
        .post(REPORTS, ALPHA, &signed(&arrival_report("r-1", "600")))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["error"], "ERR-ReferenceConflict");
    assert_eq!(count(&pool, "attested_reports").await, 1);
}

/// `ERR-Malformed`: a document that lacks what its kind needs, carries what it does not define, or
/// an amount that is not one exact spelling of minor units. Nothing recorded.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn err_malformed_a_report_that_does_not_fit_its_kind(pool: PgPool) {
    let ingress = world(&pool).await;
    let mut no_amount = arrival_report("m-1", "500");
    no_amount.as_object_mut().unwrap().remove("amount");
    let mut unknown = arrival_report("m-2", "500");
    unknown["final"] = json!(true);
    let mut version = arrival_report("m-3", "500");
    version["version"] = json!(2);
    let mut kind = arrival_report("m-4", "500");
    kind["kind"] = json!("balance");
    let mut funder = document("m-5", "positions", json!({"positions": [], "funder": "x"}));
    funder["through"] = json!(null);
    let bad = [
        no_amount,
        unknown,
        version,
        kind,
        funder,
        arrival_report("m-6", "05"),
        arrival_report("m-7", "0"),
        arrival_report("m-8", "1.5"),
        document("m-9", "reversal", json!({"amount": "5", "position": "a"})),
        document(
            "m-10",
            "positions",
            json!({"positions": [{"position": "a", "amount": "1"},
                                 {"position": "a", "amount": "2"}]}),
        ),
    ];
    for d in bad {
        let (status, answer) = ingress.post(REPORTS, ALPHA, &signed(&d)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{d}: {answer}");
        assert_eq!(answer["error"], "ERR-Malformed", "{d}");
    }
    let mut envelope = signed(&arrival_report("m-11", "500"));
    envelope["signature"] = json!("0x00");
    let (status, _) = ingress.post(REPORTS, ALPHA, &envelope).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    envelope["report"] = json!("not json");
    let (status, _) = ingress.post(REPORTS, ALPHA, &envelope).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(count(&pool, "intake").await, 0);
}

/// §10: a report that fits the surface and not what it names is recorded as refused, with its named
/// error: the reference is spent, and it never becomes a report.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_be_5_a_report_of_an_asset_not_on_its_backend_is_err_unknown_route(pool: PgPool) {
    let ingress = world(&pool).await;
    let mut other_token = arrival_report("u-1", "500");
    other_token["token"] = json!("usdc");
    let mut other_asset = arrival_report("u-2", "500");
    other_asset["asset"] = json!(ASSET);
    for d in [other_token, other_asset] {
        let (status, answer) = ingress.post(REPORTS, ALPHA, &signed(&d)).await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        assert_eq!(answer["state"], "Refused");
        assert_eq!(answer["error"], "ERR-UnknownRoute");
    }
    assert_eq!(count(&pool, "attested_reports").await, 0);
    // In the log (REQ-ST-4), and a repeat answers alike (REQ-ID-1).
    let refusals: i64 = scalar(
        &pool,
        "SELECT count(*) FROM event_log WHERE kind = 'refusal' AND payload ->> 'kind' = 'report'",
    )
    .await;
    assert_eq!(refusals, 2);
    let mut other_token = arrival_report("u-1", "500");
    other_token["token"] = json!("usdc");
    let (status, answer) = ingress.post(REPORTS, ALPHA, &signed(&other_token)).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["error"], "ERR-UnknownRoute");
}

/// `REQ-BE-6`, guardrail 5: a reversal names the arrival it reverses, by the party's identifier, and
/// never takes more than it; a restoration names the reversal, in full, once.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_be_6_a_reversal_and_a_restoration_name_what_they_answer(pool: PgPool) {
    let ingress = world(&pool).await;
    ingress
        .post(REPORTS, ALPHA, &signed(&arrival_report("a-1", "500")))
        .await;
    let reversal = |id: &str, amount: &str, reverses: &str| {
        document(
            id,
            "reversal",
            json!({"amount": amount, "position": "acct-1", "reverses": reverses}),
        )
    };
    let restoration = |id: &str, amount: &str, restores: &str| {
        document(
            id,
            "restoration",
            json!({"amount": amount, "position": "acct-1", "restores": restores}),
        )
    };
    let cases = [
        (
            reversal("v-1", "100", "nothing"),
            Some("ERR-SourceNotFound"),
        ),
        (reversal("v-2", "501", "a-1"), Some("ERR-SourceMismatch")),
        (reversal("v-3", "300", "a-1"), None),
        (reversal("v-4", "201", "a-1"), Some("ERR-SourceMismatch")),
        (reversal("v-5", "200", "a-1"), None),
        (reversal("v-6", "1", "a-1"), Some("ERR-SourceMismatch")),
        (restoration("s-1", "300", "a-1"), Some("ERR-SourceNotFound")),
        (restoration("s-2", "299", "v-3"), Some("ERR-SourceMismatch")),
        (restoration("s-3", "300", "v-3"), None),
        (restoration("s-4", "300", "v-3"), Some("ERR-SourceConsumed")),
    ];
    for (d, error) in cases {
        let (status, answer) = ingress.post(REPORTS, ALPHA, &signed(&d)).await;
        assert_eq!(status, StatusCode::CREATED, "{d}: {answer}");
        match error {
            Some(e) => assert_eq!(answer["error"], e, "{d}: {answer}"),
            None => assert_eq!(answer["state"], "Recorded", "{d}: {answer}"),
        }
    }
}

/// `REQ-BE-1`: a snapshot of positions names the last report it reflects, one that exists.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_be_4_a_positions_report_names_a_report_it_reflects(pool: PgPool) {
    let ingress = world(&pool).await;
    let lines = json!([{"position": "acct-1", "amount": "500"}]);
    let (_, answer) = ingress
        .post(
            REPORTS,
            ALPHA,
            &signed(&document(
                "p-1",
                "positions",
                json!({"positions": lines, "through": "nothing"}),
            )),
        )
        .await;
    assert_eq!(answer["error"], "ERR-SourceNotFound", "{answer}");
    ingress
        .post(REPORTS, ALPHA, &signed(&arrival_report("a-1", "500")))
        .await;
    let (_, answer) = ingress
        .post(
            REPORTS,
            ALPHA,
            &signed(&document(
                "p-2",
                "positions",
                json!({"positions": lines, "through": "a-1"}),
            )),
        )
        .await;
    assert_eq!(answer["state"], "Recorded", "{answer}");
}

/// A `payout` needs a withdrawal of this backend whose instructions were accepted (`REQ-RQ-9`),
/// and its redemption final (`REQ-OT-3`, `not-final`, with nothing recorded).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ot_3_a_payout_report_needs_its_instructions_and_a_final_redemption(pool: PgPool) {
    let ingress = world(&pool).await;
    let payout = |id: &str, bridge: &str| {
        document(
            id,
            "payout",
            json!({"amount": "500", "position": "acct-1", "bridge_reference": bridge}),
        )
    };
    let (_, answer) = ingress
        .post(REPORTS, ALPHA, &signed(&payout("o-1", "bz-w-b1")))
        .await;
    assert_eq!(answer["error"], "ERR-SourceNotFound", "{answer}");

    exec(
        &pool,
        &format!(
            "INSERT INTO observations (network, block_number, block_hash, block_time, \
                 position_kind, position_index, token, amount, sender, executed, \
                 on_chain_reference) \
             VALUES ('{ISSUING}', 9, '\\xcc', now(), 'log', 0, 'fiat-issued', 500, \
                 '{ISSUING_ADDRESS}', TRUE, '\\xb1'); \
             INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                 destination_network, destination, state, source_network, source_block_number, \
                 source_block_hash, source_position_kind, source_position_index, \
                 on_chain_reference) \
             VALUES ('bz-w-b1', 'redemption-b1', 'withdrawal', '{ATTESTED}', 500, \
                 '{ATTESTED_NETWORK}', 'commitment:0x00', 'Received', '{ISSUING}', 9, '\\xcc', \
                 'log', 0, '\\xb1');"
        ),
    )
    .await;
    let (_, answer) = ingress
        .post(REPORTS, ALPHA, &signed(&payout("o-2", "bz-w-b1")))
        .await;
    assert_eq!(
        answer["error"], "ERR-SourceNotFound",
        "no instructions: {answer}"
    );

    exec(
        &pool,
        "INSERT INTO intake (request_reference, requester, kind, body, body_digest) \
           VALUES ('relay', 'alpha', 'withdrawal', '{}', '\\x01'); \
         INSERT INTO relayed_instructions (transfer_id, intake_id, instructions, commitment) \
           SELECT t.id, i.id, '\\x01', decode(repeat('00', 32), 'hex') \
           FROM transfers t, intake i WHERE i.request_reference = 'relay';",
    )
    .await;
    let before = count(&pool, "intake").await;
    let (status, answer) = ingress
        .post(REPORTS, ALPHA, &signed(&payout("o-3", "bz-w-b1")))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{answer}");
    assert_eq!(answer["error"], "not-final");
    assert_eq!(count(&pool, "intake").await, before);
    assert_eq!(count(&pool, "attested_reports").await, 0);
}

/// `REQ-RQ-3`: a deposit naming a report the core has not recorded yet is not final: nothing is
/// recorded and the reference is not spent, never a recorded `ERR-SourceNotFound`.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rq_3_a_deposit_on_a_report_not_yet_recorded_is_not_final(pool: PgPool) {
    let ingress = world(&pool).await;
    let (_, report) = ingress
        .post(REPORTS, ALPHA, &signed(&arrival_report("a-1", "500")))
        .await;
    let body = json!({
        "request_reference": "d-1",
        "coordinate": report["coordinate"],
        "destination": { "network": ISSUING, "address": ISSUING_ADDRESS },
        "refund_destination": { "network": ATTESTED_NETWORK, "address": "payer" },
    });
    let (status, answer) = ingress.post("/v0/deposits", ALPHA, &body).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{answer}");
    assert_eq!(answer["error"], "not-final");
    assert_eq!(count(&pool, "intake_answers").await, 0);
    assert_eq!(
        scalar::<i64>(
            &pool,
            "SELECT count(*) FROM intake WHERE request_reference = 'd-1'"
        )
        .await,
        0
    );
}

/// The store's own rules hold whoever writes: a report row names its declared party and key, and
/// the store numbers it (`REQ-BE-4`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_be_4_the_store_refuses_a_report_row_that_is_not_the_partys(pool: PgPool) {
    let ingress = world(&pool).await;
    let _ = &ingress;
    let public = hex::encode(
        SigningKey::from_bytes(&PARTY_KEY_SEED)
            .verifying_key()
            .to_bytes(),
    );
    exec(
        &pool,
        "INSERT INTO intake (request_reference, requester, kind, body, body_digest) \
           VALUES ('f', 'beta', 'report', '{}', '\\x01');",
    )
    .await;
    let insert = |requester: &str, key: &str, id_in_body: &str| {
        format!(
            "INSERT INTO attested_reports (network, seq, intake_id, requester, party, report_id, \
                 kind, asset, body, signed_bytes, signature, public_key, digest) \
             SELECT '{ATTESTED_NETWORK}', 99, i.id, '{requester}', '{PARTY}', 'f', 'arrival', \
                 '{ATTESTED}', jsonb_build_object('report_id', '{id_in_body}', 'kind', 'arrival', \
                     'network', '{ATTESTED_NETWORK}', 'asset', '{ATTESTED}'), \
                 '\\x01', decode(repeat('00', 64), 'hex'), '\\x{key}', \
                 decode(repeat('00', 32), 'hex') \
             FROM intake i WHERE i.request_reference = 'f'"
        )
    };
    let refused = |sql: String| {
        let pool = pool.clone();
        async move {
            let e = sqlx::raw_sql(&sql).execute(&pool).await.expect_err(&sql);
            assert_eq!(
                e.as_database_error().and_then(|d| d.code()).as_deref(),
                Some("BZ001"),
                "{e}"
            );
        }
    };
    refused(insert("beta", &public, "f")).await;
    refused(insert("alpha", &"00".repeat(32), "f")).await;
    refused(insert("alpha", &public, "other")).await;
    exec(&pool, &insert("alpha", &public, "f")).await;
    // The store's number, not the writer's.
    assert_eq!(
        scalar::<i64>(&pool, "SELECT seq FROM attested_reports").await,
        1
    );
    // Append-only.
    assert!(
        sqlx::query("UPDATE attested_reports SET party = 'x'")
            .execute(&pool)
            .await
            .is_err()
    );
}
