// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! PINS TODAY'S BEHAVIOUR, WHICH IS NOT DESIRED AND IS NOT A RULING.
//!
//! The exposure recorded in `features/F-010-venues/NOTES.md` ("What the binding rules do not do") and in
//! amendment 0018 ("The exposure the binding rules do not close"): a deposit binds a final observation at
//! the reserve to whichever requester names its coordinate first, and a grant is only a kind. So a second
//! requester holding the deposit grant binds an ordinary arrival, and a swap's output, that another
//! requester was waiting for, to a destination of its own; the rightful one is refused `ERR-SourceConsumed`.
//! The mint is backed (the observed amount, once); what is lost is attribution and direction.
//!
//! This test exists so that the exposure is not described only in prose that rests on a scratch run. It
//! names no criterion and verifies none (a green run here is not a claim that anything is met).
//!
//! The ruling that would have changed this was withdrawn: `d-0026-holdout` (2026-11-09) withdrew amendment
//! 0026, and amendment 0029 (directed deposits) replaces this path. Until it lands, this assertion stands.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;

/// Bob's account (a well-known test account), another destination than `ISSUING_ADDRESS`.
const OTHER_DESTINATION: &str = "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty";

fn to(body: Value, address: &str) -> Value {
    let mut body = body;
    body["destination"]["address"] = json!(address);
    body
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn today_a_second_requester_binds_an_arrival_and_a_swap_output_another_was_waiting_for(
    pool: PgPool,
) {
    let ingress = Ingress::new(&pool).await;
    for (block, kind) in [(60, "arrival"), (61, "conversion_output")] {
        let coordinate = observe_seen(
            &pool,
            block,
            Seen {
                kind: Some(kind),
                ..arrival_seen(1_000)
            },
        )
        .await;
        // `beta` holds only the deposit grant. It names the coordinate before `alpha`, whose
        // arrival or swap it was, and mints it to a destination of its own.
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
        assert_eq!(taken["state"], "Accepted", "{kind}: {taken}");
        // The rightful requester is refused.
        let (status, late) = ingress
            .post(
                "/v0/deposits",
                ALPHA,
                &deposit_body(&format!("alpha-{kind}"), coordinate),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(late["state"], "Refused", "{kind}: {late}");
        assert_eq!(late["error"], "ERR-SourceConsumed", "{kind}: {late}");
    }
    let bound_to_beta: i64 = scalar(
        &pool,
        &format!(
            "SELECT count(*) FROM transfers WHERE kind = 'deposit' AND destination = '{OTHER_DESTINATION}'"
        ),
    )
    .await;
    assert_eq!(bound_to_beta, 2);
}
