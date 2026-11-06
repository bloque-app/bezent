// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Amendment 0026 (`d-requester-arrivals`, ruled 2026-11-06: "Bind by sender"): each requester declares
//! its senders, and a deposit binds only an arrival sent from one of the requesting requester's declared
//! senders (`REQ-PN-3`, `REQ-IN-3`, `AC-A2.5`). Through the real ingress.
//!
//! Until the ruling this file pinned the exposure of amendment 0018 ("The exposure the binding rules do
//! not close"): a second requester holding the deposit grant bound an ordinary arrival, and a swap's
//! output, that another requester was waiting for, to a destination of its own, and the rightful request
//! was refused `ERR-SourceConsumed`. The same scenario now fails to claim either.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;

/// Bob's account (a well-known test account), another destination than `ISSUING_ADDRESS`.
const OTHER_DESTINATION: &str = "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty";
/// alpha's own sender, as an endpoint spells an EVM address (checksummed).
const ALPHA_SENDER: &str = "0x8ba1f109551bD432803012645Ac136ddd64DBA72";
/// beta's own sender.
const BETA_SENDER: &str = "0x00000000000000000000000000000000000000b2";
/// A sender no requester declared.
const STRANGER: &str = "0x00000000000000000000000000000000000000ee";
/// The venue's pool a conversion's output is sent from.
const POOL: &str = "0x00000000000000000000000000000000000000cc";

fn to(body: Value, address: &str) -> Value {
    let mut body = body;
    body["destination"]["address"] = json!(address);
    body
}

/// Declares alpha's and beta's own senders on the origin network, as the migration owner does
/// (`crates/store/README.md`, "Deploying"). alpha's is declared in lower case: on a network of
/// address format `evm` the comparison ignores case.
async fn declare_own_senders(pool: &PgPool) {
    exec(
        pool,
        &format!(
            "INSERT INTO requester_senders (requester, network, address) VALUES \
               ('alpha', '{ORIGIN}', '{}'), ('beta', '{ORIGIN}', '{BETA_SENDER}')",
            ALPHA_SENDER.to_ascii_lowercase()
        ),
    )
    .await;
}

/// A final arrival of the origin token at the reserve from `sender`, of `kind`, at `block`.
async fn arrival_from(pool: &PgPool, block: i64, sender: &str, kind: &str) -> Value {
    observe_seen(
        pool,
        block,
        Seen {
            sender,
            kind: Some(kind),
            ..arrival_seen(1_000)
        },
    )
    .await
}

/// `AC-A2.5`, `REQ-PN-3`, `REQ-IN-3`: a requester binds an arrival from its own declared sender, and
/// another requester's attempt on it is refused `ERR-SenderNotDeclared`, binding nothing; the rightful
/// request then succeeds.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a2_5_req_pn_3_req_in_3_an_arrival_binds_only_for_the_requester_that_declared_its_sender(
    pool: PgPool,
) {
    let ingress = Ingress::new(&pool).await;
    declare_own_senders(&pool).await;
    let coordinate = arrival_from(&pool, 60, ALPHA_SENDER, "arrival").await;

    let (status, theirs) = ingress
        .post(
            "/v0/deposits",
            BETA,
            &to(
                deposit_body("beta-1", coordinate.clone()),
                OTHER_DESTINATION,
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{theirs}");
    assert_eq!(theirs["state"], "Refused", "{theirs}");
    assert_eq!(theirs["error"], "ERR-SenderNotDeclared", "{theirs}");
    assert!(
        theirs.get("source").is_none(),
        "a refusal binds nothing: {theirs}"
    );

    let (status, ours) = ingress
        .post("/v0/deposits", ALPHA, &deposit_body("alpha-1", coordinate))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{ours}");
    assert_eq!(ours["state"], "Accepted", "{ours}");
    let bound: i64 = scalar(
        &pool,
        &format!(
            "SELECT count(*) FROM transfers WHERE kind = 'deposit' AND source_block_number = 60 \
               AND destination = '{ISSUING_ADDRESS}'"
        ),
    )
    .await;
    assert_eq!(bound, 1);
}

/// `AC-A2.5`, `REQ-PN-3`, `REQ-IN-6`, `AC-A4.2`: an arrival from a sender no requester declared is
/// bound by no one, and stays an unassociated arrival with its owner (`CND-Unassociated`): not minted,
/// not dropped. Its release is still a refund request with a second approver (`REQ-RQ-6`, unchanged).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a2_5_req_pn_3_req_in_6_an_arrival_from_an_undeclared_sender_is_bound_by_no_one_and_stays_unassociated(
    pool: PgPool,
) {
    let ingress = Ingress::new(&pool).await;
    declare_own_senders(&pool).await;
    let coordinate = arrival_from(&pool, 61, STRANGER, "arrival").await;

    for (credential, reference) in [(ALPHA, "alpha-1"), (BETA, "beta-1")] {
        let (_, answer) = ingress
            .post(
                "/v0/deposits",
                credential,
                &deposit_body(reference, coordinate.clone()),
            )
            .await;
        assert_eq!(answer["state"], "Refused", "{answer}");
        assert_eq!(answer["error"], "ERR-SenderNotDeclared", "{answer}");
    }
    let unassociated: i64 = scalar(
        &pool,
        "SELECT count(*) FROM unassociated_arrivals WHERE block_number = 61 AND owner = 'operations'",
    )
    .await;
    assert_eq!(
        unassociated, 1,
        "the arrival stays unassociated with its owner"
    );
    let bound: i64 = scalar(
        &pool,
        "SELECT count(*) FROM transfers WHERE source_block_number = 61",
    )
    .await;
    assert_eq!(bound, 0);

    // The release path is unchanged: a refund request takes it in, for a second approver.
    let (status, refund) = ingress
        .post(
            "/v0/refunds",
            ALPHA,
            &json!({
                "request_reference": "alpha-refund",
                "coordinate": coordinate,
                "destination": { "network": ORIGIN, "address": ORIGIN_ADDRESS },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{refund}");
    assert_eq!(refund["state"], "Received", "{refund}");
}

/// `REQ-PN-3`, `REQ-IN-3`, `REQ-CV-2`: the scenario that pinned amendment 0018's exposure, now failing
/// to claim. A second requester holding the deposit grant names, before the rightful one, an ordinary
/// arrival and a swap's output; neither binds to its destination. The ordinary arrival then binds for
/// the requester whose sender it came from. The output is sent from the venue's pool, which no
/// requester declared: named directly (amendment 0018, Open 2) it binds for neither; it is bound
/// through a deposit of a convertible arrival instead (`services/core/tests/conversions.rs`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pn_3_req_in_3_a_second_requester_no_longer_binds_an_arrival_or_a_swap_output_another_was_waiting_for(
    pool: PgPool,
) {
    let ingress = Ingress::new(&pool).await;
    declare_own_senders(&pool).await;
    for (block, kind, sender, rightful) in [
        (60, "arrival", ALPHA_SENDER, "Accepted"),
        (61, "conversion_output", POOL, "Refused"),
    ] {
        let coordinate = arrival_from(&pool, block, sender, kind).await;
        let (_, taken) = ingress
            .post(
                "/v0/deposits",
                BETA,
                &to(
                    deposit_body(&format!("beta-{kind}"), coordinate.clone()),
                    OTHER_DESTINATION,
                ),
            )
            .await;
        assert_eq!(taken["state"], "Refused", "{kind}: {taken}");
        assert_eq!(taken["error"], "ERR-SenderNotDeclared", "{kind}: {taken}");
        let (status, late) = ingress
            .post(
                "/v0/deposits",
                ALPHA,
                &deposit_body(&format!("alpha-{kind}"), coordinate),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(late["state"], rightful, "{kind}: {late}");
        if rightful == "Refused" {
            assert_eq!(late["error"], "ERR-SenderNotDeclared", "{kind}: {late}");
        }
    }
    let bound_to_beta: i64 = scalar(
        &pool,
        &format!(
            "SELECT count(*) FROM transfers WHERE kind = 'deposit' AND destination = '{OTHER_DESTINATION}' \
               AND source_network IS NOT NULL"
        ),
    )
    .await;
    assert_eq!(bound_to_beta, 0);
}

/// `REQ-PN-3`, `INV-2`: the store refuses, whoever writes it, an answer that binds a deposit to a
/// request whose requester did not declare the arrival's sender; the same answer for the requester
/// that did is accepted. The ingress's refusal is the first check; this is the store's backstop.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pn_3_the_store_refuses_an_answer_binding_a_deposit_to_a_requester_that_did_not_declare_its_sender(
    pool: PgPool,
) {
    let _ = Ingress::new(&pool).await;
    declare_own_senders(&pool).await;
    arrival_from(&pool, 62, ALPHA_SENDER, "arrival").await;
    let ingress = ingress_pool(&pool).await;
    for (requester, refused) in [("beta", true), ("alpha", false)] {
        let mut tx = ingress.begin().await.unwrap();
        let intake: i64 = sqlx::query_scalar(
            "INSERT INTO intake (request_reference, requester, kind, body, body_digest) \
             VALUES ($1, $2, 'deposit', '{}', '\\x01') RETURNING id",
        )
        .bind(format!("{requester}-store"))
        .bind(requester)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let transfer: i64 = sqlx::query_scalar(
            "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                 destination_network, destination, refund_destination_network, refund_destination, \
                 source_network, source_block_number, source_block_hash, source_position_kind, \
                 source_position_index, state) \
             VALUES ($1, $2, 'deposit', $3, 1000, $4, $5, $6, $7, $6, 62, $8, 'log', 0, 'Received') \
             RETURNING id",
        )
        .bind(format!("bz-d-store-{requester}"))
        .bind(format!("{requester}-store"))
        .bind(ASSET)
        .bind(ISSUING)
        .bind(ISSUING_ADDRESS)
        .bind(ORIGIN)
        .bind(ORIGIN_ADDRESS)
        .bind(block_hash(62))
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let answered = sqlx::query(
            "INSERT INTO intake_answers (intake_id, transfer_id, bridge_reference) \
             SELECT $1, id, bridge_reference FROM transfers WHERE id = $2",
        )
        .bind(intake)
        .bind(transfer)
        .execute(&mut *tx)
        .await;
        if refused {
            let err = answered.expect_err("an answer binding beta to alpha's arrival");
            let text = err.to_string();
            assert!(text.contains("REQ-PN-3"), "{text}");
            assert_eq!(
                err.as_database_error().and_then(|e| e.code()).as_deref(),
                Some("BZ001"),
                "{text}"
            );
            tx.rollback().await.unwrap();
        } else {
            answered.expect("an answer binding alpha to its own arrival");
            tx.commit().await.unwrap();
        }
    }
}
