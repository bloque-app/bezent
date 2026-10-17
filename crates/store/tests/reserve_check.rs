// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! F-004 (migration 0030): what the store itself holds of the reserve book, the reserve check and
//! the statements (`INV-17`, `REQ-BK-1`, `REQ-IC-2`, `NFR-5`, `REQ-PR-2`). Role checks run under
//! `SET LOCAL ROLE` inside a transaction that is rolled back, so no pooled connection keeps a role
//! (#27).

mod common;

use common::*;
use sqlx::PgPool;

const UNIQUE: &str = "23505";

fn hx(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
const CHECK: &str = "23514";
const PRIVILEGE: &str = "42501";

/// Runs `sql` as `role` in a transaction that is rolled back; returns the SQLSTATE of its error.
async fn as_role(pool: &PgPool, role: &str, sql: &str) -> Option<String> {
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(&format!("SET LOCAL ROLE {role}"))
        .execute(&mut *tx)
        .await
        .unwrap();
    let out = sqlx::raw_sql(sql).execute(&mut *tx).await;
    tx.rollback().await.unwrap();
    out.err().and_then(|e| {
        e.as_database_error()
            .and_then(|d| d.code().map(|c| c.into_owned()))
    })
}

async fn assets(pool: &PgPool) {
    exec(
        pool,
        "INSERT INTO assets (asset, precision, assurance_class) VALUES \
           ('asset-a', 6, 'public'), ('asset-b', 6, 'public')",
    )
    .await;
}

async fn evaluation(pool: &PgPool, asset: &str, outcome: &str) -> i64 {
    let differences = if outcome == "diverges" {
        "[{\"term\": \"reserve\"}]"
    } else {
        "[]"
    };
    sqlx::query_scalar(
        "INSERT INTO reserve_evaluations (asset, evaluated_at, outcome, differences) \
         VALUES ($1, now(), $2, $3::jsonb) RETURNING id",
    )
    .bind(asset)
    .bind(outcome)
    .bind(differences)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn entry(kind: &str, reference: &str, corrects: &str) -> String {
    format!(
        "WITH e AS (INSERT INTO book_entries (reference, kind, corrects) \
                    VALUES ('{reference}', {kind}, {corrects}) RETURNING id) \
         INSERT INTO book_postings (entry_id, asset, account, network, side, amount, item) \
         SELECT id, 'asset-a', 'reserve', 'origin-a', 'debit', 10, NULL FROM e \
         UNION ALL SELECT id, 'asset-a', 'owed-mint', NULL, 'credit', 10, 'arrival-1' FROM e"
    )
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_17_a_movement_is_booked_once_corrections_aside(pool: PgPool) {
    exec(&pool, &entry("'arrival'", "origin-a:1:0xaa:log:0", "NULL")).await;
    assert_sqlstate(
        &pool,
        &entry("'arrival'", "origin-a:1:0xaa:log:0", "NULL"),
        UNIQUE,
    )
    .await;
    // A correction is a new entry naming the one it corrects; it may share the reference.
    let first: i64 = sqlx::query_scalar("SELECT min(id) FROM book_entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    exec(
        &pool,
        &entry("'correction'", "origin-a:1:0xaa:log:0", &first.to_string()),
    )
    .await;
    assert_sqlstate(&pool, &entry("'correction'", "c-2", "NULL"), CHECK).await;
    // Entries written before F-004 carry no kind and are not constrained by it.
    exec(&pool, &entry("NULL", "legacy", "NULL")).await;
    exec(&pool, &entry("NULL", "legacy", "NULL")).await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_17_req_ar_5_only_the_core_posts_to_the_book(pool: PgPool) {
    let sql = entry("'arrival'", "origin-a:2:0xaa:log:0", "NULL");
    assert_eq!(
        as_role(&pool, bezent_store::SERVICE_ROLE, &sql)
            .await
            .as_deref(),
        Some(PRIVILEGE)
    );
    assert_eq!(as_role(&pool, bezent_store::CORE_ROLE, &sql).await, None);
    assert_eq!(
        as_role(
            &pool,
            bezent_store::CORE_ROLE,
            "UPDATE book_entries SET memo = 'x'"
        )
        .await
        .as_deref(),
        Some(PRIVILEGE)
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_1_req_ic_2_the_core_pauses_only_an_assets_routes_on_that_assets_divergence(
    pool: PgPool,
) {
    assets(&pool).await;
    let held = evaluation(&pool, "asset-a", "holds").await;
    let other = evaluation(&pool, "asset-b", "diverges").await;
    let diverged = evaluation(&pool, "asset-a", "diverges").await;
    let core = bezent_store::CORE_ROLE;
    let pause = |asset: &str, cause: &str, evaluation: String| {
        format!(
            "INSERT INTO pauses (asset, cause, evaluation_id) VALUES ('{asset}', {cause}, {evaluation})"
        )
    };
    let refused = Some(bezent_store::REFUSAL_SQLSTATE);
    for sql in [
        pause("asset-a", "'REQ-BK-1'", held.to_string()),
        pause("asset-a", "'REQ-BK-1'", other.to_string()),
        pause("asset-a", "NULL", "NULL".into()),
    ] {
        assert_eq!(
            as_role(&pool, core, &sql).await.as_deref(),
            refused,
            "{sql}"
        );
    }
    // Only the asset, never a network, a direction or the whole bridge.
    assert_eq!(
        as_role(
            &pool,
            core,
            &format!(
                "INSERT INTO pauses (asset, network, cause, evaluation_id) \
                 VALUES ('asset-a', NULL, 'REQ-BK-1', {diverged})"
            )
        )
        .await
        .as_deref(),
        Some(PRIVILEGE)
    );
    assert_eq!(
        as_role(
            &pool,
            core,
            &pause("asset-a", "'REQ-BK-1'", diverged.to_string())
        )
        .await,
        None
    );
    // No other service pauses, and lifting a pause is the operator's.
    assert_eq!(
        as_role(
            &pool,
            bezent_store::SERVICE_ROLE,
            &pause("asset-a", "'REQ-BK-1'", diverged.to_string())
        )
        .await
        .as_deref(),
        Some(PRIVILEGE)
    );
    exec(&pool, &pause("asset-a", "'REQ-BK-1'", diverged.to_string())).await;
    assert_eq!(
        as_role(&pool, core, "DELETE FROM pauses").await.as_deref(),
        Some(PRIVILEGE)
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn nfr_5_evaluations_are_append_only_and_a_divergence_names_its_differences(pool: PgPool) {
    assets(&pool).await;
    let id = evaluation(&pool, "asset-a", "holds").await;
    assert_refused(
        &pool,
        &format!("UPDATE reserve_evaluations SET outcome = 'diverges' WHERE id = {id}"),
        "REQ-BK-1",
    )
    .await;
    assert_refused(
        &pool,
        &format!("DELETE FROM reserve_evaluations WHERE id = {id}"),
        "NFR-5",
    )
    .await;
    assert_refused(&pool, "TRUNCATE reserve_evaluations CASCADE", "NFR-5").await;
    assert_sqlstate(
        &pool,
        "INSERT INTO reserve_evaluations (asset, evaluated_at, outcome) \
         VALUES ('asset-a', now(), 'diverges')",
        CHECK,
    )
    .await;
    assert_sqlstate(
        &pool,
        "INSERT INTO reserve_evaluations (asset, evaluated_at, outcome) \
         VALUES ('asset-a', now(), 'incomplete')",
        CHECK,
    )
    .await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pr_2_statements_are_append_only_and_each_names_its_predecessor(pool: PgPool) {
    assets(&pool).await;
    let e = evaluation(&pool, "asset-a", "holds").await;
    let statement = |sequence: i64, name: &str, sha: u8, previous: Option<u8>| {
        format!(
            "INSERT INTO reserve_statements (asset, sequence, evaluation_id, issued_at, name, \
                 sha256, previous_sha256, public_key, signature) \
             VALUES ('asset-a', {sequence}, {e}, now(), '{name}', '\\x{}', {}, '\\x{}', '\\x{}')",
            hx(&[sha; 32]),
            previous.map_or("NULL".into(), |p| format!("'\\x{}'", hx(&[p; 32]))),
            hx(&[7u8; 32]),
            hx(&[8u8; 64]),
        )
    };
    exec(&pool, &statement(1, "asset-a/1.json", 1, None)).await;
    // The first has no predecessor; every later one names the one before, exactly.
    assert_sqlstate(&pool, &statement(1, "asset-a/1b.json", 9, Some(1)), CHECK).await;
    assert_refused(
        &pool,
        &statement(2, "asset-a/2.json", 2, Some(9)),
        "REQ-PR-2",
    )
    .await;
    assert_refused(
        &pool,
        &statement(3, "asset-a/3.json", 3, Some(1)),
        "REQ-PR-2",
    )
    .await;
    exec(&pool, &statement(2, "asset-a/2.json", 2, Some(1))).await;
    // No fork: one statement per place in the chain.
    assert_sqlstate(&pool, &statement(2, "asset-a/2b.json", 4, Some(1)), UNIQUE).await;
    assert_refused(
        &pool,
        "UPDATE reserve_statements SET sha256 = sha256 WHERE sequence = 1",
        "REQ-PR-2",
    )
    .await;
    assert_refused(&pool, "DELETE FROM reserve_statements", "REQ-PR-2").await;
    assert_refused(&pool, "TRUNCATE reserve_statements", "REQ-PR-2").await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ic_2_the_core_pauses_an_asset_on_a_stalled_check_or_a_quarantined_observation(
    pool: PgPool,
) {
    // Migration 0031 (review M1, M2): still only the asset's routes, still never lifted by the
    // core; now also for an evaluation of it that could not be made, or an observation of it the
    // book could not post.
    assets(&pool).await;
    let held = evaluation(&pool, "asset-a", "holds").await;
    let incomplete = |asset: &'static str| {
        sqlx::query_scalar::<_, i64>(
            "INSERT INTO reserve_evaluations (asset, evaluated_at, outcome, reason) \
             VALUES ($1, now(), 'incomplete', 'unreachable') RETURNING id",
        )
        .bind(asset)
        .fetch_one(&pool)
    };
    let stalled = incomplete("asset-a").await.unwrap();
    let other = incomplete("asset-b").await.unwrap();
    let observed: i64 = sqlx::query_scalar(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, executed) \
         VALUES ('origin-a', 100, '\\xaa', '2026-10-13T00:00:00Z', 'log', 3, TRUE) RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let quarantined: i64 = sqlx::query_scalar(
        "INSERT INTO book_quarantine (observation_id, asset, reason) \
         VALUES ($1, 'asset-a', 'refused') RETURNING id",
    )
    .bind(observed)
    .fetch_one(&pool)
    .await
    .unwrap();
    let core = bezent_store::CORE_ROLE;
    let pause = |asset: &str, evaluation: String, quarantine: String| {
        format!(
            "INSERT INTO pauses (asset, cause, evaluation_id, quarantine_id) \
             VALUES ('{asset}', 'REQ-IC-2', {evaluation}, {quarantine})"
        )
    };
    let refused = Some(bezent_store::REFUSAL_SQLSTATE);
    for sql in [
        pause("asset-a", held.to_string(), "NULL".into()),
        pause("asset-a", other.to_string(), "NULL".into()),
        pause("asset-b", "NULL".into(), quarantined.to_string()),
    ] {
        assert_eq!(
            as_role(&pool, core, &sql).await.as_deref(),
            refused,
            "{sql}"
        );
    }
    // Naming both is no cause (the guard refuses it before the table's check does).
    assert_eq!(
        as_role(
            &pool,
            core,
            &pause("asset-a", stalled.to_string(), quarantined.to_string())
        )
        .await
        .as_deref(),
        refused
    );
    // As the owner, too: the table's check refuses it.
    let owner = sqlx::raw_sql(&pause(
        "asset-a",
        stalled.to_string(),
        quarantined.to_string(),
    ))
    .execute(&pool)
    .await
    .expect_err("a pause naming both");
    assert_eq!(
        owner.as_database_error().and_then(|d| d.code()).as_deref(),
        Some(CHECK)
    );
    for sql in [
        pause("asset-a", stalled.to_string(), "NULL".into()),
        pause("asset-a", "NULL".into(), quarantined.to_string()),
    ] {
        assert_eq!(as_role(&pool, core, &sql).await, None, "{sql}");
    }
    // The quarantine is append-only, and the core cannot lift a pause.
    assert_eq!(
        as_role(&pool, core, "DELETE FROM book_quarantine")
            .await
            .as_deref(),
        Some(PRIVILEGE)
    );
}

/// Review SF-C: the lag bound is held against the route budgets whatever the order of setup. A
/// network inserted with its bound, then a route with a smaller budget, is refused; the bound
/// defaults to the finality time plus a minute (`NFR-5`, `NFR-6`, `REQ-IC-2`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn nfr_5_req_ic_2_sf_c_the_lag_bound_is_held_against_budgets_in_any_order(pool: PgPool) {
    assets(&pool).await;
    // The default: finality plus a minute, at most an hour.
    exec(
        &pool,
        "INSERT INTO networks (network, role, address_format, finality_seconds) VALUES \
           ('issuing-x', 'issuing', 'ss58', 12), ('origin-slow', 'origin', 'evm', 1200), \
           ('origin-huge', 'origin', 'evm', 3600)",
    )
    .await;
    for (network, bound) in [
        ("issuing-x", 72),
        ("origin-slow", 1260),
        ("origin-huge", 3600),
    ] {
        let got: i32 =
            sqlx::query_scalar("SELECT reserve_point_lag_seconds FROM networks WHERE network = $1")
                .bind(network)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(got, bound, "{network}");
    }
    let route = |network: &str, budget: i64| {
        format!(
            "INSERT INTO routes (asset, network, direction, minimum, maximum, window_limit, \
                 window_seconds, capacity, cutoffs, completion_budget_seconds) \
             VALUES ('asset-a', '{network}', 'deposit', 1, 1000, 10000, 86400, 10, '[]', {budget})"
        )
    };
    // A route inserted after its network, with a smaller budget than the network's bound.
    assert_refused(&pool, &route("origin-slow", 1259), "NFR-6").await;
    assert_refused(&pool, &route("origin-huge", 600), "NFR-6").await;
    // Or than the issuing network's, which serves every route.
    assert_refused(&pool, &route("origin-slow", 71), "NFR-6").await;
    exec(&pool, &route("origin-slow", 1260)).await;
    // And a budget lowered afterwards.
    assert_refused(
        &pool,
        "UPDATE routes SET completion_budget_seconds = 600 WHERE network = 'origin-slow'",
        "NFR-6",
    )
    .await;
    // A bound changed through the finality time is held against the budgets too.
    assert_refused(
        &pool,
        "UPDATE networks SET finality_seconds = 1300 WHERE network = 'origin-slow'",
        "NFR-6",
    )
    .await;
}
