// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `bezent-migrate`: the history rules, the owner rule, the grants check, and the command line.
//! Each test runs in a fresh, empty database that `#[sqlx::test]` creates on `DATABASE_URL`'s server.

use std::process::{Command, Output};

use bezent_migrate::{apply, catalogue_findings, refuse_service_login, status};
use bezent_store::MIGRATOR;
use sqlx::PgPool;
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
