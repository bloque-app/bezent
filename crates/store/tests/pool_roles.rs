// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Issue #27: roles are never switched on pooled connections.
//!
//! The flaky test of #27 (`ac_i1_2_core_refuses_a_returned_signature_over_other_bytes`) ran
//! `SET ROLE bezent_signer` on a connection of the test's owner pool and returned it to the pool
//! still acting as the signer; a later query of the same test, meant to run as the owner, drew that
//! connection and was refused (`permission denied for table event_log`). In a service the same
//! leak would run queries under the wrong role. So:
//!
//! - every pool is made with [`bezent_store::login::guard`]: a connection that comes back acting
//!   as another role than its login is closed, never handed out again, and counted;
//! - every service logs in as its own login and never switches roles (one pool per login);
//! - a role is switched only with `SET LOCAL ROLE`, inside a transaction, and the test below that
//!   scans the sources fails on any session-level `SET ROLE`.

mod common;

use std::path::{Path, PathBuf};

use bezent_store::login::{guard, role_leaks};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

async fn backend(pool: &PgPool) -> i32 {
    sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// #27's root cause, reproduced: a session `SET ROLE` on a pooled connection never reaches a later
/// query. The guard closes the connection when it comes back, and counts the leak.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_27_a_connection_returned_acting_as_another_role_is_closed(pool: PgPool) {
    let owner = guard(PgPoolOptions::new().max_connections(1))
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let first = backend(&owner).await;
    let before = role_leaks();
    {
        let mut conn = owner.acquire().await.unwrap();
        // role-switch-ok: the leak under test (#27).
        sqlx::raw_sql("SET ROLE bezent_signer")
            .execute(&mut *conn)
            .await
            .unwrap();
    }
    // The query #27 saw refused: the owner reads the log again, on a new connection.
    let (n, me): (i64, String) =
        sqlx::query_as("SELECT (SELECT count(*) FROM event_log), current_user::text")
            .fetch_one(&owner)
            .await
            .expect("the owner reads the log: the leaked role was not handed out again");
    assert_eq!(n, 0);
    assert_ne!(me, bezent_store::SIGNER_ROLE);
    assert_ne!(
        backend(&owner).await,
        first,
        "the leaked connection was reused"
    );
    assert!(role_leaks() > before, "the leak was not counted");
}

/// `SET LOCAL ROLE` inside a transaction ends with it: the connection comes back as its login,
/// and the pool keeps it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_27_set_local_role_in_a_transaction_does_not_leak(pool: PgPool) {
    let owner = guard(PgPoolOptions::new().max_connections(1))
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let first = backend(&owner).await;
    for commit in [true, false] {
        let mut tx = owner.begin().await.unwrap();
        sqlx::raw_sql("SET LOCAL ROLE bezent_signer")
            .execute(&mut *tx)
            .await
            .unwrap();
        let me: String = sqlx::query_scalar("SELECT current_user::text")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(me, bezent_store::SIGNER_ROLE);
        if commit {
            tx.commit().await.unwrap();
        } else {
            drop(tx);
        }
        let (session, current): (String, String) =
            sqlx::query_as("SELECT session_user::text, current_user::text")
                .fetch_one(&owner)
                .await
                .unwrap();
        assert_eq!(session, current);
        assert_eq!(
            backend(&owner).await,
            first,
            "a clean connection was closed"
        );
    }
}

/// What switches a session's role, as the sources spell SQL: `SET ROLE` (and so `RESET ROLE`) and
/// `SET SESSION AUTHORIZATION`. Spelt in pieces so this file does not match itself.
const SWITCHES: [&str; 2] = [
    concat!("SET", " ROLE"),
    concat!("SESSION", " AUTHORIZATION"),
];

/// The repository's Rust sources, outside build directories.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if !name.starts_with('.') && name != "target" && name != "node_modules" {
                sources(&path, out);
            }
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

/// #27: no Rust source switches the role of a session (`SET ROLE`, `RESET ROLE` or
/// `SET SESSION AUTHORIZATION`) on a pooled connection; it switches with `SET LOCAL ROLE`, in a
/// transaction. The few places that switch on purpose, to prove a refusal or this guard, say so on
/// the line or the line before with `role-switch-ok`.
#[test]
fn issue_27_no_source_switches_a_sessions_role() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for dir in ["crates", "services", "tools", "e2e"] {
        let d = root.join(dir);
        if d.is_dir() {
            sources(&d, &mut files);
        }
    }
    assert!(files.len() > 50, "the scan found too few sources");
    let mut found = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let switches = SWITCHES.iter().any(|s| line.contains(s));
            let comment = line.trim_start().starts_with("//");
            let marked = line.contains("role-switch-ok")
                || i.checked_sub(1)
                    .is_some_and(|p| lines[p].contains("role-switch-ok"));
            if switches && !comment && !marked {
                found.push(format!("{}:{}: {}", file.display(), i + 1, line.trim()));
            }
        }
    }
    assert!(
        found.is_empty(),
        "a session-level role switch (use SET LOCAL ROLE in a transaction, #27):\n{}",
        found.join("\n")
    );
}
