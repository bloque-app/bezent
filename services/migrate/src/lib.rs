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
//! - [`apply`] refuses a diverging history, applies what is pending, and reads the history again.
//! - [`refuse_service_login`] refuses to run as a login that is a service's.
//! - [`catalogue_findings`] is the catalogue-wide grants check of `bezent_store::grants`, the one
//!   the store tests run.
//!
//! Output is versions and role or object names, never a URL, a password or a checksum.

use bezent_store::grants::{self, PUBLIC_FUNCTIONS, ROLES};
use sqlx::migrate::Migrator;
use sqlx::{PgConnection, PgPool};

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
pub async fn apply(pool: &PgPool, migrator: &Migrator) -> Result<Vec<i64>, String> {
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
    migrator
        .run(pool)
        .await
        .map_err(|e| format!("migrations: {e}"))?;
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

/// Refuses a connection whose login is a member of one of the store's service roles (or is one):
/// the migration owner is a login of its own, and a service's login must never migrate.
pub async fn refuse_service_login(pool: &PgPool) -> Result<(), String> {
    let roles: Vec<String> = sqlx::query_scalar(
        // The recorded memberships, followed transitively, and the login itself. `pg_has_role`
        // would answer yes for every role to a superuser, who is the owner on a local server.
        "WITH RECURSIVE held(oid) AS ( \
             SELECT oid FROM pg_roles WHERE rolname = session_user \
             UNION \
             SELECT m.roleid FROM pg_auth_members m JOIN held h ON m.member = h.oid) \
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
    let logins: Vec<String> = logins.iter().cloned().chain([owner]).collect();
    let mut found = grants::stray_grants(&mut *conn).await.map_err(read)?;
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
