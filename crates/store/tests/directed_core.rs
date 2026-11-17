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
         VALUES ($4, 'directed:return-' || $1, 'refund', \
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

/// The block time the issuing network's dry runs below ran at, as the endpoint recorded it.
const DRY_RUN_TIME: &str = "2026-10-13T00:59:00Z";

/// Attempt 1 of deposit `id` ordered on the issuing network, and its preparation, whose dry run at
/// (`issuing`, 90, `\xcc`, [`DRY_RUN_TIME`]) the network refused with `reason` (the issuing
/// network's refusal of a destination is `Kreivo(Assets(CannotMint))`, the only one in
/// `bezent_store::DESTINATION_REFUSALS`).
async fn refused_dry_run(pool: &PgPool, id: i64, reason: &str) {
    dry_run(pool, id, Some(reason), 90, "cc").await;
}

/// Attempt 1 of deposit `id` ordered again on the issuing network, and its preparation at
/// (`issuing`, `block`, `\x<hash>`, [`DRY_RUN_TIME`]): refused with `reason`, or passed (`None`).
/// A refusal to mint records the asset `Live` at that head, as the Kreivo endpoint records it
/// ([`dry_run_with`] for another status).
async fn dry_run(pool: &PgPool, id: i64, reason: Option<&str>, block: i64, hash: &str) {
    let status = reason
        .filter(|r| bezent_store::DESTINATION_REFUSALS.contains(r))
        .map(|_| bezent_store::ASSET_LIVE);
    dry_run_with(pool, id, reason, status, block, hash, Some(DRY_RUN_TIME)).await;
}

/// [`dry_run`], the asset of `status` at the dry run's head (`"null"`: a null status, as the
/// endpoint records one it could not read; `None`: no asset recorded), the dry
/// run's block time `time` (`None`: none recorded, as on rows before it).
async fn dry_run_with(
    pool: &PgPool,
    id: i64,
    reason: Option<&str>,
    status: Option<&str>,
    block: i64,
    hash: &str,
    time: Option<&str>,
) {
    let asset = match status {
        Some("null") => {
            ", 'asset', jsonb_build_object('id', 'Here(1)', 'status', NULL, 'supply', NULL)"
                .to_owned()
        }
        Some(s) => format!(
            ", 'asset', jsonb_build_object('id', 'Here(1)', 'status', '{s}'::text, 'supply', '0')"
        ),
        None => String::new(),
    };
    let (passed, outcome) = match reason {
        Some(r) => (
            "FALSE",
            format!("jsonb_build_object('outcome', 'refused', 'reason', '{r}'::text{asset})"),
        ),
        None => (
            "TRUE",
            "jsonb_build_object('outcome', 'passed', 'reason', NULL)".to_owned(),
        ),
    };
    let time = time.map_or_else(|| "NULL".to_owned(), |t| format!("'{t}'"));
    exec(
        pool,
        &format!(
            "WITH i AS (INSERT INTO intents (transfer_id, network, purpose, attempt, payload) \
               VALUES ({id}, 'issuing', 'mint', 1, '{{\"order\": {{}}, \"amount\": \"5\"}}') \
               RETURNING id) \
             INSERT INTO prepared_transactions (order_intent_id, attempt, network, lane, key_id, \
                 nonce, valid_from_block, valid_until_block, checkpoint_hash, call, \
                 signing_payload, message, dry_run_block_number, dry_run_block_hash, \
                 dry_run_passed, dry_run, dry_run_block_time) \
             SELECT id, 1, 'issuing', '\\x01', 'mint-key', 0, 0, 0, '\\x02', '\\x03', '\\x04', \
                 '\\x05', {block}, '\\x{hash}', {passed}, {outcome}, {time} FROM i"
        ),
    )
    .await;
}

/// Deposit `id` enters review from `Source-Final` holding `CND-DestinationRefused`: at the
/// condition's deadline, on the issuing network's final head 100 (`at_head`), as the core's
/// deadline review writes it; or with no coordinate, as its review of a destination or a list it
/// cannot read writes it.
async fn into_review(pool: &PgPool, id: i64, at_head: bool) {
    let point = if at_head {
        "'issuing', 100, '\\xbb', '2026-10-13T01:00:00Z'"
    } else {
        "NULL, NULL, NULL, NULL"
    };
    exec(
        pool,
        &format!(
            "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
               VALUES ('issuing', 100, '\\xbb', '2026-10-13T01:00:00Z') ON CONFLICT DO NOTHING; \
             INSERT INTO transitions (transfer_id, from_state, to_state, condition, network, \
                 block_number, block_hash, block_time) \
             VALUES ({id}, 'Source-Final', 'Needs-Review', 'CND-DestinationRefused', {point})"
        ),
    )
    .await;
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

    // In review for its destination (the network's refusal recorded, entered at the deadline),
    // with no resolution: still refused.
    refused_dry_run(&pool, id, "Kreivo(Assets(CannotMint))").await;
    into_review(&pool, id, true).await;
    assert!(flag(&pool, &format!("SELECT directed_review_refund_open({id})")).await);
    let err = sqlx::raw_sql(&refund_sql(REFUND))
        .execute(&core)
        .await
        .expect_err("a refund with no approved resolution");
    assert_refusal(&err, "REQ-GV-5", "a refund with no approved resolution");

    // An approved resolution: to another address it is refused, whoever writes; to the refund
    // destination the deposit took from the arrival, the core may make it.
    let ops = [
        operator_login(&pool, OPERATORS[0]).await,
        operator_login(&pool, OPERATORS[1]).await,
    ];
    let action: i64 = sqlx::query_scalar(
        "INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, \
             block_hash, block_time, justification) \
         VALUES ('review-resolution', $1, 'Refunding', 'issuing', 90, '\\xcc', $2::timestamptz, \
             'cannot receive') RETURNING id",
    )
    .bind(id)
    .bind(DRY_RUN_TIME)
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

/// `REQ-RF-1`, `REQ-IN-9`, guardrail 4 (review M1 of PR #136): the directed review exit opens only
/// for a review of the issuing network's own recorded refusal of the destination, never for an
/// unknown. Four directed deposits in review for `CND-DestinationRefused`:
///
/// - entered at the deadline, but no dry run recorded a refusal: closed;
/// - a refusal recorded by a dry run that is the network's, but the review entered with no
///   coordinate (the core's review of a destination or a list it cannot read): closed;
/// - a dry run refused for another reason (a mint window bound): closed;
/// - the network's refusal recorded and the review entered at its deadline: open, until a mint
///   carrying the deposit's mint reference is observed on the issuing network (defence in depth).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_1_the_directed_review_exit_opens_only_on_the_networks_recorded_refusal(
    pool: PgPool,
) {
    declarations(&pool).await;
    for block in [7, 8, 9, 10] {
        final_arrival(&pool, block, "directed_arrival").await;
    }
    exec(
        &pool,
        "UPDATE routes SET maximum = 100000000, window_limit = 100000000",
    )
    .await;
    let core = login(&pool, "dc_core_m1_for_tests", bezent_store::CORE_ROLE).await;
    let open = |id: i64| {
        let pool = pool.clone();
        async move { flag(&pool, &format!("SELECT directed_review_refund_open({id})")).await }
    };

    let no_refusal = directed_deposit(&core, 7).await;
    into_review(&pool, no_refusal, true).await;
    assert!(!open(no_refusal).await, "no refusal recorded");

    let unknown = directed_deposit(&core, 8).await;
    refused_dry_run(&pool, unknown, "Kreivo(Assets(CannotMint))").await;
    into_review(&pool, unknown, false).await;
    assert!(!open(unknown).await, "a review of an unknown");

    let window = directed_deposit(&core, 9).await;
    refused_dry_run(&pool, window, "AboveWindow").await;
    into_review(&pool, window, true).await;
    assert!(
        !open(window).await,
        "a refusal that is not the destination's"
    );

    let refused = directed_deposit(&core, 10).await;
    refused_dry_run(&pool, refused, "Kreivo(Assets(CannotMint))").await;
    into_review(&pool, refused, true).await;
    assert!(
        open(refused).await,
        "the network's refusal, reviewed at its deadline"
    );
    // Its evidence is the refused dry run's block with the block time recorded with it (review
    // L-b), and nothing else on the issuing network.
    for (block, hash, time, want) in [
        (90, "cc", DRY_RUN_TIME, true),
        (90, "cc", "2026-10-13T01:00:00Z", false),
        (100, "bb", "2026-10-13T01:00:00Z", false),
        (90, "bb", DRY_RUN_TIME, false),
    ] {
        assert_eq!(
            flag(
                &pool,
                &format!(
                    "SELECT directed_refusal_recorded({refused}, 'issuing', {block}, '\\x{hash}', \
                       '{time}')"
                )
            )
            .await,
            want,
            "evidence at {block}, {time}"
        );
    }
    // The core may ask both questions.
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT directed_review_refund_open($1)")
            .bind(refused)
            .fetch_one(&core)
            .await
            .unwrap()
    );
    // A mint of it observed, whatever its outcome, closes the exit.
    exec(
        &pool,
        &format!(
            "INSERT INTO observations (network, block_number, block_hash, block_time, \
                 position_kind, position_index, token, amount, recipient, executed, \
                 on_chain_reference, kind, detail) \
             VALUES ('issuing', 101, '\\xbd', '{BLOCK_TIME}', 'log', 0, 'asset-a', {AMOUNT}, \
                 '{RECIPIENT}', FALSE, reference_tagged('bezent/mint-reference/v0', ARRAY[ \
                   coordinate_digest('origin-a', 10, '\\xaa', 'log', 1)]), 'mint', \
                 jsonb_build_object('event', 'Minted'))"
        ),
    )
    .await;
    assert!(!open(refused).await, "a mint of it is observed");
}

/// `REQ-RF-1`, `REQ-IN-9`, guardrail 4 (review M1 of PR #136): the endpoint names the assets API's
/// inner error, and only its refusal to mint to the account, `Kreivo(Assets(CannotMint))`, is the
/// network's refusal of the destination. A dry run refused with any other `Kreivo(…)`, or a bare
/// `Kreivo`, is an unknown, and its review never opens the exit. The store's list is the Rust one,
/// `bezent_store::DESTINATION_REFUSALS`, which the core reads.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_1_only_the_refusal_to_mint_to_the_destination_opens_the_exit(pool: PgPool) {
    let listed: Vec<String> = sqlx::query_scalar("SELECT unnest(directed_destination_refusals())")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(listed, bezent_store::DESTINATION_REFUSALS);

    declarations(&pool).await;
    exec(
        &pool,
        "UPDATE routes SET maximum = 100000000, window_limit = 100000000",
    )
    .await;
    let core = login(&pool, "dc_core_m1b_for_tests", bezent_store::CORE_ROLE).await;
    let reasons = [
        ("Kreivo(Assets(NotAuthorised))", false),
        ("Kreivo(UnknownError)", false),
        ("Kreivo(ExtQueryError)", false),
        ("Kreivo(Assets(NotALocalAsset))", false),
        ("Kreivo(Assets(NotAGroupContract))", false),
        ("Kreivo(Assets(CannotDeposit))", false),
        ("Kreivo", false),
        ("Kreivo(0x0209)", false),
        ("Kreivo(Assets(CannotMint))", true),
    ];
    for (n, (reason, open)) in reasons.into_iter().enumerate() {
        let block = 20 + i64::try_from(n).unwrap();
        final_arrival(&pool, block, "directed_arrival").await;
        let id = directed_deposit(&core, block).await;
        refused_dry_run(&pool, id, reason).await;
        into_review(&pool, id, true).await;
        assert_eq!(
            flag(&pool, &format!("SELECT directed_review_refund_open({id})")).await,
            open,
            "{reason}"
        );
        assert_eq!(
            flag(
                &pool,
                &format!(
                    "SELECT directed_refusal_recorded({id}, 'issuing', 90, '\\xcc', '{DRY_RUN_TIME}')"
                )
            )
            .await,
            open,
            "{reason}: evidence"
        );
    }
}

/// `REQ-RF-1`, `REQ-IN-9` (review L-a of PR #136): the evidence is the latest dry run of attempt
/// 1's mint orders. A refusal of the destination, then a dry run that passed, is no refusal any
/// more: the exit closes, and the old refusal's block is no evidence. A refusal after that opens it
/// again, on its own block only, until a later dry run is refused for an unknown.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_1_a_later_passing_dry_run_closes_the_exit(pool: PgPool) {
    declarations(&pool).await;
    final_arrival(&pool, 7, "directed_arrival").await;
    let core = login(&pool, "dc_core_la_for_tests", bezent_store::CORE_ROLE).await;
    let id = directed_deposit(&core, 7).await;
    refused_dry_run(&pool, id, "Kreivo(Assets(CannotMint))").await;
    into_review(&pool, id, true).await;
    let open = || {
        let pool = pool.clone();
        async move { flag(&pool, &format!("SELECT directed_review_refund_open({id})")).await }
    };
    let evidence = |block: i64, hash: &'static str| {
        let pool = pool.clone();
        async move {
            flag(
                &pool,
                &format!(
                    "SELECT directed_refusal_recorded({id}, 'issuing', {block}, '\\x{hash}', \
                       '{DRY_RUN_TIME}')"
                ),
            )
            .await
        }
    };
    assert!(open().await, "the refusal, reviewed at its deadline");
    assert!(evidence(90, "cc").await);

    dry_run(&pool, id, None, 95, "c5").await;
    assert!(!open().await, "a later dry run passed");
    assert!(
        !evidence(90, "cc").await,
        "the earlier refusal is no evidence"
    );
    assert!(
        !evidence(95, "c5").await,
        "a dry run that passed is no refusal"
    );

    // A refusal of the destination after that opens it again, on its own block; a later refusal
    // for anything else closes it again.
    dry_run(&pool, id, Some("Kreivo(Assets(CannotMint))"), 97, "c7").await;
    assert!(open().await, "refused again");
    assert!(evidence(97, "c7").await);
    assert!(!evidence(90, "cc").await);
    dry_run(&pool, id, Some("Kreivo(UnknownError)"), 99, "c9").await;
    assert!(!open().await, "the latest refusal is an unknown");
    assert!(!evidence(97, "c7").await);
}

/// `REQ-RF-1`, `REQ-IN-8`, `REQ-IN-9` (review of PR #136): Kreivo answers `CannotMint` for every
/// error of `mint_into`, the whole asset's too. A refusal to mint is the destination's only when the
/// endpoint recorded the asset `Live` at the dry run's head (`bezent_store::ASSET_LIVE`): a frozen
/// asset, one being destroyed, one gone or one whose status could not be read (no status, or no
/// asset recorded at all) never opens the exit, nor is it evidence.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_1_a_refusal_to_mint_opens_the_exit_only_while_the_asset_is_live(pool: PgPool) {
    declarations(&pool).await;
    exec(
        &pool,
        "UPDATE routes SET maximum = 100000000, window_limit = 100000000",
    )
    .await;
    let core = login(&pool, "dc_core_live_for_tests", bezent_store::CORE_ROLE).await;
    let cases = [
        (Some(bezent_store::ASSET_LIVE), true),
        (Some("Frozen"), false),
        (Some("Destroying"), false),
        (Some("live"), false),
        // A status the endpoint could not read (or an asset that does not exist) is recorded as
        // null, and no asset at all by an endpoint older than this: neither is the destination's.
        (Some("null"), false),
        (None, false),
    ];
    for (n, (status, open)) in cases.into_iter().enumerate() {
        let block = 40 + i64::try_from(n).unwrap();
        final_arrival(&pool, block, "directed_arrival").await;
        let id = directed_deposit(&core, block).await;
        dry_run_with(
            &pool,
            id,
            Some("Kreivo(Assets(CannotMint))"),
            status,
            90,
            "cc",
            Some(DRY_RUN_TIME),
        )
        .await;
        into_review(&pool, id, true).await;
        assert_eq!(
            flag(&pool, &format!("SELECT directed_review_refund_open({id})")).await,
            open,
            "{status:?}"
        );
        assert_eq!(
            flag(
                &pool,
                &format!(
                    "SELECT directed_refusal_recorded({id}, 'issuing', 90, '\\xcc', '{DRY_RUN_TIME}')"
                )
            )
            .await,
            open,
            "{status:?}: evidence"
        );
    }
}

/// `REQ-RF-1` (review L1 and L4 of PR #136): a refused dry run without its block time is no
/// evidence on any path, the "is it open" question included, so the exit never shows open when it
/// cannot be taken. The newest dry run is the newest by its block, then by row; and no dry run of
/// attempt 1 that passed at or after the refusal's block may exist.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_1_the_newest_dry_run_by_block_with_its_time_is_the_evidence(pool: PgPool) {
    declarations(&pool).await;
    exec(
        &pool,
        "UPDATE routes SET maximum = 100000000, window_limit = 100000000",
    )
    .await;
    let core = login(&pool, "dc_core_l4_for_tests", bezent_store::CORE_ROLE).await;
    let open = |id: i64| {
        let pool = pool.clone();
        async move { flag(&pool, &format!("SELECT directed_review_refund_open({id})")).await }
    };
    let refusal = Some("Kreivo(Assets(CannotMint))");
    let live = Some(bezent_store::ASSET_LIVE);

    // L1: no block time recorded with the refusal.
    final_arrival(&pool, 60, "directed_arrival").await;
    let untimed = directed_deposit(&core, 60).await;
    dry_run_with(&pool, untimed, refusal, live, 90, "cc", None).await;
    into_review(&pool, untimed, true).await;
    assert!(!open(untimed).await, "a refusal without its block time");

    // L4: a row recorded later for an earlier block is not the newest; the refusal at the later
    // block is, and opens the exit.
    final_arrival(&pool, 61, "directed_arrival").await;
    let late_row = directed_deposit(&core, 61).await;
    dry_run_with(&pool, late_row, refusal, live, 97, "c7", Some(DRY_RUN_TIME)).await;
    dry_run_with(
        &pool,
        late_row,
        Some("Kreivo(UnknownError)"),
        None,
        95,
        "c5",
        Some(DRY_RUN_TIME),
    )
    .await;
    into_review(&pool, late_row, true).await;
    assert!(open(late_row).await, "the newest by block is the refusal");
    assert!(
        flag(
            &pool,
            &format!(
                "SELECT directed_refusal_recorded({late_row}, 'issuing', 97, '\\xc7', \
                   '{DRY_RUN_TIME}')"
            )
        )
        .await
    );

    // L4: the newest by block is a refusal for an unknown, though recorded first: closed.
    final_arrival(&pool, 62, "directed_arrival").await;
    let early_row = directed_deposit(&core, 62).await;
    dry_run_with(
        &pool,
        early_row,
        Some("Kreivo(UnknownError)"),
        None,
        97,
        "c7",
        Some(DRY_RUN_TIME),
    )
    .await;
    dry_run_with(
        &pool,
        early_row,
        refusal,
        live,
        95,
        "c5",
        Some(DRY_RUN_TIME),
    )
    .await;
    into_review(&pool, early_row, true).await;
    assert!(!open(early_row).await, "the newest by block is an unknown");

    // L4: a dry run that passed at the refusal's own block, recorded before it.
    final_arrival(&pool, 63, "directed_arrival").await;
    let same_block = directed_deposit(&core, 63).await;
    dry_run_with(&pool, same_block, None, None, 97, "c7", Some(DRY_RUN_TIME)).await;
    dry_run_with(
        &pool,
        same_block,
        refusal,
        live,
        97,
        "c7",
        Some(DRY_RUN_TIME),
    )
    .await;
    into_review(&pool, same_block, true).await;
    assert!(
        !open(same_block).await,
        "a dry run passed at the refusal's block"
    );
}

/// `REQ-RF-1`, `INV-8` (review, info, of PR #136): a delivery anomaly recorded for the deposit, one
/// its destination network's endpoint wrote naming an intent of it, or one the core surfaced for
/// it, makes its leg unknown, and the exit closes.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_1_a_delivery_anomaly_closes_the_exit(pool: PgPool) {
    declarations(&pool).await;
    exec(
        &pool,
        "UPDATE routes SET maximum = 100000000, window_limit = 100000000",
    )
    .await;
    final_arrival(&pool, 7, "directed_arrival").await;
    final_arrival(&pool, 8, "directed_arrival").await;
    let core = login(&pool, "dc_core_an_for_tests", bezent_store::CORE_ROLE).await;
    let named = directed_deposit(&core, 7).await;
    let surfaced = directed_deposit(&core, 8).await;
    for id in [named, surfaced] {
        refused_dry_run(&pool, id, "Kreivo(Assets(CannotMint))").await;
        into_review(&pool, id, true).await;
        assert!(flag(&pool, &format!("SELECT directed_review_refund_open({id})")).await);
    }
    // Another network's anomaly naming the intent is not this deposit's.
    exec(
        &pool,
        &format!(
            "INSERT INTO event_log (kind, payload) SELECT 'delivery-anomaly', \
               jsonb_build_object('network', 'origin-a', 'intent', max(id)::text) \
             FROM intents WHERE transfer_id = {named}"
        ),
    )
    .await;
    assert!(
        flag(
            &pool,
            &format!("SELECT directed_review_refund_open({named})")
        )
        .await
    );
    exec(
        &pool,
        &format!(
            "INSERT INTO event_log (kind, payload) SELECT 'delivery-anomaly', \
               jsonb_build_object('network', 'issuing', 'intent', max(id)::text) \
             FROM intents WHERE transfer_id = {named}"
        ),
    )
    .await;
    assert!(
        !flag(
            &pool,
            &format!("SELECT directed_review_refund_open({named})")
        )
        .await
    );
    exec(
        &pool,
        &format!(
            "INSERT INTO event_log (kind, transfer_id, payload) \
             VALUES ('delivery-anomaly-surfaced', {surfaced}, '{{\"anomaly\": \"1\"}}')"
        ),
    )
    .await;
    assert!(
        !flag(
            &pool,
            &format!("SELECT directed_review_refund_open({surfaced})")
        )
        .await
    );
}

/// `REQ-LM-1` (review L1 of PR #136): what the window has used is read through the route's
/// `created_at` index, not by a scan of every transfer of the route. The planner is asked about the
/// sum `route_usage_locked()` runs, and the one `directed_window_fits()` runs for what the other
/// deposits took (review L-c), as the functions write them (the test reads their source to be sure
/// they still do), over a route with a long history and few transfers in the window.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_lm_1_the_window_is_read_by_its_index(pool: PgPool) {
    declarations(&pool).await;
    // Twenty thousand old deposits of the route and twenty in the window, written past the
    // lifecycle's checks (only the planner's statistics matter here).
    exec(
        &pool,
        "ALTER TABLE transfers DISABLE TRIGGER USER; \
         INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index, created_at) \
         SELECT 'bz-d-old-' || g, 'old-' || g, 'deposit', 'asset-a', 1, 'issuing', 'x', \
             'Completed', 'origin-a', g, '\\xaa', 'log', 1, now() - interval '30 days' \
         FROM generate_series(1, 20000) g; \
         INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index) \
         SELECT 'bz-d-new-' || g, 'new-' || g, 'deposit', 'asset-a', 1, 'issuing', 'x', \
             'Source-Final', 'origin-a', 100000 + g, '\\xaa', 'log', 1 \
         FROM generate_series(1, 20) g; \
         ALTER TABLE transfers ENABLE TRIGGER USER; \
         ANALYZE transfers; ANALYZE transitions",
    )
    .await;
    for (function, prefilter) in [
        (
            "route_usage_locked",
            "AND (t.created_at > now() - make_interval(secs => p_window_seconds)\n           \
             OR t.id = ANY (ARRAY(SELECT w.transfer_id FROM transitions w",
        ),
        (
            "directed_window_fits",
            "AND (d.created_at > now() - make_interval(secs => r.window_seconds)\n           \
             OR d.id = ANY (ARRAY(SELECT w.transfer_id FROM transitions w",
        ),
        // Review L-c: the route's network by the index's own expression.
        (
            "directed_window_fits",
            "WHERE d.asset = t.asset AND d.kind = 'deposit'\n      \
             AND (CASE d.kind WHEN 'deposit' THEN d.source_network ELSE d.destination_network END)\n          \
             = t.source_network\n      AND d.id <> t.id\n      \
             AND d.state NOT IN",
        ),
    ] {
        let source = text(
            &pool,
            &format!("SELECT prosrc FROM pg_proc WHERE proname = '{function}'"),
        )
        .await;
        assert!(source.contains(prefilter), "{function}: {source}");
    }
    let rows: Vec<String> = sqlx::query_scalar(
        "EXPLAIN SELECT COALESCE(sum(t.amount), 0)::text FROM transfers t \
         WHERE t.asset = 'asset-a' AND t.kind = 'deposit' \
           AND (CASE t.kind WHEN 'deposit' THEN t.source_network ELSE t.destination_network END) \
               = 'origin-a' \
           AND t.state NOT IN ('Refused', 'Expired', 'Withdrawn') \
           AND (t.created_at > now() - make_interval(secs => 86400) \
                OR t.id = ANY (ARRAY(SELECT w.transfer_id FROM transitions w \
                                     WHERE w.condition = 'CND-DepositWindow')))",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let plan = rows.join("\n");
    assert!(plan.contains("transfers_route_window"), "{plan}");
    assert!(!plan.contains("Seq Scan on transfers"), "{plan}");
    // Review L-c: and what directed_window_fits() has the other deposits take, as it writes it.
    let rows: Vec<String> = sqlx::query_scalar(
        "EXPLAIN SELECT COALESCE(sum(d.amount), 0) FROM transfers d \
         WHERE d.asset = 'asset-a' AND d.kind = 'deposit' \
           AND (CASE d.kind WHEN 'deposit' THEN d.source_network ELSE d.destination_network END) \
               = 'origin-a' \
           AND d.id <> 0 \
           AND d.state NOT IN ('Refused', 'Expired', 'Withdrawn') \
           AND d.condition IS DISTINCT FROM 'CND-DepositWindow' \
           AND (d.created_at > now() - make_interval(secs => 86400) \
                OR d.id = ANY (ARRAY(SELECT w.transfer_id FROM transitions w \
                                     WHERE w.condition = 'CND-DepositWindow'))) \
           AND CASE \
               WHEN d.id IN (SELECT w.transfer_id FROM transitions w \
                             WHERE w.condition = 'CND-DepositWindow') THEN \
                   (SELECT min(x.seen_at) FROM transitions x \
                    WHERE x.transfer_id = d.id \
                      AND x.condition IS DISTINCT FROM 'CND-DepositWindow' \
                      AND x.id > (SELECT min(w.id) FROM transitions w \
                                  WHERE w.transfer_id = d.id \
                                    AND w.condition = 'CND-DepositWindow')) \
                   > now() - make_interval(secs => 86400) \
               ELSE d.created_at > now() - make_interval(secs => 86400) \
           END",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let plan = rows.join("\n");
    assert!(
        plan.contains("transfers_route_window"),
        "directed_window_fits: {plan}"
    );
    assert!(
        !plan.contains("Seq Scan on transfers"),
        "directed_window_fits: {plan}"
    );
    // And it counts what it did: the twenty in the window.
    assert_eq!(
        text(
            &pool,
            "SELECT used FROM route_usage_locked('asset-a', 'origin-a', 'deposit', 86400)"
        )
        .await,
        "20"
    );
}
