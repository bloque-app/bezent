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
    assert_eq!(apply(&pool, &MIGRATOR).await.unwrap(), all_versions());
    let s = status(&pool, &MIGRATOR).await.unwrap();
    assert!(s.is_current(), "{s:?}");
    assert_eq!(s.applied, all_versions());
    assert_eq!(apply(&pool, &MIGRATOR).await.unwrap(), Vec::<i64>::new());
}

#[sqlx::test]
async fn a_different_checksum_diverges_and_nothing_is_applied(pool: PgPool) {
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
