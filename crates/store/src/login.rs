// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Store logins and their pools (issues #46, #27; `REQ-AR-3`, `REQ-GV-5`).
//!
//! Every service logs in as a login of its own: a member of its service's role and of nothing
//! else, that cannot become any other role. It acts as that login for its whole life, through the
//! privileges it inherits from the role; it never switches roles. So:
//!
//! - [`check_login`] is what each service runs at start-up, before it serves: it refuses a login
//!   that is anything more than a member of its role, or a role whose privileges are not exactly
//!   its rows of the grant matrix ([`crate::grants::MATRIX`]). The signer was the first to check
//!   its own (`F-005` review S3); this is that check, for every service.
//! - [`pool_options`] (or [`guard`] on options of one's own) is how every service makes its pool:
//!   one pool per login, and a connection that comes back acting as another role than its login
//!   (a session `SET ROLE` that was never reset) is closed, never handed out again, and counted
//!   in [`role_leaks`] (#27: a test's `SET ROLE` once leaked to a later query on the same pooled
//!   connection). A role is only ever switched with `SET LOCAL ROLE`, inside a transaction.
//! - [`ensure_login`] makes or re-keys such a login, for the sidecar and the tests. A deployment
//!   creates its logins the same way, as the migration owner (`crates/store/README.md`).

use std::sync::atomic::{AtomicU64, Ordering};

use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// The roles a login of `role` is also a member of, by inheritance, which hold nothing: the
/// retired [`crate::SERVICE_ROLE`] and the endpoints' mark [`crate::ENDPOINT_ROLE`] (migration
/// 0045). A login can become neither (they are held without SET).
pub fn marks(role: &str) -> &'static [&'static str] {
    match role {
        crate::INGRESS_ROLE => &[crate::SERVICE_ROLE],
        crate::KREIVO_ENDPOINT_ROLE | crate::EVM_ENDPOINT_ROLE => {
            &[crate::ENDPOINT_ROLE, crate::SERVICE_ROLE]
        }
        _ => &[],
    }
}

/// Proves, before a service serves, that `pool` logs in as a login of `role` and of nothing else
/// (#46; `REQ-AR-3`):
///
/// - the server is PostgreSQL 16 or later (before 16, every member of a role may `SET ROLE` into
///   it, so the next point cannot hold);
/// - the connection acts as its login (`current_user = session_user`): no role was switched;
/// - the login is not a role of the store's own, is no superuser, and cannot create roles or
///   databases, replicate or bypass row security;
/// - the login holds no role WITH ADMIN OPTION, directly or through a role it reaches (review M2
///   of PR #56), and no role it reaches is a superuser or may create roles or databases, replicate
///   or bypass row security (S1);
/// - the login is a member of `role` and of no other role, held WITH SET FALSE: it can `SET ROLE`
///   into no role (S1); through `role` it reaches only the marks of [`marks`], which hold nothing;
/// - neither the login nor any role it reaches owns an object of the store, so none can alter,
///   drop or disable a trigger, and none may create in the schema or the database;
/// - the store's migrations are exactly [`crate::MIGRATOR`]'s, versions and checksums (S4);
/// - the login's privileges, inherited ones included, are exactly `role`'s rows of
///   [`crate::grants::MATRIX`], so a grant widened by hand is refused.
///
/// The error says what is wrong, for the operator who reads it in the service's log.
pub async fn check_login(pool: &PgPool, role: &str) -> Result<(), String> {
    let refuse = |why: String| {
        Err(format!(
            "the store login must be a member of {role} and of nothing else, and act as itself \
             (#46, REQ-AR-3): {why}"
        ))
    };
    let store = |e: sqlx::Error| format!("store: {e}");
    let mut conn = pool.acquire().await.map_err(store)?;

    let (version, session, current, can_login, attributes): (i32, String, String, bool, bool) =
        sqlx::query_as(
            "SELECT current_setting('server_version_num')::int, session_user::text, \
                    current_user::text, rolcanlogin, \
                    rolsuper OR rolcreaterole OR rolcreatedb OR rolreplication OR rolbypassrls \
             FROM pg_roles WHERE rolname = session_user",
        )
        .fetch_one(&mut *conn)
        .await
        .map_err(store)?;
    if version < 160000 {
        return refuse(format!(
            "the server is PostgreSQL {version}; 16 or later is required, so that no login can \
             switch to a role it only inherits"
        ));
    }
    if current != session {
        return refuse(format!(
            "the connection acts as {current}, not as its login {session}: a service never \
             switches roles on a pooled connection (#27)"
        ));
    }
    if crate::grants::ROLES.contains(&session.as_str()) || !can_login {
        return refuse(format!(
            "{session} is a role of the store itself, not a login of its own; create a login \
             that is a member of {role}"
        ));
    }
    if attributes {
        return refuse(format!(
            "{session} is a superuser, or may create roles or databases, replicate or bypass row \
             security"
        ));
    }
    // Review M2 of PR #56: a login holding a role WITH ADMIN OPTION (directly, or through a role
    // it reaches) may grant that role to any other role, the ingress's included, and every running
    // service would gain its privileges at once.
    let admin: Vec<String> = sqlx::query_scalar(
        "SELECT g.rolname::text || ' (held by ' || u.rolname::text || ')' FROM pg_auth_members m \
         JOIN pg_roles g ON g.oid = m.roleid JOIN pg_roles u ON u.oid = m.member \
         WHERE m.admin_option AND pg_has_role(session_user, m.member, 'MEMBER') \
         UNION \
         SELECT rolname::text FROM pg_roles \
         WHERE rolname <> session_user \
           AND pg_has_role(session_user, oid, 'MEMBER WITH ADMIN OPTION') \
         ORDER BY 1",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(store)?;
    if !admin.is_empty() {
        return refuse(format!(
            "{session} holds ADMIN OPTION on {admin:?}: it could grant a role to any other"
        ));
    }

    let direct: Vec<String> = sqlx::query_scalar(
        "SELECT g.rolname::text FROM pg_auth_members m \
         JOIN pg_roles g ON g.oid = m.roleid JOIN pg_roles u ON u.oid = m.member \
         WHERE u.rolname = session_user ORDER BY 1",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(store)?;
    if direct != [role] {
        return refuse(format!("{session} is a member of {direct:?}"));
    }
    let settable: Vec<String> = sqlx::query_scalar(
        "SELECT rolname::text FROM pg_roles \
         WHERE rolname <> session_user AND pg_has_role(session_user, oid, 'SET') ORDER BY 1",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(store)?;
    // Review S1 of PR #56: the membership is held WITH SET FALSE, so the login never acts as its
    // role, and an attribute the role is given (CREATEROLE, say) is never the login's.
    if !settable.is_empty() {
        return refuse(format!(
            "{session} can switch to {settable:?}; grant {role} WITH INHERIT TRUE, SET FALSE"
        ));
    }
    let reachable: Vec<String> = sqlx::query_scalar(
        "SELECT rolname::text FROM pg_roles \
         WHERE rolname <> session_user AND pg_has_role(session_user, oid, 'MEMBER') ORDER BY 1",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(store)?;
    let mut allowed: Vec<&str> = std::iter::once(role)
        .chain(marks(role).iter().copied())
        .collect();
    allowed.sort_unstable();
    if reachable.iter().any(|r| !allowed.contains(&r.as_str())) {
        return refuse(format!("{session} reaches {reachable:?}"));
    }
    // Review S1: no role the login reaches is a superuser or may create roles or databases,
    // replicate or bypass row security.
    let mighty: Vec<String> = sqlx::query_scalar(
        "SELECT rolname::text FROM pg_roles \
         WHERE rolname <> session_user AND pg_has_role(session_user, oid, 'MEMBER') \
           AND (rolsuper OR rolcreaterole OR rolcreatedb OR rolreplication OR rolbypassrls) \
         ORDER BY 1",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(store)?;
    if !mighty.is_empty() {
        return refuse(format!(
            "{session} reaches {mighty:?}, which is a superuser, or may create roles or \
             databases, replicate or bypass row security"
        ));
    }

    for who in std::iter::once(session.as_str()).chain(reachable.iter().map(String::as_str)) {
        let owns: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_class WHERE relowner = r.oid) \
                 OR EXISTS (SELECT 1 FROM pg_namespace WHERE nspowner = r.oid) \
                 OR EXISTS (SELECT 1 FROM pg_proc WHERE proowner = r.oid) \
                 OR EXISTS (SELECT 1 FROM pg_type WHERE typowner = r.oid) \
                 OR EXISTS (SELECT 1 FROM pg_database WHERE datdba = r.oid) \
             FROM pg_roles r WHERE r.rolname = $1",
        )
        .bind(who)
        .fetch_one(&mut *conn)
        .await
        .map_err(store)?;
        if owns {
            return refuse(format!("{who} owns objects of the store"));
        }
    }
    let creates: bool = sqlx::query_scalar(
        "SELECT has_schema_privilege(session_user, 'public', 'CREATE') \
             OR has_database_privilege(session_user, current_database(), 'CREATE') \
             OR has_database_privilege(session_user, current_database(), 'TEMPORARY')",
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(store)?;
    if creates {
        return refuse(format!("{session} may create objects in the store"));
    }

    // Review S4 of PR #56: the store's migrations are exactly the ones this binary was built with,
    // checksums included; a store behind (or ahead, or migrated from other files) is refused, not
    // only one whose grants differ.
    let applied: Vec<(i64, Vec<u8>)> =
        match sqlx::query_as("SELECT version, checksum FROM bezent_applied_migrations()")
            .fetch_all(&mut *conn)
            .await
        {
            Ok(applied) => applied,
            Err(e) => {
                return refuse(format!(
                    "the store's migrations cannot be read ({e}): migrate the store to this \
                     release first"
                ));
            }
        };
    let built: Vec<(i64, Vec<u8>)> = crate::MIGRATOR
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
        .map(|m| (m.version, m.checksum.to_vec()))
        .collect();
    let missing: Vec<i64> = built
        .iter()
        .filter(|b| !applied.contains(b))
        .map(|b| b.0)
        .collect();
    let unknown: Vec<i64> = applied
        .iter()
        .filter(|a| !built.contains(a))
        .map(|a| a.0)
        .collect();
    if !missing.is_empty() || !unknown.is_empty() {
        return refuse(format!(
            "the store's migrations are not this release's: it lacks (or applied other files \
             for) {missing:?}, and applied {unknown:?} this release does not know (migrate the \
             store to this release first)"
        ));
    }

    let held = crate::grants::held(&mut conn, &session)
        .await
        .map_err(store)?;
    let expected = crate::grants::expected(role);
    if held != expected {
        let extra: Vec<_> = held.iter().filter(|g| !expected.contains(g)).collect();
        let missing: Vec<_> = expected.iter().filter(|g| !held.contains(g)).collect();
        return refuse(format!(
            "{session}'s privileges are not {role}'s rows of the grant matrix: it also holds \
             {extra:?}, and lacks {missing:?} (migrate the store to this release first)"
        ));
    }
    Ok(())
}

static ROLE_LEAKS: AtomicU64 = AtomicU64::new(0);

/// How many pooled connections, in this process, came back acting as another role than their
/// login and were closed (#27). Always 0 unless a role was switched outside a transaction.
pub fn role_leaks() -> u64 {
    ROLE_LEAKS.load(Ordering::Relaxed)
}

/// `options` with the #27 check: a connection that comes back to the pool acting as another role
/// than its login is closed and never handed out again, and the leak is
/// logged and counted ([`role_leaks`]).
pub fn guard(options: PgPoolOptions) -> PgPoolOptions {
    options.after_release(|conn, _| {
        Box::pin(async move {
            let (session, current): (String, String) =
                sqlx::query_as("SELECT session_user::text, current_user::text")
                    .fetch_one(&mut *conn)
                    .await?;
            if session == current {
                return Ok(true);
            }
            ROLE_LEAKS.fetch_add(1, Ordering::Relaxed);
            eprintln!(
                "bezent-store: a pooled connection of {session} came back acting as {current}; \
                 closed (#27: roles are never switched on a pooled connection)"
            );
            Ok(false)
        })
    })
}

/// [`PgPoolOptions`] with [`guard`]: how every service makes its pool, one per login.
pub fn pool_options() -> PgPoolOptions {
    guard(PgPoolOptions::new())
}

/// `base` as login `name`, with `password`.
pub fn as_login(base: &PgConnectOptions, name: &str, password: &str) -> PgConnectOptions {
    base.clone().username(name).password(password)
}

/// Makes login `name` (or re-keys it, when it exists) a member of `role` and of nothing else, as
/// the migration owner `owner`, with `password`: held WITH INHERIT TRUE, SET FALSE and without
/// ADMIN OPTION (review M2, S1 of PR #56). Memberships it held before are revoked, whoever granted
/// them, so a login made for an earlier release is brought to this one's rule (#46). For the sidecar and the
/// tests; `name` and `password` are the caller's and are not quoted, so they must be plain
/// identifiers and hex.
pub async fn ensure_login(
    owner: &PgPool,
    name: &str,
    role: &str,
    password: &str,
) -> Result<(), String> {
    let plain = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    };
    if !plain(name) || !plain(password) || !crate::grants::ROLES.contains(&role) {
        return Err(format!(
            "cannot make the login {name}: not a plain name, password or role"
        ));
    }
    let sql = format!(
        "DO $$ \
         DECLARE other TEXT; grantor TEXT; \
         BEGIN \
           IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{name}') THEN \
             BEGIN CREATE ROLE \"{name}\" LOGIN PASSWORD '{password}'; \
             EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END; \
           END IF; \
           ALTER ROLE \"{name}\" LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION \
             NOBYPASSRLS INHERIT PASSWORD '{password}'; \
           FOR other, grantor IN SELECT g.rolname, gr.rolname FROM pg_auth_members m \
               JOIN pg_roles g ON g.oid = m.roleid JOIN pg_roles u ON u.oid = m.member \
               JOIN pg_roles gr ON gr.oid = m.grantor \
               WHERE u.rolname = '{name}' \
                 AND (g.rolname <> '{role}' OR m.admin_option OR m.set_option \
                      OR NOT m.inherit_option) LOOP \
             EXECUTE format('REVOKE %I FROM %I GRANTED BY %I CASCADE', other, '{name}', grantor); \
           END LOOP; \
           IF NOT EXISTS (SELECT 1 FROM pg_auth_members m \
               JOIN pg_roles g ON g.oid = m.roleid JOIN pg_roles u ON u.oid = m.member \
               WHERE u.rolname = '{name}' AND g.rolname = '{role}') THEN \
             BEGIN EXECUTE 'GRANT {role} TO \"{name}\" WITH INHERIT TRUE, SET FALSE'; \
             EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END; \
           END IF; \
         END $$"
    );
    // Logins are shared by every database on the server: two databases making the same login at
    // once may collide on the role's catalogue row ("tuple concurrently updated"); the second
    // tries again.
    let mut tries = 0;
    loop {
        match sqlx::raw_sql(&sql).execute(owner).await {
            Ok(_) => return Ok(()),
            Err(e) if tries < 8 => {
                tries += 1;
                tokio::time::sleep(std::time::Duration::from_millis(20 * tries)).await;
                let _ = e;
            }
            Err(e) => return Err(format!("cannot make the login {name}: {e}")),
        }
    }
}

/// A pool logged in as login `name`, made (or re-keyed) by [`ensure_login`] a member of `role` and
/// of nothing else, on `owner`'s database, with [`guard`] on `options`.
pub async fn connect_login(
    owner: &PgPool,
    name: &str,
    role: &str,
    password: &str,
    options: PgPoolOptions,
) -> Result<PgPool, String> {
    ensure_login(owner, name, role, password).await?;
    guard(options)
        .connect_with(as_login(&owner.connect_options(), name, password))
        .await
        .map_err(|e| format!("cannot log in as {name}: {e}"))
}
