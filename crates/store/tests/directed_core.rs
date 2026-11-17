// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-018, T-018-07 to T-018-09 (migration 0145, amendment 0029 A5, Open 1 to 3): the store's rules
//! for what the core does with a directed arrival, whoever writes. The core's own behaviour is
//! `services/core/tests/directed.rs`.
//!
//! - `CND-DepositWindow` is a condition, and a waiting deposit is a claim on its route's window
//!   (`REQ-LM-1`, Open 1 and 5).
//! - The core's return of a directed arrival is accepted with no release; nobody else's is, and no
//!   other refund of an unassociated arrival is (`REQ-IN-6`, `REQ-IN-11`, Open 3).
//! - Only a directed deposit in review for `CND-DestinationRefused`, entered from `Source-Final`, with
//!   no signature on any of its intents, may be refunded, by the core, under an approved resolution,
//!   to its refund destination (`REQ-RV-2`, `REQ-GV-5`, `INV-8`, Open 2).

mod common;

use bezent_store::login::{as_login, ensure_login, pool_options};
use common::*;
use sqlx::PgPool;

const PASSWORD: &str = "test-only-directed-core-password";
const RESERVE: &str = "0xA0b86991c6218b36C1d19D4a2e9Eb0cE3606eB48";
const DIRECTING: &str = "0xD1Ec7ed00000000000000000000000000000d1ec";
const REFUND: &str = "0x52908400098527886E0F7030069857D2E4169EE7";
const RECIPIENT: &str = "0x8eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a48";
const AMOUNT: i64 = 5_000_000;

async fn login(owner: &PgPool, name: &str, role: &str) -> PgPool {
    ensure_login(owner, name, role, PASSWORD)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    pool_options()
        .max_connections(1)
        .connect_with(as_login(&owner.connect_options(), name, PASSWORD))
        .await
        .unwrap_or_else(|e| panic!("log in as {name}: {e}"))
}

async fn declarations(pool: &PgPool) {
    exec(
        pool,
        &format!(
            "INSERT INTO networks (network, role, address_format) VALUES \
               ('issuing', 'issuing', 'ss58'), ('origin-a', 'origin', 'evm'); \
             INSERT INTO assets (asset, precision, assurance_class) VALUES ('asset-a', 6, 'public'); \
             INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
               ('origin-a', 'token-a', 'asset-a', 6); \
             INSERT INTO reserves (asset, network, address) VALUES \
               ('asset-a', 'origin-a', '{RESERVE}'); \
             INSERT INTO routes (asset, network, direction, minimum, maximum, window_limit, \
                 window_seconds, capacity, cutoffs, completion_budget_seconds) VALUES \
               ('asset-a', 'origin-a', 'deposit', 1, 6000000, 10000000, 86400, 100, '[]', 3600)"
        ),
    )
    .await;
}

/// A final directed arrival at (`origin-a`, `block`, `\xaa`, log 1), with `kind` its observation's
/// kind (`directed_arrival`, or `arrival` for a plain one).
async fn final_arrival(pool: &PgPool, block: i64, kind: &str) {
    let event = if kind == "directed_arrival" {
        "DirectedArrival"
    } else {
        "Arrival"
    };
    exec(
        pool,
        &format!(
            "WITH o AS (INSERT INTO observations (network, block_number, block_hash, block_time, \
                 position_kind, position_index, token, amount, sender, recipient, executed, kind, \
                 detail) \
               VALUES ('origin-a', {block}, '\\xaa', '{BLOCK_TIME}', 'log', 1, 'token-a', {AMOUNT}, \
                 '0xfunder', '{RESERVE}', TRUE, '{kind}', \
                 jsonb_build_object('event', '{event}', 'directing', '{DIRECTING}', \
                   'direction_log', 2, 'recipient', '{RECIPIENT}', 'refund', '{REFUND}')) \
               RETURNING id) \
             INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
               final_block_hash) SELECT id, 'final', {block} + 20, '\\xff' FROM o; \
             INSERT INTO final_heads (network, block_number, block_hash, block_time) \
               VALUES ('origin-a', {block} + 20, '\\xff', '{BLOCK_TIME}') ON CONFLICT DO NOTHING"
        ),
    )
    .await;
}

async fn directed_deposit(core: &PgPool, block: i64) -> i64 {
    sqlx::query_scalar("SELECT directed_deposit('origin-a', $1, '\\xaa', 'log', 1)")
        .bind(block)
        .fetch_one(core)
        .await
        .expect("directed_deposit")
}

/// The `bz-u-` reference of the return of the arrival at `block`, as the owner derives it (the
/// derivation is not the core's to call; the core derives it in Rust).
async fn return_reference(pool: &PgPool, block: i64) -> String {
    text(
        pool,
        &format!("SELECT refund_reference('origin-a', {block}, '\\xaa', 'log', 1)"),
    )
    .await
}

/// The core's return of the arrival at `block`, in Received; returns its id.
async fn give_back(owner: &PgPool, core: &PgPool, block: i64) -> i64 {
    let reference = return_reference(owner, block).await;
    sqlx::query_scalar(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index) \
         VALUES ($4, 'return-' || $1, 'refund', \
             'asset-a', $2, 'origin-a', $3, 'Received', 'origin-a', $1, '\\xaa', 'log', 1) \
         RETURNING id",
    )
    .bind(block)
    .bind(AMOUNT)
    .bind(REFUND)
    .bind(reference)
    .fetch_one(core)
    .await
    .expect("the core's return")
}

/// `REQ-IN-11`, `REQ-ID-2`: the payout reference of a directed arrival's return is derived as
/// `services/core/src/refs.rs::unassociated_refund_reference` derives it (the same vector is pinned
/// there, `req_in_11_bridge_references_match_the_ingress_and_the_store`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_11_the_return_reference_is_the_cores(pool: PgPool) {
    assert_eq!(
        text(
            &pool,
            "SELECT encode(directed_return_reference('origin-a', 7, '\\xaa', 'log', 1), 'hex')"
        )
        .await,
        "d56cb2ff42543eae4fc551d92778ae1e65d180bc21dbe7a46a2556691ddc0edb"
    );
}

/// `REQ-IN-11`, `REQ-IN-6`, `REQ-GV-5` (Open 3: a return needs no second approver): the core's
/// return of a directed arrival is accepted by the core with no recorded release. A plain arrival's
/// refund still needs one, from the core as from anyone.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_11_the_core_accepts_its_return_with_no_release_and_nothing_else(pool: PgPool) {
    declarations(&pool).await;
    final_arrival(&pool, 7, "directed_arrival").await;
    final_arrival(&pool, 8, "arrival").await;
    let core = login(&pool, "dc_core_a_for_tests", bezent_store::CORE_ROLE).await;

    let returned = give_back(&pool, &core, 7).await;
    exec(
        &core,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state) \
             VALUES ({returned}, 'Received', 'Accepted')"
        ),
    )
    .await;
    assert_eq!(
        text(
            &pool,
            &format!("SELECT state::text FROM transfers WHERE id = {returned}")
        )
        .await,
        "Accepted"
    );

    // A plain arrival's refund, as the ingress takes it in: no release, no acceptance.
    let plain: i64 = sqlx::query_scalar(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index) \
         VALUES ('bz-u-plain', 'r-plain', 'refund', 'asset-a', $1, 'origin-a', $2, 'Received', \
             'origin-a', 8, '\\xaa', 'log', 1) RETURNING id",
    )
    .bind(AMOUNT)
    .bind(REFUND)
    .fetch_one(&pool)
    .await
    .unwrap();
    let err = sqlx::raw_sql(&format!(
        "INSERT INTO transitions (transfer_id, from_state, to_state) \
         VALUES ({plain}, 'Received', 'Accepted')"
    ))
    .execute(&core)
    .await
    .expect_err("a plain refund accepted with no release");
    assert_refusal(&err, "REQ-IN-6", "a plain refund with no release");
}

/// `REQ-LM-1` (amendment 0029, Open 1 and 5): `CND-DepositWindow` is a condition a deposit holds by
/// a transition, and while it holds it the deposit is a claim on its route's window, which the
/// ingress reads too; one deposit's question (`directed_window_fits()`) counts only those ahead of
/// it by coordinate.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_lm_1_a_waiting_deposit_is_a_claim_on_the_window(pool: PgPool) {
    declarations(&pool).await;
    for block in [7, 8, 9] {
        final_arrival(&pool, block, "directed_arrival").await;
    }
    let core = login(&pool, "dc_core_b_for_tests", bezent_store::CORE_ROLE).await;
    let a = directed_deposit(&core, 7).await;
    let b = directed_deposit(&core, 8).await;
    let c = directed_deposit(&core, 9).await;
    let fits = |id: i64| {
        let core = core.clone();
        async move {
            sqlx::query_scalar::<_, bool>("SELECT directed_window_fits($1)")
                .bind(id)
                .fetch_one(&core)
                .await
                .unwrap()
        }
    };
    // a, b, c of 5 each against a window of 10: with a and b taken, c does not fit.
    assert!(!fits(c).await);
    for id in [b, c] {
        exec(
            &core,
            &format!(
                "INSERT INTO transitions (transfer_id, from_state, to_state, condition) \
                 VALUES ({id}, 'Source-Final', 'Source-Final', 'CND-DepositWindow')"
            ),
        )
        .await;
    }
    // b waits behind nothing but a's share: 5 + 5 fits; c waits behind b: 5 + 5 + 5 does not.
    assert!(fits(b).await);
    assert!(!fits(c).await);
    assert_eq!(
        text(
            &pool,
            "SELECT used FROM route_usage_locked('asset-a', 'origin-a', 'deposit', 86400)"
        )
        .await,
        "15000000",
        "a's share and the claims of b and c"
    );
    let _ = a;
}

/// `REQ-RV-2`, `REQ-GV-5`, `INV-8` (Open 2): the refund of a directed deposit is refused while the
/// deposit is not in such a review, for a requester's deposit whatever its review, and to any other
/// destination; the cancellation of a first attempt is refused for any other deposit.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rv_2_only_a_directed_deposit_in_review_for_its_destination_is_refunded(pool: PgPool) {
    declarations(&pool).await;
    final_arrival(&pool, 7, "directed_arrival").await;
    let core = login(&pool, "dc_core_c_for_tests", bezent_store::CORE_ROLE).await;
    let id = directed_deposit(&core, 7).await;
    let bridge = return_reference(&pool, 7).await;
    let payout = text(
        &pool,
        "SELECT encode(directed_return_reference('origin-a', 7, '\\xaa', 'log', 1), 'hex')",
    )
    .await;
    let refund_sql = |destination: &str| {
        format!(
            "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                 destination_network, destination, refund_of, state, source_network, \
                 source_block_number, source_block_hash, source_position_kind, \
                 source_position_index, on_chain_reference) \
             VALUES ('{bridge}', 'refund-7', 'refund', \
                 'asset-a', {AMOUNT}, 'origin-a', '{destination}', {id}, 'Received', 'origin-a', 7, \
                 '\\xaa', 'log', 1, '\\x{payout}')"
        )
    };
    // At Source-Final: no review, no refund.
    let err = sqlx::raw_sql(&refund_sql(REFUND))
        .execute(&core)
        .await
        .expect_err("a refund out of no review");
    assert_refusal(&err, "REQ-RV-2", "a refund out of no review");

    // In review for its destination, with no resolution: still refused.
    exec(
        &pool,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state, condition) \
             VALUES ({id}, 'Source-Final', 'Needs-Review', 'CND-DestinationRefused')"
        ),
    )
    .await;
    assert!(flag(&pool, &format!("SELECT directed_review_refund_open({id})")).await);
    let err = sqlx::raw_sql(&refund_sql(REFUND))
        .execute(&core)
        .await
        .expect_err("a refund with no approved resolution");
    assert_refusal(&err, "REQ-GV-5", "a refund with no approved resolution");

    // An approved resolution: to another address it is refused, whoever writes; to the refund
    // destination the deposit took from the arrival, the core may make it.
    exec(
        &pool,
        "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
         VALUES ('issuing', 100, '\\xbb', '2026-10-13T01:00:00Z')",
    )
    .await;
    let ops = [
        operator_login(&pool, OPERATORS[0]).await,
        operator_login(&pool, OPERATORS[1]).await,
    ];
    let action: i64 = sqlx::query_scalar(
        "INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, \
             block_hash, block_time, justification) \
         VALUES ('review-resolution', $1, 'Refunding', 'issuing', 100, '\\xbb', \
             '2026-10-13T01:00:00Z', 'cannot receive') RETURNING id",
    )
    .bind(id)
    .fetch_one(&ops[0])
    .await
    .unwrap();
    exec(
        &ops[1],
        &format!(
            "INSERT INTO manual_approvals (action_id, justification) VALUES ({action}, 'checked')"
        ),
    )
    .await;
    let other = "0x00000000000000000000000000000000000000ee";
    let err = sqlx::raw_sql(&refund_sql(other))
        .execute(&core)
        .await
        .expect_err("a refund to another address");
    assert_refusal(&err, "REQ-RF-2", "a refund to another address");
    assert_refused(&pool, &refund_sql(other), "REQ-RF-2").await;
    exec(&core, &refund_sql(REFUND)).await;
    assert_eq!(
        text(
            &pool,
            &format!("SELECT destination FROM transfers WHERE refund_of = {id}")
        )
        .await,
        REFUND
    );
}

/// `INV-8`, `REQ-EO-6` (amendment 0014, Open 2): a first attempt is cancelled only for a directed
/// deposit in such a review; a requester's deposit's first attempt never is.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_8_a_first_attempt_is_cancelled_only_for_a_directed_deposit_in_review(pool: PgPool) {
    declarations(&pool).await;
    final_arrival(&pool, 7, "arrival").await;
    // A requester's deposit bound to the plain arrival, in review for its destination, with an
    // approved resolution into Refunding and a mint ordered, never signed.
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index, \
             source_final_network, source_final_block_number, source_final_block_hash, \
             source_final_block_time, source_final_seen_at) \
         VALUES ('bz-d-req', 'r-req', 'deposit', 'asset-a', $1, 'issuing', $2, 'Source-Final', \
             'origin-a', 7, '\\xaa', 'log', 1, 'origin-a', 27, '\\xff', $3::timestamptz, now()) \
         RETURNING id",
    )
    .bind(AMOUNT)
    .bind(RECIPIENT)
    .bind(BLOCK_TIME)
    .fetch_one(&pool)
    .await
    .unwrap();
    exec(
        &pool,
        &format!(
            "INSERT INTO intents (transfer_id, network, purpose, attempt, payload) \
             VALUES ({id}, 'issuing', 'mint', 1, '{{\"order\": {{}}, \"amount\": \"5\"}}'); \
             INSERT INTO transitions (transfer_id, from_state, to_state, condition) \
             VALUES ({id}, 'Source-Final', 'Needs-Review', 'CND-DestinationRefused')"
        ),
    )
    .await;
    assert!(!flag(&pool, &format!("SELECT directed_review_refund_open({id})")).await);
    let ops = [
        operator_login(&pool, OPERATORS[0]).await,
        operator_login(&pool, OPERATORS[1]).await,
    ];
    exec(
        &pool,
        "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
         VALUES ('issuing', 100, '\\xbb', '2026-10-13T01:00:00Z')",
    )
    .await;
    let action: i64 = sqlx::query_scalar(
        "INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, \
             block_hash, block_time, justification) \
         VALUES ('review-resolution', $1, 'Refunding', 'issuing', 100, '\\xbb', \
             '2026-10-13T01:00:00Z', 'cannot receive') RETURNING id",
    )
    .bind(id)
    .fetch_one(&ops[0])
    .await
    .unwrap();
    exec(
        &ops[1],
        &format!(
            "INSERT INTO manual_approvals (action_id, justification) VALUES ({action}, 'checked')"
        ),
    )
    .await;
    assert_refused(
        &pool,
        &format!(
            "INSERT INTO unsigned_attempt_cancellations (transfer_id, attempt, manual_action_id) \
             VALUES ({id}, 1, {action})"
        ),
        "INV-8",
    )
    .await;
}
