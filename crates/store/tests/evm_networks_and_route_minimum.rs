// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Migrations 0056 and 0057 over data that already exists (T-001-03; `DEC-8`, `REQ-LM-1`,
//! `REQ-AS-9`): the backfill of `evm_endpoint_networks` from the role's current members, and the
//! one-unit rule for a route minimum, against a route already declared and against an asset with
//! several tokens on one network.

mod common;

use common::{REVERSIBLE_MIGRATOR, exec};
use sqlx::PgPool;

const ROUTE: &str = "INSERT INTO routes (asset, network, direction, minimum, maximum, window_limit, \
     window_seconds, capacity, completion_budget_seconds) \
     VALUES ('bBTC', 'bsc', 'deposit', {min}, 100000000000000000000, 100000000000000000000, 86400, 10, 3600)";

fn route(minimum: &str) -> String {
    ROUTE.replace("{min}", minimum)
}

/// Roles are server-wide: the catalogue-wide grants checks of other tests list every role that
/// reaches a store role, so this test leaves none behind.
async fn drop_roles(pool: &PgPool) {
    exec(
        pool,
        "DO $$ DECLARE r TEXT; BEGIN \
           FOREACH r IN ARRAY ARRAY['bf_direct', 'bf_two_grantors', 'bf_not_a_member', \
                                    'bf_no_login', 'bf_grantor_a', 'bf_grantor_b'] LOOP \
             IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = r) THEN \
               EXECUTE format('DROP OWNED BY %I', r); EXECUTE format('DROP ROLE %I', r); \
             END IF; \
           END LOOP; END $$",
    )
    .await;
}

async fn mapping(pool: &PgPool) -> Vec<(String, String)> {
    sqlx::query_as("SELECT login::text, network FROM evm_endpoint_networks ORDER BY 1, 2")
        .fetch_all(pool)
        .await
        .unwrap()
}

/// 0056's backfill: a login that holds the role is mapped to every EVM origin network declared
/// when the migration runs, and to no other. From PostgreSQL 16 `pg_auth_members` holds one row
/// per grantor, so a login granted by two grantors must still get one row per network, not abort
/// the migration on the primary key.
#[sqlx::test(migrations = false)]
async fn s7_the_backfill_maps_each_member_login_once_per_evm_network(pool: PgPool) {
    REVERSIBLE_MIGRATOR.run(&pool).await.expect("up");
    REVERSIBLE_MIGRATOR
        .undo(&pool, 55)
        .await
        .expect("down to 0055");
    drop_roles(&pool).await;
    exec(
        &pool,
        "INSERT INTO networks (network, role, address_format) VALUES \
           ('kreivo', 'issuing', 'ss58'), ('polygon', 'origin', 'evm'), \
           ('base', 'origin', 'evm'), ('elsewhere', 'origin', 'opaque'); \
         DO $$ BEGIN \
           CREATE ROLE bf_direct LOGIN; \
           CREATE ROLE bf_two_grantors LOGIN; \
           CREATE ROLE bf_not_a_member LOGIN; \
           CREATE ROLE bf_no_login NOLOGIN; \
           CREATE ROLE bf_grantor_a NOLOGIN; \
           CREATE ROLE bf_grantor_b NOLOGIN; \
         EXCEPTION WHEN duplicate_object THEN NULL; END $$; \
         GRANT bezent_evm_endpoint TO bf_grantor_a, bf_grantor_b WITH ADMIN OPTION; \
         GRANT bezent_evm_endpoint TO bf_direct; \
         GRANT bezent_evm_endpoint TO bf_no_login; \
         GRANT bezent_evm_endpoint TO bf_two_grantors GRANTED BY bf_grantor_a; \
         GRANT bezent_evm_endpoint TO bf_two_grantors GRANTED BY bf_grantor_b",
    )
    .await;
    let grants: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_auth_members m JOIN pg_roles u ON u.oid = m.member \
         WHERE u.rolname = 'bf_two_grantors'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(grants, 2, "the login holds the role from two grantors");

    let applied = REVERSIBLE_MIGRATOR.run(&pool).await;
    let rows = mapping(&pool).await;
    drop_roles(&pool).await;
    applied.expect("0056 applies");
    let of = |login: &str| -> Vec<&str> {
        rows.iter()
            .filter(|(l, _)| l == login)
            .map(|(_, n)| n.as_str())
            .collect()
    };
    assert_eq!(of("bf_direct"), ["base", "polygon"]);
    assert_eq!(of("bf_two_grantors"), ["base", "polygon"]);
    for login in ["bf_not_a_member", "bf_no_login", "bf_grantor_a"] {
        assert!(of(login).is_empty(), "{login} is not an instance: {rows:?}");
    }
    assert!(
        rows.iter().all(|(_, n)| n != "kreivo" && n != "elsewhere"),
        "only EVM origin networks: {rows:?}"
    );
}

/// 0057 over a deployment that already holds a route under its ratio: the migration stops, naming
/// the route; once the route is corrected it applies.
#[sqlx::test(migrations = false)]
async fn req_lm_1_a_route_already_under_its_ratio_stops_0057_naming_it(pool: PgPool) {
    REVERSIBLE_MIGRATOR.run(&pool).await.expect("up");
    REVERSIBLE_MIGRATOR
        .undo(&pool, 56)
        .await
        .expect("down to 0056");
    exec(
        &pool,
        "INSERT INTO networks (network, role, address_format) VALUES \
           ('kreivo', 'issuing', 'ss58'), ('bsc', 'origin', 'evm'); \
         INSERT INTO assets (asset, precision, assurance_class) VALUES ('bBTC', 8, 'public'); \
         INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
           ('bsc', '0xbtcb', 'bBTC', 18)",
    )
    .await;
    exec(&pool, &route("1")).await;

    let e = REVERSIBLE_MIGRATOR
        .run(&pool)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("bBTC bsc deposit") && e.contains("REQ-LM-1"),
        "{e}"
    );
    let applied: i64 =
        sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE version = 57")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(applied, 0, "0057 did not apply");

    exec(&pool, "UPDATE routes SET minimum = 10000000000").await;
    REVERSIBLE_MIGRATOR.run(&pool).await.expect("0057 applies");
}

async fn bz001(pool: &PgPool, sql: &str) -> Result<(), String> {
    match sqlx::raw_sql(sql).execute(pool).await {
        Ok(_) => Ok(()),
        Err(sqlx::Error::Database(d)) => {
            assert_eq!(d.code().as_deref(), Some("BZ001"), "{}", d.message());
            Err(d.message().to_string())
        }
        Err(e) => panic!("{e}"),
    }
}

/// 0057 reads the one token an asset has on a network (`route_minimum_on_route` selects it into a
/// single ratio). That is sound only because 0050's unique index admits one token per (network,
/// asset): a second token of another ratio cannot be declared, so no route is ever held to an
/// arbitrary one of several. If the index is ever dropped, this fails first, and the route-side
/// trigger must then check every token, as the token-side one does.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_lm_1_an_asset_has_one_token_per_network_so_one_ratio_per_route(pool: PgPool) {
    exec(
        &pool,
        "INSERT INTO networks (network, role, address_format) VALUES \
           ('kreivo', 'issuing', 'ss58'), ('bsc', 'origin', 'evm'); \
         INSERT INTO assets (asset, precision, assurance_class) VALUES ('bBTC', 8, 'public'); \
         INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
           ('bsc', '0xa-lenient', 'bBTC', 10)",
    )
    .await;
    let e = sqlx::raw_sql(
        "INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
           ('bsc', '0xb-strict', 'bBTC', 18)",
    )
    .execute(&pool)
    .await
    .unwrap_err()
    .to_string();
    assert!(e.contains("admitted_tokens_one_per_asset"), "{e}");
    // The route is held to that one token's ratio (100): 50 refused, 100 accepted.
    bz001(&pool, &route("50")).await.unwrap_err();
    bz001(&pool, &route("100")).await.unwrap();
}
