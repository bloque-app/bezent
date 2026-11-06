// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Migration 0135, amendment 0026 (`d-requester-arrivals`, "Bind by sender"): each requester declares its
//! senders (`requester_senders`, the migration owner's declaration), and the answer that binds a deposit to
//! its request is refused unless the arrival at the deposit's source was sent from one of the request's
//! requester's declared senders (`REQ-PN-3`, `REQ-IN-3`).
//!
//! Store layer. Everything is written as the migration owner, whom this guard does not exempt. The
//! ingress's refusal by name (`ERR-SenderNotDeclared`) is `services/ingress/tests/bind_by_sender.rs`.

mod common;

use common::*;
use sqlx::PgPool;

const RESERVE: &str = "0xreserve";
const SENDER: &str = "0xAbCdEf0000000000000000000000000000000001";

async fn declarations(pool: &PgPool) {
    exec(
        pool,
        &format!(
            "INSERT INTO networks (network, role, address_format) VALUES \
               ('issuing', 'issuing', 'opaque'), ('origin-a', 'origin', 'evm'), \
               ('origin-b', 'origin', 'opaque'); \
             INSERT INTO assets (asset, precision, assurance_class) VALUES ('asset-a', 6, 'public'); \
             INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
               ('origin-a', 'token-a', 'asset-a', 6), ('origin-b', 'token-b', 'asset-a', 6); \
             INSERT INTO reserves (asset, network, address) VALUES \
               ('asset-a', 'origin-a', '{RESERVE}'), ('asset-a', 'origin-b', '{RESERVE}'); \
             INSERT INTO requesters (requester) VALUES ('alpha'), ('beta'); \
             INSERT INTO requester_senders (requester, network, address) VALUES \
               ('alpha', 'origin-a', lower('{SENDER}')), ('alpha', 'origin-b', 'Opaque-Sender')"
        ),
    )
    .await;
}

/// An executed, final arrival of `token` at the reserve on `network`, at `block`, log 0, from `sender`.
async fn arrival(pool: &PgPool, network: &str, block: i64, token: &str, sender: Option<&str>) {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, token, amount, sender, recipient, executed, kind) \
         VALUES ($1, $2, '\\xaa', $3::timestamptz, 'log', 0, $4, 1000, $5, $6, TRUE, 'arrival') \
         RETURNING id",
    )
    .bind(network)
    .bind(block)
    .bind(BLOCK_TIME)
    .bind(token)
    .bind(sender)
    .bind(RESERVE)
    .fetch_one(pool)
    .await
    .expect("observe");
    exec(
        pool,
        &format!(
            "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
               VALUES ('{network}', {block} + 20, '\\xff', '{BLOCK_TIME}') ON CONFLICT DO NOTHING; \
             INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
               final_block_hash) VALUES ({id}, 'final', {block} + 20, '\\xff')"
        ),
    )
    .await;
}

/// A request of `requester` and the deposit it binds to the arrival at (`network`, `block`, log 0), as
/// the ingress records them before it answers: the request's id and the transfer's.
async fn request_and_deposit(
    pool: &PgPool,
    requester: &str,
    reference: &str,
    network: &str,
    block: i64,
) -> (i64, i64) {
    let intake: i64 = sqlx::query_scalar(
        "INSERT INTO intake (request_reference, requester, kind, body, body_digest) \
         VALUES ($1, $2, 'deposit', '{}', '\\x01') RETURNING id",
    )
    .bind(reference)
    .bind(requester)
    .fetch_one(pool)
    .await
    .expect("intake");
    let transfer: i64 = sqlx::query_scalar(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index) \
         VALUES ('bz-d-' || $1, $1, 'deposit', 'asset-a', 1000, 'issuing', 'dest-1', 'Received', \
             $2, $3, '\\xaa', 'log', 0) RETURNING id",
    )
    .bind(reference)
    .bind(network)
    .bind(block)
    .fetch_one(pool)
    .await
    .expect("the deposit");
    (intake, transfer)
}

async fn answer(pool: &PgPool, intake: i64, transfer: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO intake_answers (intake_id, transfer_id, bridge_reference) \
         SELECT $1, id, bridge_reference FROM transfers WHERE id = $2",
    )
    .bind(intake)
    .bind(transfer)
    .execute(pool)
    .await
    .map(|_| ())
}

/// `REQ-PN-3`, `REQ-IN-3`: the answer binding a deposit is refused for a requester that did not
/// declare the arrival's sender, and accepted for the one that did: compared without case on a network
/// of address format `evm`, exactly on any other.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pn_3_req_in_3_a_deposit_is_answered_only_to_a_requester_that_declared_its_arrivals_sender(
    pool: PgPool,
) {
    declarations(&pool).await;
    arrival(&pool, "origin-a", 1, "token-a", Some(SENDER)).await;
    let (theirs, deposit) = request_and_deposit(&pool, "beta", "beta-1", "origin-a", 1).await;
    let err = answer(&pool, theirs, deposit)
        .await
        .expect_err("beta declared no sender");
    assert_refusal(&err, "REQ-PN-3", "an answer to beta");
    // The same deposit, answered to alpha's request instead.
    let ours: i64 = sqlx::query_scalar(
        "INSERT INTO intake (request_reference, requester, kind, body, body_digest) \
         VALUES ('alpha-1', 'alpha', 'deposit', '{}', '\\x01') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    answer(&pool, ours, deposit)
        .await
        .expect("alpha declared the sender, in another case");

    // On a network whose addresses are opaque, the spelling is the address.
    arrival(&pool, "origin-b", 3, "token-b", Some("opaque-sender")).await;
    let (ours, deposit) = request_and_deposit(&pool, "alpha", "alpha-2", "origin-b", 3).await;
    let err = answer(&pool, ours, deposit)
        .await
        .expect_err("another spelling on an opaque network");
    assert_refusal(&err, "REQ-PN-3", "an opaque sender in another case");
    arrival(&pool, "origin-b", 4, "token-b", Some("Opaque-Sender")).await;
    let (ours, deposit) = request_and_deposit(&pool, "alpha", "alpha-3", "origin-b", 4).await;
    answer(&pool, ours, deposit)
        .await
        .expect("the declared spelling");
}

/// `REQ-PN-3`, `REQ-IN-6`: an arrival with no recorded sender is never declared, so no answer binds it;
/// an answer that binds nothing (a refusal, or a deposit with no source) is not checked.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pn_3_req_in_6_no_sender_binds_for_no_one_and_a_refusal_is_not_checked(pool: PgPool) {
    declarations(&pool).await;
    arrival(&pool, "origin-a", 1, "token-a", None).await;
    let (ours, deposit) = request_and_deposit(&pool, "alpha", "alpha-1", "origin-a", 1).await;
    let err = answer(&pool, ours, deposit).await.expect_err("no sender");
    assert_refusal(&err, "REQ-PN-3", "an arrival with no sender");

    // beta's requests: one refused, its transfer binding nothing, and one whose deposit has no source.
    for (reference, error) in [("beta-1", "'ERR-SenderNotDeclared'"), ("beta-2", "NULL")] {
        let intake: i64 = sqlx::query_scalar(
            "INSERT INTO intake (request_reference, requester, kind, body, body_digest) \
             VALUES ($1, 'beta', 'deposit', '{}', '\\x01') RETURNING id",
        )
        .bind(reference)
        .fetch_one(&pool)
        .await
        .unwrap();
        let unbound = transfer(&pool, &format!("bz-r-{reference}")).await;
        exec(
            &pool,
            &format!(
                "INSERT INTO intake_answers (intake_id, transfer_id, bridge_reference, error) \
                   VALUES ({intake}, {unbound}, 'bz-r-{reference}', {error})"
            ),
        )
        .await;
    }
}

/// `REQ-PN-3`: what a requester declared, as `sender_declared()` reads it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pn_3_sender_declared_reads_the_declaration(pool: PgPool) {
    declarations(&pool).await;
    for (requester, network, sender, declared) in [
        ("alpha", "origin-a", Some(SENDER), true),
        (
            "alpha",
            "origin-a",
            Some("0xabcdef0000000000000000000000000000000001"),
            true,
        ),
        ("alpha", "origin-b", Some(SENDER), false),
        ("beta", "origin-a", Some(SENDER), false),
        ("alpha", "origin-a", None, false),
    ] {
        let got: bool = sqlx::query_scalar("SELECT sender_declared($1, $2, $3)")
            .bind(requester)
            .bind(network)
            .bind(sender)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(got, declared, "{requester} {network} {sender:?}");
    }
}
