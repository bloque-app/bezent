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
//! - Issue #36 (migration 0050): a payout order's asset and token are its transfer's, and a mint
//!   order's recipient is its deposit's destination (`INV-6`, `INV-7`, `REQ-AS-2`, `REQ-AS-3`).

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
         VALUES ('bz-d-1', 'req-1', 'deposit', 'usd', 1000000, 'kreivo', \
             '0x0202020202020202020202020202020202020202020202020202020202020202', 'Received', \
             'polygon', 10, '\\xaa', 'log', 0, '\\x11')",
    )
    .await;
    let core = login(&pool, "audit_core_for_tests", bezent_store::CORE_ROLE).await;
    let to = r#"{"asset": "Here(101)", "to": "0x0202020202020202020202020202020202020202020202020202020202020202"}"#;
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
            mint(
                r#"{"asset": "Here(102)", "to": "0x0202020202020202020202020202020202020202020202020202020202020202"}"#,
                "kreivo",
                "100",
            ),
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

/// Issue #36 (`INV-6`, `REQ-IN-4`, `REQ-RQ-7`): with only the core compromised, a mint order's
/// recipient was not tied to the deposit's destination, so the core could mint a real, final
/// arrival to an account of its own. The order's `to` is the 32-byte account the deposit's
/// destination decodes to: written as `0x` and 64 hex digits, or as an SS58 address of a one- or
/// two-byte network prefix, the form the ingress checked.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_36_a_mint_orders_the_deposits_destination_and_no_other_recipient(pool: PgPool) {
    delivery_declarations(&pool).await;
    // Deposit 0 names the account 0x02… in hex, deposit 1 the same account in SS58 (one-byte
    // network prefix), deposit 2 the account 0x03… in SS58 (two-byte prefix).
    let ss58_one_byte_prefix = "5C7LYpP2ZH3tpKbvVvwiVe54AapxErdPBbvkYhe6y9ZBkqWt";
    let ss58_two_byte_prefix = "a3aH71MptGVJWsn1bUN9oWEAV6zVWcVMRcRVJxCXxSiibATrs";
    let account = |byte: &str| format!("0x{}", byte.repeat(32));
    let destinations = [
        account("02"),
        ss58_one_byte_prefix.to_owned(),
        ss58_two_byte_prefix.to_owned(),
    ];
    for (index, destination) in destinations.iter().enumerate() {
        exec(
            &pool,
            &format!(
                "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, \
                     amount, destination_network, destination, state, source_network, \
                     source_block_number, source_block_hash, source_position_kind, \
                     source_position_index, on_chain_reference) \
                 VALUES ('bz-d-{index}', 'req-{index}', 'deposit', 'usd', 1000000, 'kreivo', \
                     '{destination}', 'Received', 'polygon', 10, '\\xaa', 'log', {index}, \
                     '\\x1{index}')"
            ),
        )
        .await;
    }
    make_final(&pool, "polygon", 10, "bb").await;
    let core = login(&pool, "issue_36_core_for_tests", bezent_store::CORE_ROLE).await;
    let mint = |index: usize, to: Option<&str>| {
        let order_json = match to {
            Some(to) => format!(r#"{{"asset": "Here(101)", "to": "{to}"}}"#),
            None => r#"{"asset": "Here(101)"}"#.to_owned(),
        };
        order(
            &format!("bz-d-{index}"),
            "kreivo",
            "mint",
            &format!("1{index}"),
            &order_json,
            "100",
        )
    };

    // Another account than the deposit's, whichever way the deposit's destination was written.
    for index in 0..3 {
        for (what, to) in [
            ("an account of the core's own", Some(account("0a"))),
            ("the account next to it", Some(account("04"))),
            ("no recipient", None),
        ] {
            let err = attempt(&core, &mint(index, to.as_deref()))
                .await
                .expect_err(&format!("a mint order to {what}, deposit {index}"));
            assert_refusal(&err, "INV-6", what);
        }
    }
    // Deposit 2 names the account 0x03…, not the 0x02… of deposits 0 and 1.
    let err = attempt(&core, &mint(2, Some(&account("02"))))
        .await
        .expect_err("a mint order to another deposit's account");
    assert_refusal(&err, "INV-6", "another deposit's account");
    // Its own destination, as the core writes it (lower-case hex of the 32-byte account).
    for (index, bytes) in [(0, "02"), (1, "02"), (2, "03")] {
        attempt(&core, &mint(index, Some(&account(bytes))))
            .await
            .unwrap_or_else(|e| panic!("the mint to the deposit's own destination {index}: {e}"));
    }
}

/// Issue #36 (`INV-7`, `REQ-AS-2`, `REQ-AS-3`, `REQ-OT-4`): a payout order's asset and token were
/// not tied to the transfer's asset in the store, so a compromised core could order the payout in
/// the token of another asset (the amount is scaled for the transfer's own) and multiply the value
/// paid out. The order's asset is the transfer's, and its token, when it names one, the one
/// admitted for that asset on the destination network.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_36_a_payout_orders_its_transfers_asset_and_token_and_no_other(pool: PgPool) {
    delivery_declarations(&pool).await;
    // A second asset, whose token has 18 decimals on the same network.
    exec(
        &pool,
        "INSERT INTO assets (asset, precision, assurance_class, issuing_asset) \
           VALUES ('eur', 2, 'public', 'Here(102)'); \
         INSERT INTO admitted_tokens (network, token, asset, precision) \
           VALUES ('polygon', '0xother', 'eur', 18); \
         INSERT INTO reserves (asset, network, address) VALUES ('eur', 'polygon', '0xreserve2'); \
         INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
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
    make_final(&pool, "kreivo", 50, "dd").await;
    let core = login(&pool, "issue_36_core_for_tests", bezent_store::CORE_ROLE).await;
    let payout = |order_json: &str| {
        order(
            "bz-w-1",
            "polygon",
            "payout",
            "3333333333333333333333333333333333333333333333333333333333333333",
            order_json,
            "1000000",
        )
    };

    for (what, order_json) in [
        (
            "another asset",
            r#"{"to": "0xbeneficiary", "asset": "eur", "token": null}"#,
        ),
        (
            "another asset, its own token",
            r#"{"to": "0xbeneficiary", "asset": "eur", "token": "0xother"}"#,
        ),
        (
            "the asset, another asset's token",
            r#"{"to": "0xbeneficiary", "asset": "usd", "token": "0xother"}"#,
        ),
        (
            "a token and no asset",
            r#"{"to": "0xbeneficiary", "token": "0xtoken"}"#,
        ),
        ("no asset", r#"{"to": "0xbeneficiary", "token": null}"#),
    ] {
        let err = attempt(&core, &payout(order_json))
            .await
            .expect_err(&format!("a payout order of {what}"));
        assert_refusal(&err, "INV-7", what);
    }
    for (what, order_json) in [
        (
            "the asset, no token (as the core writes it)",
            r#"{"to": "0xbeneficiary", "asset": "usd", "token": null}"#,
        ),
        (
            "the asset, its token",
            r#"{"to": "0xbeneficiary", "asset": "usd", "token": "0xtoken"}"#,
        ),
        (
            "the asset, its token in another case",
            r#"{"to": "0xbeneficiary", "asset": "usd", "token": "0xTOKEN"}"#,
        ),
    ] {
        attempt(&core, &payout(order_json))
            .await
            .unwrap_or_else(|e| panic!("a payout order of {what}: {e}"));
    }
}

/// Issue #36 (`INV-6`, `REQ-IN-4`; review of PR #42, nit): a destination that does not decode to
/// a 32-byte account (a 20-byte address, an SS58 address with a trailing byte) has no mint order
/// at all, whatever recipient the order names; and the recipient is read without regard to case on
/// both sides (the core writes lower-case hex; the destination may be upper-case hex).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_36_a_mint_to_an_undecodable_destination_is_refused_and_hex_case_is_folded(
    pool: PgPool,
) {
    delivery_declarations(&pool).await;
    let ss58 = "5C7LYpP2ZH3tpKbvVvwiVe54AapxErdPBbvkYhe6y9ZBkqWt";
    let destinations = [
        // A 20-byte address (an EVM address is not an issuing-network account).
        format!("0x{}", "02".repeat(20)),
        // A well-formed SS58 address with one more base58 digit: a trailing byte.
        format!("{ss58}1"),
        // Upper-case hex.
        format!("0x{}", "AB".repeat(32)),
        // Lower-case hex.
        format!("0x{}", "cd".repeat(32)),
    ];
    for (index, destination) in destinations.iter().enumerate() {
        exec(
            &pool,
            &format!(
                "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, \
                     amount, destination_network, destination, state, source_network, \
                     source_block_number, source_block_hash, source_position_kind, \
                     source_position_index, on_chain_reference) \
                 VALUES ('bz-d-{index}', 'req-{index}', 'deposit', 'usd', 1000000, 'kreivo', \
                     '{destination}', 'Received', 'polygon', 10, '\\xaa', 'log', {index}, \
                     '\\x1{index}')"
            ),
        )
        .await;
    }
    make_final(&pool, "polygon", 10, "bb").await;
    let core = login(&pool, "issue_36_core_for_tests", bezent_store::CORE_ROLE).await;
    let mint = |index: usize, to: &str| {
        order(
            &format!("bz-d-{index}"),
            "kreivo",
            "mint",
            &format!("1{index}"),
            &format!(r#"{{"asset": "Here(101)", "to": "{to}"}}"#),
            "100",
        )
    };

    // Undecodable: refused for every recipient the order could name, among them the 32 bytes the
    // destination would be padded to or cut down to.
    for (index, what) in [
        (0, "a 20-byte address"),
        (1, "an SS58 address with a trailing byte"),
    ] {
        for to in [
            format!("0x{}", "02".repeat(20)),
            format!("0x{}", "02".repeat(32)),
            format!("0x{}", "00".repeat(12) + &"02".repeat(20)),
        ] {
            let err = attempt(&core, &mint(index, &to))
                .await
                .expect_err(&format!("a mint order to {to} for {what}"));
            assert_refusal(&err, "INV-6", what);
        }
    }
    // Decodable, in either case: the recipient is the account, however it is spelled.
    for (index, bytes) in [(2, "ab"), (3, "cd")] {
        for to in [
            format!("0x{}", bytes.repeat(32)),
            format!("0x{}", bytes.to_uppercase().repeat(32)),
        ] {
            attempt(&core, &mint(index, &to))
                .await
                .unwrap_or_else(|e| panic!("the mint of deposit {index} to {to}: {e}"));
        }
    }
}

/// Issue #36 (`INV-7`, `REQ-AS-2`, `REQ-AS-3`; review of PR #42, finding 1): the payout's scaling
/// reads the precision of the one token admitted for the asset on the destination network, so the
/// store admits only one: a second token for the same asset on a network is refused, whatever its
/// precision, and a payout order naming it is refused as a token that is not admitted. The same
/// token on another network, and another asset's token on the network, are not affected.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_36_a_network_admits_one_token_per_asset_and_the_payout_uses_it(pool: PgPool) {
    delivery_declarations(&pool).await;
    exec(
        &pool,
        "INSERT INTO networks (network, role, address_format, backend) VALUES \
           ('base', 'origin', 'evm', 'evm:8453'); \
         INSERT INTO assets (asset, precision, assurance_class, issuing_asset) \
           VALUES ('eur', 2, 'public', 'Here(102)'); \
         INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
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

    // A second token for `usd` on polygon, of the same precision and of another (18): refused. The
    // first stays the only one.
    for (token, precision) in [("0xsecond", 6), ("0xsecond", 18), ("0xTOKEN2", 18)] {
        assert_sqlstate(
            &pool,
            &format!(
                "INSERT INTO admitted_tokens (network, token, asset, precision) \
                 VALUES ('polygon', '{token}', 'usd', {precision})"
            ),
            "23505",
        )
        .await;
    }
    let admitted: Vec<String> = sqlx::query_scalar(
        "SELECT token FROM admitted_tokens WHERE network = 'polygon' AND asset = 'usd'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(admitted, ["0xtoken"]);
    // The same asset's token on another network, and another asset's on this one, are admitted.
    exec(
        &pool,
        "INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
           ('base', '0xtoken', 'usd', 6), ('polygon', '0xother', 'eur', 18)",
    )
    .await;

    make_final(&pool, "kreivo", 50, "dd").await;
    let core = login(&pool, "issue_36_core_for_tests", bezent_store::CORE_ROLE).await;
    let payout = |order_json: &str| {
        order(
            "bz-w-1",
            "polygon",
            "payout",
            "3333333333333333333333333333333333333333333333333333333333333333",
            order_json,
            "1000000",
        )
    };
    let err = attempt(
        &core,
        &payout(r#"{"to": "0xbeneficiary", "asset": "usd", "token": "0xsecond"}"#),
    )
    .await
    .expect_err("a payout order in a token that was never admitted for the asset");
    assert_refusal(&err, "INV-7", "a second token");
    attempt(
        &core,
        &payout(r#"{"to": "0xbeneficiary", "asset": "usd", "token": "0xtoken"}"#),
    )
    .await
    .expect("the payout order in the one admitted token");
}

/// Issue #36 (`INV-7`, `REQ-IN-6`, `REQ-AS-3`): and the refund of an unassociated arrival, paid
/// out in the arrival's own minor units, is ordered in that arrival's token only: the asset the
/// refund names (or, when its token is not admitted, no asset and that token itself).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_36_an_unassociated_refund_pays_out_in_the_arrivals_own_token(pool: PgPool) {
    delivery_declarations(&pool).await;
    exec(
        &pool,
        "INSERT INTO assets (asset, precision, assurance_class, issuing_asset) \
           VALUES ('eur', 2, 'public', 'Here(102)'); \
         INSERT INTO admitted_tokens (network, token, asset, precision) \
           VALUES ('polygon', '0xother', 'eur', 18); \
         INSERT INTO reserves (asset, network, address) VALUES ('eur', 'polygon', '0xreserve2'); \
         INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, token, amount, sender, recipient, executed, kind) \
           VALUES ('polygon', 10, '\\xaa', now(), 'log', 7, '0xstray', 777, '0xsender', \
             '0xreserve', TRUE, 'other_arrival'); \
         INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index, on_chain_reference) \
           VALUES ('bz-u-1', 'req-u-1', 'refund', 'usd', 1000000, 'polygon', '0xback', 'Received', \
             'polygon', 10, '\\xaa', 'log', 0, '\\x44'); \
         INSERT INTO transfers (bridge_reference, request_reference, kind, token, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index, on_chain_reference) \
           VALUES ('bz-u-2', 'req-u-2', 'refund', '0xstray', 777, 'polygon', '0xback', \
             'Received', 'polygon', 10, '\\xaa', 'log', 7, '\\x55')",
    )
    .await;
    make_final(&pool, "polygon", 10, "bb").await;
    let core = login(&pool, "issue_36_core_for_tests", bezent_store::CORE_ROLE).await;
    let payout = |reference: &str, on_chain: &str, order_json: &str, amount: &str| {
        order(reference, "polygon", "payout", on_chain, order_json, amount)
    };

    // The refund naming the arrival's asset.
    for (what, order_json) in [
        (
            "another asset",
            r#"{"to": "0xback", "asset": "eur", "token": null}"#,
        ),
        (
            "another asset's token",
            r#"{"to": "0xback", "asset": "usd", "token": "0xother"}"#,
        ),
        (
            "a token, no asset",
            r#"{"to": "0xback", "token": "0xtoken"}"#,
        ),
    ] {
        let err = attempt(&core, &payout("bz-u-1", "44", order_json, "1000000"))
            .await
            .expect_err(&format!("a refund payout of {what}"));
        assert_refusal(&err, "REQ-IN-6", what);
    }
    attempt(
        &core,
        &payout(
            "bz-u-1",
            "44",
            r#"{"to": "0xback", "asset": "usd", "token": null}"#,
            "1000000",
        ),
    )
    .await
    .expect("the refund in the arrival's asset");

    // The refund of a token that is not admitted names that token and no asset.
    for (what, order_json) in [
        (
            "an admitted asset in place of the stray token",
            r#"{"to": "0xback", "asset": "usd", "token": null}"#,
        ),
        (
            "an admitted token in place of the stray one",
            r#"{"to": "0xback", "token": "0xtoken"}"#,
        ),
        (
            "the stray token and an asset besides",
            r#"{"to": "0xback", "asset": "usd", "token": "0xstray"}"#,
        ),
        ("no token", r#"{"to": "0xback"}"#),
    ] {
        let err = attempt(&core, &payout("bz-u-2", "55", order_json, "777"))
            .await
            .expect_err(&format!("a refund payout of {what}"));
        assert_refusal(&err, "REQ-IN-6", what);
    }
    attempt(
        &core,
        &payout(
            "bz-u-2",
            "55",
            r#"{"to": "0xback", "asset": null, "token": "0xstray"}"#,
            "777",
        ),
    )
    .await
    .expect("the refund in the stray token");
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
