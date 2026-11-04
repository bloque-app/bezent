// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-014 · Operations, at the store layer (SPEC §9: the store keeps these true whatever the
//! application does).
//!
//! * `AC-H3.1`, `REQ-ST-6`: every quantity the operator must see continuously is one read of
//!   `ops_observables()` (migration 0090).
//! * `AC-I3.1`, `REQ-GV-5`: an administrative act, here the pause and the lifting of one, leaves
//!   its actor and justification in an append-only record (migration 0091).
//! * `AC-I3.2`, `REQ-GV-5`: no operator login can edit a status, move value, or rewrite what the
//!   chains showed: the catalogue says so, and each forbidden write is attempted.
//! * `AC-I2.1`, `REQ-GV-1`, `REQ-GV-2`: a pause an operator makes holds new work for its scope
//!   (the core's gate reads `pauses`), and lifting it needs a second login.

mod common;

use common::{
    OPERATORS, assert_refused, bound_deposit, exec, finalise_coordinate, operator_login, text,
    transition, unrelated_login,
};
use sqlx::PgPool;
use std::collections::BTreeMap;

type Key = (String, String, String, String);
type Row = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// Every metric of the function, as (metric, asset, network, direction) to its value.
async fn observables(pool: &PgPool) -> BTreeMap<Key, Option<String>> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT metric, asset, network, direction, value::text FROM ops_observables()",
    )
    .fetch_all(pool)
    .await
    .expect("read ops_observables");
    rows.into_iter()
        .map(|(m, a, n, d, v)| {
            (
                (
                    m,
                    a.unwrap_or_default(),
                    n.unwrap_or_default(),
                    d.unwrap_or_default(),
                ),
                v,
            )
        })
        .collect()
}

fn value(
    all: &BTreeMap<Key, Option<String>>,
    metric: &str,
    asset: &str,
    network: &str,
    direction: &str,
) -> Option<f64> {
    let key = (
        metric.to_owned(),
        asset.to_owned(),
        network.to_owned(),
        direction.to_owned(),
    );
    all.get(&key)
        .unwrap_or_else(|| {
            panic!(
                "no row for {key:?}; have {:?}",
                all.keys().collect::<Vec<_>>()
            )
        })
        .as_deref()
        .map(|v| v.parse().expect("a number"))
}

/// One world with something in every place REQ-ST-6 names.
async fn world(pool: &PgPool) -> i64 {
    // A deposit in `Source-Final`, which also declares `origin-a`, its reserve and `asset-a`.
    let id = bound_deposit(pool, "obs-1", 100).await;
    exec(
        pool,
        "INSERT INTO routes (asset, network, direction, minimum, maximum, window_limit, \
             window_seconds, capacity, cutoffs, completion_budget_seconds) \
           VALUES ('asset-a', 'origin-a', 'deposit', 1, 1000000, 10000000, 86400, 10, '[]', 3600); \
         INSERT INTO routes (asset, network, direction, minimum, maximum, window_limit, \
             window_seconds, capacity, cutoffs, completion_budget_seconds) \
           VALUES ('asset-a', 'origin-a', 'withdrawal', 1, 1000000, 10000000, 86400, 10, '[]', 3600)",
    )
    .await;
    id
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_h3_1_the_age_of_the_oldest_nonfinal_transfer_is_per_route(pool: PgPool) {
    world(&pool).await;
    // An older one in the same route (a transfer's creation time is write-once: insert it aged).
    exec(
        &pool,
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, created_at, \
             source_network, source_block_number, source_block_hash, source_position_kind, \
             source_position_index) \
         VALUES ('obs-old', 'req-obs-old', 'deposit', 'asset-a', 1000, 'issuing', 'dest-1', 'Received', \
             now() - interval '2 hours', 'origin-a', 900, '\\xaa', 'log', 0)",
    )
    .await;
    let all = observables(&pool).await;
    let age = value(
        &all,
        "oldest_nonfinal_transfer_age_seconds",
        "asset-a",
        "origin-a",
        "deposit",
    )
    .expect("known");
    assert!((7199.0..7300.0).contains(&age), "{age}");
    assert_eq!(
        value(&all, "nonfinal_transfers", "asset-a", "origin-a", "deposit"),
        Some(2.0)
    );
    // A declared route with nothing in flight reads zero, not absent.
    assert_eq!(
        value(
            &all,
            "oldest_nonfinal_transfer_age_seconds",
            "asset-a",
            "origin-a",
            "withdrawal"
        ),
        Some(0.0)
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_h3_1_an_unknown_leg_is_counted_and_aged_until_it_is_seen_included(pool: PgPool) {
    let id = world(&pool).await;
    let before = observables(&pool).await;
    assert_eq!(
        value(&before, "unknown_legs", "asset-a", "origin-a", "deposit"),
        Some(0.0)
    );
    // Delivering: a signed attempt, not proven either way.
    transition(&pool, id, "Source-Final", "Delivering")
        .await
        .unwrap();
    let after = observables(&pool).await;
    assert_eq!(
        value(&after, "unknown_legs", "asset-a", "origin-a", "deposit"),
        Some(1.0)
    );
    let age = value(
        &after,
        "unknown_legs_oldest_age_seconds",
        "asset-a",
        "origin-a",
        "deposit",
    )
    .expect("known");
    assert!(age >= 0.0, "{age}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_h3_1_observer_lag_and_open_gaps_are_per_network(pool: PgPool) {
    world(&pool).await;
    exec(
        &pool,
        "INSERT INTO networks (network, role, address_format) VALUES ('silent', 'origin', 'opaque'); \
         UPDATE final_heads SET seen_at = now() - interval '45 seconds' WHERE false",
    )
    .await;
    // `final_heads` is append-only: the lag is read as of now.
    let all = observables(&pool).await;
    let lag = value(&all, "observer_lag_seconds", "", "origin-a", "").expect("recorded a head");
    assert!((0.0..60.0).contains(&lag), "{lag}");
    // A network that never recorded a final head is not known, which is not a zero.
    assert_eq!(value(&all, "observer_lag_seconds", "", "silent", ""), None);
    assert_eq!(value(&all, "open_gaps", "", "origin-a", ""), Some(0.0));

    exec(
        &pool,
        "INSERT INTO observation_ranges (network, first_block, last_block, outcome, \
             final_block_number, final_block_hash, reason) \
           VALUES ('origin-a', 500, 510, 'gap', 600, '\\x06', 'the provider answered no logs')",
    )
    .await;
    let all = observables(&pool).await;
    assert_eq!(value(&all, "open_gaps", "", "origin-a", ""), Some(1.0));
    // Filled by a later read: no longer open.
    exec(
        &pool,
        "INSERT INTO observation_ranges (network, first_block, last_block, outcome, \
             final_block_number, final_block_hash) VALUES ('origin-a', 500, 520, 'read', 600, '\\x06')",
    )
    .await;
    let all = observables(&pool).await;
    assert_eq!(value(&all, "open_gaps", "", "origin-a", ""), Some(0.0));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_h3_1_the_reserve_equation_and_the_reserve_per_network(pool: PgPool) {
    world(&pool).await;
    exec(
        &pool,
        "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
           VALUES ('origin-a', 130, '\\x78', now()), ('origin-a', 131, '\\x79', now()); \
         INSERT INTO reserve_evaluations (asset, evaluated_at, outcome) \
           VALUES ('asset-a', now() - interval '10 minutes', 'holds'); \
         INSERT INTO reserve_evaluations (asset, evaluated_at, outcome, differences) \
           VALUES ('asset-a', now() - interval '90 seconds', 'diverges', \
                   '[{\"term\": \"reserve\", \"chain\": \"1\", \"book\": \"2\"}]'); \
         INSERT INTO reserve_readings (network, block_number, block_hash, kind, subject, holder, amount) \
           VALUES ('origin-a', 130, '\\x78', 'reserve', 'token-a', '0xreserve-for-tests-origin-a', 7000), \
                  ('origin-a', 131, '\\x79', 'reserve', 'token-a', '0xreserve-for-tests-origin-a', 9000)",
    )
    .await;
    let all = observables(&pool).await;
    // The latest evaluation counts, not the first.
    assert_eq!(
        value(&all, "reserve_equation_outcome", "asset-a", "", ""),
        Some(1.0)
    );
    let age = value(&all, "reserve_equation_age_seconds", "asset-a", "", "").unwrap();
    assert!((89.0..200.0).contains(&age), "{age}");
    // The latest reading counts.
    assert_eq!(
        value(&all, "reserve_amount", "asset-a", "origin-a", ""),
        Some(9000.0)
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_h3_1_undelivered_notifications_and_unassociated_arrivals(pool: PgPool) {
    let id = world(&pool).await;
    exec(
        &pool,
        &format!(
            "INSERT INTO requesters (requester) VALUES ('alpha'); \
             INSERT INTO webhook_deliveries (requester, seq, dedupe_key, target, transfer_id, state, finished_at) \
               VALUES ('alpha', 1, 'log-1', 'transfer', {id}, 'pending', NULL), \
                      ('alpha', 2, 'log-2', 'transfer', {id}, 'dead', now())"
        ),
    )
    .await;
    // A final arrival at the reserve that no transfer binds: CND-Unassociated.
    finalise_coordinate(&pool, "origin-a", 300, "cc", "log", 0, "asset-a", "1234").await;
    let all = observables(&pool).await;
    assert_eq!(
        value(&all, "undelivered_notifications", "", "", ""),
        Some(1.0)
    );
    assert_eq!(value(&all, "dead_notifications", "", "", ""), Some(1.0));
    assert!(
        value(
            &all,
            "undelivered_notifications_oldest_age_seconds",
            "",
            "",
            ""
        )
        .unwrap()
            >= 0.0
    );
    assert_eq!(
        value(&all, "unassociated_arrivals", "asset-a", "origin-a", ""),
        Some(1.0)
    );
    assert!(
        value(
            &all,
            "unassociated_arrivals_oldest_age_seconds",
            "asset-a",
            "origin-a",
            ""
        )
        .unwrap()
            >= 0.0
    );
}

/// The view is for the core and the operators; another login reads nothing of it, and it shows
/// no row of any table: only the columns the metric names.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_h3_1_the_view_is_for_the_core_and_the_operators_only(pool: PgPool) {
    world(&pool).await;
    let alice = operator_login(&pool, OPERATORS[0]).await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM ops_observables()")
        .fetch_one(&alice)
        .await
        .expect("an operator reads the view without any grant on the tables under it");
    assert!(n > 0);
    let other = unrelated_login(&pool, "ops_unrelated_for_tests").await;
    let err = sqlx::query("SELECT 1 FROM ops_observables()")
        .fetch_all(&other)
        .await
        .unwrap_err();
    common::assert_error_sqlstate(&err, "42501", "a login of no bezent role reads the view");
    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT a.attname::text FROM pg_proc p, unnest(p.proargnames, p.proargmodes) \
           WITH ORDINALITY AS a(attname, mode, n) \
         WHERE p.proname = 'ops_observables' AND a.mode = 't' ORDER BY a.n",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        columns,
        ["metric", "asset", "network", "direction", "value"]
    );
}

// ---------------------------------------------------------------------------------------------
// AC-I3.1, AC-I2.1

async fn pause_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM pauses")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i3_1_a_pause_records_its_actor_and_justification(pool: PgPool) {
    world(&pool).await;
    let alice = operator_login(&pool, OPERATORS[0]).await;
    let pid: i64 = sqlx::query_scalar(
        "SELECT operator_pause('asset-a', NULL, 'deposit', 'divergence page 41')",
    )
    .fetch_one(&alice)
    .await
    .expect("an operator pauses");
    assert_eq!(pause_count(&pool).await, 1);
    let (kind, actor, why, detail): (String, String, String, String) =
        sqlx::query_as("SELECT kind, actor, justification, detail::text FROM admin_actions")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(kind, "pause");
    // The login, not a name the caller gave.
    assert_eq!(actor, OPERATORS[0]);
    assert_eq!(why, "divergence page 41");
    assert!(detail.contains(&format!("\"pause_id\": {pid}")), "{detail}");

    // No justification, no pause: a blank one is no justification either.
    for blank in ["", "   "] {
        let err = sqlx::query("SELECT operator_pause('asset-a', NULL, NULL, $1)")
            .bind(blank)
            .fetch_one(&alice)
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("admin_actions_justification_check"),
            "{err}"
        );
    }
    assert_eq!(
        pause_count(&pool).await,
        1,
        "a refused act leaves no pause behind"
    );

    // A scope that names nothing declared is refused whole: no pause, no record.
    assert!(
        sqlx::query("SELECT operator_pause('no-such-asset', NULL, NULL, 'x')")
            .fetch_one(&alice)
            .await
            .is_err()
    );
    assert_eq!(pause_count(&pool).await, 1);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM admin_actions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i3_1_the_record_is_append_only_and_only_the_functions_write_it(pool: PgPool) {
    world(&pool).await;
    let alice = operator_login(&pool, OPERATORS[0]).await;
    sqlx::query("SELECT operator_pause('asset-a', NULL, NULL, 'a reason')")
        .fetch_one(&alice)
        .await
        .unwrap();
    // Even the schema owner cannot rewrite it (the store's rule, REQ-GV-5).
    assert_refused(
        &pool,
        "UPDATE admin_actions SET justification = 'other'",
        "REQ-GV-5",
    )
    .await;
    assert_refused(&pool, "DELETE FROM admin_actions", "REQ-GV-5").await;
    assert_refused(&pool, "TRUNCATE admin_actions", "REQ-GV-5").await;
    // Whoever writes a row, the actor is the session's login and the time the store's: a name
    // given is replaced (here the schema owner, which may insert).
    exec(
        &pool,
        "INSERT INTO admin_actions (kind, subject, actor, justification, recorded_at) \
           VALUES ('other', 's', 'forged', 'j', '2001-01-01')",
    )
    .await;
    let (actor, recent): (String, bool) = sqlx::query_as(
        "SELECT actor, recorded_at > now() - interval '1 hour' FROM admin_actions WHERE subject = 's'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_ne!(actor, "forged");
    assert!(recent);
    // An operator names no actor and writes no row of it: only the functions do.
    for sql in [
        "INSERT INTO admin_actions (kind, subject, justification) VALUES ('other', 's', 'j')",
        "INSERT INTO admin_actions (kind, subject, actor, justification) VALUES ('other', 's', 'someone-else', 'j')",
        "UPDATE admin_actions SET justification = 'x'",
        "DELETE FROM admin_actions",
        "INSERT INTO pauses (asset) VALUES ('asset-a')",
        "DELETE FROM pauses",
        "UPDATE pauses SET asset = NULL",
    ] {
        let err = sqlx::query(sql).execute(&alice).await.unwrap_err();
        common::assert_error_sqlstate(&err, "42501", sql);
    }
    // A login that is no operator's cannot call them either.
    let other = unrelated_login(&pool, "ops_unrelated_for_tests").await;
    let err = sqlx::query("SELECT operator_pause('asset-a', NULL, NULL, 'x')")
        .fetch_one(&other)
        .await
        .unwrap_err();
    common::assert_error_sqlstate(&err, "42501", "a login of no bezent role pauses");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i3_1_acts_performed_elsewhere_are_recorded_but_a_pause_is_not_after_the_fact(
    pool: PgPool,
) {
    let alice = operator_login(&pool, OPERATORS[0]).await;
    sqlx::query(
        "SELECT operator_record('key-rotation', 'delivery key, issuing network', \
           '{\"old\": \"key-1\", \"new\": \"key-2\"}'::jsonb, 'scheduled rotation')",
    )
    .fetch_one(&alice)
    .await
    .expect("a rotation is recorded");
    let (actor, subject): (String, String) =
        sqlx::query_as("SELECT actor, subject FROM admin_actions WHERE kind = 'key-rotation'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (actor.as_str(), subject.as_str()),
        (OPERATORS[0], "delivery key, issuing network")
    );
    // The kinds with a function of their own cannot be recorded without it.
    for kind in ["pause", "unpause-request", "unpause"] {
        let err = sqlx::query("SELECT operator_record($1, 's', '{}'::jsonb, 'j')")
            .bind(kind)
            .fetch_one(&alice)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("REQ-GV-5"), "{kind}: {err}");
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_lifting_a_pause_needs_a_second_person_and_a_pause_holds_its_scope(pool: PgPool) {
    world(&pool).await;
    let alice = operator_login(&pool, OPERATORS[0]).await;
    let bob = operator_login(&pool, OPERATORS[1]).await;
    let pid: i64 =
        sqlx::query_scalar("SELECT operator_pause('asset-a', 'origin-a', NULL, 'incident 7')")
            .fetch_one(&alice)
            .await
            .unwrap();
    // The scope is what the core's gate and the ingress read (REQ-GV-1): a NULL matches every value.
    let holds: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pauses WHERE (asset IS NULL OR asset = 'asset-a') \
           AND (network IS NULL OR network = 'origin-a') AND (direction IS NULL OR direction = 'deposit'))",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(holds);

    let request: i64 =
        sqlx::query_scalar("SELECT operator_request_unpause($1, 'cause is understood')")
            .bind(pid)
            .fetch_one(&alice)
            .await
            .unwrap();
    // The requester cannot approve their own request (REQ-GV-2) ...
    let err = sqlx::query("SELECT operator_approve_unpause($1, 'me again')")
        .bind(request)
        .fetch_one(&alice)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("REQ-GV-2"), "{err}");
    assert_eq!(pause_count(&pool).await, 1, "the pause stands");
    // ... and a request that does not exist approves nothing.
    assert!(
        sqlx::query("SELECT operator_approve_unpause(999999, 'x')")
            .fetch_one(&bob)
            .await
            .is_err()
    );
    // Another login lifts it.
    sqlx::query("SELECT operator_approve_unpause($1, 'second look agrees')")
        .bind(request)
        .fetch_one(&bob)
        .await
        .expect("a second operator approves");
    assert_eq!(pause_count(&pool).await, 0);
    // Not twice.
    assert!(
        sqlx::query("SELECT operator_approve_unpause($1, 'again')")
            .bind(request)
            .fetch_one(&bob)
            .await
            .is_err()
    );

    let record: Vec<(String, String)> =
        sqlx::query_as("SELECT kind, actor FROM admin_actions ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        record,
        [
            ("pause".to_owned(), OPERATORS[0].to_owned()),
            ("unpause-request".to_owned(), OPERATORS[0].to_owned()),
            ("unpause".to_owned(), OPERATORS[1].to_owned()),
        ]
    );
    // A pause the core made (cause set) is lifted the same way, and by nobody else.
    assert_eq!(text(&pool, "SELECT count(*)::text FROM pauses").await, "0");
}

// ---------------------------------------------------------------------------------------------
// AC-I3.2

/// The catalogue: no operator privilege on any table is an UPDATE, a DELETE or a TRUNCATE; the
/// INSERTs are exactly the three records (and the ledger-account declaration); every function an
/// operator may call is on the list below, and each of those is read-only or writes only the
/// record or `pauses`.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i3_2_no_operator_privilege_edits_a_status_or_moves_value(pool: PgPool) {
    // Table-level: nothing but SELECT, and nothing at all on the tables that hold value and status.
    let dangerous: Vec<String> = sqlx::query_scalar(
        "SELECT table_name || ' ' || privilege_type FROM information_schema.role_table_grants \
         WHERE grantee = 'bezent_operator' AND table_schema = 'public' \
           AND privilege_type NOT IN ('SELECT') ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        dangerous.is_empty(),
        "table privileges beyond SELECT: {dangerous:?}"
    );

    // Column-level: the columns of the three records and the ledger declaration, and no UPDATE.
    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT table_name || '.' || column_name || ' ' || privilege_type \
         FROM information_schema.role_column_grants \
         WHERE grantee = 'bezent_operator' AND table_schema = 'public' \
           AND privilege_type <> 'SELECT' ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    for c in &columns {
        let table = c.split('.').next().unwrap();
        assert!(c.ends_with(" INSERT"), "{c}: an operator may only insert");
        assert!(
            ["manual_actions", "manual_approvals", "ledger_accounts"].contains(&table),
            "{c}: not one of the records an operator writes"
        );
    }

    // The functions an operator can execute, as the store defines them (not as a list says).
    let mut conn = pool.acquire().await.unwrap();
    let public = bezent_store::grants::public_functions(&mut conn)
        .await
        .unwrap();
    drop(conn);
    let functions: Vec<String> = sqlx::query_scalar(
        "SELECT p.oid::regprocedure::text FROM pg_proc p \
         JOIN pg_namespace n ON n.oid = p.pronamespace \
         WHERE n.nspname = 'public' AND p.prokind = 'f' \
           AND has_function_privilege('bezent_operator', p.oid, 'EXECUTE') \
           AND p.prorettype <> 'trigger'::regtype \
         ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let functions: Vec<String> = functions
        .into_iter()
        .filter(|f| {
            !public
                .iter()
                .any(|p| f.starts_with(p.split('(').next().unwrap_or(p)))
        })
        .collect();
    let allowed = [
        "bezent_applied_migrations()",
        "funding_arrival(text,text,text,bigint)",
        "one_business_day_after(timestamp with time zone)",
        "operator_approve_unpause(bigint,text)",
        "operator_pause(text,text,text,text)",
        "operator_record(text,text,jsonb,text)",
        "operator_request_unpause(bigint,text)",
        "ops_observables()",
        "source_bindable(text,text,bigint,bytea,position_kind,bigint)",
    ];
    assert_eq!(
        functions, allowed,
        "the operator's callable surface changed"
    );

    // The four that write say what they write: their source touches `pauses` and `admin_actions`
    // and no other table.
    let sources: Vec<String> = sqlx::query_scalar(
        "SELECT prosrc FROM pg_proc WHERE proname IN \
           ('operator_pause', 'operator_request_unpause', 'operator_approve_unpause', 'operator_record')",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(sources.len(), 4);
    for src in sources {
        for forbidden in [
            "transfers",
            "transitions",
            "book_",
            "observations",
            "reserve_",
            "intents",
            "signatures",
            "event_log",
            "final_heads",
            "routes",
            "manual_",
        ] {
            assert!(
                !src.to_lowercase().contains(forbidden),
                "an operator function touches {forbidden}: {src}"
            );
        }
    }
}

/// And each forbidden write, attempted as an operator's own login, is refused.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i3_2_each_forbidden_write_is_refused_to_an_operator(pool: PgPool) {
    let id = bound_deposit(&pool, "edit-1", 100).await;
    let alice = operator_login(&pool, OPERATORS[0]).await;
    let mut attempts: Vec<String> = vec![
        // A status, and what a transfer was asked.
        format!("UPDATE transfers SET state = 'Completed' WHERE id = {id}"),
        format!("UPDATE transfers SET condition = NULL WHERE id = {id}"),
        format!("UPDATE transfers SET amount = 1 WHERE id = {id}"),
        // What the chains showed, the limits, a pause.
        "UPDATE observations SET executed = false".to_owned(),
        "UPDATE routes SET maximum = 1".to_owned(),
        "UPDATE pauses SET asset = NULL".to_owned(),
        "UPDATE reserve_readings SET amount = 0".to_owned(),
    ];
    // Every table that holds a status, value, a signature, an observation or the log: no write of
    // any kind, by statements that parse whatever the columns are.
    for table in [
        "transfers",
        "transitions",
        "intake",
        "intents",
        "signed_transactions",
        "signatures",
        "observations",
        "observation_outcomes",
        "final_heads",
        "book_entries",
        "book_postings",
        "reserve_readings",
        "reserve_evaluations",
        "non_execution_proofs",
        "event_log",
        "routes",
        "reserves",
        "requester_credentials",
        "requesters",
        "pauses",
        "admin_actions",
        "unsigned_attempt_cancellations",
        "webhook_deliveries",
    ] {
        attempts.push(format!("INSERT INTO {table} DEFAULT VALUES"));
        attempts.push(format!("DELETE FROM {table}"));
        attempts.push(format!("TRUNCATE {table}"));
    }
    for sql in &attempts {
        let err = sqlx::raw_sql(sql)
            .execute(&alice)
            .await
            .expect_err(&format!("an operator must not: {sql}"));
        // Refused by a missing privilege, never by luck of a bad column list: the privilege is
        // checked before the statement is planned against the data.
        let code = err
            .as_database_error()
            .and_then(|e| e.code().map(|c| c.to_string()))
            .unwrap_or_default();
        assert_eq!(code, "42501", "{sql}: {err}");
    }
    // Nothing moved.
    assert_eq!(
        text(
            &pool,
            &format!("SELECT state FROM transfers WHERE id = {id}")
        )
        .await,
        "Source-Final"
    );
}
