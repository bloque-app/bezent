// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The store's migrations in production: what [`bezent-migrate`](../src/main.rs) does.
//!
//! Every service refuses a store whose applied migrations differ from its own, by version and
//! checksum (`bezent_store::login::check_login`), so a release is only ever deployed onto a store
//! that was brought to exactly its migrations. This crate is the one production path that does it.
//! It never seeds anything: declarations and test data are the sidecar's (test) or the owner's
//! (production), not a migration runner's.
//!
//! - [`status`] reads the migration history without changing the database.
//! - [`apply`] takes a lock of its own, refuses a diverging history, applies what is pending, and
//!   reads the history again. Each migration commits on its own: a failure at version N leaves the
//!   versions before it applied, and a new run resumes at N (see [`apply`]).
//! - [`with_lock_timeout`] bounds every wait for a lock, so a run behind a held lock fails instead of
//!   waiting for ever.
//! - [`owner::OwnerConfig`] reads the owner's login from a configuration file (a `SecretRef`: a file or
//!   Vault, issue #64) instead of a URL in a file or the environment.
//! - [`refuse_service_login`] refuses to run as a login that is a service's.
//! - [`catalogue_findings`] is the catalogue-wide grants check of `bezent_store::grants`, the one
//!   the store tests run.
//!
//! Output is versions and role or object names, never a URL, a password or a checksum.

pub mod owner;

use bezent_store::grants::{self, PUBLIC_FUNCTIONS, ROLES};
use std::borrow::Cow;
use std::time::Duration;

use sqlx::migrate::{MigrateError, Migrator};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgConnection, PgPool};

/// The advisory lock that serialises runners of this crate on one database (`"bezentmg"`). `sqlx`'s
/// own lock is switched off in its favour; this one also covers the reads before and after, so
/// the versions [`apply`] returns are the ones this run applied.
const RUNNER_LOCK: i64 = 0x6265_7a65_6e74_6d67;

/// SQLSTATE `lock_not_available`: what `lock_timeout` raises.
const LOCK_NOT_AVAILABLE: &str = "55P03";

/// `options`, with `lock_timeout` set on every connection it opens. A statement, or the wait for the
/// runner lock, that waits longer than `timeout` for a lock fails with a lock timeout instead of
/// queueing behind it (and everything that queues behind the migration). `Duration::ZERO` sets no
/// timeout. `statement_timeout` is left alone: a long migration is not an error.
pub fn with_lock_timeout(options: PgPoolOptions, timeout: Duration) -> PgPoolOptions {
    options.after_connect(move |conn, _| {
        Box::pin(async move {
            sqlx::query("SELECT set_config('lock_timeout', $1, false)")
                .bind(format!("{}ms", timeout.as_millis()))
                .execute(conn)
                .await
                .map(|_| ())
        })
    })
}

/// Words for a failure; a lock timeout says what it was and what to do.
fn describe(e: &sqlx::Error) -> String {
    let timed_out = e
        .as_database_error()
        .is_some_and(|d| d.code().as_deref() == Some(LOCK_NOT_AVAILABLE));
    if timed_out {
        format!(
            "waited longer than the lock timeout (--lock-timeout) for a lock another session holds; \
             stop the services and end any idle transaction on the store, then run again: {e}"
        )
    } else {
        e.to_string()
    }
}

/// Where a store stands against a release's migrations.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// The versions applied, ascending.
    pub applied: Vec<i64>,
    /// The release's versions not applied yet, ascending.
    pub pending: Vec<i64>,
    /// Why the history is not a prefix of the release's, one line each; empty when it is.
    pub divergences: Vec<String>,
}

impl Status {
    /// Whether the store is exactly at the release: nothing pending, nothing diverging.
    pub fn is_current(&self) -> bool {
        self.pending.is_empty() && self.divergences.is_empty()
    }
}

/// The history of the store `pool` is connected to, against `migrator`. Read-only: it does not
/// create the history table.
///
/// A history diverges when a version is applied that the release does not have, an applied version's
/// checksum differs from the release's, a migration was applied only in part (`success = false`), a
/// pending version is lower than one already applied, or the store has tables and no history at all.
pub async fn status(pool: &PgPool, migrator: &Migrator) -> Result<Status, String> {
    let read = |e: sqlx::Error| format!("cannot read the migration history: {e}");
    let history: bool = sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
        .fetch_one(pool)
        .await
        .map_err(read)?;
    let applied: Vec<(i64, Vec<u8>, bool)> = if history {
        sqlx::query_as("SELECT version, checksum, success FROM _sqlx_migrations ORDER BY version")
            .fetch_all(pool)
            .await
            .map_err(read)?
    } else {
        Vec::new()
    };
    let mut status = Status::default();
    if !history {
        let tables: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_tables WHERE schemaname = current_schema()",
        )
        .fetch_one(pool)
        .await
        .map_err(read)?;
        if tables > 0 {
            status.divergences.push(format!(
                "the database holds {tables} table(s) and no migration history: not a bezent store \
                 made by this tool"
            ));
        }
    }
    for (version, checksum, success) in &applied {
        match migrator.iter().find(|m| m.version == *version) {
            None => status.divergences.push(format!(
                "version {version} is applied, and this release has no such migration"
            )),
            Some(m) if m.checksum.as_ref() != checksum.as_slice() => {
                status.divergences.push(format!(
                    "version {version} is applied with a different checksum than this release's"
                ))
            }
            Some(_) => {}
        }
        if !success {
            status
                .divergences
                .push(format!("version {version} was started and did not finish"));
        }
        status.applied.push(*version);
    }
    let highest = status.applied.last().copied();
    for m in migrator.iter() {
        if !status.applied.contains(&m.version) {
            if highest.is_some_and(|h| m.version < h) {
                status.divergences.push(format!(
                    "version {} is pending, and a higher version is already applied",
                    m.version
                ));
            }
            status.pending.push(m.version);
        }
    }
    Ok(status)
}

/// Applies the migrations `migrator` has that the store lacks, as the connection's login, and
/// returns the versions it applied. Refuses, changing nothing, a store whose history diverges
/// ([`status`]). Afterwards it reads the history again and fails unless the store is exactly at the
/// release.
///
/// Runners on one database serialise on an advisory lock: a second waits (up to the connection's
/// `lock_timeout`, see [`with_lock_timeout`]) and then finds nothing to do, so no version is applied
/// twice and the versions each returns are disjoint.
///
/// **It is not all-or-nothing.** Each migration commits on its own. If the migration at version N
/// fails, the versions before N stay applied and the error names N. The services refuse a store that
/// is not exactly at their release, so they stay down until a later run applies N and what follows;
/// that run resumes at N, and the migrations already applied are not run again.
pub async fn apply(pool: &PgPool, migrator: &Migrator) -> Result<Vec<i64>, String> {
    let mut guard = pool
        .acquire()
        .await
        .map_err(|e| format!("cannot connect to the store: {e}"))?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(RUNNER_LOCK)
        .execute(&mut *guard)
        .await
        .map_err(|e| format!("cannot take the runner lock: {}", describe(&e)))?;
    let result = apply_locked(pool, migrator).await;
    let released = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(RUNNER_LOCK)
        .execute(&mut *guard)
        .await;
    if released.is_err() {
        // End the session, which releases the lock, rather than hand a locked connection back.
        drop(guard.detach());
    }
    result
}

async fn apply_locked(pool: &PgPool, migrator: &Migrator) -> Result<Vec<i64>, String> {
    let before = status(pool, migrator).await?;
    if !before.divergences.is_empty() {
        return Err(format!(
            "refusing to migrate a store whose history diverges: {}",
            before.divergences.join("; ")
        ));
    }
    if before.pending.is_empty() {
        return Ok(Vec::new());
    }
    // `sqlx`'s own lock is off: the runner lock above covers it, and `sqlx` leaves its session lock
    // held on a connection when a migration fails, which would block the next run on this pool.
    let unlocked = Migrator {
        migrations: Cow::Owned(migrator.migrations.to_vec()),
        ignore_missing: migrator.ignore_missing,
        locking: false,
        no_tx: migrator.no_tx,
    };
    if let Err(e) = unlocked.run(pool).await {
        let done = status(pool, migrator).await.map(|s| s.applied).ok();
        let kept = done
            .map(|applied| {
                let new: Vec<i64> = applied
                    .into_iter()
                    .filter(|v| before.pending.contains(v))
                    .collect();
                if new.is_empty() {
                    "none of this run's migrations was applied".to_owned()
                } else {
                    format!("applied by this run and kept: {}", versions(&new))
                }
            })
            .unwrap_or_else(|| "could not read which migrations were kept".to_owned());
        let cause = match &e {
            MigrateError::ExecuteMigration(inner, version) => {
                format!("migration {version} failed: {}", describe(inner))
            }
            MigrateError::Execute(inner) => format!("migrations: {}", describe(inner)),
            other => format!("migrations: {other}"),
        };
        return Err(format!(
            "{cause}. Each migration commits on its own ({kept}); the services stay down until a \
             later run brings the store to the release, and it resumes at the failed version"
        ));
    }
    let after = status(pool, migrator).await?;
    if !after.is_current() {
        return Err(format!(
            "the store is not at the release after migrating: pending {:?}, {}",
            after.pending,
            after.divergences.join("; ")
        ));
    }
    Ok(before.pending)
}

fn versions(v: &[i64]) -> String {
    v.iter().map(i64::to_string).collect::<Vec<_>>().join(", ")
}

/// Refuses a connection whose login is a member of one of the store's service roles (or is one):
/// the migration owner is a login of its own, and a service's login must never migrate.
///
/// A membership with ADMIN OPTION alone is not counted (see the query), so a login holding only
/// ADMIN on its role is not refused here. That is not a licence: the service's own start-up check
/// (`bezent_store::login::check_login`, tested in `crates/store/tests/logins.rs`) still refuses any
/// login holding ADMIN OPTION on a store role, and [`catalogue_findings`] reports it.
pub async fn refuse_service_login(pool: &PgPool) -> Result<(), String> {
    let roles: Vec<String> = sqlx::query_scalar(
        // The recorded memberships that let the login act as the role or inherit its privileges,
        // followed transitively, and the login itself. `pg_has_role` would answer yes for every
        // role to a superuser, who is the owner on a local server. A membership with ADMIN
        // OPTION alone (SET FALSE, INHERIT FALSE) is not counted: PostgreSQL 16 records the owner
        // that creates a role as such a member, and on a server with no superuser (a managed one)
        // the owner needs it to grant the role to the services' logins.
        "WITH RECURSIVE held(oid) AS ( \
             SELECT oid FROM pg_roles WHERE rolname = session_user \
             UNION \
             SELECT m.roleid FROM pg_auth_members m JOIN held h ON m.member = h.oid \
             WHERE m.set_option OR m.inherit_option) \
         SELECT r.rolname::text FROM pg_roles r JOIN held h ON h.oid = r.oid \
         WHERE r.rolname = ANY ($1) ORDER BY 1",
    )
    .bind(ROLES)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("cannot read the login's roles: {e}"))?;
    if roles.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "this login is a member of {}: it is a service's login, not the migration owner's",
            roles.join(", ")
        ))
    }
}

/// The catalogue-wide grants check of the store tests, on the store `conn` is connected to: every
/// grant the matrix does not name, every role whose privileges are not its rows of the matrix,
/// every function PUBLIC may call that is not listed, and every role that reaches a store role
/// without being a store role or one of `logins` (the service and operator logins this deployment
/// made). Lines are role and object names only. Empty when the catalogue is as the release means it.
pub async fn catalogue_findings(
    conn: &mut PgConnection,
    logins: &[String],
) -> Result<Vec<String>, String> {
    let read = |e: sqlx::Error| format!("cannot read the catalogue: {e}");
    // The owner is the login running this, and is expected whatever else it is a member of.
    let owner: String = sqlx::query_scalar("SELECT session_user::text")
        .fetch_one(&mut *conn)
        .await
        .map_err(read)?;
    let logins: Vec<String> = logins.iter().cloned().chain([owner.clone()]).collect();
    let mut found = grants::stray_grants(&mut *conn).await.map_err(read)?;
    found.extend(separation_findings(&mut *conn, &owner).await?);
    found.extend(
        grants::stray_members(&mut *conn, &logins)
            .await
            .map_err(read)?,
    );
    for role in ROLES {
        if grants::held(&mut *conn, role).await.map_err(read)? != grants::expected(role) {
            found.push(format!(
                "{role}'s privileges are not its rows of the grant matrix"
            ));
        }
    }
    let public = grants::public_functions(&mut *conn).await.map_err(read)?;
    if public != PUBLIC_FUNCTIONS {
        found.push(format!(
            "the functions PUBLIC may call are {public:?}, not the listed helpers"
        ));
    }
    Ok(found)
}

/// The separation of the owner and the logins from the store's roles, which the migrations record
/// but do not enforce (the owner can re-grant itself a role, or give one a password): a tripwire
/// that makes such a change visible at the owner's next run or `--check`. Reports
///
/// - the owner holding SET or INHERIT on a store role, directly or through other roles it can act
///   as (ADMIN OPTION alone is what the migrations leave it, and is expected);
/// - a store role that can log in (the roles are NOLOGIN; only the declared logins log in);
/// - a login other than the owner holding ADMIN OPTION on a store role, or SET or INHERIT on more
///   than one of them (a login is brought to its role alone).
///
/// Lines are role names only.
async fn separation_findings(conn: &mut PgConnection, owner: &str) -> Result<Vec<String>, String> {
    let read = |e: sqlx::Error| format!("cannot read the catalogue: {e}");
    let mut found: Vec<String> = sqlx::query_scalar(
        "WITH RECURSIVE held(oid) AS ( \
             SELECT oid FROM pg_roles WHERE rolname = $2 \
             UNION \
             SELECT m.roleid FROM pg_auth_members m JOIN held h ON m.member = h.oid \
             WHERE m.set_option OR m.inherit_option) \
         SELECT 'the owner ' || $2 || ' can act as or inherit ' || r.rolname::text \
         FROM pg_roles r JOIN held h ON h.oid = r.oid \
         WHERE r.rolname = ANY ($1) ORDER BY 1",
    )
    .bind(ROLES)
    .bind(owner)
    .fetch_all(&mut *conn)
    .await
    .map_err(read)?;
    found.extend(
        sqlx::query_scalar::<_, String>(
            "SELECT 'the store role ' || rolname::text || ' can log in' \
             FROM pg_roles WHERE rolname = ANY ($1) AND rolcanlogin ORDER BY 1",
        )
        .bind(ROLES)
        .fetch_all(&mut *conn)
        .await
        .map_err(read)?,
    );
    found.extend(
        sqlx::query_scalar::<_, String>(
            "SELECT u.rolname::text || ' holds ADMIN OPTION on ' || g.rolname::text \
             FROM pg_auth_members m JOIN pg_roles g ON g.oid = m.roleid \
             JOIN pg_roles u ON u.oid = m.member \
             WHERE g.rolname = ANY ($1) AND m.admin_option AND u.rolcanlogin AND u.rolname <> $2 \
             UNION \
             SELECT u.rolname::text || ' can act as or inherit ' || count(*) || ' store roles' \
             FROM pg_auth_members m JOIN pg_roles g ON g.oid = m.roleid \
             JOIN pg_roles u ON u.oid = m.member \
             WHERE g.rolname = ANY ($1) AND (m.set_option OR m.inherit_option) \
               AND u.rolcanlogin AND u.rolname <> $2 \
             GROUP BY u.rolname HAVING count(*) > 1 \
             ORDER BY 1",
        )
        .bind(ROLES)
        .bind(owner)
        .fetch_all(&mut *conn)
        .await
        .map_err(read)?,
    );
    Ok(found)
}
