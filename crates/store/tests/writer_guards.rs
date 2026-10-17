// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The review of PR #56 (migration 0046): who may record what a transfer's state and the log rest
//! on, checked through real service logins (a member of one role each, as the services log in).
//!
//! - M1 (`INV-6`, `REQ-RQ-7`; guardrails 2 and 3): the ingress records only a request's decision
//!   out of `Received`; every transition that rests on chain data is the core's; and whoever
//!   writes it, a transfer reaches `Source-Final` only on a final, unwithdrawn, unreorged
//!   observation of its source.
//! - S2 (`AC-I1.2`, `REQ-ST-4`): each kind of log entry is written only by the role whose events
//!   it records.
//! - M1b (migration 0047): who creates a transfer, and which.
//! - M1c, S7, S8 (migration 0048): a deposit's amount and asset are its bound observation's
//!   (`REQ-IN-4`, `REQ-CV-2`, `REQ-RQ-7`); the core refunds a withdrawal only after an ordered
//!   attempt proven not executed (`INV-8`, `REQ-RF-1`); no login chooses a transfer's id.

mod common;

use bezent_store::login::{as_login, ensure_login, pool_options};
use common::*;
use sqlx::PgPool;

const PASSWORD: &str = "test-only-writer-guards-password";
const RESERVE: &str = "0xreserve";
const INSUFFICIENT_PRIVILEGE: &str = "42501";

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

async fn declarations(pool: &PgPool) {
    exec(
        pool,
        &format!(
            "INSERT INTO networks (network, role, address_format) VALUES \
               ('issuing', 'issuing', 'opaque'), ('origin-a', 'origin', 'opaque'); \
             INSERT INTO assets (asset, precision, assurance_class) VALUES \
               ('asset-a', 6, 'public'); \
             INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
               ('origin-a', 'token-a', 'asset-a', 6); \
             INSERT INTO reserves (asset, network, address) VALUES \
               ('asset-a', 'origin-a', '{RESERVE}')"
        ),
    )
    .await;
}

/// The endpoint's record, as the migration owner: an arrival at the reserve at (`origin-a`,
/// `block`, `\xaa`, log 0), not yet final.
async fn arrival(pool: &PgPool, block: i64) {
    exec(
        pool,
        &format!(
            "INSERT INTO observations (network, block_number, block_hash, block_time, \
                 position_kind, position_index, token, amount, sender, recipient, executed, kind) \
             VALUES ('origin-a', {block}, '\\xaa', '{BLOCK_TIME}', 'log', 0, 'token-a', 1000, \
                 '0xsender', '{RESERVE}', TRUE, 'arrival')"
        ),
    )
    .await;
}

/// The endpoint's outcome for the arrival at `block`, as the migration owner: `outcome`, decided
/// at (`final_block`, `\xbb`).
async fn outcome(pool: &PgPool, block: i64, outcome: &str, final_block: i64) {
    exec(
        pool,
        &format!(
            "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
                 final_block_hash) \
             SELECT id, '{outcome}', {final_block}, '\\xbb' FROM observations \
             WHERE network = 'origin-a' AND block_number = {block}"
        ),
    )
    .await;
}

/// A final head the endpoint read, as the migration owner.
async fn final_head(pool: &PgPool, block: i64, hash: &str) {
    exec(
        pool,
        &format!(
            "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
             VALUES ('origin-a', {block}, '\\x{hash}', '{BLOCK_TIME}')"
        ),
    )
    .await;
}

/// A deposit bound to the arrival at `block`, in `Received`, inserted by `pool`.
fn bound(reference: &str, block: i64) -> String {
    format!(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index) \
         VALUES ('{reference}', 'req-{reference}', 'deposit', 'asset-a', 1000, 'issuing', \
             'dest-1', 'Received', 'origin-a', {block}, '\\xaa', 'log', 0)"
    )
}

/// A deposit bound to the arrival at `block` and inserted at `Source-Final` (REQ-IN-3).
fn at_source_final(reference: &str, block: i64) -> String {
    format!(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index, \
             source_final_network, source_final_block_number, source_final_block_hash, \
             source_final_block_time, source_final_seen_at) \
         VALUES ('{reference}', 'req-{reference}', 'deposit', 'asset-a', 1000, 'issuing', \
             'dest-1', 'Source-Final', 'origin-a', {block}, '\\xaa', 'log', 0, \
             'origin-a', {block} + 20, '\\xbb', '{BLOCK_TIME}', now())"
    )
}

/// A transition of the transfer `reference` from `from` to `to`, resting on (`network`, 30,
/// `\xbb`) when `network` is given.
fn edge(reference: &str, from: &str, to: &str, network: Option<&str>) -> String {
    let coordinate = match network {
        Some(n) => format!("'{n}', 30, '\\xbb', '{BLOCK_TIME}'::timestamptz"),
        None => "NULL, NULL, NULL, NULL".to_owned(),
    };
    format!(
        "INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
             block_hash, block_time) \
         SELECT id, '{from}', '{to}', {coordinate} FROM transfers \
         WHERE bridge_reference = '{reference}'"
    )
}

async fn run(pool: &PgPool, sql: &str) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(sql).execute(pool).await.map(|_| ())
}

async fn state(pool: &PgPool, reference: &str) -> String {
    text(
        pool,
        &format!("SELECT state::text FROM transfers WHERE bridge_reference = '{reference}'"),
    )
    .await
}

/// M1, the reviewer's scenario: a compromised ingress moves a bound deposit from `Accepted` to
/// `Source-Final` before its arrival is final (or after it was reorged out), and the core mints
/// from it. The ingress may only take in the request and decide it; it cannot mark the arrival
/// final, nor move the transfer past `Accepted`, nor append any other edge.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m1_an_ingress_login_cannot_mark_an_arrival_final_nor_move_a_transfer_past_accepted(
    pool: PgPool,
) {
    declarations(&pool).await;
    arrival(&pool, 1).await;
    let ingress = login(&pool, "wg_m1_ingress_for_tests", bezent_store::INGRESS_ROLE).await;
    let core = login(&pool, "wg_m1_core_for_tests", bezent_store::CORE_ROLE).await;

    // What a request causes: its transfer, bound to the arrival, and the decision out of Received.
    run(&ingress, &bound("bz-d-m1-a", 1))
        .await
        .expect("the ingress takes in the request");
    run(&ingress, &edge("bz-d-m1-a", "Received", "Accepted", None))
        .await
        .expect("the ingress decides it");
    assert_eq!(state(&pool, "bz-d-m1-a").await, "Accepted");

    // It cannot say the arrival is final: no outcome, no final head (REQ-RQ-7).
    for sql in [
        "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
             final_block_hash) SELECT id, 'final', 21, '\\xbb' FROM observations"
            .to_owned(),
        format!(
            "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
             VALUES ('origin-a', 21, '\\xbb', '{BLOCK_TIME}')"
        ),
    ] {
        let err = run(&ingress, &sql).await.expect_err(&sql);
        assert_error_sqlstate(&err, INSUFFICIENT_PRIVILEGE, &sql);
    }

    // It cannot move the transfer past Accepted, nor out of it any other way: not before the
    // arrival is final, and not after.
    for final_yet in [false, true] {
        if final_yet {
            outcome(&pool, 1, "final", 21).await;
            final_head(&pool, 21, "bb").await;
        }
        for (to, network) in [
            ("Source-Final", Some("origin-a")),
            ("Withdrawn", Some("origin-a")),
            ("Expired", Some("origin-a")),
            ("Needs-Review", Some("origin-a")),
            ("Accepted", None),
        ] {
            let sql = edge("bz-d-m1-a", "Accepted", to, network);
            let err = run(&ingress, &sql)
                .await
                .expect_err(&format!("the ingress ran Accepted -> {to}"));
            assert_refusal(&err, "INV-6", &format!("ingress Accepted -> {to}"));
            assert_refusal(&err, "REQ-RQ-7", &format!("ingress Accepted -> {to}"));
        }
    }
    assert_eq!(state(&pool, "bz-d-m1-a").await, "Accepted");

    // Nor the decision of a transfer no request makes (a withdrawal, made from a redemption).
    exec(
        &pool,
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state) \
         VALUES ('m1-w', 'req-m1-w', 'withdrawal', 'asset-a', 1000, 'origin-a', '0xpayee', \
             'Received')",
    )
    .await;
    let err = run(&ingress, &edge("m1-w", "Received", "Accepted", None))
        .await
        .expect_err("the ingress decided a withdrawal");
    assert_refusal(&err, "REQ-RQ-7", "ingress decides a withdrawal");

    // The core moves it once the arrival is final; the ingress then appends nothing after it.
    run(
        &core,
        &edge("bz-d-m1-a", "Accepted", "Source-Final", Some("origin-a")),
    )
    .await
    .expect("the core records Source-Final on the final arrival");
    assert_eq!(state(&pool, "bz-d-m1-a").await, "Source-Final");
    for (to, network) in [
        ("Source-Final", None),
        ("Needs-Review", Some("issuing")),
        ("Delivering", None),
    ] {
        let err = run(&ingress, &edge("bz-d-m1-a", "Source-Final", to, network))
            .await
            .expect_err(&format!("the ingress ran Source-Final -> {to}"));
        assert_refusal(&err, "INV-6", &format!("ingress Source-Final -> {to}"));
    }

    // An insert at Source-Final by the ingress (the table admits a bound deposit there,
    // REQ-IN-3) only on a final arrival too: here, one observed and not final.
    arrival(&pool, 2).await;
    let err = run(&ingress, &at_source_final("bz-d-m1-c", 2))
        .await
        .expect_err("the ingress inserted a deposit at Source-Final on a non-final arrival");
    assert_refusal(&err, "INV-6", "ingress insert at Source-Final");

    ingress.close().await;
    core.close().await;
}

/// M1, whoever writes it: the core (and the migration owner too) cannot write `Source-Final` for an
/// arrival that is undecided, withdrawn, decided at a point that is no recorded final head, or
/// reorged out (the final block at its height has another hash).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m1_the_core_cannot_write_source_final_for_a_non_final_or_reorged_arrival(pool: PgPool) {
    declarations(&pool).await;
    let core = login(&pool, "wg_m1_core2_for_tests", bezent_store::CORE_ROLE).await;
    for block in 1..=6 {
        arrival(&pool, block).await;
        let reference = format!("m1-{block}");
        exec(&pool, &bound(&reference, block)).await;
        exec(&pool, &edge(&reference, "Received", "Accepted", None)).await;
    }
    // 1: observed, undecided.
    // 2: withdrawn before finality (REQ-IN-7).
    outcome(&pool, 2, "withdrawn", 22).await;
    final_head(&pool, 22, "bb").await;
    // 3: decided final at a point no final head was recorded at.
    outcome(&pool, 3, "final", 23).await;
    // 4: decided final at a recorded head, but the final block at its height has another hash.
    outcome(&pool, 4, "final", 24).await;
    final_head(&pool, 24, "bb").await;
    final_head(&pool, 4, "cc").await;
    // 5: final at a recorded head whose hash is not the one the outcome names.
    outcome(&pool, 5, "final", 25).await;
    final_head(&pool, 25, "cc").await;
    for (block, why) in [
        (1, "undecided"),
        (2, "withdrawn"),
        (3, "final head"),
        (4, "another hash"),
        (5, "final head"),
    ] {
        let sql = edge(
            &format!("m1-{block}"),
            "Accepted",
            "Source-Final",
            Some("origin-a"),
        );
        for (who, pool) in [("the core", &core), ("the owner", &pool)] {
            let err = run(pool, &sql)
                .await
                .expect_err(&format!("{who} wrote Source-Final on arrival {block}"));
            assert_refusal(&err, "INV-6", &format!("{who}, arrival {block}"));
            assert!(
                err.as_database_error().unwrap().message().contains(why),
                "{who}, arrival {block}: {err}"
            );
        }
        let err = run(&pool, &at_source_final(&format!("m1-ins-{block}"), block))
            .await
            .expect_err(&format!("an insert at Source-Final on arrival {block}"));
        assert_refusal(
            &err,
            "INV-6",
            &format!("insert at Source-Final, arrival {block}"),
        );
    }

    // 6: final at a recorded head, the block at its height unchanged: the core records it.
    outcome(&pool, 6, "final", 26).await;
    final_head(&pool, 26, "bb").await;
    final_head(&pool, 6, "aa").await;
    run(
        &core,
        &edge("m1-6", "Accepted", "Source-Final", Some("origin-a")),
    )
    .await
    .expect("the core records Source-Final on a final arrival");
    assert_eq!(state(&pool, "m1-6").await, "Source-Final");
    core.close().await;
}

/// S2, the reviewer's scenario: the ingress appends `condition-cleared` for `CND-ReserveShort`,
/// which the webhook dispatcher fans out as "released"; or a `transition` entry. Each kind is
/// written only by the role whose events it records.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s2_log_entries_are_written_only_by_the_roles_whose_events_they_record(pool: PgPool) {
    let ingress = login(&pool, "wg_s2_ingress_for_tests", bezent_store::INGRESS_ROLE).await;
    let core = login(&pool, "wg_s2_core_for_tests", bezent_store::CORE_ROLE).await;
    let endpoint = login(
        &pool,
        "wg_s2_endpoint_for_tests",
        bezent_store::KREIVO_ENDPOINT_ROLE,
    )
    .await;
    let webhooks = login(
        &pool,
        "wg_s2_webhooks_for_tests",
        bezent_store::WEBHOOKS_ROLE,
    )
    .await;
    // A key-compromise names its detection and, from an endpoint, a network it covers (0049).
    exec(
        &pool,
        "INSERT INTO networks (network, role, address_format) VALUES ('issuing', 'issuing', 'opaque') \
         ON CONFLICT DO NOTHING",
    )
    .await;
    let entry = |kind: &str| {
        format!(
            "INSERT INTO event_log (kind, payload) VALUES ('{kind}', \
             '{{\"condition\":\"CND-ReserveShort\",\"asset\":\"asset-a\", \
               \"dedupe\":\"wg-s2\",\"network\":\"issuing\"}}')"
        )
    };
    let cases: [(&str, &PgPool, &[&str], &[&str]); 3] = [
        (
            "ingress",
            &ingress,
            &["refusal"],
            &[
                "condition-cleared",
                "condition-raised",
                "transition",
                "transfer",
                "review-evidence",
                "payout-window-alert",
                "key-compromise",
                "delivery-anomaly",
            ],
        ),
        (
            "core",
            &core,
            &[
                "condition-cleared",
                "condition-raised",
                "review-evidence",
                "payout-window-alert",
                "key-compromise",
            ],
            &["transition", "transfer", "refusal"],
        ),
        (
            "endpoint",
            &endpoint,
            &["delivery-anomaly", "key-compromise", "pause-requested"],
            &[
                "condition-cleared",
                "condition-raised",
                "transition",
                "transfer",
                "refusal",
            ],
        ),
    ];
    for (who, pool, allowed, refused) in cases {
        for kind in allowed {
            run(pool, &entry(kind))
                .await
                .unwrap_or_else(|e| panic!("{who} could not record {kind}: {e}"));
        }
        for kind in refused {
            let err = run(pool, &entry(kind))
                .await
                .expect_err(&format!("{who} recorded {kind}"));
            let db = err.as_database_error().unwrap();
            assert_eq!(
                db.code().as_deref(),
                Some(bezent_store::REFUSAL_SQLSTATE),
                "{who} {kind}: {}",
                db.message()
            );
        }
    }
    // The dispatcher appends nothing at all.
    let err = run(&webhooks, &entry("condition-cleared"))
        .await
        .expect_err("the dispatcher recorded an entry");
    assert_error_sqlstate(&err, INSUFFICIENT_PRIVILEGE, "dispatcher");
    for p in [ingress, core, endpoint, webhooks] {
        p.close().await;
    }
}

// ---------------------------------------------------------------------------------------------
// M1b (migration 0047): who creates a transfer, and which.

/// An insert into `transfers`: a deposit in `Received` of 1000 `asset-a` to (`issuing`, `dest-1`),
/// with `fields` (column, SQL value) added or replacing those.
fn insert(fields: &[(&str, &str)]) -> String {
    let mut cols: Vec<(String, String)> = [
        ("request_reference", "'req-m1b'"),
        ("kind", "'deposit'"),
        ("asset", "'asset-a'"),
        ("amount", "1000"),
        ("destination_network", "'issuing'"),
        ("destination", "'dest-1'"),
        ("state", "'Received'"),
    ]
    .iter()
    .map(|(c, v)| ((*c).to_owned(), (*v).to_owned()))
    .collect();
    for (c, v) in fields {
        match cols.iter_mut().find(|(k, _)| k == c) {
            Some(x) => x.1 = (*v).to_owned(),
            None => cols.push(((*c).to_owned(), (*v).to_owned())),
        }
    }
    let (names, values): (Vec<String>, Vec<String>) = cols.into_iter().unzip();
    format!(
        "INSERT INTO transfers ({}) VALUES ({})",
        names.join(", "),
        values.join(", ")
    )
}

/// The source binding of the coordinate (`network`, `block`, `hash`, log 0), as insert fields.
fn source<'a>(network: &'a str, block: &'a str, hash: &'a str) -> [(&'static str, &'a str); 5] {
    [
        ("source_network", network),
        ("source_block_number", block),
        ("source_block_hash", hash),
        ("source_position_kind", "'log'"),
        ("source_position_index", "0"),
    ]
}

/// Declares the issuing network (the core's withdrawals are made from redemptions observed there),
/// and, through [`finalise_coordinate`], the origin network `origin-a` with a reserve and a final
/// arrival at each of `blocks`.
async fn networks(pool: &PgPool, blocks: &[i64]) {
    exec(
        pool,
        "INSERT INTO networks (network, role, address_format) \
         VALUES ('issuing', 'issuing', 'opaque') ON CONFLICT DO NOTHING",
    )
    .await;
    for block in blocks {
        finalise_coordinate(pool, "origin-a", *block, "aa", "log", 0, "asset-a", "1000").await;
    }
}

/// A withdrawal as the core makes it from a redemption (redeemer `redeemer-1`, on-chain reference
/// `\x77`; inserted by the owner here), taken to `Delivering` with one signed attempt that is not
/// proven not executed: its payout may still execute. Its source is a final coordinate at
/// (`origin-a`, `block`), the stand-in the shared helpers use for every source.
async fn withdrawal_in_flight(pool: &PgPool, block: i64) -> i64 {
    networks(pool, &[block]).await;
    let id: i64 = sqlx::query_scalar(&format!(
        "{} RETURNING id",
        insert(&[
            ("bridge_reference", "'bz-w-m1b'"),
            ("request_reference", "'redemption-m1b'"),
            ("kind", "'withdrawal'"),
            ("destination", "'0xpayee'"),
            ("redeemer", "'redeemer-1'"),
            ("on_chain_reference", "'\\x77'"),
        ])
    ))
    .fetch_one(pool)
    .await
    .expect("the withdrawal");
    to_source_final(pool, id, block).await;
    walk(pool, id, &["Source-Final", "Delivering"]).await;
    id
}

/// M1b, the reviewer's scenario: a compromised ingress inserts, at `Received`, the refund of a
/// withdrawal whose payout is being delivered, naming its source, its redeemer and its on-chain
/// reference. The core would accept it at once and order a refund mint beside the payout, and the
/// row would close the withdrawal's attempt set. A refund of a withdrawal is the core's alone
/// (REQ-RF-3, REQ-RF-4), and even the core makes it only once every attempt is proven not executed
/// at finality (INV-8, REQ-RF-1).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m1b_an_ingress_login_cannot_forge_the_refund_of_an_in_flight_withdrawal(pool: PgPool) {
    let w = withdrawal_in_flight(&pool, 1).await;
    let ingress = login(
        &pool,
        "wg_m1b_ingress_for_tests",
        bezent_store::INGRESS_ROLE,
    )
    .await;
    let core = login(&pool, "wg_m1b_core_for_tests", bezent_store::CORE_ROLE).await;
    let original = w.to_string();
    let refund = |reference: &'static str, whole: bool| {
        let mut fields = vec![
            ("bridge_reference", reference),
            ("kind", "'refund'"),
            ("destination", "'redeemer-1'"),
            ("refund_of", original.as_str()),
        ];
        if whole {
            fields.extend([
                ("redeemer", "'redeemer-1'"),
                ("on_chain_reference", "'\\x77'"),
            ]);
            fields.extend(source("'origin-a'", "1", "'\\xaa'"));
        }
        insert(&fields)
    };

    for (sql, what) in [
        (refund("'bz-u-forged'", true), "in the request namespace"),
        (refund("'bz-f-forged'", true), "in the core's namespace"),
        (refund("'bz-u-bare'", false), "naming only the withdrawal"),
    ] {
        let err = run(&ingress, &sql)
            .await
            .expect_err(&format!("the ingress forged a refund {what}"));
        assert_refusal(&err, "REQ-RF-3", what);
        assert_refusal(&err, "INV-8", what);
    }
    // Nothing was made: the withdrawal is still delivering, its attempt set open.
    assert_eq!(state(&pool, "bz-w-m1b").await, "Delivering");
    assert!(
        !flag(
            &pool,
            &format!("SELECT EXISTS (SELECT 1 FROM transfers WHERE refund_of = {w})")
        )
        .await
    );

    // The core may not make it either while the signed attempt is not proven not executed.
    let err = run(&core, &refund("'bz-f-m1b'", true))
        .await
        .expect_err("the core made a refund before the proof");
    assert_refusal(&err, "INV-8", "core, attempt unproven");
    // Once it is, the core makes it (REQ-RF-1, REQ-RF-3, REQ-RF-4).
    prove_not_executed(&pool, w).await;
    run(&core, &refund("'bz-f-m1b'", true))
        .await
        .expect("the core makes the refund once every attempt is proven not executed");
    assert_eq!(state(&pool, "bz-f-m1b").await, "Received");

    ingress.close().await;
    core.close().await;
}

/// M1b: the ingress never inserts a withdrawal (the core makes them from the redemptions it
/// observes, REQ-RQ-4), nor anything at `Source-Final` (it rests on chain data, INV-6), nor a row
/// carrying a key that a redemption or the core supplies. What a request makes, it still inserts.
/// The core makes no deposit.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m1b_an_ingress_login_cannot_insert_a_withdrawal_nor_a_transfer_at_source_final(
    pool: PgPool,
) {
    networks(&pool, &[1, 2, 3]).await;
    let ingress = login(
        &pool,
        "wg_m1b2_ingress_for_tests",
        bezent_store::INGRESS_ROLE,
    )
    .await;
    let core = login(&pool, "wg_m1b2_core_for_tests", bezent_store::CORE_ROLE).await;
    let redemption = source("'issuing'", "5", "'\\xcc'");
    let withdrawal = |reference: &'static str| {
        let mut fields = vec![
            ("bridge_reference", reference),
            ("kind", "'withdrawal'"),
            ("destination_network", "'origin-a'"),
            ("destination", "'0xpayee'"),
            ("redeemer", "'redeemer-1'"),
            ("on_chain_reference", "'\\x55'"),
        ];
        fields.extend(redemption);
        insert(&fields)
    };

    for (sql, what) in [
        (withdrawal("'bz-w-m1b2'"), "a withdrawal"),
        (
            withdrawal("'bz-d-m1b2'"),
            "a withdrawal in the request namespace",
        ),
        (
            insert(&[
                ("bridge_reference", "'bz-d-bare-w'"),
                ("kind", "'withdrawal'"),
            ]),
            "a bare withdrawal",
        ),
    ] {
        let err = run(&ingress, &sql)
            .await
            .expect_err(&format!("the ingress inserted {what}"));
        assert_refusal(&err, "REQ-RQ-7", what);
    }

    // A deposit at Source-Final, even on an arrival that is final: the store admits that row
    // (REQ-IN-3), but never from the ingress.
    let err = run(&ingress, &at_source_final("bz-d-m1b-sf", 1))
        .await
        .expect_err("the ingress inserted a deposit at Source-Final");
    assert_refusal(
        &err,
        "INV-6",
        "ingress insert at Source-Final on a final arrival",
    );
    run(&pool, &at_source_final("bz-d-m1b-sf", 1))
        .await
        .expect("the arrival is final: the same row from the owner is admitted");

    // Keys a redemption or the core supplies.
    for (fields, what) in [
        (
            vec![
                ("bridge_reference", "'bz-d-ocr'"),
                ("on_chain_reference", "'\\x55'"),
            ],
            "an on-chain reference",
        ),
        (
            vec![
                ("bridge_reference", "'bz-d-red'"),
                ("redeemer", "'redeemer-1'"),
            ],
            "a redeemer",
        ),
        (
            vec![
                ("bridge_reference", "'bz-r-dref'"),
                (
                    "destination_reference",
                    "'\\x0101010101010101010101010101010101010101010101010101010101010101'",
                ),
            ],
            "a destination reference",
        ),
    ] {
        let err = run(&ingress, &insert(&fields))
            .await
            .expect_err(&format!("the ingress inserted a deposit with {what}"));
        assert_refusal(&err, "REQ-RQ-7", what);
    }

    // What a request makes: a bound deposit, a refusal, the refund of an unassociated arrival.
    let mut deposit = vec![("bridge_reference", "'bz-d-m1b-ok'")];
    deposit.extend(source("'origin-a'", "2", "'\\xaa'"));
    // Paid out on the arrival's network (0049).
    let mut unassociated = vec![
        ("bridge_reference", "'bz-u-m1b-ok'"),
        ("kind", "'refund'"),
        ("destination_network", "'origin-a'"),
    ];
    unassociated.extend(source("'origin-a'", "3", "'\\xaa'"));
    for (sql, what) in [
        (insert(&deposit), "a bound deposit"),
        (
            insert(&[("bridge_reference", "'bz-r-m1b-ok'")]),
            "a refusal",
        ),
        (insert(&unassociated), "an unassociated arrival's refund"),
    ] {
        run(&ingress, &sql)
            .await
            .unwrap_or_else(|e| panic!("the ingress could not insert {what}: {e}"));
    }

    // The core makes no request's transfer.
    let err = run(&core, &insert(&[("bridge_reference", "'bz-d-m1b-core'")]))
        .await
        .expect_err("the core inserted a deposit");
    assert_refusal(&err, "REQ-RQ-4", "core deposit");

    ingress.close().await;
    core.close().await;
}

/// M1b: the keys the core's withdrawal is created under (its bridge reference, its source binding,
/// its on-chain reference, its destination reference) belong to the core's creation path. A row
/// the ingress inserts first cannot hold one, so the core's `INSERT … ON CONFLICT DO NOTHING` of
/// the real withdrawal still makes it, and the redemption is paid.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m1b_a_row_inserted_first_cannot_block_the_cores_withdrawal(pool: PgPool) {
    networks(&pool, &[1]).await;
    let ingress = login(
        &pool,
        "wg_m1b3_ingress_for_tests",
        bezent_store::INGRESS_ROLE,
    )
    .await;
    let core = login(&pool, "wg_m1b3_core_for_tests", bezent_store::CORE_ROLE).await;
    let payout = "'\\x0202020202020202020202020202020202020202020202020202020202020202'";

    let mut squat_source = vec![("bridge_reference", "'bz-d-squat-source'")];
    squat_source.extend(source("'issuing'", "5", "'\\xcc'"));
    for (fields, what) in [
        (
            vec![("bridge_reference", "'bz-w-m1b-real'")],
            "its bridge reference",
        ),
        (squat_source, "its source coordinate"),
        (
            vec![
                ("bridge_reference", "'bz-d-squat-ocr'"),
                ("on_chain_reference", "'\\x55'"),
            ],
            "its on-chain reference",
        ),
        (
            vec![
                ("bridge_reference", "'bz-r-squat-dref'"),
                ("destination_reference", payout),
            ],
            "its destination reference",
        ),
    ] {
        let err = run(&ingress, &insert(&fields))
            .await
            .expect_err(&format!("the ingress took {what} first"));
        assert_refusal(&err, "REQ-RQ-7", what);
    }

    let made: Option<i64> = sqlx::query_scalar(&format!(
        "{} ON CONFLICT DO NOTHING RETURNING id",
        insert(&[
            ("bridge_reference", "'bz-w-m1b-real'"),
            ("request_reference", "'redemption-m1b-real'"),
            ("kind", "'withdrawal'"),
            ("destination_network", "'origin-a'"),
            ("destination", "'0xpayee'"),
            ("redeemer", "'redeemer-1'"),
            ("on_chain_reference", "'\\x55'"),
            ("destination_reference", payout),
            ("source_network", "'issuing'"),
            ("source_block_number", "5"),
            ("source_block_hash", "'\\xcc'"),
            ("source_position_kind", "'log'"),
            ("source_position_index", "0"),
        ])
    ))
    .fetch_optional(&core)
    .await
    .expect("the core's withdrawal insert");
    assert!(made.is_some(), "the core's withdrawal was not made");
    assert_eq!(
        text(
            &pool,
            "SELECT kind FROM transfers WHERE bridge_reference = 'bz-w-m1b-real'"
        )
        .await,
        "withdrawal"
    );

    ingress.close().await;
    core.close().await;
}

// ---------------------------------------------------------------------------------------------
// M1c, S7, S8 (migration 0048): what a transfer's amount rests on, when the core may refund a
// withdrawal, and who draws a transfer's id.

/// A deposit of `amount` `asset` bound to the coordinate (`origin-a`, `block`, `\xaa`, log 0).
fn deposit_at(reference: &str, block: &str, amount: &str, asset: &str) -> String {
    let mut fields = vec![
        ("bridge_reference", reference),
        ("amount", amount),
        ("asset", asset),
    ];
    fields.extend(source("'origin-a'", block, "'\\xaa'"));
    insert(&fields)
}

/// M1c, the reviewer's scenario: a compromised ingress binds a real, final arrival of 1000 units
/// to a deposit of one unit more, or of 10^9, or of another bridged asset, and the core would mint
/// what the row says. A deposit's amount and asset are its bound observation's, the arrival's or
/// the conversion output's (REQ-IN-4, REQ-CV-2, REQ-RQ-7): the store sets them from it when they
/// are not given, and refuses a row that differs, whoever writes it, at insert or when a binding
/// made ahead of its observation becomes final.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m1c_a_deposit_takes_its_amount_and_asset_from_its_observation_never_from_the_request(
    pool: PgPool,
) {
    // A final arrival of 1000 token-a at block 1, and a conversion's output of 777 token-a at
    // block 2, both at the reserve; token-a is admitted for asset-a. asset-b is another family.
    networks(&pool, &[1]).await;
    exec(
        &pool,
        &format!(
            "INSERT INTO observations (network, block_number, block_hash, block_time, \
                 position_kind, position_index, token, amount, sender, recipient, executed, kind) \
             VALUES ('origin-a', 2, '\\xaa', '{BLOCK_TIME}', 'log', 0, 'token-a', 777, '0xpool', \
                 '0xreserve-for-tests-origin-a', TRUE, 'conversion_output'); \
             INSERT INTO assets (asset, precision, assurance_class) \
             VALUES ('asset-b', 6, 'public')"
        ),
    )
    .await;
    finalise_coordinate(&pool, "origin-a", 2, "aa", "log", 0, "asset-a", "777").await;
    let ingress = login(
        &pool,
        "wg_m1c_ingress_for_tests",
        bezent_store::INGRESS_ROLE,
    )
    .await;

    let mut refund = vec![
        ("bridge_reference", "'bz-u-m1c-plus-one'"),
        ("kind", "'refund'"),
        ("amount", "1001"),
    ];
    refund.extend(source("'origin-a'", "1", "'\\xaa'"));
    for (sql, spec, what) in [
        (
            deposit_at("'bz-d-m1c-plus-one'", "1", "1001", "'asset-a'"),
            "REQ-IN-4",
            "the arrival plus one",
        ),
        (
            deposit_at("'bz-d-m1c-billion'", "1", "1000000000", "'asset-a'"),
            "REQ-IN-4",
            "10^9 against an arrival of 1000",
        ),
        (
            deposit_at("'bz-d-m1c-family'", "1", "1000", "'asset-b'"),
            "REQ-IN-4",
            "another bridged asset",
        ),
        (
            deposit_at("'bz-d-m1c-conversion'", "2", "1000", "'asset-a'"),
            "REQ-CV-2",
            "more than the conversion's output",
        ),
        (
            insert(&refund),
            "REQ-RQ-7",
            "a refund of the arrival plus one",
        ),
    ] {
        let err = run(&ingress, &sql)
            .await
            .expect_err(&format!("the ingress inserted {what}"));
        assert_refusal(&err, spec, what);
    }
    // Whoever writes it: the owner is refused the same row.
    let err = run(
        &pool,
        &deposit_at("'bz-d-m1c-owner'", "1", "1001", "'asset-a'"),
    )
    .await
    .expect_err("the owner inserted a deposit of more than its arrival");
    assert_refusal(&err, "REQ-IN-4", "owner, the arrival plus one");
    assert!(
        !flag(
            &pool,
            "SELECT EXISTS (SELECT 1 FROM transfers WHERE bridge_reference LIKE 'bz-_-m1c-%')"
        )
        .await
    );

    // The arrival's own amount and asset bind; a deposit that gives neither gets the conversion
    // output's from the store.
    run(
        &ingress,
        &deposit_at("'bz-d-m1c-ok'", "1", "1000", "'asset-a'"),
    )
    .await
    .expect("a deposit of the arrival's amount and asset");
    run(
        &ingress,
        &deposit_at("'bz-d-m1c-conversion-ok'", "2", "NULL", "NULL"),
    )
    .await
    .expect("a deposit that names only the conversion output's coordinate");
    assert_eq!(
        text(
            &pool,
            "SELECT amount::text || ' ' || asset FROM transfers \
             WHERE bridge_reference = 'bz-d-m1c-conversion-ok'"
        )
        .await,
        "777 asset-a"
    );

    // Bound ahead of its observation with an amount of its own: the arrival then observed is 1000,
    // and the deposit never becomes Source-Final, whoever moves it.
    run(
        &ingress,
        &deposit_at("'bz-d-m1c-ahead'", "3", "5", "'asset-a'"),
    )
    .await
    .expect("a binding ahead of its observation");
    finalise_coordinate(&pool, "origin-a", 3, "aa", "log", 0, "asset-a", "1000").await;
    let ahead = transfer_id(&pool, "bz-d-m1c-ahead").await;
    bare_transition(&pool, ahead, "Received", "Accepted")
        .await
        .expect("Received -> Accepted");
    let err = transition(&pool, ahead, "Accepted", "Source-Final")
        .await
        .expect_err("a deposit of 5 became final on an arrival of 1000");
    assert_refusal(&err, "REQ-IN-4", "Source-Final, amount not the arrival's");
    assert_eq!(state(&pool, "bz-d-m1c-ahead").await, "Accepted");

    ingress.close().await;
}

async fn transfer_id(pool: &PgPool, reference: &str) -> i64 {
    sqlx::query_scalar("SELECT id FROM transfers WHERE bridge_reference = $1")
        .bind(reference)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("{reference}: {e}"))
}

/// A withdrawal as the core makes it from a redemption (redeemer `redeemer-1`, on-chain reference
/// `ocr`), inserted by the owner, taken to `Source-Final` on the stand-in source (`origin-a`,
/// `block`).
async fn withdrawal_at_source_final(pool: &PgPool, reference: &str, ocr: &str, block: i64) -> i64 {
    let bridge_reference = format!("'{reference}'");
    let request_reference = format!("'redemption-{reference}'");
    let id: i64 = sqlx::query_scalar(&format!(
        "{} RETURNING id",
        insert(&[
            ("bridge_reference", bridge_reference.as_str()),
            ("request_reference", request_reference.as_str()),
            ("kind", "'withdrawal'"),
            ("destination", "'0xpayee'"),
            ("redeemer", "'redeemer-1'"),
            ("on_chain_reference", ocr),
        ])
    ))
    .fetch_one(pool)
    .await
    .expect("the withdrawal");
    to_source_final(pool, id, block).await;
    id
}

/// The refund of withdrawal `original` as the core makes it: to its redeemer, with its on-chain
/// reference `ocr`, its source at (`origin-a`, `block`), and `amount`.
fn refund_of_withdrawal(
    reference: &str,
    original: i64,
    ocr: &str,
    block: &str,
    amount: &str,
) -> String {
    let original = original.to_string();
    let mut fields = vec![
        ("bridge_reference", reference),
        ("kind", "'refund'"),
        ("destination", "'redeemer-1'"),
        ("refund_of", original.as_str()),
        ("redeemer", "'redeemer-1'"),
        ("on_chain_reference", ocr),
        ("amount", amount),
    ];
    fields.extend(source("'origin-a'", block, "'\\xaa'"));
    insert(&fields)
}

/// S7, the reviewer's scenario: a withdrawal reviewed before any payout was ordered (out of
/// `Source-Final`) has no attempt with an intent, so `attempts_unproven()` is empty, and the core
/// could refund it, closing its attempt set, though no leg was ever proven not executed. The core
/// now refunds a withdrawal only after at least one ordered attempt, every attempt proven not
/// executed at finality, out of a review entered while delivering, with the withdrawal's own
/// source, asset and amount (INV-8, REQ-RF-1, REQ-RF-3, REQ-RF-4).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s7_the_core_cannot_refund_a_withdrawal_no_payout_attempt_was_ordered_for(pool: PgPool) {
    networks(&pool, &[1, 2]).await;
    let core = login(&pool, "wg_s7_core_for_tests", bezent_store::CORE_ROLE).await;

    // Reviewed out of Source-Final: nothing was ordered, nothing can be proven not executed.
    let early = withdrawal_at_source_final(&pool, "bz-w-s7-early", "'\\x71'", 1).await;
    transition(&pool, early, "Source-Final", "Needs-Review")
        .await
        .expect("Source-Final -> Needs-Review");
    assert!(
        flag(
            &pool,
            &format!("SELECT cardinality(attempts_unproven({early})) = 0")
        )
        .await
    );
    let err = run(
        &core,
        &refund_of_withdrawal("'bz-f-s7-early'", early, "'\\x71'", "1", "1000"),
    )
    .await
    .expect_err("the core refunded a withdrawal that never ordered a payout");
    assert_refusal(
        &err,
        "INV-8",
        "refund of a withdrawal reviewed before delivery",
    );
    assert_refusal(
        &err,
        "REQ-RF-1",
        "refund of a withdrawal reviewed before delivery",
    );
    assert_eq!(state(&pool, "bz-w-s7-early").await, "Needs-Review");

    // Reviewed while delivering, its one attempt ordered and signed.
    let late = withdrawal_at_source_final(&pool, "bz-w-s7-late", "'\\x72'", 2).await;
    walk(&pool, late, &["Source-Final", "Delivering"]).await;
    bare_transition(&pool, late, "Delivering", "Needs-Review")
        .await
        .expect("Delivering -> Needs-Review");
    // Not before the attempt is proven not executed.
    let err = run(
        &core,
        &refund_of_withdrawal("'bz-f-s7-late'", late, "'\\x72'", "2", "1000"),
    )
    .await
    .expect_err("the core refunded before the proof");
    assert_refusal(&err, "INV-8", "refund before the proof");
    prove_not_executed(&pool, late).await;
    // Never of another amount, nor on another source than the withdrawal's.
    for (sql, what) in [
        (
            refund_of_withdrawal("'bz-f-s7-late'", late, "'\\x72'", "2", "1001"),
            "another amount",
        ),
        (
            refund_of_withdrawal("'bz-f-s7-late'", late, "'\\x72'", "1", "1000"),
            "another source",
        ),
    ] {
        let err = run(&core, &sql)
            .await
            .expect_err(&format!("the core refunded {what}"));
        assert_refusal(&err, "REQ-RF-3", what);
    }
    run(
        &core,
        &refund_of_withdrawal("'bz-f-s7-late'", late, "'\\x72'", "2", "1000"),
    )
    .await
    .expect("the core refunds once its ordered attempt is proven not executed");
    assert_eq!(state(&pool, "bz-f-s7-late").await, "Received");

    core.close().await;
}

/// S8, the reviewer's scenario: `id` is GENERATED ALWAYS, but `OVERRIDING SYSTEM VALUE` needs only
/// INSERT. A compromised ingress inserted valid deposits at the ids just above the sequence, so
/// the core's next withdrawal met a primary-key conflict and its bare `ON CONFLICT DO NOTHING` made
/// nothing, payout after payout. A login's row now takes the id the store's sequence gave its own
/// session, so the core's insert still makes the withdrawal.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s8_an_ingress_login_cannot_claim_the_id_of_the_cores_next_withdrawal(pool: PgPool) {
    networks(&pool, &[1, 2, 3]).await;
    let ingress = login(&pool, "wg_s8_ingress_for_tests", bezent_store::INGRESS_ROLE).await;
    let core = login(&pool, "wg_s8_core_for_tests", bezent_store::CORE_ROLE).await;
    let overriding =
        |sql: String| sql.replacen(") VALUES (", ") OVERRIDING SYSTEM VALUE VALUES (", 1);
    let last: i64 = sqlx::query_scalar(
        "SELECT coalesce(pg_sequence_last_value(pg_get_serial_sequence('transfers', 'id')::regclass), 0)",
    )
    .fetch_one(&pool)
    .await
    .expect("the sequence");

    // A valid deposit, but at an id the ingress chooses.
    let claim = |reference: &str, block: &str, id: i64| {
        let id = id.to_string();
        let mut fields = vec![("bridge_reference", reference), ("id", id.as_str())];
        fields.extend(source("'origin-a'", block, "'\\xaa'"));
        overriding(insert(&fields))
    };

    // In a session that drew no id, and in one that drew its own.
    let err = run(&ingress, &claim("'bz-d-s8-claim'", "1", last + 1))
        .await
        .expect_err("the ingress claimed an id in a session that drew none");
    assert_refusal(&err, "REQ-RQ-4", "claimed id, no id drawn");
    run(
        &ingress,
        &deposit_at("'bz-d-s8-own'", "2", "1000", "'asset-a'"),
    )
    .await
    .expect("a deposit with the id the store draws");
    let own = transfer_id(&pool, "bz-d-s8-own").await;
    for (sql, what) in [
        (claim("'bz-d-s8-claim'", "1", own + 1), "the next id"),
        (
            claim("'bz-d-s8-claim'", "1", own + 3),
            "an id further ahead",
        ),
        (claim("'bz-d-s8-ahead'", "3", own + 2), "the id after next"),
    ] {
        let err = run(&ingress, &sql)
            .await
            .expect_err(&format!("the ingress claimed {what}"));
        assert_refusal(&err, "REQ-RQ-4", what);
    }
    assert!(
        !flag(
            &pool,
            "SELECT EXISTS (SELECT 1 FROM transfers \
             WHERE bridge_reference IN ('bz-d-s8-claim', 'bz-d-s8-ahead'))"
        )
        .await
    );

    // The core's withdrawal is made, under the id the sequence gives it.
    let payout = "'\\x0303030303030303030303030303030303030303030303030303030303030303'";
    let made: Option<i64> = sqlx::query_scalar(&format!(
        "{} ON CONFLICT (bridge_reference) DO NOTHING RETURNING id",
        insert(&[
            ("bridge_reference", "'bz-w-s8'"),
            ("request_reference", "'redemption-s8'"),
            ("kind", "'withdrawal'"),
            ("destination_network", "'origin-a'"),
            ("destination", "'0xpayee'"),
            ("redeemer", "'redeemer-1'"),
            ("on_chain_reference", "'\\x56'"),
            ("destination_reference", payout),
            ("source_network", "'issuing'"),
            ("source_block_number", "6"),
            ("source_block_hash", "'\\xcc'"),
            ("source_position_kind", "'log'"),
            ("source_position_index", "0"),
        ])
    ))
    .fetch_optional(&core)
    .await
    .expect("the core's withdrawal insert");
    let made = made.expect("the core's withdrawal was not made");
    assert!(
        made > own,
        "the withdrawal took id {made}, not one the sequence drew after {own}"
    );

    // The owner (drills, restores, NFR-7) still writes the ids it restores.
    run(
        &pool,
        &overriding(insert(&[
            ("id", "1000000"),
            ("bridge_reference", "'bz-r-s8-restored'"),
        ])),
    )
    .await
    .expect("the owner restores a row under its own id");

    ingress.close().await;
    core.close().await;
}
