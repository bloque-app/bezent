// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The exhaustive write-path audit of PR #56 (migration 0049), each finding reproduced through a
//! real service login (a member of one role, as the services log in) and refused.
//!
//! - Must-fix 1 (`REQ-OB-6`, `INV-8`): the Kreivo endpoint's login writes only the issuing
//!   network's chain records, as the EVM endpoint's writes only EVM networks'.
//! - Should-fixes: a delivery order rests on a final observation of its transfer's source and
//!   moves exactly what it carries (`INV-6`, `INV-7`); no login chooses a transition's id; a
//!   key-compromise alert is its detector's, deduplicated by detection key (`AC-I1.2`); the
//!   destination-network rules hold in the store (`REQ-RQ-3`, `REQ-IN-6`).

mod common;

use bezent_store::login::{as_login, ensure_login, pool_options};
use common::*;
use sqlx::PgPool;

const PASSWORD: &str = "test-only-audit-findings-password";

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

async fn run(pool: &PgPool, sql: &str) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(sql).execute(pool).await.map(|_| ())
}

/// Runs `sql` in a transaction that is rolled back, so one check leaves nothing for the next.
async fn attempt(pool: &PgPool, sql: &str) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await.unwrap();
    let out = sqlx::raw_sql(sql).execute(&mut *tx).await.map(|_| ());
    tx.rollback().await.unwrap();
    out
}

// ---------------------------------------------------------------------------------------------
// Must-fix 1.

/// The chain records of `network`, each as one insert, over the order, signing intent and signed
/// transaction [`chain_fixture`] made for it; `n` keeps rows of different calls apart.
fn chain_records(network: &str, f: &ChainFixture, n: i64) -> Vec<(&'static str, String)> {
    vec![
        (
            "observations",
            format!(
                "INSERT INTO observations (network, block_number, block_hash, block_time, \
                   position_kind, position_index, executed) \
                 VALUES ('{network}', {n}, '\\x01', now(), 'log', 0, true)"
            ),
        ),
        (
            "observation_outcomes",
            format!(
                "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
                   final_block_hash) VALUES ({}, 'final', 1, '\\x01')",
                f.observation
            ),
        ),
        (
            "final_heads",
            format!(
                "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
                 VALUES ('{network}', {n}, '\\x01', now())"
            ),
        ),
        (
            "observation_ranges",
            format!(
                "INSERT INTO observation_ranges (network, first_block, last_block, outcome, \
                   final_block_number, final_block_hash) \
                 VALUES ('{network}', {n}, {n}, 'read', {n}, '\\x01')"
            ),
        ),
        (
            "reserve_readings",
            format!(
                "INSERT INTO reserve_readings (network, block_number, block_hash, kind, subject, \
                   holder, amount) VALUES ('{network}', 1, '\\x01', '{}', 'subject', '{}', 1)",
                f.reading_kind, f.reading_holder
            ),
        ),
        (
            "prepared_transactions",
            format!(
                "INSERT INTO prepared_transactions (order_intent_id, attempt, network, lane, \
                   key_id, nonce, valid_from_block, valid_until_block, checkpoint_hash, call, \
                   signing_payload, message, dry_run_block_number, dry_run_block_hash, \
                   dry_run_passed, dry_run) \
                 VALUES ({}, 1, '{network}', '\\x01', 'k', 1, 0, 5, '\\x01', '\\x02', '\\x02', \
                   '\\x02', 0, '\\x01', true, '{{}}')",
                f.unprepared
            ),
        ),
        (
            "signed_transactions",
            format!(
                "INSERT INTO signed_transactions (intent_id, attempt, bytes, tx_hash) \
                 VALUES ({}, 1, '\\x03', '\\x03')",
                f.unsigned
            ),
        ),
        (
            "non_execution_proofs",
            format!(
                "INSERT INTO non_execution_proofs (signed_transaction_id, network, signer, nonce, \
                   valid_from_block, valid_until_block, final_block_number, final_block_hash, \
                   nonce_at_final, kind) \
                 VALUES ({}, '{network}', '\\x01', 0, 0, 5, 10, '\\x0a', 0, 'nonce-unconsumed')",
                f.signed
            ),
        ),
    ]
}

struct ChainFixture {
    observation: i64,
    unprepared: i64,
    unsigned: i64,
    signed: i64,
    reading_kind: &'static str,
    reading_holder: &'static str,
}

/// What the chain records of `network` rest on, recorded as the owner (the core's order and
/// signing intent, the endpoint's preparation, signed transaction, observation and final head).
async fn chain_fixture(pool: &PgPool, network: &str, purpose: &str) -> ChainFixture {
    let one = |sql: String| async move {
        sqlx::query_scalar::<_, i64>(&sql)
            .fetch_one(pool)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    };
    let observation = one(format!(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
           position_index, executed) VALUES ('{network}', 1, '\\x01', now(), 'log', 0, true) \
         RETURNING id"
    ))
    .await;
    exec(
        pool,
        &format!(
            "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
             VALUES ('{network}', 1, '\\x01', now())"
        ),
    )
    .await;
    let order = |reference: &'static str| {
        one(format!(
            "INSERT INTO intents (network, purpose, on_chain_reference, payload) \
             VALUES ('{network}', '{purpose}', '\\x{reference}', \
               '{{\"order\": {{}}, \"amount\": \"1\"}}') RETURNING id"
        ))
    };
    let prepare = |order: i64, reference: &'static str, nonce: i64| {
        one(format!(
            "INSERT INTO prepared_transactions (order_intent_id, attempt, network, lane, key_id, \
               nonce, valid_from_block, valid_until_block, checkpoint_hash, call, \
               signing_payload, message, dry_run_block_number, dry_run_block_hash, \
               dry_run_passed, dry_run) \
             VALUES ({order}, 1, '{network}', '\\x01', 'k', {nonce}, 0, 5, '\\x01', '\\x{reference}', \
               '\\x{reference}', '\\x{reference}', 0, '\\x01', true, '{{}}') RETURNING id"
        ))
    };
    let sign_for = |prepared: i64, reference: &'static str| {
        one(format!(
            "INSERT INTO intents (network, purpose, on_chain_reference, payload) \
             VALUES ('{network}', '{purpose}', '\\x{reference}', \
               '{{\"message\": \"0x{reference}\", \"amount\": \"1\", \"prepared\": {prepared}}}') \
             RETURNING id"
        ))
    };
    let first = order("01").await;
    let prepared = prepare(first, "01", 0).await;
    let signing = sign_for(prepared, "01").await;
    let unprepared = order("02").await;
    let third = order("03").await;
    let prepared_third = prepare(third, "03", 2).await;
    let unsigned = sign_for(prepared_third, "03").await;
    let signed = one(format!(
        "INSERT INTO signed_transactions (intent_id, attempt, bytes, tx_hash) \
         VALUES ({signing}, 1, '\\x01', '\\x01') RETURNING id"
    ))
    .await;
    let issuing = text(
        pool,
        &format!("SELECT role FROM networks WHERE network = '{network}'"),
    )
    .await
        == "issuing";
    ChainFixture {
        observation,
        unprepared,
        unsigned,
        signed,
        reading_kind: if issuing { "supply" } else { "reserve" },
        reading_holder: if issuing { "" } else { "0xreserve" },
    }
}

/// Must-fix 1 (`REQ-OB-6`, `INV-8`): the Kreivo endpoint's login could write an EVM network's
/// final heads, read ranges, prepared and signed transactions and proofs of non-execution (only
/// the EVM endpoint was narrowed, 0035). A faked proof for a payout lets the core refund it. Every
/// chain table now holds each endpoint's login, checked by its session's login, to its own
/// networks; the Kreivo endpoint still writes the issuing network's, and the EVM endpoint the EVM
/// network's.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn must_fix_1_each_endpoint_login_writes_only_its_own_networks_chain_records(pool: PgPool) {
    exec(
        &pool,
        "INSERT INTO networks (network, role, address_format) VALUES \
           ('kreivo', 'issuing', 'ss58'), ('polygon', 'origin', 'evm')",
    )
    .await;
    let polygon = chain_fixture(&pool, "polygon", "payout").await;
    let kreivo = chain_fixture(&pool, "kreivo", "mint").await;

    let kreivo_login = login(
        &pool,
        "audit_kreivo_endpoint_for_tests",
        bezent_store::KREIVO_ENDPOINT_ROLE,
    )
    .await;
    let evm_login = login(
        &pool,
        "audit_evm_endpoint_for_tests",
        bezent_store::EVM_ENDPOINT_ROLE,
    )
    .await;

    // The audit's scenario: the Kreivo endpoint writes the EVM network's records, the proof first.
    for (table, sql) in chain_records("polygon", &polygon, 7) {
        let err = attempt(&kreivo_login, &sql)
            .await
            .expect_err(&format!("the Kreivo endpoint wrote {table} of polygon"));
        assert_refusal(&err, "INV-8", table);
    }
    // And the EVM endpoint the issuing network's (0035, unchanged).
    for (table, sql) in chain_records("kreivo", &kreivo, 7) {
        let err = attempt(&evm_login, &sql)
            .await
            .expect_err(&format!("the EVM endpoint wrote {table} of kreivo"));
        assert_refusal(&err, "INV-8", table);
    }
    // Each still writes its own.
    for (table, sql) in chain_records("kreivo", &kreivo, 8) {
        attempt(&kreivo_login, &sql)
            .await
            .unwrap_or_else(|e| panic!("the Kreivo endpoint's own {table}: {e}"));
    }
    for (table, sql) in chain_records("polygon", &polygon, 8) {
        attempt(&evm_login, &sql)
            .await
            .unwrap_or_else(|e| panic!("the EVM endpoint's own {table}: {e}"));
    }
    // A network nobody declared is no endpoint's.
    let err = run(
        &kreivo_login,
        "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
         VALUES ('nowhere', 1, '\\x01', now())",
    )
    .await
    .expect_err("an undeclared network's head");
    assert_refusal(&err, "INV-8", "final_heads of nowhere");
}

// ---------------------------------------------------------------------------------------------
// Should-fixes.

/// An issuing network, an EVM origin network, one asset (precision 2, `Here(101)` on the issuing
/// network) admitted on the origin network as a 6-decimal token, and its reserve there. Then a
/// final arrival of 1 000 000 at the reserve, at each of `arrivals` positions of block 10.
async fn delivery_declarations(pool: &PgPool) {
    exec(
        pool,
        "INSERT INTO networks (network, role, address_format, backend) VALUES \
           ('kreivo', 'issuing', 'ss58', NULL), ('polygon', 'origin', 'evm', 'evm:137'); \
         INSERT INTO assets (asset, precision, assurance_class, issuing_asset) VALUES \
           ('usd', 2, 'public', 'Here(101)'); \
         INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
           ('polygon', '0xtoken', 'usd', 6); \
         INSERT INTO reserves (asset, network, address) VALUES ('usd', 'polygon', '0xreserve'); \
         INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, token, amount, sender, recipient, executed, kind) \
         SELECT 'polygon', 10, '\\xaa', now(), 'log', i, '0xtoken', 1000000, '0xsender', \
             '0xreserve', TRUE, 'arrival' FROM generate_series(0, 3) i",
    )
    .await;
}

/// Makes the observations of `network` at `block` final at `block + 20` (a recorded final head).
async fn make_final(pool: &PgPool, network: &str, block: i64, hash: &str) {
    exec(
        pool,
        &format!(
            "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
             VALUES ('{network}', {block} + 20, '\\x{hash}', now()); \
             INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
                 final_block_hash) \
             SELECT id, 'final', {block} + 20, '\\x{hash}' FROM observations \
             WHERE network = '{network}' AND block_number = {block}"
        ),
    )
    .await;
}

/// A delivery order of `purpose` for transfer `reference` on `network`, carrying `reference_hex`,
/// `order` and `amount`, as the core writes it.
fn order(
    reference: &str,
    network: &str,
    purpose: &str,
    on_chain: &str,
    order: &str,
    amount: &str,
) -> String {
    format!(
        "INSERT INTO intents (transfer_id, network, purpose, on_chain_reference, attempt, payload) \
         SELECT id, '{network}', '{purpose}', '\\x{on_chain}', 1, \
           '{{\"order\": {order}, \"amount\": \"{amount}\"}}' \
         FROM transfers WHERE bridge_reference = '{reference}'"
    )
}

/// Should-fix (`INV-6`, `REQ-IN-4`, `REQ-AS-9`): a compromised core could record a mint order not
/// tied to a final arrival, or of another amount. The store now takes a mint order only for a
/// deposit whose bound arrival is final and executed, for exactly the arrival's amount scaled to
/// the asset (1 000 000 at 6 decimals is 100 at 2), of the asset's issuing asset, on the issuing
/// network.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn should_fix_a_mint_order_rests_on_a_final_bound_arrival_and_its_amount(pool: PgPool) {
    delivery_declarations(&pool).await;
    exec(
        &pool,
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index, on_chain_reference) \
         VALUES ('bz-d-1', 'req-1', 'deposit', 'usd', 1000000, 'kreivo', '0xdest', 'Received', \
             'polygon', 10, '\\xaa', 'log', 0, '\\x11')",
    )
    .await;
    let core = login(&pool, "audit_core_for_tests", bezent_store::CORE_ROLE).await;
    let to = r#"{"asset": "Here(101)", "to": "0x0202"}"#;
    let mint = |order_json: &str, network: &str, amount: &str| {
        order("bz-d-1", network, "mint", "11", order_json, amount)
    };

    // The audit's scenario: the arrival is observed, not final, and the core orders the mint.
    let err = attempt(&core, &mint(to, "kreivo", "100"))
        .await
        .expect_err("a mint order before the arrival is final");
    assert_refusal(&err, "INV-6", "a mint before finality");

    make_final(&pool, "polygon", 10, "bb").await;
    // Final, but another amount, asset or network than the arrival's.
    for (what, sql) in [
        ("another amount", mint(to, "kreivo", "101")),
        ("ten thousand times it", mint(to, "kreivo", "1000000")),
        (
            "another asset",
            mint(r#"{"asset": "Here(102)", "to": "0x0202"}"#, "kreivo", "100"),
        ),
        ("an origin network", mint(to, "polygon", "100")),
    ] {
        let err = attempt(&core, &sql)
            .await
            .expect_err(&format!("a mint order of {what}"));
        assert_refusal(&err, "INV-6", what);
    }
    // Its own amount, scaled down to the asset's precision: taken.
    attempt(&core, &mint(to, "kreivo", "100"))
        .await
        .expect("the mint the arrival carries");
}

/// Should-fix (`INV-7`, `REQ-OT-3`): and a payout order only for a withdrawal whose redemption is
/// final and executed, for exactly the redeemed amount scaled to the destination's token (100 at
/// 2 decimals is 1 000 000 at 6), to the withdrawal's destination, on its network.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn should_fix_a_payout_order_rests_on_a_final_redemption_and_its_amount(pool: PgPool) {
    delivery_declarations(&pool).await;
    exec(
        &pool,
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, token, amount, sender, executed, kind, on_chain_reference) \
         VALUES ('kreivo', 50, '\\xcc', now(), 'log', 0, 'Here(101)', 100, '0xredeemer', TRUE, \
             'redemption', '\\x22'); \
         INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index, on_chain_reference, \
             redeemer, destination_reference) \
         VALUES ('bz-w-1', 'redemption-1', 'withdrawal', 'usd', 1000000, 'polygon', \
             '0xbeneficiary', 'Received', 'kreivo', 50, '\\xcc', 'log', 0, '\\x22', '0xredeemer', \
             '\\x3333333333333333333333333333333333333333333333333333333333333333')",
    )
    .await;
    let core = login(&pool, "audit_core_for_tests", bezent_store::CORE_ROLE).await;
    let to = r#"{"to": "0xbeneficiary", "asset": "usd", "token": null}"#;
    let payout = |order_json: &str, amount: &str| {
        order(
            "bz-w-1",
            "polygon",
            "payout",
            "3333333333333333333333333333333333333333333333333333333333333333",
            order_json,
            amount,
        )
    };

    let err = attempt(&core, &payout(to, "1000000"))
        .await
        .expect_err("a payout order before the redemption is final");
    assert_refusal(&err, "INV-7", "a payout before finality");

    make_final(&pool, "kreivo", 50, "dd").await;
    for (what, sql) in [
        ("another amount", payout(to, "1000001")),
        (
            "another beneficiary",
            payout(
                r#"{"to": "0xattacker", "asset": "usd", "token": null}"#,
                "1000000",
            ),
        ),
    ] {
        let err = attempt(&core, &sql)
            .await
            .expect_err(&format!("a payout order of {what}"));
        assert_refusal(&err, "INV-7", what);
    }
    attempt(&core, &payout(to, "1000000"))
        .await
        .expect("the payout the redemption carries");
}

/// Should-fix (`REQ-TR-6`, `REQ-RQ-7`): the ingress could insert a transition under an id of its
/// choosing (`OVERRIDING SYSTEM VALUE`), the core's next one included, and stall the core on the
/// conflict. A transition's id is now the one the store's sequence gave the session.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn should_fix_no_login_chooses_or_claims_a_transition_id(pool: PgPool) {
    delivery_declarations(&pool).await;
    exec(
        &pool,
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state) \
         VALUES ('bz-d-2', 'req-2', 'deposit', 'usd', 1000000, 'kreivo', '0xdest', 'Received')",
    )
    .await;
    let ingress = login(&pool, "audit_ingress_for_tests", bezent_store::INGRESS_ROLE).await;
    let next: i64 = sqlx::query_scalar("SELECT COALESCE(max(id), 0) + 1 FROM transitions")
        .fetch_one(&pool)
        .await
        .unwrap();
    for id in [next, next + 1, next + 1000] {
        let err = attempt(
            &ingress,
            &format!(
                "INSERT INTO transitions (id, transfer_id, from_state, to_state) \
                 OVERRIDING SYSTEM VALUE \
                 SELECT {id}, id, 'Received', 'Accepted' FROM transfers \
                 WHERE bridge_reference = 'bz-d-2'"
            ),
        )
        .await
        .expect_err(&format!("the ingress chose transition id {id}"));
        assert_refusal(&err, "REQ-TR-6", "a chosen transition id");
    }
    // Its own decision, under the id the store draws: taken.
    run(
        &ingress,
        "INSERT INTO transitions (transfer_id, from_state, to_state) \
         SELECT id, 'Received', 'Accepted' FROM transfers WHERE bridge_reference = 'bz-d-2'",
    )
    .await
    .expect("the ingress's decision");
}

/// Should-fix (`AC-I1.2`, `REQ-IC-4`): any endpoint could pre-write a key-compromise alert under
/// the dedupe key, intent or coordinate the real detector would use, and the real alert and its
/// pause request were never written. The store now stamps each alert with its detector (the
/// writing login's role) and a detection key it derives; a detector finds only its own, and one per
/// (detector, detection key).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn should_fix_a_pre_written_key_compromise_never_suppresses_the_detectors_own(pool: PgPool) {
    delivery_declarations(&pool).await;
    let core = login(&pool, "audit_core_for_tests", bezent_store::CORE_ROLE).await;
    let evm = login(
        &pool,
        "audit_evm_endpoint_for_tests",
        bezent_store::EVM_ENDPOINT_ROLE,
    )
    .await;
    let kreivo = login(
        &pool,
        "audit_kreivo_endpoint_for_tests",
        bezent_store::KREIVO_ENDPOINT_ROLE,
    )
    .await;
    let ingress = login(&pool, "audit_ingress_for_tests", bezent_store::INGRESS_ROLE).await;
    let signer = login(&pool, "audit_signer_for_tests", bezent_store::SIGNER_ROLE).await;
    let alert = |payload: &str| {
        format!("INSERT INTO event_log (kind, payload) VALUES ('key-compromise', '{payload}')")
    };
    let supply = r#"{"dedupe": "INV-4:supply-mismatch:usd:reading:1", "network": "polygon"}"#;

    // The audit's scenario: the EVM endpoint writes the core's supply-mismatch alert first.
    run(&evm, &alert(supply))
        .await
        .expect("an endpoint's own row, on its network");
    let raised_by_core = "SELECT EXISTS (SELECT 1 FROM event_log WHERE kind = 'key-compromise' \
         AND payload ->> 'detector' = 'core' \
         AND payload ->> 'detection_key' = 'dedupe:INV-4:supply-mismatch:usd:reading:1')";
    assert!(
        !flag(&pool, raised_by_core).await,
        "the endpoint's row is the core's"
    );
    assert_eq!(
        text(
            &pool,
            "SELECT payload ->> 'detector' FROM event_log WHERE kind = 'key-compromise'"
        )
        .await,
        "evm-endpoint"
    );
    // The core's own detection is written beside it, and found.
    run(&core, &alert(supply))
        .await
        .expect("the core's own alert");
    assert!(flag(&pool, raised_by_core).await);
    // Once per detector and detection key.
    let err = run(&core, &alert(supply))
        .await
        .expect_err("a second alert for one detection");
    assert_error_sqlstate(&err, "23505", "a second alert");

    // An endpoint raises only on its own networks; a row naming no detection is refused; the
    // ingress raises nothing.
    let coordinate =
        r#"{"coordinate": {"network": "polygon", "block_hash": "0x01", "position_index": 0}}"#;
    let err = run(&kreivo, &alert(coordinate))
        .await
        .expect_err("the Kreivo endpoint on polygon");
    assert_refusal(&err, "AC-I1.2", "another network's alert");
    let err = run(&core, &alert(r#"{"reason": "free-form"}"#))
        .await
        .expect_err("an alert naming nothing");
    assert_refusal(&err, "AC-I1.2", "a free-form alert");
    let err = run(&ingress, &alert(supply))
        .await
        .expect_err("the ingress");
    assert_refusal(&err, "AC-I1.2", "the ingress's alert");
    // The signer raises one (must-fix 2), and nothing but an alert and its pause request.
    run(&signer, &alert(r#"{"requested_intent": 7}"#))
        .await
        .expect("the signer's alert");
    run(
        &signer,
        "INSERT INTO event_log (kind, payload) VALUES ('pause-requested', '{\"scope\": \"global\"}')",
    )
    .await
    .expect("the signer's pause request");
    let err = run(
        &signer,
        "INSERT INTO event_log (kind, payload) VALUES ('delivery-anomaly', '{\"dedupe\": \"x\"}')",
    )
    .await
    .expect_err("the signer's anomaly");
    assert_refusal(&err, "AC-I1.2", "the signer's anomaly");
}

/// Should-fix (`REQ-RQ-3`, `REQ-IN-6`, `REQ-RQ-7`): the ingress's destination-network rules were
/// held only in its code. The store refuses, from any login but the owner, a bound deposit whose
/// destination is not on the issuing network or whose refund destination is not on its arrival's
/// network, an unassociated arrival's refund paid out on another network, and the acceptance of
/// either.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn should_fix_the_destination_network_rules_hold_in_the_store(pool: PgPool) {
    delivery_declarations(&pool).await;
    make_final(&pool, "polygon", 10, "bb").await;
    let ingress = login(&pool, "audit_ingress_for_tests", bezent_store::INGRESS_ROLE).await;
    let deposit = |index: i64, destination: &str, refund: &str| {
        format!(
            "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                 destination_network, destination, refund_destination_network, \
                 refund_destination, state, source_network, source_block_number, \
                 source_block_hash, source_position_kind, source_position_index) \
             VALUES ('bz-d-{index}', 'req-{index}', 'deposit', 'usd', 1000000, '{destination}', \
                 '0xdest', '{refund}', '0xback', 'Received', 'polygon', 10, '\\xaa', 'log', {index})"
        )
    };
    let refund = |index: i64, destination: &str| {
        format!(
            "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                 destination_network, destination, state, source_network, source_block_number, \
                 source_block_hash, source_position_kind, source_position_index) \
             VALUES ('bz-u-{index}', 'req-u-{index}', 'refund', 'usd', 1000000, \
                 '{destination}', '0xback', 'Received', 'polygon', 10, '\\xaa', 'log', {index})"
        )
    };
    for (what, sql) in [
        (
            "a deposit minted on an origin network",
            deposit(1, "polygon", "polygon"),
        ),
        (
            "a deposit refunded on the issuing network",
            deposit(1, "kreivo", "kreivo"),
        ),
        (
            "a refund paid out on the issuing network",
            refund(2, "kreivo"),
        ),
    ] {
        let err = attempt(&ingress, &sql)
            .await
            .expect_err(&format!("{what} was taken"));
        assert_refusal(&err, "REQ-RQ-7", what);
    }
    attempt(&ingress, &deposit(1, "kreivo", "polygon"))
        .await
        .expect("a deposit on the right networks");
    attempt(&ingress, &refund(2, "polygon"))
        .await
        .expect("a refund on the arrival's network");

    // Unbound, the wrong destination is taken in (a refusal is recorded so), and never accepted.
    run(
        &ingress,
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state) \
         VALUES ('bz-d-9', 'req-9', 'deposit', 'usd', 1000000, 'polygon', '0xdest', 'Received')",
    )
    .await
    .expect("an unbound deposit");
    let err = run(
        &ingress,
        "INSERT INTO transitions (transfer_id, from_state, to_state) \
         SELECT id, 'Received', 'Accepted' FROM transfers WHERE bridge_reference = 'bz-d-9'",
    )
    .await
    .expect_err("accepted with an origin destination");
    assert_refusal(&err, "REQ-RQ-7", "its acceptance");
}
