// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `bezent-migrate`: the history rules, the owner rule, the grants check, and the command line.
//! Each test runs in a fresh, empty database that `#[sqlx::test]` creates on `DATABASE_URL`'s server.

use std::borrow::Cow;
use std::process::{Command, Output};
use std::time::Duration;

use bezent_migrate::{apply, catalogue_findings, refuse_service_login, status, with_lock_timeout};
use bezent_store::MIGRATOR;
use sqlx::PgPool;
use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::postgres::PgPoolOptions;

/// Roles and memberships belong to the server, not to a test's database: the tests that create a
/// login, or read the catalogue's logins, take this lock, so that one's login is not another's stray.
static SERVER_ROLES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The tests that change a store role's attributes or a login's memberships on the shared server
/// (the owner tripwires: SET on a role, LOGIN on a role) take it for writing; every other test
/// that migrates or reads the catalogue takes it for reading, so none sees such a change half-way.
/// Taken first, before `SERVER_ROLES`.
static ROLE_ATTRS: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

fn all_versions() -> Vec<i64> {
    MIGRATOR.iter().map(|m| m.version).collect()
}

async fn has_history(pool: &PgPool) -> bool {
    sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn status_reads_without_changing_anything(pool: PgPool) {
    let _attrs = ROLE_ATTRS.read().await;
    let s = status(&pool, &MIGRATOR).await.unwrap();
    assert_eq!(s.applied, Vec::<i64>::new());
    assert_eq!(s.pending, all_versions());
    assert!(s.divergences.is_empty());
    assert!(!s.is_current());
    assert!(
        !has_history(&pool).await,
        "a read created the history table"
    );
}

#[sqlx::test]
async fn apply_brings_the_store_to_the_release_and_is_then_a_no_op(pool: PgPool) {
    let _attrs = ROLE_ATTRS.read().await;
    assert_eq!(apply(&pool, &MIGRATOR).await.unwrap(), all_versions());
    let s = status(&pool, &MIGRATOR).await.unwrap();
    assert!(s.is_current(), "{s:?}");
    assert_eq!(s.applied, all_versions());
    assert_eq!(apply(&pool, &MIGRATOR).await.unwrap(), Vec::<i64>::new());
}

#[sqlx::test]
async fn a_different_checksum_diverges_and_nothing_is_applied(pool: PgPool) {
    let _attrs = ROLE_ATTRS.read().await;
    apply(&pool, &MIGRATOR).await.unwrap();
    sqlx::query("UPDATE _sqlx_migrations SET checksum = '\\x00' WHERE version = 1")
        .execute(&pool)
        .await
        .unwrap();
    let s = status(&pool, &MIGRATOR).await.unwrap();
    assert_eq!(s.divergences.len(), 1, "{s:?}");
    assert!(s.divergences[0].contains("version 1 "), "{s:?}");
    let e = apply(&pool, &MIGRATOR).await.unwrap_err();
    assert!(e.contains("diverges"), "{e}");
}

#[sqlx::test]
async fn a_pending_release_on_a_diverged_history_is_refused_before_any_change(pool: PgPool) {
    let _attrs = ROLE_ATTRS.read().await;
    // Only the first migration applied, with a wrong checksum: the rest are pending, and none runs.
    let first = MIGRATOR.iter().next().unwrap();
    sqlx::query(
        "CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL, \
         installed_on TIMESTAMPTZ NOT NULL DEFAULT now(), success BOOLEAN NOT NULL, \
         checksum BYTEA NOT NULL, execution_time BIGINT NOT NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES ($1, 'x', true, '\\x00', 0)",
    )
    .bind(first.version)
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        apply(&pool, &MIGRATOR)
            .await
            .unwrap_err()
            .contains("diverges")
    );
    let tables: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pg_tables WHERE schemaname = 'public'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(tables, 1, "only the history table may exist");
}

#[sqlx::test]
async fn a_version_this_release_lacks_diverges(pool: PgPool) {
    let _attrs = ROLE_ATTRS.read().await;
    apply(&pool, &MIGRATOR).await.unwrap();
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (99999, 'from a later release', true, '\\x00', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let s = status(&pool, &MIGRATOR).await.unwrap();
    assert!(s.divergences.iter().any(|d| d.contains("99999")), "{s:?}");
}

#[sqlx::test]
async fn an_unfinished_migration_diverges(pool: PgPool) {
    let _attrs = ROLE_ATTRS.read().await;
    apply(&pool, &MIGRATOR).await.unwrap();
    sqlx::query("UPDATE _sqlx_migrations SET success = false WHERE version = 2")
        .execute(&pool)
        .await
        .unwrap();
    let s = status(&pool, &MIGRATOR).await.unwrap();
    assert!(
        s.divergences.iter().any(|d| d.contains("did not finish")),
        "{s:?}"
    );
}

#[sqlx::test]
async fn tables_with_no_history_diverge(pool: PgPool) {
    let _attrs = ROLE_ATTRS.read().await;
    sqlx::query("CREATE TABLE networks (network text)")
        .execute(&pool)
        .await
        .unwrap();
    let s = status(&pool, &MIGRATOR).await.unwrap();
    assert!(
        s.divergences
            .iter()
            .any(|d| d.contains("no migration history")),
        "{s:?}"
    );
    assert!(apply(&pool, &MIGRATOR).await.is_err());
}

#[sqlx::test]
async fn a_service_login_is_not_the_owner(pool: PgPool) {
    let _attrs = ROLE_ATTRS.read().await;
    let _roles = SERVER_ROLES.lock().await;
    apply(&pool, &MIGRATOR).await.unwrap();
    refuse_service_login(&pool).await.expect("the test owner");
    sqlx::raw_sql(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'migrate_test_service_login') THEN \
             CREATE ROLE migrate_test_service_login LOGIN; \
           END IF; END $$; \
         GRANT bezent_core TO migrate_test_service_login",
    )
    .execute(&pool)
    .await
    .unwrap();
    let as_service = PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|conn, _| {
            Box::pin(async move {
                // role-switch-ok: a test acts as a service's login without its password.
                sqlx::query("SET SESSION AUTHORIZATION migrate_test_service_login")
                    .execute(conn)
                    .await
                    .map(|_| ())
            })
        })
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let refused = refuse_service_login(&as_service).await;
    as_service.close().await;
    sqlx::raw_sql("DROP OWNED BY migrate_test_service_login; DROP ROLE migrate_test_service_login")
        .execute(&pool)
        .await
        .unwrap();
    let e = refused.unwrap_err();
    assert!(e.contains("bezent_core"), "{e}");
}

/// Every role that can log in, on the server: the other tests' logins are not this store's strays.
async fn logins(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar("SELECT rolname::text FROM pg_roles WHERE rolcanlogin ORDER BY 1")
        .fetch_all(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn the_grants_check_is_clean_on_a_migrated_store_and_sees_a_widening(pool: PgPool) {
    let _attrs = ROLE_ATTRS.read().await;
    let _roles = SERVER_ROLES.lock().await;
    apply(&pool, &MIGRATOR).await.unwrap();
    let logins = logins(&pool).await;
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        catalogue_findings(&mut conn, &logins).await.unwrap(),
        Vec::<String>::new()
    );
    drop(conn);
    sqlx::query("GRANT SELECT ON transfers TO PUBLIC")
        .execute(&pool)
        .await
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    let found = catalogue_findings(&mut conn, &logins).await.unwrap();
    assert!(found.iter().any(|f| f.contains("transfers")), "{found:?}");
}

/// A pool whose connections act as `login` (`SET SESSION AUTHORIZATION`, so `session_user` is it).
async fn acting_as(pool: &PgPool, login: &'static str) -> PgPool {
    PgPoolOptions::new()
        .max_connections(1)
        .after_connect(move |conn, _| {
            Box::pin(async move {
                // role-switch-ok: a test acts as a login without its password.
                sqlx::query(&format!("SET SESSION AUTHORIZATION {login}"))
                    .execute(conn)
                    .await
                    .map(|_| ())
            })
        })
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap()
}

/// Review of PR #56, item 3: the runner does not refuse a login that holds ADMIN OPTION alone (the
/// owner is exactly that), but one that can act as the role is refused, and the catalogue check
/// reports the ADMIN-only login (the services' own start-up check refuses it:
/// `crates/store/tests/logins.rs`).
#[sqlx::test]
async fn an_admin_only_login_is_not_refused_by_the_runner_but_is_reported(pool: PgPool) {
    let _attrs = ROLE_ATTRS.read().await;
    let _roles = SERVER_ROLES.lock().await;
    apply(&pool, &MIGRATOR).await.unwrap();
    sqlx::raw_sql(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'migrate_test_admin_login') THEN \
             CREATE ROLE migrate_test_admin_login LOGIN; \
           END IF; END $$; \
         GRANT bezent_core TO migrate_test_admin_login WITH ADMIN TRUE, SET FALSE, INHERIT FALSE",
    )
    .execute(&pool)
    .await
    .unwrap();
    let as_login = acting_as(&pool, "migrate_test_admin_login").await;
    let admin_only = refuse_service_login(&as_login).await;
    as_login.close().await;
    let known = logins(&pool).await;
    let mut conn = pool.acquire().await.unwrap();
    let found = catalogue_findings(&mut conn, &known).await.unwrap();
    drop(conn);
    sqlx::raw_sql("GRANT bezent_core TO migrate_test_admin_login WITH SET TRUE")
        .execute(&pool)
        .await
        .unwrap();
    let as_login = acting_as(&pool, "migrate_test_admin_login").await;
    let with_set = refuse_service_login(&as_login).await;
    as_login.close().await;
    sqlx::raw_sql("DROP OWNED BY migrate_test_admin_login; DROP ROLE migrate_test_admin_login")
        .execute(&pool)
        .await
        .unwrap();
    admin_only.expect("ADMIN OPTION alone is not a service login");
    assert!(
        found
            .iter()
            .any(|f| f.contains("migrate_test_admin_login holds ADMIN OPTION on bezent_core")),
        "{found:?}"
    );
    assert!(with_set.unwrap_err().contains("bezent_core"));
}

/// Review of PR #56, item 2: what the owner could do to itself or to a role afterwards is reported
/// by the catalogue check, whoever the owner is (here: `owner`, a pool of the owner's connections).
async fn owner_tripwires(owner: &PgPool, known: &[String]) {
    async fn findings(owner: &PgPool, known: &[String]) -> Vec<String> {
        let mut conn = owner.acquire().await.unwrap();
        catalogue_findings(&mut conn, known).await.unwrap()
    }
    assert_eq!(findings(owner, known).await, Vec::<String>::new());
    let me: String = sqlx::query_scalar("SELECT session_user::text")
        .fetch_one(owner)
        .await
        .unwrap();
    // The owner grants itself SET on the signer's role.
    sqlx::raw_sql(&format!(
        "GRANT bezent_signer TO {me} WITH SET TRUE, INHERIT FALSE"
    ))
    .execute(owner)
    .await
    .unwrap();
    let set = findings(owner, known).await;
    sqlx::raw_sql(&format!("REVOKE bezent_signer FROM {me}"))
        .execute(owner)
        .await
        .unwrap();
    assert!(
        set.iter()
            .any(|f| f.contains("can act as or inherit bezent_signer")),
        "{set:?}"
    );
    // The same with INHERIT only.
    sqlx::raw_sql(&format!(
        "GRANT bezent_signer TO {me} WITH SET FALSE, INHERIT TRUE"
    ))
    .execute(owner)
    .await
    .unwrap();
    let inherit = findings(owner, known).await;
    sqlx::raw_sql(&format!("REVOKE bezent_signer FROM {me}"))
        .execute(owner)
        .await
        .unwrap();
    assert!(
        inherit
            .iter()
            .any(|f| f.contains("can act as or inherit bezent_signer")),
        "{inherit:?}"
    );
    // The owner lets a store role log in.
    sqlx::raw_sql("ALTER ROLE bezent_core LOGIN")
        .execute(owner)
        .await
        .unwrap();
    let login = findings(owner, known).await;
    sqlx::raw_sql("ALTER ROLE bezent_core NOLOGIN")
        .execute(owner)
        .await
        .unwrap();
    assert!(
        login
            .iter()
            .any(|f| f.contains("the store role bezent_core can log in")),
        "{login:?}"
    );
    assert_eq!(findings(owner, known).await, Vec::<String>::new());
}

#[sqlx::test]
async fn the_catalogue_check_sees_a_superuser_owner_act_as_a_store_role(pool: PgPool) {
    let _attrs = ROLE_ATTRS.write().await;
    let _roles = SERVER_ROLES.lock().await;
    apply(&pool, &MIGRATOR).await.unwrap();
    let known = logins(&pool).await;
    owner_tripwires(&pool, &known).await;
}

// ---- an owner that is not a superuser ---------------------------------------------------------

/// What a store's roles are, whoever ran the migrations: every store role's attributes and
/// privileges, and the memberships among them, sorted. Not the owner's own memberships.
async fn store_shape(pool: &PgPool) -> Vec<String> {
    let mut conn = pool.acquire().await.unwrap();
    let mut shape: Vec<String> = sqlx::query_scalar(
        "SELECT rolname::text || ' ' || rolsuper::text || rolcreaterole::text || rolcreatedb::text \
                || rolcanlogin::text || rolbypassrls::text || rolinherit::text \
         FROM pg_roles WHERE rolname LIKE 'bezent\\_%' ORDER BY 1",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    shape.extend(
        sqlx::query_scalar::<_, String>(
            "SELECT g.rolname::text || ' > ' || u.rolname::text || ' ' || m.admin_option::text \
                    || m.inherit_option::text || m.set_option::text \
             FROM pg_auth_members m JOIN pg_roles g ON g.oid = m.roleid \
             JOIN pg_roles u ON u.oid = m.member \
             WHERE g.rolname LIKE 'bezent\\_%' AND u.rolname LIKE 'bezent\\_%' ORDER BY 1",
        )
        .fetch_all(&mut *conn)
        .await
        .unwrap(),
    );
    for role in bezent_store::grants::ROLES {
        for (object, privilege) in bezent_store::grants::held(&mut conn, role).await.unwrap() {
            shape.push(format!("{role} holds {privilege} on {object}"));
        }
    }
    shape
}

/// The pattern of a managed server with no superuser (AWS RDS): the owner is a login with
/// CREATEROLE and CREATEDB, not a superuser. On PostgreSQL 16 or later a role that makes a role is
/// recorded as a member of it, with ADMIN OPTION (and SET and INHERIT if `createrole_self_grant`
/// says so); the migrations must run, and leave the store as a superuser's run does.
async fn createrole_owner_run(pool: &PgPool, self_grant: &str) {
    const OWNER: &str = "migrate_test_createrole_owner";
    const DATABASE: &str = "migrate_test_createrole_owner_db";
    const REFERENCE: &str = "migrate_test_createrole_reference_db";
    // On a server of its own (the CI job that sets this) the store's roles do not exist yet, so the
    // owner makes them, as it does on a new managed server. Elsewhere other tests share the server
    // and its roles: they are left alone, and the owner is given ADMIN OPTION on them instead.
    let fresh = std::env::var_os("BEZENT_TEST_FRESH_SERVER").is_some();
    async fn admin(pool: &PgPool, sql: &str) {
        sqlx::raw_sql(sql).execute(pool).await.unwrap();
    }
    async fn cleanup(pool: &PgPool, fresh: bool) {
        for db in [DATABASE, REFERENCE] {
            admin(pool, &format!("DROP DATABASE IF EXISTS {db} WITH (FORCE)")).await;
        }
        admin(
            pool,
            &format!(
                "DO $$ BEGIN IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{OWNER}') THEN \
                   DROP OWNED BY {OWNER}; DROP ROLE {OWNER}; END IF; END $$"
            ),
        )
        .await;
        if fresh {
            // Memberships between the roles make a drop fail until the other is gone: go round.
            admin(
                pool,
                "DO $$ DECLARE r TEXT; BEGIN FOR i IN 1..4 LOOP \
                   FOR r IN SELECT rolname::text FROM pg_roles WHERE rolname LIKE 'bezent\\_%' LOOP \
                     BEGIN EXECUTE format('DROP ROLE %I', r); EXCEPTION WHEN OTHERS THEN NULL; END; \
                   END LOOP; END LOOP; END $$",
            )
            .await;
        }
    }
    cleanup(pool, fresh).await;
    if fresh {
        let left: Vec<String> = sqlx::query_scalar(
            "SELECT rolname::text FROM pg_roles WHERE rolname LIKE 'bezent\\_%'",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(left, Vec::<String>::new(), "this server is not fresh");
    }
    admin(
        pool,
        &format!("CREATE ROLE {OWNER} LOGIN CREATEROLE CREATEDB PASSWORD 'test-only-owner-login'"),
    )
    .await;
    admin(
        pool,
        &format!("ALTER ROLE {OWNER} SET createrole_self_grant = '{self_grant}'"),
    )
    .await;
    admin(pool, &format!("CREATE DATABASE {DATABASE} OWNER {OWNER}")).await;
    admin(pool, &format!("CREATE DATABASE {REFERENCE}")).await;
    if !fresh {
        admin(
            pool,
            &format!(
                "DO $$ DECLARE r TEXT; BEGIN \
                   FOR r IN SELECT rolname::text FROM pg_roles WHERE rolname LIKE 'bezent\\_%' LOOP \
                     EXECUTE format('GRANT %I TO {OWNER} WITH ADMIN TRUE, INHERIT FALSE, SET FALSE', r); \
                   END LOOP; END $$"
            ),
        )
        .await;
    }

    let options = (*pool.connect_options())
        .clone()
        .database(DATABASE)
        .username(OWNER)
        .password("test-only-owner-login");
    let owner = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    let is_super: bool =
        sqlx::query_scalar("SELECT rolsuper FROM pg_roles WHERE rolname = session_user")
            .fetch_one(&owner)
            .await
            .unwrap();
    assert!(!is_super, "the owner must not be a superuser");

    refuse_service_login(&owner)
        .await
        .expect("the owner is not a service login");
    assert_eq!(apply(&owner, &MIGRATOR).await.unwrap(), all_versions());
    let s = status(&owner, &MIGRATOR).await.unwrap();
    assert!(s.is_current(), "{s:?}");
    // Still the owner after the run, and still not refused.
    refuse_service_login(&owner)
        .await
        .expect("the owner is not a service login");

    // The same roles, attributes, memberships and privileges as the server's superuser makes of the
    // same release in a database of its own.
    let su: bool = sqlx::query_scalar("SELECT rolsuper FROM pg_roles WHERE rolname = session_user")
        .fetch_one(pool)
        .await
        .unwrap();
    assert!(su, "the test server's login is a superuser");
    let reference_pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with((*pool.connect_options()).clone().database(REFERENCE))
        .await
        .unwrap();
    apply(&reference_pool, &MIGRATOR).await.unwrap();
    assert_eq!(
        store_shape(&owner).await,
        store_shape(&reference_pool).await
    );
    reference_pool.close().await;

    // The owner holds ADMIN OPTION on the roles it made (to grant them to the services' logins),
    // and neither SET nor INHERIT: it never acts as a service role nor inherits its privileges.
    let acts: Vec<String> = sqlx::query_scalar(
        "SELECT g.rolname::text FROM pg_auth_members m JOIN pg_roles g ON g.oid = m.roleid \
         WHERE m.member = (SELECT oid FROM pg_roles WHERE rolname = session_user) \
           AND g.rolname LIKE 'bezent\\_%' AND (m.inherit_option OR m.set_option)",
    )
    .fetch_all(&owner)
    .await
    .unwrap();
    assert_eq!(
        acts,
        Vec::<String>::new(),
        "the owner can act as a store role"
    );

    // The catalogue-wide grants check is clean, and the owner can make a service's login.
    let mut known = logins(&owner).await;
    known.push(OWNER.to_owned());
    let mut conn = owner.acquire().await.unwrap();
    assert_eq!(
        catalogue_findings(&mut conn, &known).await.unwrap(),
        Vec::<String>::new()
    );
    drop(conn);
    // The owner can do more than the migrations leave it, and the check says so.
    owner_tripwires(&owner, &known).await;
    bezent_store::login::ensure_login(&owner, "migrate_test_rds_core", "bezent_core", "00ff00ff")
        .await
        .unwrap();
    owner.close().await;
    admin(
        pool,
        "DROP OWNED BY migrate_test_rds_core; DROP ROLE migrate_test_rds_core",
    )
    .await;
    cleanup(pool, fresh).await;
}

#[sqlx::test]
async fn a_createrole_owner_without_superuser_runs_the_migrations(pool: PgPool) {
    let _attrs = ROLE_ATTRS.write().await;
    let _roles = SERVER_ROLES.lock().await;
    createrole_owner_run(&pool, "").await;
}

#[sqlx::test]
async fn a_createrole_owner_the_server_makes_a_full_member_is_brought_to_admin_only(pool: PgPool) {
    let _attrs = ROLE_ATTRS.write().await;
    let _roles = SERVER_ROLES.lock().await;
    createrole_owner_run(&pool, "set, inherit").await;
}

// ---- partial failure, locks, concurrency -------------------------------------------------------

fn migrator(sql: &[(i64, &'static str)]) -> Migrator {
    Migrator {
        migrations: Cow::Owned(
            sql.iter()
                .map(|(v, sql)| {
                    Migration::new(
                        *v,
                        Cow::Borrowed("test"),
                        MigrationType::Simple,
                        Cow::Borrowed(*sql),
                        false,
                    )
                })
                .collect(),
        ),
        ..Migrator::DEFAULT
    }
}

async fn applied(pool: &PgPool) -> Vec<i64> {
    sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success ORDER BY version")
        .fetch_all(pool)
        .await
        .unwrap()
}

/// A pool of its own on the test's database, so that a test can run two runners at once.
async fn pool_with(pool: &PgPool, lock_timeout: Duration) -> PgPool {
    with_lock_timeout(PgPoolOptions::new().max_connections(4), lock_timeout)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap()
}

#[sqlx::test]
async fn a_failure_at_n_keeps_the_versions_before_it_and_a_second_run_resumes(pool: PgPool) {
    let broken = migrator(&[
        (1, "CREATE TABLE one (id int)"),
        (2, "CREATE TABLE two (id int)"),
        (3, "CREATE TABLE three (id int); SELECT 1/0"),
        (4, "CREATE TABLE four (id int)"),
    ]);
    let e = apply(&pool, &broken).await.unwrap_err();
    assert!(e.contains("migration 3 failed"), "{e}");
    assert!(e.contains("kept: 1, 2"), "{e}");
    // 1 and 2 stay applied; 3 rolled back with its own statements; 4 never ran.
    assert_eq!(applied(&pool).await, vec![1, 2]);
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT tablename::text FROM pg_tables WHERE schemaname = 'public' \
         AND tablename <> '_sqlx_migrations' ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(tables, vec!["one", "two"]);
    let s = status(&pool, &broken).await.unwrap();
    assert_eq!((s.applied, s.pending), (vec![1, 2], vec![3, 4]));
    assert!(s.divergences.is_empty(), "the stopped store is a prefix");

    // The cause fixed (the same versions, 3 now sound): the next run resumes at 3.
    let fixed = migrator(&[
        (1, "CREATE TABLE one (id int)"),
        (2, "CREATE TABLE two (id int)"),
        (3, "CREATE TABLE three (id int)"),
        (4, "CREATE TABLE four (id int)"),
    ]);
    assert_eq!(apply(&pool, &fixed).await.unwrap(), vec![3, 4]);
    assert_eq!(applied(&pool).await, vec![1, 2, 3, 4]);
    assert!(status(&pool, &fixed).await.unwrap().is_current());
}

#[sqlx::test]
async fn a_migration_behind_a_held_lock_fails_at_the_lock_timeout_and_resumes_after(pool: PgPool) {
    let release = migrator(&[
        (1, "CREATE TABLE held (id int)"),
        (2, "ALTER TABLE held ADD COLUMN extra int"),
    ]);
    let mut blocker = pool.acquire().await.unwrap();
    let runner = pool_with(&pool, Duration::from_millis(800)).await;
    // Version 1 applies; a session then holds the table while version 2 wants it.
    let first = migrator(&[(1, "CREATE TABLE held (id int)")]);
    apply(&runner, &first).await.unwrap();
    sqlx::query("BEGIN").execute(&mut *blocker).await.unwrap();
    sqlx::query("LOCK TABLE held IN ACCESS SHARE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let e = apply(&runner, &release).await.unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(20), "it waited");
    assert!(e.contains("migration 2 failed"), "{e}");
    assert!(e.contains("lock timeout"), "{e}");
    assert_eq!(applied(&pool).await, vec![1]);
    // The lock released, the same runner finishes.
    sqlx::query("ROLLBACK")
        .execute(&mut *blocker)
        .await
        .unwrap();
    drop(blocker);
    assert_eq!(apply(&runner, &release).await.unwrap(), vec![2]);
    runner.close().await;
}

#[sqlx::test]
async fn two_runners_at_once_serialise_and_nothing_is_applied_twice(pool: PgPool) {
    let _roles = SERVER_ROLES.lock().await;
    let (a, b) = (
        pool_with(&pool, Duration::from_secs(120)).await,
        pool_with(&pool, Duration::from_secs(120)).await,
    );
    let (ra, rb) = tokio::join!(apply(&a, &MIGRATOR), apply(&b, &MIGRATOR));
    let (ra, rb) = (ra.unwrap(), rb.unwrap());
    let mut union: Vec<i64> = ra.iter().chain(&rb).copied().collect();
    union.sort_unstable();
    assert_eq!(
        union,
        all_versions(),
        "applied twice or not at all: {ra:?} {rb:?}"
    );
    assert!(
        ra.is_empty() || rb.is_empty(),
        "the second run found work to do: {ra:?} {rb:?}"
    );
    assert!(status(&pool, &MIGRATOR).await.unwrap().is_current());
    a.close().await;
    b.close().await;
}

#[sqlx::test]
async fn a_runner_behind_another_fails_at_the_lock_timeout(pool: PgPool) {
    let _roles = SERVER_ROLES.lock().await;
    // Another runner holds the runner lock (the key is the crate's, "bezentmg").
    let mut other = pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock(7090207764428123495)")
        .execute(&mut *other)
        .await
        .unwrap();
    let waiting = pool_with(&pool, Duration::from_millis(800)).await;
    let e = apply(&waiting, &MIGRATOR).await.unwrap_err();
    assert!(
        e.contains("runner lock") && e.contains("lock timeout"),
        "{e}"
    );
    assert!(!has_history(&pool).await, "it changed the store");
    sqlx::query("SELECT pg_advisory_unlock(7090207764428123495)")
        .execute(&mut *other)
        .await
        .unwrap();
    drop(other);
    assert_eq!(apply(&waiting, &MIGRATOR).await.unwrap(), all_versions());
    waiting.close().await;
}

// ---- the command line ------------------------------------------------------------------------

/// `DATABASE_URL` with the test's database in place of the one it names.
fn url_for(pool: &PgPool) -> String {
    let base = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let database = pool.connect_options().get_database().unwrap().to_owned();
    let (head, _) = base.rsplit_once('/').expect("a database in DATABASE_URL");
    format!("{head}/{database}")
}

fn run(args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bezent-migrate"))
        .args(args)
        .envs(env.iter().copied())
        .output()
        .expect("run bezent-migrate")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn usage_errors_exit_2() {
    for args in [
        vec![],
        vec!["--check"],
        vec!["--url-file", "a", "--url-env", "B"],
        vec!["--url-env", "B", "--grants", "sometimes"],
        vec!["--url-env", "B", "extra"],
        vec!["--url-env", "B", "--lock-timeout", "soon"],
        vec!["--url-env", "B", "--lock-timeout"],
    ] {
        let out = run(&args, &[]);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", text(&out));
    }
}

#[sqlx::test]
async fn the_command_line_checks_applies_and_refuses(pool: PgPool) {
    let _attrs = ROLE_ATTRS.read().await;
    let _roles = SERVER_ROLES.lock().await;
    let url = url_for(&pool);
    let logins = logins(&pool).await;
    let mut common = vec!["--url-env", "BEZENT_MIGRATE_TEST_URL"];
    for l in &logins {
        common.push("--login");
        common.push(l);
    }
    let env = [("BEZENT_MIGRATE_TEST_URL", url.as_str())];
    let with = |extra: &[&'static str]| {
        let mut args = common.clone();
        args.extend_from_slice(extra);
        args
    };

    // --check on an empty store: pending, exit 3, nothing created.
    let out = run(&with(&["--check"]), &env);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("pending 1, 2"));
    assert!(!has_history(&pool).await);

    // Apply: every version printed, grants as the release means them.
    let out = run(&with(&[]), &env);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    for v in all_versions() {
        assert!(stdout.contains(&format!("applied {v}\n")), "{stdout}");
    }
    assert!(
        stdout.contains("grants: as the release means them"),
        "{stdout}"
    );

    // --check, now current.
    let out = run(&with(&["--check"]), &env);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("current at"));

    // Divergence: exit 4, with --check and without.
    sqlx::query("UPDATE _sqlx_migrations SET checksum = '\\x00' WHERE version = 1")
        .execute(&pool)
        .await
        .unwrap();
    for extra in [&["--check"][..], &[][..]] {
        let out = run(&with(extra), &env);
        assert_eq!(out.status.code(), Some(4), "{extra:?}: {}", text(&out));
    }
}

#[sqlx::test]
async fn the_grants_check_fails_the_run_unless_told_to_report(pool: PgPool) {
    let _attrs = ROLE_ATTRS.read().await;
    let url = url_for(&pool);
    let env = [("BEZENT_MIGRATE_TEST_URL", url.as_str())];
    let base = [
        "--url-env",
        "BEZENT_MIGRATE_TEST_URL",
        "--login",
        "postgres",
    ];
    apply(&pool, &MIGRATOR).await.unwrap();
    sqlx::query("GRANT SELECT ON transfers TO PUBLIC")
        .execute(&pool)
        .await
        .unwrap();
    let mut args = base.to_vec();
    args.extend(["--check"]);
    let out = run(&args, &env);
    assert_eq!(out.status.code(), Some(5), "{}", text(&out));
    assert!(text(&out).contains("transfers"));
    args.extend(["--grants", "report"]);
    assert_eq!(run(&args, &env).status.code(), Some(0));
    args.extend(["--grants", "skip"]);
    assert_eq!(run(&args, &env).status.code(), Some(0));
}

#[sqlx::test]
async fn the_command_line_reads_the_url_from_a_file(pool: PgPool) {
    let _roles = SERVER_ROLES.lock().await;
    let dir = std::env::temp_dir().join(format!("bezent-migrate-file-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("url");
    std::fs::write(&file, format!("{}\n", url_for(&pool))).unwrap();
    let out = run(
        &[
            "--url-file",
            file.to_str().unwrap(),
            "--grants",
            "skip",
            "--lock-timeout",
            "30",
        ],
        &[],
    );
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(status(&pool, &MIGRATOR).await.unwrap().is_current());
}

#[test]
fn the_url_is_never_printed() {
    // A URL with a recognisable password, to a server nobody listens on, from a file and from the
    // environment: the failure says what failed and repeats neither.
    let url = "postgres://owner:hunter2-secret@127.0.0.1:1/bezent";
    let dir = std::env::temp_dir().join(format!("bezent-migrate-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("url");
    std::fs::write(&file, url).unwrap();
    let from_file = run(&["--url-file", file.to_str().unwrap()], &[]);
    let bad_url = run(&["--url-env", "U"], &[("U", "owner:hunter2-secret")]);
    let remote = run(
        &["--url-env", "U"],
        &[("U", "postgres://owner:hunter2-secret@db.example.net/b")],
    );
    std::fs::remove_dir_all(&dir).ok();
    for out in [from_file, bad_url, remote] {
        assert_eq!(out.status.code(), Some(1), "{}", text(&out));
        assert!(!text(&out).contains("hunter2"), "{}", text(&out));
    }
}
