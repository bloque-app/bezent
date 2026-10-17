// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Issue #30 (migration 0034) and the review of PR #44: what an observation is, who says so, and
//! what a request may bind (`REQ-OB-6`, `REQ-RQ-3`, `REQ-RQ-6`, `REQ-IN-3`, `REQ-IN-6`,
//! `REQ-CV-1`, `REQ-CV-2`, `INV-2`, `INV-8`).
//!
//! Every observation carries the kind its endpoint wrote. A deposit binds only an `arrival` or a
//! `conversion_output` at a reserve; the refund of an unassociated arrival any arrival at a
//! reserve; nothing binds an observation without an explicit bindable kind (M1). The store holds
//! this on the binding, by insert or by update, and when the source becomes final (S2). Each
//! endpoint's login records only its own network's observations, of the kinds that network's role
//! produces. Role checks run under `SET LOCAL ROLE` inside a transaction that is rolled back, so no
//! pooled connection keeps a role (#27).

mod common;

use common::*;
use sqlx::PgPool;

const RESERVE: &str = "0xreserve";

async fn declarations(pool: &PgPool) {
    exec(
        pool,
        &format!(
            "INSERT INTO networks (network, role, address_format) VALUES \
               ('issuing', 'issuing', 'opaque'), ('origin-a', 'origin', 'opaque'), \
               ('origin-b', 'origin', 'opaque'); \
             INSERT INTO assets (asset, precision, assurance_class) VALUES \
               ('asset-a', 6, 'public'); \
             INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
               ('origin-a', 'token-a', 'asset-a', 6); \
             INSERT INTO reserves (asset, network, address) VALUES \
               ('asset-a', 'origin-a', '{RESERVE}'), ('asset-a', 'origin-b', '0xother-reserve')"
        ),
    )
    .await;
}

/// Observes, as the migration owner, an executed movement at (`origin-a`, `block`, log 0) of kind
/// `kind` (`None`: the endpoint wrote none).
async fn observe(
    pool: &PgPool,
    block: i64,
    sender: &str,
    recipient: &str,
    reference: Option<&str>,
    kind: Option<&str>,
) {
    sqlx::query(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, token, amount, sender, recipient, on_chain_reference, executed, \
             kind) \
         VALUES ('origin-a', $1, '\\xaa', '2026-10-13T00:00:00Z', 'log', 0, 'token-a', 1000, \
             $2, $3, decode($4, 'hex'), TRUE, coalesce($5, 'other'))",
    )
    .bind(block)
    .bind(sender)
    .bind(recipient)
    .bind(reference)
    .bind(kind)
    .execute(pool)
    .await
    .expect("observe");
}

/// An arrival of kind `kind` at the reserve, from a sender that is nobody declared.
async fn at_reserve(pool: &PgPool, block: i64, kind: Option<&str>) {
    observe(pool, block, "0xsender", RESERVE, None, kind).await;
}

fn insert(kind: &str, reference: &str, block: i64) -> String {
    format!(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index) \
         VALUES ('{reference}', 'req-{reference}', '{kind}', 'asset-a', 1000, 'issuing', 'dest-1', \
             'Received', 'origin-a', {block}, '\\xaa', 'log', 0)"
    )
}

async fn bound(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM transfers")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// M1: nothing without an explicit bindable kind binds, as a deposit or as a refund. An
/// observation that names no kind is `other`; the derived arrival test is gone.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m1_inv_2_nothing_without_an_explicit_bindable_kind_binds(pool: PgPool) {
    declarations(&pool).await;
    let never = [
        None,
        Some("other"),
        Some("payout_leg"),
        Some("outflow"),
        Some("conversion_input"),
        Some("self_transfer"),
        Some("transaction"),
    ];
    for (i, kind) in never.into_iter().enumerate() {
        let block = 10 + i64::try_from(i).unwrap();
        at_reserve(&pool, block, kind).await;
        assert_refused(
            &pool,
            &insert("deposit", &format!("d-{i}"), block),
            "REQ-RQ-3",
        )
        .await;
        assert_refused(
            &pool,
            &insert("refund", &format!("r-{i}"), block),
            "REQ-RQ-6",
        )
        .await;
    }
    // An arrival that carries an on-chain reference is no arrival a request names.
    observe(&pool, 30, "0xsender", RESERVE, Some("01"), Some("arrival")).await;
    assert_refused(&pool, &insert("deposit", "d-ref", 30), "REQ-RQ-3").await;
    assert_refused(&pool, &insert("refund", "r-ref", 30), "REQ-RQ-6").await;
    // Nor one to an address that is no reserve.
    observe(&pool, 31, "0xsender", "0xsomeone", None, Some("arrival")).await;
    assert_refused(&pool, &insert("deposit", "d-elsewhere", 31), "REQ-RQ-3").await;
    assert_refused(&pool, &insert("refund", "r-elsewhere", 31), "REQ-RQ-6").await;
    assert_eq!(bound(&pool).await, 0);

    // An explicit arrival binds.
    at_reserve(&pool, 40, Some("arrival")).await;
    exec(&pool, &insert("deposit", "d-ok", 40)).await;
    assert_eq!(bound(&pool).await, 1);
}

/// M2: a deposit binds an `arrival` or a `conversion_output` at the reserve (REQ-CV-2); a
/// convertible arrival is never minted against (REQ-CV-1), nor a token not admitted, a rebalance
/// leg or a venue delivery.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m2_req_cv_1_req_cv_2_a_deposit_binds_an_arrival_or_a_conversion_output(pool: PgPool) {
    declarations(&pool).await;
    at_reserve(&pool, 1, Some("arrival")).await;
    at_reserve(&pool, 2, Some("conversion_output")).await;
    exec(&pool, &insert("deposit", "d-arrival", 1)).await;
    exec(&pool, &insert("deposit", "d-conversion", 2)).await;

    at_reserve(&pool, 10, Some("convertible_arrival")).await;
    assert_refused(&pool, &insert("deposit", "d-convertible", 10), "REQ-CV-1").await;
    at_reserve(&pool, 11, Some("other_arrival")).await;
    assert_refused(&pool, &insert("deposit", "d-other", 11), "REQ-RQ-3").await;
    observe(&pool, 12, "0xother-reserve", RESERVE, None, Some("arrival")).await;
    assert_refused(&pool, &insert("deposit", "d-rebalance", 12), "REQ-RQ-3").await;
    exec(
        &pool,
        "INSERT INTO routes (asset, network, direction, minimum, maximum, window_limit, \
             window_seconds, capacity, venue_addresses, completion_budget_seconds) \
         VALUES ('asset-a', 'origin-a', 'deposit', 1, 100000, 100000, 86400, 10, \
             '[\"0xvenue\"]', 3600)",
    )
    .await;
    observe(&pool, 13, "0xvenue", RESERVE, None, Some("arrival")).await;
    assert_refused(&pool, &insert("deposit", "d-venue", 13), "REQ-RQ-3").await;
    assert_eq!(bound(&pool).await, 2);
}

/// S1: the refund of an unassociated arrival binds any arrival at a reserve (REQ-IN-6): a token
/// not admitted, a conversion's output, a venue delivery and a leg from another reserve, besides
/// an arrival. Wider than a deposit, but never a convertible arrival (M1).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s1_req_rq_6_req_in_6_a_refund_binds_any_unassociated_arrival(pool: PgPool) {
    declarations(&pool).await;
    exec(
        &pool,
        "INSERT INTO routes (asset, network, direction, minimum, maximum, window_limit, \
             window_seconds, capacity, venue_addresses, completion_budget_seconds) \
         VALUES ('asset-a', 'origin-a', 'deposit', 1, 100000, 100000, 86400, 10, \
             '[\"0xvenue\"]', 3600)",
    )
    .await;
    for (block, kind) in [
        (1, "arrival"),
        (2, "other_arrival"),
        (4, "conversion_output"),
    ] {
        at_reserve(&pool, block, Some(kind)).await;
        exec(&pool, &insert("refund", &format!("r-{kind}"), block)).await;
    }
    observe(&pool, 5, "0xvenue", RESERVE, None, Some("arrival")).await;
    exec(&pool, &insert("refund", "r-venue", 5)).await;
    observe(&pool, 6, "0xother-reserve", RESERVE, None, Some("arrival")).await;
    exec(&pool, &insert("refund", "r-rebalance", 6)).await;
    assert_eq!(bound(&pool).await, 5);
}

/// S2: the binding rule runs on a binding made by an update, and again when the source becomes
/// final; a source on a declared origin network never becomes final with no observation.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s2_inv_2_the_binding_rule_holds_on_update_and_at_source_final(pool: PgPool) {
    declarations(&pool).await;
    // A deposit with no source, bound later by an update to a payout leg: refused.
    observe(
        &pool,
        1,
        RESERVE,
        "0xsomeone",
        Some("01"),
        Some("payout_leg"),
    )
    .await;
    let id = transfer(&pool, "d-update").await;
    assert_refused(
        &pool,
        &format!(
            "UPDATE transfers SET source_network = 'origin-a', source_block_number = 1, \
               source_block_hash = '\\xaa', source_position_kind = 'log', \
               source_position_index = 0 WHERE id = {id}"
        ),
        "REQ-RQ-3",
    )
    .await;

    // A deposit bound ahead of its observation, which then turns out to be no arrival: it never
    // reaches Source-Final.
    exec(&pool, &insert("deposit", "d-ahead", 2)).await;
    at_reserve(&pool, 2, Some("convertible_arrival")).await;
    let ahead: i64 =
        sqlx::query_scalar("SELECT id FROM transfers WHERE bridge_reference = 'd-ahead'")
            .fetch_one(&pool)
            .await
            .unwrap();
    bare_transition(&pool, ahead, "Received", "Accepted")
        .await
        .expect("Received -> Accepted");
    let err = transition(&pool, ahead, "Accepted", "Source-Final")
        .await
        .expect_err("a convertible arrival never becomes a deposit's final source");
    assert_refusal(&err, "REQ-CV-1", "Source-Final on a convertible arrival");

    // A deposit bound to a coordinate nothing was observed at never becomes final either.
    exec(&pool, &insert("deposit", "d-unseen", 3)).await;
    let unseen: i64 =
        sqlx::query_scalar("SELECT id FROM transfers WHERE bridge_reference = 'd-unseen'")
            .fetch_one(&pool)
            .await
            .unwrap();
    bare_transition(&pool, unseen, "Received", "Accepted")
        .await
        .expect("Received -> Accepted");
    let err = transition(&pool, unseen, "Accepted", "Source-Final")
        .await
        .expect_err("no observation, no final source");
    assert_refusal(&err, "REQ-IN-3", "Source-Final with no observation");

    // An arrival bound by an update, then made final: accepted.
    at_reserve(&pool, 4, Some("arrival")).await;
    let ok = transfer(&pool, "d-ok").await;
    bare_transition(&pool, ok, "Received", "Accepted")
        .await
        .expect("Received -> Accepted");
    bind_source(&pool, ok, 4).await;
    transition(&pool, ok, "Accepted", "Source-Final")
        .await
        .expect("an arrival becomes the deposit's final source");
}

/// Runs `sql` as `role` in a transaction that is rolled back; returns the SQLSTATE of its error.
async fn as_role(pool: &PgPool, role: &str, sql: &str) -> Option<String> {
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(&format!("SET LOCAL ROLE {role}"))
        .execute(&mut *tx)
        .await
        .unwrap();
    let out = sqlx::raw_sql(sql).execute(&mut *tx).await;
    tx.rollback().await.unwrap();
    out.err().and_then(|e| {
        e.as_database_error()
            .and_then(|d| d.code().map(|c| c.into_owned()))
    })
}

fn observation_sql(network: &str, block: i64, kind: &str) -> String {
    format!(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
           position_index, token, amount, recipient, executed, kind) \
         VALUES ('{network}', {block}, '\\x07', '2026-10-13T00:00:00Z', 'log', 0, 'token-a', 1, \
           '{RESERVE}', TRUE, '{kind}')"
    )
}

const REFUSED: Option<&str> = Some(bezent_store::REFUSAL_SQLSTATE);

/// REQ-OB-6, INV-2, INV-8: each endpoint's login records only its own network's observations
/// and outcomes, and a network only the kinds its role produces. The endpoint role alone, with no
/// registered scope, records none; the Kreivo endpoint's role the issuing network's.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ob_6_inv_2_each_endpoint_records_only_its_own_networks_kinds(pool: PgPool) {
    declarations(&pool).await;
    let kreivo = bezent_store::KREIVO_ENDPOINT_ROLE;
    // The issuing network's kinds, by its endpoint.
    for kind in [
        "mint",
        "redemption",
        "refund_mint",
        "control",
        "transaction",
        "other",
    ] {
        assert_eq!(
            as_role(&pool, kreivo, &observation_sql("issuing", 1, kind)).await,
            None,
            "{kind}"
        );
    }
    // Never an origin network's kind on the issuing network, by anyone.
    for kind in ["arrival", "conversion_output", "payout_leg"] {
        assert_eq!(
            as_role(&pool, kreivo, &observation_sql("issuing", 1, kind))
                .await
                .as_deref(),
            REFUSED,
            "{kind}"
        );
        assert_refused(&pool, &observation_sql("issuing", 1, kind), "REQ-OB-6").await;
    }
    // Nor an issuing network's kind on an origin network.
    assert_refused(&pool, &observation_sql("origin-a", 1, "mint"), "REQ-OB-6").await;
    // Only `other` on a network nobody declared.
    assert_refused(
        &pool,
        &observation_sql("undeclared", 1, "arrival"),
        "REQ-OB-6",
    )
    .await;
    // The Kreivo endpoint records no origin network's observation, not even `other`; the endpoint
    // role alone records none at all.
    for (network, kind) in [
        ("origin-a", "arrival"),
        ("origin-a", "other"),
        ("undeclared", "other"),
    ] {
        assert_eq!(
            as_role(&pool, kreivo, &observation_sql(network, 2, kind))
                .await
                .as_deref(),
            REFUSED,
            "{network} {kind}"
        );
    }
    // The endpoint role alone is only a mark, with no privilege of its own (migration 0045).
    for (network, kind) in [("issuing", "mint"), ("origin-a", "arrival")] {
        assert_eq!(
            as_role(
                &pool,
                bezent_store::ENDPOINT_ROLE,
                &observation_sql(network, 3, kind)
            )
            .await
            .as_deref(),
            Some("42501"),
            "{network} {kind}"
        );
    }

    // Outcomes too: the Kreivo endpoint decides none of an origin network's observations.
    exec(&pool, &observation_sql("origin-a", 4, "arrival")).await;
    exec(&pool, &observation_sql("issuing", 4, "mint")).await;
    let outcome = |network: &str| {
        format!(
            "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
               final_block_hash) SELECT id, 'final', 9, '\\x09' FROM observations \
             WHERE network = '{network}'"
        )
    };
    assert_eq!(
        as_role(&pool, kreivo, &outcome("origin-a"))
            .await
            .as_deref(),
        REFUSED
    );
    assert_eq!(as_role(&pool, kreivo, &outcome("issuing")).await, None);
}

/// No function of 0034 is PUBLIC's; the services keep the ones their triggers run as them.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ar_3_no_function_of_0034_is_public(pool: PgPool) {
    let public: Vec<String> = sqlx::query_scalar(
        "SELECT p.oid::regprocedure::text FROM pg_proc p \
         WHERE p.proname IN ('observation_kind_on_network', 'observation_kind_for_event', \
             'observation_writer_covers', 'observations_guard_kind', \
             'observation_outcomes_guard_scope', 'source_bindable', 'transfers_source_bindable') \
           AND has_function_privilege('public', p.oid, 'EXECUTE')",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(public.is_empty(), "PUBLIC may run {public:?}");
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_proc WHERE proname IN ('observation_kind_on_network', \
           'observation_kind_for_event', 'observation_writer_covers', 'observations_guard_kind', \
           'observation_outcomes_guard_scope', 'source_bindable', 'transfers_source_bindable')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n, 7);
}

/// S2: the backfill trusts no `detail` on an origin network's row. Only the issuing network's
/// rows take the kind their `detail.event` names (when it produces that kind); every origin row,
/// and any row whose kind cannot be told, is `other`, which nothing binds.
#[sqlx::test(migrations = false)]
async fn s2_inv_2_the_backfill_trusts_no_event_on_origin_rows(pool: PgPool) {
    REVERSIBLE_MIGRATOR.run(&pool).await.expect("up");
    // Back to just before 0034, to record rows as the endpoints did before it.
    REVERSIBLE_MIGRATOR
        .undo(&pool, 33)
        .await
        .expect("down to 0033");
    exec(
        &pool,
        "INSERT INTO networks (network, role, address_format) VALUES \
           ('issuing', 'issuing', 'opaque'), ('origin-a', 'origin', 'opaque')",
    )
    .await;
    let rows = [
        ("origin-a", 1, "log", r#"{"event": "Arrival"}"#),
        ("origin-a", 2, "log", r#"{"event": "Conversion"}"#),
        ("origin-a", 3, "log", r#"{"event": "Payout"}"#),
        ("origin-a", 4, "log", "{}"),
        ("origin-a", 5, "log", r#"{"event": "Minted"}"#),
        ("origin-a", 6, "log", r#"{"event": "ZeroTransfer"}"#),
        ("issuing", 7, "log", r#"{"event": "Minted"}"#),
        ("issuing", 8, "log", r#"{"event": "BoundsSet"}"#),
        (
            "issuing",
            9,
            "transaction",
            r#"{"signed_transaction": "1"}"#,
        ),
        ("issuing", 10, "log", r#"{"event": "Arrival"}"#),
        ("undeclared", 11, "log", r#"{"event": "Arrival"}"#),
        (
            "origin-a",
            12,
            "transaction",
            r#"{"signed_transaction": "1"}"#,
        ),
    ];
    for (network, block, position, detail) in rows {
        exec(
            &pool,
            &format!(
                "INSERT INTO observations (network, block_number, block_hash, block_time, \
                   position_kind, position_index, executed, detail) \
                 VALUES ('{network}', {block}, '\\xaa', '2026-10-13T00:00:00Z', '{position}', 0, \
                   TRUE, '{detail}')"
            ),
        )
        .await;
    }
    REVERSIBLE_MIGRATOR.run(&pool).await.expect("0034 again");
    let kinds: Vec<(i64, String)> =
        sqlx::query_as("SELECT block_number, kind::text FROM observations ORDER BY block_number")
            .fetch_all(&pool)
            .await
            .unwrap();
    let expected = [
        (1, "other"),
        (2, "other"),
        (3, "other"),
        (4, "other"),
        (5, "other"),
        (6, "other"),
        (7, "mint"),
        (8, "control"),
        (9, "transaction"),
        (10, "other"),
        (11, "other"),
        (12, "other"),
    ];
    let got: Vec<(i64, &str)> = kinds.iter().map(|(b, k)| (*b, k.as_str())).collect();
    assert_eq!(got, expected);
    // Observations stay append-only after the backfill.
    assert_refused(&pool, "UPDATE observations SET kind = 'arrival'", "INV-10").await;
}

/// M1: a refund never binds a `convertible_arrival` (INV-2, REQ-CV-1): it is held until it is
/// converted, and its value is released only through its conversion, never refunded in its own
/// right as well. A deposit does not bind it either.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m1_inv_2_req_cv_1_a_refund_never_binds_a_convertible_arrival(pool: PgPool) {
    declarations(&pool).await;
    at_reserve(&pool, 1, Some("convertible_arrival")).await;
    assert_refused(&pool, &insert("refund", "r-convertible", 1), "REQ-RQ-6").await;
    assert_refused(&pool, &insert("deposit", "d-convertible", 1), "REQ-CV-1").await;
    assert_eq!(bound(&pool).await, 0);
    // Its conversion's output is what a refund (or a deposit) names.
    at_reserve(&pool, 2, Some("conversion_output")).await;
    exec(&pool, &insert("refund", "r-output", 2)).await;
    assert_eq!(bound(&pool).await, 1);
}

/// S1: the venue and reserve sender exclusions apply to an `arrival` only: a conversion's output
/// is sent by the venue by design, and a pool declared in `venue_addresses` must not block its
/// deposit (REQ-CV-2); an arrival from that address is still a venue delivery.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s1_req_cv_2_a_conversion_output_from_a_venue_address_still_deposits(pool: PgPool) {
    declarations(&pool).await;
    exec(
        &pool,
        "INSERT INTO routes (asset, network, direction, minimum, maximum, window_limit, \
             window_seconds, capacity, venue_addresses, completion_budget_seconds) \
         VALUES ('asset-a', 'origin-a', 'deposit', 1, 100000, 100000, 86400, 10, \
             '[\"0xpool\"]', 3600)",
    )
    .await;
    observe(&pool, 1, "0xpool", RESERVE, None, Some("conversion_output")).await;
    exec(&pool, &insert("deposit", "d-output", 1)).await;
    observe(&pool, 2, "0xpool", RESERVE, None, Some("arrival")).await;
    assert_refused(&pool, &insert("deposit", "d-delivery", 2), "REQ-RQ-3").await;
    assert_eq!(bound(&pool).await, 1);
}

/// S3: `kind` agrees with `detail.event` wherever both are present, or the observation is
/// refused (INV-2): an event the book counts as an arrival cannot be recorded unbindable, nor an
/// unbooked one bindable. With no event, any kind of the network's role stands.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s3_inv_2_kind_agrees_with_detail_event_where_both_are_present(pool: PgPool) {
    declarations(&pool).await;
    let with = |block: i64, kind: &str, detail: &str| {
        format!(
            "INSERT INTO observations (network, block_number, block_hash, block_time, \
               position_kind, position_index, executed, kind, detail) \
             VALUES ('origin-a', {block}, '\\x07', '2026-10-13T00:00:00Z', 'log', 0, TRUE, \
               '{kind}', '{detail}')"
        )
    };
    assert_refused(
        &pool,
        &with(1, "other", r#"{"event": "Arrival"}"#),
        "REQ-OB-6",
    )
    .await;
    assert_refused(
        &pool,
        &with(2, "arrival", r#"{"event": "Conversion"}"#),
        "REQ-OB-6",
    )
    .await;
    assert_refused(
        &pool,
        &with(3, "arrival", r#"{"event": "Unknown"}"#),
        "REQ-OB-6",
    )
    .await;
    exec(&pool, &with(4, "arrival", r#"{"event": "Arrival"}"#)).await;
    exec(
        &pool,
        &with(5, "conversion_output", r#"{"event": "Conversion"}"#),
    )
    .await;
    exec(&pool, &with(6, "other", r#"{"event": "ZeroTransfer"}"#)).await;
    exec(&pool, &with(7, "arrival", "{}")).await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM observations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 4);
}
