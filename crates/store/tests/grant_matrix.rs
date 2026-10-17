// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Issues #35, #33 (migration 0045): every role holds exactly its rows of the grant matrix
//! ([`bezent_store::grants::MATRIX`], explained in `crates/store/README.md`), and nothing else. A
//! migration that widens any grant (a table, a column, a sequence or a function, to any of the
//! store's roles or to PUBLIC) fails here (`REQ-AR-3`, `REQ-OB-6`, `INV-8`, `INV-9`, `REQ-GV-5`).

mod common;

use bezent_store::grants::{self, MATRIX, PUBLIC_FUNCTIONS, ROLES};
use sqlx::PgPool;

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ar_3_every_role_holds_exactly_its_rows_of_the_grant_matrix(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let mut wrong = Vec::new();
    for role in ROLES {
        let held = grants::held(&mut conn, role).await.unwrap();
        let expected = grants::expected(role);
        let extra: Vec<_> = held.iter().filter(|g| !expected.contains(g)).collect();
        let missing: Vec<_> = expected.iter().filter(|g| !held.contains(g)).collect();
        if !extra.is_empty() || !missing.is_empty() {
            wrong.push(format!(
                "{role}: holds {extra:?} beyond the matrix, lacks {missing:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The matrix names only the store's roles, each privilege once, and nothing for the shared roles.
#[test]
fn the_matrix_is_well_formed() {
    let mut seen = std::collections::BTreeSet::new();
    for (role, privilege, objects) in MATRIX {
        assert!(ROLES.contains(role), "{role}");
        assert!(
            ![bezent_store::SERVICE_ROLE, bezent_store::ENDPOINT_ROLE].contains(role),
            "{role} holds nothing (0045)"
        );
        assert!(seen.insert((*role, *privilege)), "{role} {privilege} twice");
        let mut sorted = objects.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            objects.len(),
            "{role} {privilege}: a duplicate"
        );
    }
}

/// F1 of 0017 for every function: PUBLIC may call only the listed helpers, which run with the
/// caller's privileges. A `SECURITY DEFINER` function left PUBLIC's shows in every role's rows.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ar_3_public_may_call_only_the_listed_helpers(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        grants::public_functions(&mut conn).await.unwrap(),
        PUBLIC_FUNCTIONS
    );
    // PUBLIC holds nothing on any relation, nor on the schema but USAGE.
    let public_relations: Vec<String> = sqlx::query_scalar(
        "SELECT c.relname::text || ' ' || a.privilege_type FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace, \
         LATERAL aclexplode(coalesce(c.relacl, acldefault('r', c.relowner))) a \
         WHERE n.nspname = 'public' AND a.grantee = 0 ORDER BY 1",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    assert!(
        public_relations.is_empty(),
        "PUBLIC holds {public_relations:?}"
    );
    let public_creates: bool =
        sqlx::query_scalar("SELECT has_schema_privilege('public', 'public', 'CREATE')")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert!(!public_creates);
}

/// #46, #35: no role of the store logs in, owns anything, may create, or is a member of another,
/// but for the two marks (held without SET or ADMIN OPTION, review M2 of PR #56): the ingress's of
/// the retired service role, and the endpoints' of `bezent_endpoint`.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ar_3_the_roles_own_nothing_and_hold_only_the_marks(pool: PgPool) {
    for role in ROLES {
        let (logs_in, attributes, owns, creates): (bool, bool, bool, bool) = sqlx::query_as(
            "SELECT r.rolcanlogin, \
                    r.rolsuper OR r.rolcreaterole OR r.rolcreatedb OR r.rolreplication OR r.rolbypassrls, \
                    EXISTS (SELECT 1 FROM pg_class WHERE relowner = r.oid) \
                      OR EXISTS (SELECT 1 FROM pg_namespace WHERE nspowner = r.oid) \
                      OR EXISTS (SELECT 1 FROM pg_proc WHERE proowner = r.oid) \
                      OR EXISTS (SELECT 1 FROM pg_type WHERE typowner = r.oid), \
                    has_schema_privilege(r.oid, 'public', 'CREATE') \
                      OR has_database_privilege(r.oid, current_database(), 'CREATE') \
                      OR has_database_privilege(r.oid, current_database(), 'TEMPORARY') \
             FROM pg_roles r WHERE r.rolname = $1",
        )
        .bind(role)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!logs_in && !attributes && !owns && !creates, "{role}");
    }
    let edges: Vec<(String, String, bool)> = sqlx::query_as(
        "SELECT u.rolname::text, g.rolname::text, m.set_option OR m.admin_option \
         FROM pg_auth_members m \
         JOIN pg_roles g ON g.oid = m.roleid JOIN pg_roles u ON u.oid = m.member \
         WHERE u.rolname = ANY ($1) ORDER BY 1, 2",
    )
    .bind(ROLES)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        edges,
        [
            (bezent_store::ENDPOINT_ROLE, bezent_store::SERVICE_ROLE),
            (bezent_store::EVM_ENDPOINT_ROLE, bezent_store::ENDPOINT_ROLE),
            (bezent_store::INGRESS_ROLE, bezent_store::SERVICE_ROLE),
            (
                bezent_store::KREIVO_ENDPOINT_ROLE,
                bezent_store::ENDPOINT_ROLE
            ),
        ]
        .map(|(u, g)| (u.to_owned(), g.to_owned(), false))
        .to_vec()
    );
}

/// The expected logins of the server `conn` is on: the ones the test suites make. Roles belong to
/// the server, not to a database, so every test binary's logins (each a member of a store role) are
/// seen from here too; the suites name them `…_tests…`, `…_for_tests`, `bzs_…` (the signer's) or
/// `bezent_sidecar_…` (the sidecar's). A deployment's expected logins are its services' and its
/// operators'.
async fn suite_logins(conn: &mut sqlx::PgConnection) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT rolname::text FROM pg_roles WHERE rolcanlogin \
           AND (rolname ~ 'tests?(_|$)' OR rolname LIKE 'bzs\\_%' \
                OR rolname LIKE 'bezent\\_sidecar\\_%') \
         ORDER BY 1",
    )
    .fetch_all(conn)
    .await
    .unwrap()
}

/// Whether the store `conn` grants anything beyond the matrix: a grant in the catalogue it does not
/// account for ([`grants::stray_grants`]), or a role whose privileges are not its rows.
async fn widened(conn: &mut sqlx::PgConnection) -> Vec<String> {
    let mut found = grants::stray_grants(&mut *conn).await.unwrap();
    let logins = suite_logins(&mut *conn).await;
    found.extend(grants::stray_members(&mut *conn, &logins).await.unwrap());
    for role in ROLES {
        if grants::held(&mut *conn, role).await.unwrap() != grants::expected(role) {
            found.push(format!("{role}'s privileges"));
        }
    }
    found
}

/// Review S3 of PR #56: the test reads the catalogue, so a grant the matrix does not name fails it
/// wherever it is made: to a role the matrix does not list (a login, a `pg_*` role, a new role), on
/// an object of a new schema or a new sequence or view, as a default privilege, or to PUBLIC on
/// the schema, the database, a function or a table. Each widening is made in a transaction of its
/// own, rolled back, and seen from that transaction.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s3_the_catalogue_grants_nothing_the_matrix_does_not_name(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(widened(&mut conn).await, Vec::<String>::new());
    drop(conn);

    for widening in [
        // To a login.
        "CREATE ROLE s3_login_for_tests LOGIN; \
         GRANT INSERT ON transitions TO s3_login_for_tests",
        // To a role of the server's own.
        "GRANT SELECT ON intents TO pg_read_all_stats",
        // To a role the matrix does not list.
        "CREATE ROLE bezent_s3_new_for_tests NOLOGIN; \
         GRANT INSERT ON transitions TO bezent_s3_new_for_tests",
        // On a new schema and its table, to a store role.
        "CREATE SCHEMA s3_schema; CREATE TABLE s3_schema.t (x int); \
         GRANT USAGE ON SCHEMA s3_schema TO bezent_ingress; \
         GRANT SELECT ON s3_schema.t TO bezent_ingress",
        // A new schema the core may create in.
        "CREATE SCHEMA s3_schema; GRANT CREATE ON SCHEMA s3_schema TO bezent_core",
        // A new sequence and a new view, to store roles.
        "CREATE SEQUENCE s3_seq; GRANT USAGE ON s3_seq TO bezent_core",
        "CREATE VIEW s3_view AS SELECT 1 AS x; GRANT SELECT ON s3_view TO bezent_signer",
        // A new function, to a store role.
        "CREATE FUNCTION s3_f() RETURNS int LANGUAGE sql AS 'SELECT 1'; \
         REVOKE EXECUTE ON FUNCTION s3_f() FROM PUBLIC; \
         GRANT EXECUTE ON FUNCTION s3_f() TO bezent_ingress",
        // A default privilege, which no grant list shows until a table is made.
        "ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO bezent_ingress",
        // To PUBLIC.
        "GRANT CREATE ON SCHEMA public TO PUBLIC",
        "DO $$ BEGIN \
           EXECUTE format('GRANT TEMPORARY ON DATABASE %I TO PUBLIC', current_database()); \
         END $$",
        "GRANT EXECUTE ON FUNCTION \
           source_bindable(text, text, bigint, bytea, position_kind, bigint) TO PUBLIC",
        "GRANT SELECT ON transfers TO PUBLIC",
        // A column of a table, to a store role.
        "GRANT UPDATE (state) ON transfers TO bezent_ingress",
    ] {
        let mut tx = pool.begin().await.unwrap();
        sqlx::raw_sql(widening)
            .execute(&mut *tx)
            .await
            .unwrap_or_else(|e| panic!("{widening}: {e}"));
        let found = widened(&mut tx).await;
        tx.rollback().await.unwrap();
        assert!(!found.is_empty(), "not caught: {widening}");
    }
}

/// Review S3b of PR #56: column grants and memberships are read from the catalogue too. A column
/// granted to a role outside the store's set (a `pg_*` role, a login, a new role) fails the test,
/// and so does any role or login, outside the store's set and the expected logins, that is a
/// member of a store role, directly or through a login that is. Each widening is made in a
/// transaction of its own, rolled back, and seen from that transaction.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s3b_column_grants_and_memberships_outside_the_store_roles_are_caught(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        grants::stray_grants(&mut conn).await.unwrap(),
        Vec::<String>::new()
    );
    let logins = suite_logins(&mut conn).await;
    assert_eq!(
        grants::stray_members(&mut conn, &logins).await.unwrap(),
        Vec::<String>::new()
    );
    drop(conn);

    for (widening, member) in [
        // A column, to a role of the server's own.
        (
            "GRANT UPDATE (state) ON transfers TO pg_read_all_stats",
            false,
        ),
        // A column, to a login.
        (
            "CREATE ROLE s3b_column_login LOGIN; \
             GRANT SELECT (bridge_reference) ON transfers TO s3b_column_login",
            false,
        ),
        // A column, to a role the matrix does not list.
        (
            "CREATE ROLE bezent_s3b_new NOLOGIN; \
             GRANT INSERT (to_state) ON transitions TO bezent_s3b_new",
            false,
        ),
        // A store role, to a role of the server's own.
        ("GRANT bezent_core TO pg_read_all_stats", true),
        // A store role, to a role the matrix does not list.
        (
            "CREATE ROLE s3b_role NOLOGIN; GRANT bezent_ingress TO s3b_role",
            true,
        ),
        // A store role, to a login nobody expects, held as a service holds it.
        (
            "CREATE ROLE s3b_login LOGIN; \
             GRANT bezent_core TO s3b_login WITH INHERIT TRUE, SET FALSE",
            true,
        ),
        // An expected login, to another role, which then reaches the login's store role.
        (
            "CREATE ROLE s3b_expected_for_tests LOGIN; \
             GRANT bezent_core TO s3b_expected_for_tests WITH INHERIT TRUE, SET FALSE; \
             CREATE ROLE s3b_reach NOLOGIN; GRANT s3b_expected_for_tests TO s3b_reach",
            true,
        ),
    ] {
        let mut tx = pool.begin().await.unwrap();
        sqlx::raw_sql(widening)
            .execute(&mut *tx)
            .await
            .unwrap_or_else(|e| panic!("{widening}: {e}"));
        let found = if member {
            let logins = suite_logins(&mut tx).await;
            grants::stray_members(&mut tx, &logins).await.unwrap()
        } else {
            grants::stray_grants(&mut tx).await.unwrap()
        };
        let whole = widened(&mut tx).await;
        tx.rollback().await.unwrap();
        assert!(!found.is_empty(), "not caught: {widening}");
        assert!(
            !whole.is_empty(),
            "not caught by the matrix test: {widening}"
        );
    }
}
