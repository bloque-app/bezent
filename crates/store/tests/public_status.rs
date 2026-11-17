// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-018, T-018-10 and T-018-11 (migrations 0146, 0147): the store's half of the public status route
//! and of the declaration of directing code.
//!
//! - `public_status` (`REQ-ST-8`, `REQ-ID-4`): its columns are only what the route may say, none a
//!   requester supplied; a directed deposit, the core's return of a directed arrival and a withdrawal
//!   are rows, each under the bridge reference its coordinate or redemption derives; an ordinary
//!   arrival's deposit (a request's) is none. The route's own behaviour, the requester's deposit and
//!   its "not found" included, is `services/ingress/tests/public_status.rs`.
//! - `directing_code` (`REQ-AS-11`, `REQ-BK-5`): a declaration names an origin token admitted for
//!   the reserve's asset, is never the reserve address, and never changes.

mod common;

use bezent_store::login::{as_login, ensure_login, pool_options};
use common::*;
use sqlx::PgPool;

const PASSWORD: &str = "test-only-public-status-password";
const RESERVE: &str = "0xA0b86991c6218b36C1d19D4a2e9Eb0cE3606eB48";
const OTHER_RESERVE: &str = "0x00000000000000000000000000000000000000b2";
const DIRECTING: &str = "0xD1Ec7ed00000000000000000000000000000d1ec";
const REFUND: &str = "0x52908400098527886E0F7030069857D2E4169EE7";
const RECIPIENT: &str = "0x8eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a48";

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
             INSERT INTO assets (asset, precision, assurance_class) VALUES \
               ('asset-a', 6, 'public'), ('asset-b', 6, 'public'); \
             INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
               ('origin-a', 'token-a', 'asset-a', 6), ('origin-a', 'token-b', 'asset-b', 6); \
             INSERT INTO reserves (asset, network, address) VALUES \
               ('asset-a', 'origin-a', '{RESERVE}')"
        ),
    )
    .await;
}

/// The directing code the directed arrivals name, declared for asset-a's reserve (REQ-AS-11).
async fn declare_directing(pool: &PgPool) {
    exec(
        pool,
        &format!(
            "INSERT INTO directing_code (asset, network, address, token, minimum, maximum, \
                 from_block, reference) \
             VALUES ('asset-a', 'origin-a', '{DIRECTING}', 'token-a', 1, 50000000000, 0, \
                 'test declaration')"
        ),
    )
    .await;
}

/// An executed, final observation at (`origin-a`, `block`, `\xaa`, log 1) of `kind`.
async fn observed(pool: &PgPool, block: i64, kind: &str) {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, token, amount, sender, recipient, executed, kind, detail) \
         VALUES ('origin-a', $1, '\\xaa', $2::timestamptz, 'log', 1, 'token-a', 5000000, \
             '0xfunder', $3, TRUE, $4, jsonb_build_object('event', \
               CASE $4 WHEN 'directed_arrival' THEN 'DirectedArrival' ELSE 'Arrival' END, \
               'directing', $5::text, 'direction_log', 2, 'recipient', $6::text, \
               'refund', $7::text)) RETURNING id",
    )
    .bind(block)
    .bind(BLOCK_TIME)
    .bind(RESERVE)
    .bind(kind)
    .bind(DIRECTING)
    .bind(RECIPIENT)
    .bind(REFUND)
    .fetch_one(pool)
    .await
    .unwrap();
    exec(
        pool,
        &format!(
            "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
               VALUES ('origin-a', {block} + 20, '\\xff', '{BLOCK_TIME}') ON CONFLICT DO NOTHING; \
             INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
               final_block_hash) VALUES ({id}, 'final', {block} + 20, '\\xff')"
        ),
    )
    .await;
}

async fn rows(pool: &PgPool, reference: &str) -> Vec<(String, String, String, String)> {
    sqlx::query_as(
        "SELECT subject, role, bridge_reference, state FROM public_status \
         WHERE lookup_reference = $1 ORDER BY role",
    )
    .bind(reference)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// `REQ-ST-8`: the view has no column a requester supplied (no request reference, requester, intake
/// body, callback, expiry, extra), and no destination, refund destination or amount: only the
/// lookup key, what the row is, its state and condition, its coordinates and their finality, and the
/// time of its last change.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_8_public_status_has_only_the_columns_the_route_may_say(pool: PgPool) {
    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name::text FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name = 'public_status' ORDER BY ordinal_position",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        columns,
        [
            "lookup_reference",
            "subject",
            "role",
            "bridge_reference",
            "state",
            "condition",
            "source_network",
            "source_block_number",
            "source_block_hash",
            "source_position_kind",
            "source_position_index",
            "source_final",
            "included_network",
            "included_block_number",
            "included_block_hash",
            "final_network",
            "final_block_number",
            "final_block_hash",
            "updated_at",
        ]
    );
}

/// `REQ-ST-8`, `REQ-ID-4`, `REQ-IN-10`, `REQ-IN-11`: the core's deposit of a directed arrival and the
/// core's return of another are rows under their own derived bridge references (`bz-d-`, `bz-u-`); a
/// deposit of an ordinary arrival, which only a request makes, is none, though its reference is
/// derived the same way. The route's login reads the rows, and nothing else.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_8_req_id_4_directed_deposits_and_returns_are_rows_and_a_requests_deposit_is_not(
    pool: PgPool,
) {
    declarations(&pool).await;
    declare_directing(&pool).await;
    let core = login(
        &pool,
        "public_status_core_for_tests",
        bezent_store::CORE_ROLE,
    )
    .await;
    let public = login(
        &pool,
        "public_status_public_for_tests",
        bezent_store::PUBLIC_ROLE,
    )
    .await;
    observed(&pool, 7, "directed_arrival").await;
    observed(&pool, 8, "directed_arrival").await;
    observed(&pool, 9, "arrival").await;

    let deposit: Option<i64> =
        sqlx::query_scalar("SELECT directed_deposit('origin-a', 7, '\\xaa', 'log', 1)")
            .fetch_one(&core)
            .await
            .unwrap();
    assert!(deposit.is_some());
    let reference = |block: i64, f: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, String>(&format!(
                "SELECT {f}('origin-a', $1, '\\xaa', 'log', 1)"
            ))
            .bind(block)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let deposit_7 = reference(7, "deposit_reference").await;
    let return_8 = reference(8, "refund_reference").await;
    let deposit_9 = reference(9, "deposit_reference").await;
    // The core's return of the arrival at block 8, to its observed refund address.
    exec(
        &core,
        &format!(
            "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                 destination_network, destination, state, source_network, source_block_number, \
                 source_block_hash, source_position_kind, source_position_index) \
             VALUES ('{return_8}', 'directed:{return_8}', 'refund', 'asset-a', 5000000, \
                 'origin-a', '{REFUND}', 'Received', 'origin-a', 8, '\\xaa', 'log', 1)"
        ),
    )
    .await;
    // A deposit of the ordinary arrival at block 9, as the owner writes one (a request's, restored).
    exec(
        &pool,
        &format!(
            "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                 destination_network, destination, state, source_network, source_block_number, \
                 source_block_hash, source_position_kind, source_position_index) \
             VALUES ('{deposit_9}', 'a-request', 'deposit', 'asset-a', 5000000, 'issuing', \
                 '{RECIPIENT}', 'Received', 'origin-a', 9, '\\xaa', 'log', 1)"
        ),
    )
    .await;

    assert_eq!(
        rows(&public, &deposit_7).await,
        [(
            "directed_deposit".to_owned(),
            "deposit".to_owned(),
            deposit_7.clone(),
            "Source-Final".to_owned()
        )]
    );
    assert_eq!(
        rows(&public, &return_8).await,
        [(
            "directed_deposit".to_owned(),
            "return".to_owned(),
            return_8.clone(),
            "Received".to_owned()
        )]
    );
    assert!(
        rows(&public, &deposit_9).await.is_empty(),
        "REQ-ID-4: a request's deposit is no row"
    );
    let all: i64 = sqlx::query_scalar("SELECT count(*) FROM public_status")
        .fetch_one(&public)
        .await
        .unwrap();
    assert_eq!(all, 2);
    let err = sqlx::query("SELECT 1 FROM transfers LIMIT 1")
        .execute(&public)
        .await
        .expect_err("the route's login reads no table");
    assert!(err.to_string().contains("permission denied"), "{err}");
}

/// `REQ-AS-11`, `REQ-BK-5`: directing code is declared for a reserve with the token it moves, which
/// must be admitted for that reserve's asset there, and it is never a reserve address on the network
/// (any asset's) nor an admitted token there (review L5 of PR #139). On an `evm` network its address
/// is kept in lower case, so a second declaration in another case is the same key. Its time is the
/// clock's at the write, not the transaction's start (review L4). The declaration never changes or
/// goes, and no service writes one; the core reads it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_as_11_directing_code_is_declared_for_a_reserve_once_and_never_changes(pool: PgPool) {
    declarations(&pool).await;
    let insert = |address: &str, token: &str, asset: &str| {
        format!(
            "INSERT INTO directing_code (asset, network, address, token, minimum, maximum, \
                 from_block, reference) \
             VALUES ('{asset}', 'origin-a', '{address}', '{token}', 1000000, 50000000000, 100, \
                 'test declaration')"
        )
    };
    // Another asset's token, and the reserve itself (in another case), are refused.
    assert_refused(&pool, &insert(DIRECTING, "token-b", "asset-a"), "REQ-AS-11").await;
    assert_refused(
        &pool,
        &insert(&RESERVE.to_lowercase(), "token-a", "asset-a"),
        "REQ-AS-11",
    )
    .await;
    // An asset with no reserve there is no declaration at all.
    assert!(
        sqlx::raw_sql(&insert(DIRECTING, "token-b", "asset-b"))
            .execute(&pool)
            .await
            .is_err()
    );
    // Review L5: another asset's reserve on the network, and an admitted token, are refused too.
    exec(
        &pool,
        &format!(
            "INSERT INTO reserves (asset, network, address) VALUES \
               ('asset-b', 'origin-a', '{OTHER_RESERVE}')"
        ),
    )
    .await;
    assert_refused(
        &pool,
        &insert(OTHER_RESERVE, "token-a", "asset-a"),
        "REQ-AS-11",
    )
    .await;
    assert_refused(&pool, &insert("TOKEN-B", "token-a", "asset-a"), "REQ-AS-11").await;
    // Review L4: the time is the clock's, after the transaction began, not its start.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_sleep(0.05)")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::raw_sql(&insert(DIRECTING, "token-a", "asset-a"))
        .execute(&mut *tx)
        .await
        .unwrap();
    let (late, began, declared): (bool, String, String) = sqlx::query_as(
        "SELECT declared_at >= now() + interval '50 milliseconds', now()::text, \
                declared_at::text FROM directing_code",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(
        late,
        "declared at {declared}, the transaction began at {began}"
    );
    // Review L5: kept in lower case, so the same contract in another case is the same key.
    assert!(
        sqlx::raw_sql(&insert(
            &DIRECTING.to_uppercase().replace("0X", "0x"),
            "token-a",
            "asset-a"
        ))
        .execute(&pool)
        .await
        .is_err(),
        "a second declaration of the same contract in another case"
    );
    assert_refused(&pool, "UPDATE directing_code SET maximum = 1", "REQ-AS-11").await;
    assert_refused(&pool, "DELETE FROM directing_code", "REQ-AS-11").await;
    let core = login(
        &pool,
        "directing_code_core_for_tests",
        bezent_store::CORE_ROLE,
    )
    .await;
    let read: (String, String, String) =
        sqlx::query_as("SELECT network, address, token FROM directing_code")
            .fetch_one(&core)
            .await
            .unwrap();
    assert_eq!(
        read,
        (
            "origin-a".into(),
            DIRECTING.to_lowercase(),
            "token-a".into()
        )
    );
    let err = sqlx::raw_sql(&insert(
        "0x00000000000000000000000000000000000000d2",
        "token-a",
        "asset-a",
    ))
    .execute(&core)
    .await
    .expect_err("the core writes no declaration");
    assert!(err.to_string().contains("permission denied"), "{err}");
}
