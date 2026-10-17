// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Issue #46: every service checks its own store login at start-up, with one shared check
//! ([`bezent_store::login::check_login`]). A login that is a member of its role and of nothing
//! else passes; any other is refused with a message that says why (`REQ-AR-3`, `REQ-GV-5`).
//!
//! Logins are server-wide, so each test names its own, and grants that would widen a role are made
//! on the test's own database only.

mod common;

use bezent_store::login::{as_login, check_login, ensure_login, pool_options};
use sqlx::PgPool;

const PASSWORD: &str = "test-only-logins-password";

/// A pool logged in as `name`, made a member of `role` and of nothing else.
async fn login(owner: &PgPool, name: &str, role: &str) -> PgPool {
    ensure_login(owner, name, role, PASSWORD)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    pool_options()
        .max_connections(1)
        .connect_with(as_login(&owner.connect_options(), name, PASSWORD))
        .await
        .unwrap_or_else(|e| panic!("log in as {name}: {e}"))
}

async fn refused(pool: &PgPool, role: &str, why: &str) {
    let err = check_login(pool, role)
        .await
        .expect_err("the login was accepted");
    assert!(err.contains("#46"), "{err}");
    assert!(err.contains(why), "{why} not in: {err}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_46_each_services_login_passes_its_own_check(pool: PgPool) {
    for (name, role) in [
        ("logins_core_for_tests", bezent_store::CORE_ROLE),
        ("logins_ingress_for_tests", bezent_store::INGRESS_ROLE),
        (
            "logins_kreivo_for_tests",
            bezent_store::KREIVO_ENDPOINT_ROLE,
        ),
        ("logins_evm_for_tests", bezent_store::EVM_ENDPOINT_ROLE),
        ("logins_webhooks_for_tests", bezent_store::WEBHOOKS_ROLE),
        ("logins_signer_for_tests", bezent_store::SIGNER_ROLE),
    ] {
        let p = login(&pool, name, role).await;
        check_login(&p, role)
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        // And no other service's check passes for it.
        for other in [
            bezent_store::CORE_ROLE,
            bezent_store::INGRESS_ROLE,
            bezent_store::SIGNER_ROLE,
        ] {
            if other != role {
                assert!(check_login(&p, other).await.is_err(), "{name} as {other}");
            }
        }
        p.close().await;
    }
}

/// REQ-GV-5: every operator logs in as a login of their own, a member of `bezent_operator`; the
/// store records that login as the actor or the approver, so a shared login could never approve.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_gv_5_each_operator_has_a_login_of_their_own(pool: PgPool) {
    for name in ["logins_ops_carol_for_tests", "logins_ops_dave_for_tests"] {
        let p = login(&pool, name, bezent_store::OPERATOR_ROLE).await;
        check_login(&p, bezent_store::OPERATOR_ROLE)
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let me: String = sqlx::query_scalar("SELECT session_user::text")
            .fetch_one(&p)
            .await
            .unwrap();
        assert_eq!(me, name);
        p.close().await;
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_46_a_login_with_more_than_its_role_is_refused(pool: PgPool) {
    // A member of a second role.
    let two = login(&pool, "logins_two_roles_for_tests", bezent_store::CORE_ROLE).await;
    common::exec(&pool, "GRANT bezent_ingress TO logins_two_roles_for_tests").await;
    refused(&two, bezent_store::CORE_ROLE, "is a member of").await;
    // ensure_login brings it back to its own role alone.
    ensure_login(
        &pool,
        "logins_two_roles_for_tests",
        bezent_store::CORE_ROLE,
        PASSWORD,
    )
    .await
    .unwrap();
    check_login(&two, bezent_store::CORE_ROLE).await.unwrap();

    // A member of the retired service role, the way every login was before 0045.
    let old = login(&pool, "logins_old_core_for_tests", bezent_store::CORE_ROLE).await;
    common::exec(&pool, "GRANT bezent_service TO logins_old_core_for_tests").await;
    refused(&old, bezent_store::CORE_ROLE, "is a member of").await;

    // A privilege of its own, on this database only.
    let own = login(
        &pool,
        "logins_own_grant_for_tests",
        bezent_store::INGRESS_ROLE,
    )
    .await;
    common::exec(
        &pool,
        "GRANT SELECT ON signatures TO logins_own_grant_for_tests",
    )
    .await;
    refused(&own, bezent_store::INGRESS_ROLE, "signatures").await;

    // Its role widened by hand, on this database only.
    let widened = login(
        &pool,
        "logins_widened_for_tests",
        bezent_store::INGRESS_ROLE,
    )
    .await;
    common::exec(&pool, "GRANT INSERT ON intents TO bezent_ingress").await;
    refused(&widened, bezent_store::INGRESS_ROLE, "intents").await;
    common::exec(&pool, "REVOKE INSERT ON intents FROM bezent_ingress").await;
    check_login(&widened, bezent_store::INGRESS_ROLE)
        .await
        .unwrap();

    // A login that may create roles.
    let creates = login(
        &pool,
        "logins_createrole_for_tests",
        bezent_store::SIGNER_ROLE,
    )
    .await;
    common::exec(&pool, "ALTER ROLE logins_createrole_for_tests CREATEROLE").await;
    refused(&creates, bezent_store::SIGNER_ROLE, "create roles").await;
}

/// The role itself, or the migration owner, is never a service's login.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_46_the_owner_and_the_roles_are_not_logins(pool: PgPool) {
    // The owner (the test server's superuser).
    refused(&pool, bezent_store::CORE_ROLE, "superuser").await;
    // The owner acting as the role, through a SET ROLE on every connection: the way the tests'
    // pools did before #27.
    let switched = pool_options()
        .max_connections(1)
        .after_connect(|conn, _| {
            Box::pin(async move {
                // role-switch-ok: the refusal under test (#27).
                sqlx::Executor::execute(conn, "SET ROLE bezent_core").await?;
                Ok(())
            })
        })
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    refused(&switched, bezent_store::CORE_ROLE, "acts as bezent_core").await;
}

/// #46: a login reaching another role through its own (a role granted to the role, with SET) is
/// refused: the check asks what the login can become, not only what it was granted.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_46_a_login_that_can_become_another_role_is_refused(pool: PgPool) {
    let p = login(
        &pool,
        "logins_reach_for_tests",
        bezent_store::EVM_ENDPOINT_ROLE,
    )
    .await;
    check_login(&p, bezent_store::EVM_ENDPOINT_ROLE)
        .await
        .unwrap();
    // A second grant of the mark, with SET, to the login: it could become bezent_endpoint.
    common::exec(
        &pool,
        "GRANT bezent_endpoint TO logins_reach_for_tests WITH SET TRUE",
    )
    .await;
    refused(&p, bezent_store::EVM_ENDPOINT_ROLE, "is a member of").await;
    common::exec(&pool, "REVOKE bezent_endpoint FROM logins_reach_for_tests").await;
    check_login(&p, bezent_store::EVM_ENDPOINT_ROLE)
        .await
        .unwrap();
}

/// Review M2 of PR #56, the reviewer's scenario: the core's login holds `bezent_core` WITH ADMIN
/// OPTION, passes its check, and grants `bezent_core` to the ingress's role, which gains INSERT on
/// `intents` at once. The check refuses such a login, and a login made as the deployment makes
/// them holds no ADMIN OPTION, so it cannot grant its role on. A stand-in plays the ingress's role
/// here: roles are server-wide, and the real one is every other test's.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m2_a_core_login_that_could_grant_core_to_the_ingress_is_refused(pool: PgPool) {
    const LOGIN: &str = "logins_m2_core_for_tests";
    const STAND_IN: &str = "logins_m2_ingress_stand_in";
    common::exec(
        &pool,
        &format!(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{STAND_IN}') THEN \
                 CREATE ROLE {STAND_IN} NOLOGIN; \
               END IF; \
             END $$"
        ),
    )
    .await;
    let core = login(&pool, LOGIN, bezent_store::CORE_ROLE).await;
    check_login(&core, bezent_store::CORE_ROLE).await.unwrap();
    let grant = format!("GRANT {} TO {STAND_IN}", bezent_store::CORE_ROLE);
    let writes_intents = format!("SELECT has_table_privilege('{STAND_IN}', 'intents', 'INSERT')");

    // Made without ADMIN OPTION: it cannot grant its role on.
    let err = sqlx::raw_sql(&grant)
        .execute(&core)
        .await
        .expect_err("a login granted its role on");
    assert_eq!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("42501"),
        "{err}"
    );
    assert!(!common::flag(&pool, &writes_intents).await);

    // The reviewer's login: the role WITH ADMIN OPTION. Refused, and for good reason.
    common::exec(
        &pool,
        &format!(
            "GRANT {} TO {LOGIN} WITH ADMIN OPTION",
            bezent_store::CORE_ROLE
        ),
    )
    .await;
    refused(&core, bezent_store::CORE_ROLE, "ADMIN OPTION").await;
    sqlx::raw_sql(&grant)
        .execute(&core)
        .await
        .expect("with ADMIN OPTION the login grants its role on");
    assert!(common::flag(&pool, &writes_intents).await);

    // ensure_login, like the deployment's block, takes the option away, and with it (CASCADE)
    // what the login granted on.
    ensure_login(&pool, LOGIN, bezent_store::CORE_ROLE, PASSWORD)
        .await
        .unwrap();
    check_login(&core, bezent_store::CORE_ROLE).await.unwrap();
    assert!(!common::flag(&pool, &writes_intents).await);
    core.close().await;
}

/// Review S4 of PR #56: a store whose migration set is not this release's is refused, though its
/// grants are this release's: a migration that changes only triggers or guards (0041 here) applied
/// from other files (another checksum) or not at all, and one this release does not know.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s4_a_store_whose_migrations_are_not_this_releases_is_refused(pool: PgPool) {
    let core = login(&pool, "logins_s4_core_for_tests", bezent_store::CORE_ROLE).await;
    check_login(&core, bezent_store::CORE_ROLE).await.unwrap();
    // The newest migration is not the one that is off: only the whole set catches it.
    common::exec(
        &pool,
        "CREATE TABLE s4_saved AS SELECT * FROM _sqlx_migrations WHERE version = 41",
    )
    .await;

    common::exec(
        &pool,
        "UPDATE _sqlx_migrations SET checksum = '\\x00' WHERE version = 41",
    )
    .await;
    refused(&core, bezent_store::CORE_ROLE, "[41]").await;

    common::exec(&pool, "DELETE FROM _sqlx_migrations WHERE version = 41").await;
    refused(&core, bezent_store::CORE_ROLE, "lacks").await;

    common::exec(
        &pool,
        "INSERT INTO _sqlx_migrations SELECT * FROM s4_saved; \
         INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
           VALUES (9999, 'from a later release', TRUE, '\\x01', 0)",
    )
    .await;
    refused(&core, bezent_store::CORE_ROLE, "does not know").await;

    common::exec(
        &pool,
        "DELETE FROM _sqlx_migrations WHERE version = 9999; DROP TABLE s4_saved",
    )
    .await;
    check_login(&core, bezent_store::CORE_ROLE).await.unwrap();
    core.close().await;
}

/// The SQL block of `README.md` that starts with `marker`, as written.
fn readme_block(marker: &str) -> String {
    let readme = include_str!("../README.md");
    let start = readme
        .find(marker)
        .unwrap_or_else(|| panic!("README.md has no block {marker}"));
    let end = readme[start..].find("```").expect("the block ends") + start;
    readme[start..end].to_owned()
}

/// What the README's check lists for `login`.
async fn problems(pool: &PgPool, check: &str, login: &str) -> Vec<String> {
    sqlx::query_as::<_, (String, String)>(check)
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .filter(|(name, _)| name == login)
        .map(|(_, problem)| problem)
        .collect()
}

/// Review S5 of PR #56: the README's deployment steps bring a login made the way an earlier release
/// made them to its role alone, and its check lists what is wrong until they have run. The login:
/// NOINHERIT (the signer's ran SET ROLE), a member of `bezent_service` granted by a role other than
/// the one running the steps, of its own role with SET and ADMIN, and holding a grant of its own.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s5_the_readme_brings_an_old_login_to_its_role_alone(pool: PgPool) {
    const LOGIN: &str = "logins_s5_signer_for_tests";
    const GRANTOR: &str = "logins_s5_grantor_for_tests";
    let deploy = readme_block("-- deploy: bring a login to its role alone")
        .replace("'bezent_core_prod'", &format!("'{LOGIN}'"))
        .replace(
            "svc   TEXT := 'bezent_core';",
            &format!("svc   TEXT := '{}';", bezent_store::SIGNER_ROLE),
        );
    let check = readme_block("-- deploy: check every login of a store role");

    common::exec(
        &pool,
        &format!(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{LOGIN}') THEN \
                 CREATE ROLE {LOGIN} LOGIN PASSWORD '{PASSWORD}'; \
               END IF; \
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{GRANTOR}') THEN \
                 CREATE ROLE {GRANTOR} NOLOGIN; \
               END IF; \
             END $$; \
             ALTER ROLE {LOGIN} NOINHERIT; \
             GRANT bezent_service TO {GRANTOR} WITH ADMIN OPTION; \
             GRANT bezent_service TO {LOGIN} GRANTED BY {GRANTOR}; \
             GRANT bezent_signer TO {LOGIN} WITH ADMIN OPTION; \
             GRANT SELECT ON transfers TO {LOGIN};"
        ),
    )
    .await;
    let signer = pool_options()
        .max_connections(1)
        .connect_with(as_login(&pool.connect_options(), LOGIN, PASSWORD))
        .await
        .unwrap();
    let before = problems(&pool, &check, LOGIN).await;
    assert!(
        before.iter().any(|p| p.contains("is a member of"))
            && before.iter().any(|p| p.contains("with SET, with ADMIN")),
        "{before:?}"
    );
    assert!(
        check_login(&signer, bezent_store::SIGNER_ROLE)
            .await
            .is_err()
    );

    // The plain REVOKE the earlier README gave leaves the other grantor's membership in place.
    common::exec(&pool, &format!("REVOKE bezent_service FROM {LOGIN}")).await;
    assert!(
        problems(&pool, &check, LOGIN)
            .await
            .iter()
            .any(|p| p.contains("bezent_service")),
        "a plain REVOKE removed another grantor's membership"
    );

    common::exec(&pool, &deploy).await;
    assert_eq!(problems(&pool, &check, LOGIN).await, Vec::<String>::new());
    check_login(&signer, bezent_store::SIGNER_ROLE)
        .await
        .unwrap_or_else(|e| panic!("after the README's steps: {e}"));

    signer.close().await;
    common::exec(
        &pool,
        &format!("REVOKE bezent_service FROM {GRANTOR} CASCADE; DROP ROLE {GRANTOR}"),
    )
    .await;
}
