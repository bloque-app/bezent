// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #77 (migration 0095; `AC-I1.2`, `AC-I2.2`, `REQ-IC-2`, `REQ-IC-4`): a compromise alert
//! pauses the bridge in the transaction that records it, and that is the only way a service
//! pauses it. Each raiser here is on a login of its own that is a member of its role and of nothing
//! else, as each service's login is: the owner passes every guard, so a test that writes as the
//! owner proves nothing.

mod common;

use common::exec;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

const REFUSED: &str = bezent_store::REFUSAL_SQLSTATE;
const PRIVILEGE: &str = "42501";

/// Every role that can record a `key-compromise` alert.
const DETECTORS: [&str; 4] = [
    bezent_store::CORE_ROLE,
    bezent_store::KREIVO_ENDPOINT_ROLE,
    bezent_store::EVM_ENDPOINT_ROLE,
    bezent_store::SIGNER_ROLE,
];

/// Every role the migrations give a login.
const SERVICES: [&str; 6] = [
    bezent_store::CORE_ROLE,
    bezent_store::KREIVO_ENDPOINT_ROLE,
    bezent_store::EVM_ENDPOINT_ROLE,
    bezent_store::SIGNER_ROLE,
    bezent_store::INGRESS_ROLE,
    bezent_store::WEBHOOKS_ROLE,
];

async fn login(admin: &PgPool, role: &str) -> PgPool {
    bezent_store::login::connect_login(
        admin,
        &format!("compromise_pause_tests_{role}"),
        role,
        "compromise-pause-tests",
        PgPoolOptions::new().max_connections(2),
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"))
}

async fn declare(pool: &PgPool) {
    exec(
        pool,
        "INSERT INTO networks (network, role, address_format) VALUES \
           ('kreivo', 'issuing', 'ss58'), ('polygon', 'origin', 'evm'); \
         INSERT INTO assets (asset, precision, assurance_class, issuing_asset) VALUES \
           ('bUSDC', 6, 'public', 'Here(101)');",
    )
    .await;
}

/// The network `role` may raise an alert on (0049).
fn network_of(role: &str) -> &'static str {
    if role == bezent_store::EVM_ENDPOINT_ROLE {
        "polygon"
    } else {
        "kreivo"
    }
}

/// An alert of detection `dedupe`, as `role` writes one, with its position.
async fn raise(pool: &PgPool, role: &str, dedupe: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO event_log (kind, payload) VALUES ('key-compromise', \
           jsonb_build_object('dedupe', $1::text, 'network', $2::text)) RETURNING seq",
    )
    .bind(dedupe)
    .bind(network_of(role))
    .fetch_one(pool)
    .await
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

/// The global pause resting on alert `seq`, if there is one.
async fn pause_of(pool: &PgPool, seq: i64) -> Option<i64> {
    sqlx::query_scalar(
        "SELECT id FROM pauses WHERE alert_seq = $1 AND cause = 'REQ-IC-4' \
           AND asset IS NULL AND network IS NULL AND direction IS NULL",
    )
    .bind(seq)
    .fetch_optional(pool)
    .await
    .unwrap()
}

async fn all_pauses(pool: &PgPool) -> i64 {
    count(pool, "SELECT count(*) FROM pauses").await
}

fn sqlstate(result: Result<sqlx::postgres::PgQueryResult, sqlx::Error>) -> Option<String> {
    result.err().and_then(|e| {
        e.as_database_error()
            .and_then(|d| d.code().map(|c| c.into_owned()))
    })
}

async fn attempt(pool: &PgPool, sql: &str) -> Option<String> {
    sqlstate(sqlx::raw_sql(sql).execute(pool).await)
}

/// `AC-I1.2`, `REQ-IC-4`: an alert from any detector pauses the bridge, on its own login, with no
/// other service running.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_each_detector_s_alert_pauses_the_bridge(pool: PgPool) {
    declare(&pool).await;
    for (i, role) in DETECTORS.iter().enumerate() {
        let detector = login(&pool, role).await;
        let seq = raise(&detector, role, &format!("detection-{i}"))
            .await
            .unwrap_or_else(|e| panic!("{role}: {e}"));
        assert!(pause_of(&pool, seq).await.is_some(), "{role}: no pause");
        assert_eq!(all_pauses(&pool).await, i as i64 + 1, "{role}");
    }
}

/// `AC-I1.2`: the pause and its alert are one commit: neither is in force without the other.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_the_alert_and_its_pause_are_one_transaction(pool: PgPool) {
    declare(&pool).await;
    let role = bezent_store::SIGNER_ROLE;
    let signer = login(&pool, role).await;
    let mut tx = signer.begin().await.unwrap();
    let seq: i64 = sqlx::query_scalar(
        "INSERT INTO event_log (kind, payload) VALUES ('key-compromise', \
           '{\"dedupe\": \"rolled-back\"}') RETURNING seq",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    // In force for no one else until the commit, and gone with a rollback.
    assert_eq!(all_pauses(&pool).await, 0);
    tx.rollback().await.unwrap();
    assert_eq!(all_pauses(&pool).await, 0);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM event_log WHERE kind = 'key-compromise'"
        )
        .await,
        0
    );
    let again = raise(&signer, role, "rolled-back").await.unwrap();
    assert_eq!(again, seq, "the log leaves no gap");
    assert!(pause_of(&pool, again).await.is_some());
}

/// `REQ-AR-3`, least privilege: no service can pause the bridge except by recording an alert: not
/// with no alert, not by naming a real alert (its own or another's), and not by a request alone.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ic_4_the_guard_refuses_a_pause_with_no_alert_of_its_writing(pool: PgPool) {
    declare(&pool).await;
    let seq = raise(&pool, "owner", "real-alert").await.unwrap();
    exec(&pool, "DELETE FROM pauses").await;
    let name_it = format!("INSERT INTO pauses (cause, alert_seq) VALUES ('REQ-IC-4', {seq})");
    for role in SERVICES {
        let service = login(&pool, role).await;
        assert_eq!(
            attempt(&service, &name_it).await.as_deref(),
            Some(PRIVILEGE),
            "{role} named an alert"
        );
        let want = if role == bezent_store::CORE_ROLE {
            REFUSED
        } else {
            PRIVILEGE
        };
        for sql in [
            "INSERT INTO pauses (cause) VALUES ('REQ-IC-4')",
            "INSERT INTO pauses (asset, cause) VALUES ('bUSDC', 'REQ-IC-4')",
            "INSERT INTO pauses (asset) VALUES (NULL)",
        ] {
            assert_eq!(
                attempt(&service, sql).await.as_deref(),
                Some(want),
                "{role}: {sql}"
            );
        }
    }
    // A request, or an anomaly, with no alert pauses nothing.
    let core = login(&pool, bezent_store::CORE_ROLE).await;
    for sql in [
        "INSERT INTO event_log (kind, payload) VALUES ('pause-requested', '{\"scope\": \"global\"}')",
        "INSERT INTO event_log (kind, payload) VALUES ('delivery-anomaly', '{\"dedupe\": \"a\"}')",
    ] {
        assert_eq!(attempt(&core, sql).await, None, "{sql}");
    }
    assert_eq!(all_pauses(&pool).await, 0);

    // The ingress, the webhook dispatcher and an operator's login record no alert at all, so the
    // trigger is not a way for them to pause.
    for role in [bezent_store::INGRESS_ROLE, bezent_store::WEBHOOKS_ROLE] {
        let other = login(&pool, role).await;
        let refused = raise(&other, role, "not-a-detector").await.unwrap_err();
        assert!(refused.as_database_error().is_some(), "{role}: {refused}");
    }
    let operator = common::operator_login(&pool, common::OPERATORS[0]).await;
    assert!(
        raise(&operator, "operator", "not-a-detector")
            .await
            .is_err()
    );
    assert_eq!(all_pauses(&pool).await, 0);

    // Even the owner cannot make a compromise pause that names no alert, a row that is not in the
    // log or not an alert, or anything narrower than the whole bridge.
    let request: i64 = sqlx::query_scalar(
        "SELECT seq FROM event_log WHERE kind = 'pause-requested' ORDER BY seq LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    for (sql, code) in [
        ("INSERT INTO pauses (cause) VALUES ('REQ-IC-4')", "23514"),
        (
            "INSERT INTO pauses (cause, alert_seq) VALUES ('REQ-IC-4', 999999)",
            REFUSED,
        ),
        (
            &*format!("INSERT INTO pauses (cause, alert_seq) VALUES ('REQ-IC-4', {request})"),
            REFUSED,
        ),
        (
            &*format!(
                "INSERT INTO pauses (asset, cause, alert_seq) VALUES ('bUSDC', 'REQ-IC-4', {seq})"
            ),
            "23514",
        ),
        (
            &*format!("INSERT INTO pauses (alert_seq) VALUES ({seq})"),
            "23514",
        ),
    ] {
        assert_eq!(attempt(&pool, sql).await.as_deref(), Some(code), "{sql}");
    }
}

/// `AC-I2.2`, `REQ-GV-2`: no service lifts a pause or edits one, the raiser of its alert included.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_2_no_service_lifts_a_compromise_pause(pool: PgPool) {
    declare(&pool).await;
    let core = login(&pool, bezent_store::CORE_ROLE).await;
    let seq = raise(&core, bezent_store::CORE_ROLE, "stands")
        .await
        .unwrap();
    for role in SERVICES {
        let service = login(&pool, role).await;
        for sql in [
            "DELETE FROM pauses",
            "TRUNCATE pauses",
            "UPDATE pauses SET asset = 'bUSDC'",
            "UPDATE pauses SET cause = NULL, alert_seq = NULL",
        ] {
            assert_eq!(
                attempt(&service, sql).await.as_deref(),
                Some(PRIVILEGE),
                "{role}: {sql}"
            );
        }
        let can_call: bool = sqlx::query_scalar(
            "SELECT has_function_privilege($1, 'event_log_compromise_pauses()', 'EXECUTE')",
        )
        .bind(role)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!can_call, "{role} may execute the pause function");
    }
    assert!(pause_of(&pool, seq).await.is_some());
}

/// `AC-I1.2`: one detection, one pause. A repeat fails as it did (0049's refusal, which the raisers
/// read as "already raised"), adds nothing while paused, and pauses nothing after a lift.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_repeated_detection_never_pauses_twice(pool: PgPool) {
    declare(&pool).await;
    let role = bezent_store::KREIVO_ENDPOINT_ROLE;
    let endpoint = login(&pool, role).await;
    let seq = raise(&endpoint, role, "the-detection").await.unwrap();
    assert_eq!(all_pauses(&pool).await, 1);

    let repeat = raise(&endpoint, role, "the-detection").await.unwrap_err();
    assert_eq!(
        repeat.as_database_error().and_then(|d| d.code()).as_deref(),
        Some("23505"),
        "{repeat}"
    );
    assert_eq!(all_pauses(&pool).await, 1, "a repeat stacked");

    exec(&pool, "DELETE FROM pauses").await; // A lift, by whoever may.
    assert!(raise(&endpoint, role, "the-detection").await.is_err());
    assert_eq!(all_pauses(&pool).await, 0, "a repeat paused after the lift");

    // A new detection does pause, once lifted.
    let new = raise(&endpoint, role, "the-next-detection").await.unwrap();
    assert!(new > seq);
    assert!(pause_of(&pool, new).await.is_some());
    assert_eq!(all_pauses(&pool).await, 1);
}

/// `AC-I1.2`: a new detection while paused neither fails nor is lost. It has its own pause, so a
/// lift approved for the first alert does not lift what nobody has looked at yet.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_second_detection_while_paused_keeps_the_pause_past_the_first_lift(pool: PgPool) {
    declare(&pool).await;
    let role = bezent_store::EVM_ENDPOINT_ROLE;
    let endpoint = login(&pool, role).await;
    let first = raise(&endpoint, role, "first").await.unwrap();
    let second = raise(&endpoint, role, "second").await.unwrap();
    let first_pause = pause_of(&pool, first).await.expect("the first pause");
    assert!(
        pause_of(&pool, second).await.is_some(),
        "the second alert was lost"
    );
    exec(
        &pool,
        &format!("DELETE FROM pauses WHERE id = {first_pause}"),
    )
    .await;
    assert!(
        pause_of(&pool, second).await.is_some(),
        "lifting the first alert lifted the second"
    );
}

/// `AC-I1.2`: two detectors raising at once both commit, each with its pause (the log's head
/// serialises them; neither waits on the other's pause).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_concurrent_alerts_both_pause(pool: PgPool) {
    declare(&pool).await;
    let core = login(&pool, bezent_store::CORE_ROLE).await;
    let signer = login(&pool, bezent_store::SIGNER_ROLE).await;
    let (a, b) = tokio::join!(
        raise(&core, bezent_store::CORE_ROLE, "at-once"),
        raise(&signer, bezent_store::SIGNER_ROLE, "at-once"),
    );
    assert!(pause_of(&pool, a.unwrap()).await.is_some());
    assert!(pause_of(&pool, b.unwrap()).await.is_some());
}

/// `AC-I1.2`: a pause already in place, of a person's or of the reserve check's, does not stand in
/// for the alert's: whoever lifts it may never have seen this alert. Neither stops it nor fails it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_pause_already_in_place_does_not_stand_in_for_the_alert_s(pool: PgPool) {
    declare(&pool).await;
    exec(
        &pool,
        "INSERT INTO pauses (asset, network, direction) VALUES (NULL, NULL, NULL); \
         INSERT INTO pauses (asset) VALUES ('bUSDC');",
    )
    .await;
    let role = bezent_store::SIGNER_ROLE;
    let signer = login(&pool, role).await;
    let seq = raise(&signer, role, "with-pauses-in-place").await.unwrap();
    assert!(pause_of(&pool, seq).await.is_some());
    assert_eq!(all_pauses(&pool).await, 3);
    exec(&pool, "DELETE FROM pauses WHERE alert_seq IS NULL").await;
    assert!(pause_of(&pool, seq).await.is_some());
}

/// `INV-4`, `REQ-IC-2`: the core's supply mismatch names its asset and pauses that asset under
/// 0040, so it does not pause the bridge. No other detector can narrow its pause that way, even
/// by claiming to be the core: the store stamps the detector (0049).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_4_only_the_core_s_asset_alert_pauses_less_than_the_bridge(pool: PgPool) {
    declare(&pool).await;
    let alert = |dedupe: &str, network: &str| {
        format!(
            "INSERT INTO event_log (kind, payload) VALUES ('key-compromise', \
               '{{\"dedupe\": \"{dedupe}\", \"scope\": \"asset\", \"asset\": \"bUSDC\", \
                  \"network\": \"{network}\", \"detector\": \"core\"}}')"
        )
    };
    let core = login(&pool, bezent_store::CORE_ROLE).await;
    assert_eq!(
        attempt(&core, &alert("INV-4:mismatch", "kreivo")).await,
        None
    );
    assert_eq!(
        all_pauses(&pool).await,
        0,
        "an asset's alert paused the bridge"
    );

    for (role, network) in [
        (bezent_store::KREIVO_ENDPOINT_ROLE, "kreivo"),
        (bezent_store::EVM_ENDPOINT_ROLE, "polygon"),
        (bezent_store::SIGNER_ROLE, "kreivo"),
    ] {
        let before = all_pauses(&pool).await;
        let other = login(&pool, role).await;
        assert_eq!(attempt(&other, &alert(role, network)).await, None, "{role}");
        assert_eq!(
            all_pauses(&pool).await,
            before + 1,
            "{role} narrowed its pause"
        );
    }
}

/// `NFR-7`, issue #77 review: the owner restores and replays the log (the exemption of 0020, 0047,
/// 0049, 0050), and replaying an alert as the owner pauses nothing: it would pause the bridge again
/// for an alert whose pause was lifted, and collide with the pause row the same restore brings.
/// The `pg_dump` plain restore of `restore_drill` creates the trigger after the data, so it never
/// fires there; a restore that loads rows into a store that is already migrated does. An alert a
/// detector's login writes still pauses (every test above).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn nfr_7_an_alert_the_owner_replays_pauses_nothing_and_its_own_pause_row_still_loads(
    pool: PgPool,
) {
    declare(&pool).await;
    let seq = raise(&pool, "owner", "replayed").await.unwrap();
    assert_eq!(pause_of(&pool, seq).await, None, "a replayed alert paused");
    assert_eq!(all_pauses(&pool).await, 0);
    // The pause row the same restore brings loads, rather than colliding with one made on the way.
    exec(
        &pool,
        &format!("INSERT INTO pauses (cause, alert_seq) VALUES ('REQ-IC-4', {seq})"),
    )
    .await;
    assert!(pause_of(&pool, seq).await.is_some());
    // A detector's login, in the same store, still pauses.
    let signer = login(&pool, bezent_store::SIGNER_ROLE).await;
    let live = raise(&signer, bezent_store::SIGNER_ROLE, "live")
        .await
        .unwrap();
    assert!(pause_of(&pool, live).await.is_some());
}
