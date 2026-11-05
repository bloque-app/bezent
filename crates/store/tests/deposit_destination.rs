// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #45 (migration 0080): a deposit's destination on the issuing network decodes to a 32-byte
//! account when the transfer is created, not after the arrival is final (`INV-6`, `REQ-IN-4`;
//! `ERR-InvalidDestination`). Checked through real service logins, each a member of one role.
//!
//! The SS58 checksum is not checked here (the store has no BLAKE2); the addresses below carry a
//! real one, computed for the account `0x0102...20`, so they are also what the ingress accepts.

mod common;

use bezent_store::login::{as_login, ensure_login, pool_options};
use common::*;
use sqlx::PgPool;

const PASSWORD: &str = "test-only-deposit-destination-password";

/// The 32-byte account every valid address below names.
const ACCOUNT: &str = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
/// SS58, one-byte prefix 42.
const SS58_ONE_BYTE: &str = "5C62W7ELLAAfjCQeBU3me9ykaYomD8XTg2B9Hk6ki6Cm3v58";
/// SS58, one-byte prefix 0 (the address starts with a base58 `1`, a leading zero byte).
const SS58_PREFIX_ZERO: &str = "12KeSVQBwS9AjRA976mnJouSAoQuS5bkWudT367GBEHE8Ls";
/// SS58, two-byte prefix (network 1284).
const SS58_TWO_BYTES: &str = "VdqYr7qfJJfAErn9e34382jVy3j3sSxqDWa34zvrhCyaZg411";
/// `SS58_ONE_BYTE` with one byte after the checksum.
const SS58_TRAILING_BYTE: &str = "KVwBdUVrKZTfwFMM7CPE2zdVM5PQ8YDSjEBmauEPvMxuRsFC2";

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

/// `issuing` (ss58, the network mints are ordered on), `other-ss58` and `attested` (origin
/// networks, the latter of the `opaque` format an attested network declares) and `origin-a`.
async fn declarations(pool: &PgPool) {
    exec(
        pool,
        "INSERT INTO networks (network, role, address_format) VALUES \
           ('issuing', 'issuing', 'ss58'), ('other-ss58', 'origin', 'ss58'), \
           ('attested', 'origin', 'opaque'), ('origin-a', 'origin', 'opaque'); \
         INSERT INTO assets (asset, precision, assurance_class) VALUES ('asset-a', 6, 'public'); \
         INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
           ('origin-a', 'token-a', 'asset-a', 6); \
         INSERT INTO reserves (asset, network, address) VALUES ('asset-a', 'origin-a', '0xreserve'); \
         INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, token, amount, sender, recipient, executed, kind) \
         SELECT 'origin-a', 10, '\\xaa', now(), 'log', i, 'token-a', 1000, '0xsender', \
             '0xreserve', TRUE, 'arrival' FROM generate_series(0, 39) i",
    )
    .await;
}

fn transfer(kind: &str, reference: &str, network: &str, destination: &str) -> String {
    format!(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state) \
         VALUES ('{reference}', 'req-{reference}', '{kind}', 'asset-a', 1000, '{network}', \
             '{destination}', 'Received')"
    )
}

/// A deposit to `destination` on the issuing network, as the ingress binds it to the arrival at
/// log `index` of the origin network (`Received`).
fn deposit(reference: &str, destination: &str, index: usize) -> String {
    format!(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index) \
         VALUES ('{reference}', 'req-{reference}', 'deposit', 'asset-a', 1000, 'issuing', \
             '{destination}', 'Received', 'origin-a', 10, '\\xaa', 'log', {index})"
    )
}

/// The same deposit before it is bound: what a request the ingress refuses is recorded as.
fn unbound(reference: &str, destination: &str) -> String {
    transfer("deposit", reference, "issuing", destination)
}

fn transition(reference: &str, to: &str, error: &str) -> String {
    format!(
        "INSERT INTO transitions (transfer_id, from_state, to_state, error) \
         SELECT id, 'Received', '{to}', {error} FROM transfers WHERE bridge_reference = '{reference}'"
    )
}

async fn count(pool: &PgPool, reference: &str) -> i64 {
    sqlx::query_scalar(&format!(
        "SELECT count(*) FROM transfers WHERE bridge_reference = '{reference}'"
    ))
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A destination that does not decode to a 32-byte account is refused when the deposit is bound, and
/// the refusal names the requirement; nothing is created.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_4_a_deposit_destination_that_does_not_decode_is_refused_at_insert(pool: PgPool) {
    declarations(&pool).await;
    let ingress = login(
        &pool,
        "deposit_dest_ingress_tests",
        bezent_store::INGRESS_ROLE,
    )
    .await;
    let short_hex = format!("0x{}", &ACCOUNT[..40]); // 20 bytes: an EVM address
    let long_hex = format!("0x{ACCOUNT}00"); // 33 bytes
    let not_hex = format!("0x{}zz", &ACCOUNT[..62]);
    let no_prefix = ACCOUNT.to_owned(); // 64 hex digits without 0x
    let upper_x = format!("0X{ACCOUNT}");
    let truncated = SS58_ONE_BYTE[..SS58_ONE_BYTE.len() - 2].to_owned();
    let cases: [(&str, String); 9] = [
        ("a 20-byte address", short_hex),
        ("0x and 66 hex digits", long_hex),
        ("0x and 64 characters, two not hex", not_hex),
        ("64 hex digits without 0x", no_prefix),
        ("0X (upper-case x)", upper_x),
        (
            "an SS58 address with a trailing byte",
            SS58_TRAILING_BYTE.to_owned(),
        ),
        ("an SS58 address cut short", truncated),
        ("characters outside base58", "0OIl0OIl".to_owned()),
        ("an opaque string", "dest-1".to_owned()),
    ];
    for (i, (what, destination)) in cases.iter().enumerate() {
        let reference = format!("bz-d-bad-{i}");
        let err = sqlx::raw_sql(&deposit(&reference, destination, i))
            .execute(&ingress)
            .await
            .expect_err(what);
        assert_refusal(&err, "ERR-InvalidDestination", what);
        assert!(
            err.to_string().contains("32-byte account"),
            "{what}: the refusal does not name the requirement: {err}"
        );
        assert_eq!(count(&pool, &reference).await, 0, "{what}: a row was made");
    }
}

/// `0x` and 64 hex digits in either case, and a valid one- or two-byte-prefix SS58 address, are
/// accepted, whatever the writer's role; the checksum is the ingress's.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_4_a_destination_that_decodes_is_still_accepted(pool: PgPool) {
    declarations(&pool).await;
    let ingress = login(
        &pool,
        "deposit_dest_ingress_tests",
        bezent_store::INGRESS_ROLE,
    )
    .await;
    let lower = format!("0x{ACCOUNT}");
    let upper = format!("0x{}", ACCOUNT.to_uppercase());
    let mixed = format!("0x{}{}", ACCOUNT[..32].to_uppercase(), &ACCOUNT[32..]);
    let cases = [
        ("0x and 64 lower-case hex digits", lower),
        ("0x and 64 upper-case hex digits", upper),
        ("0x and 64 mixed-case hex digits", mixed),
        ("SS58, one-byte prefix", SS58_ONE_BYTE.to_owned()),
        ("SS58, prefix 0", SS58_PREFIX_ZERO.to_owned()),
        ("SS58, two-byte prefix", SS58_TWO_BYTES.to_owned()),
    ];
    for (i, (what, destination)) in cases.iter().enumerate() {
        let reference = format!("bz-d-good-{i}");
        sqlx::raw_sql(&deposit(&reference, destination, i))
            .execute(&ingress)
            .await
            .unwrap_or_else(|e| panic!("{what} refused: {e}"));
        assert_eq!(count(&pool, &reference).await, 1, "{what}");
    }
}

/// A request the ingress refuses with `ERR-InvalidDestination` is recorded as it always was: the
/// transfer binds no source, carries the destination it was refused for, and moves to `Refused`.
/// But it can never be accepted, and a bad destination cannot be bound to an arrival after the
/// fact either (what the issue asks: refused when the transfer is "inserted or accepted").
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_4_the_ingress_still_records_a_refused_destination_and_never_accepts_it(
    pool: PgPool,
) {
    declarations(&pool).await;
    let ingress = login(
        &pool,
        "deposit_dest_ingress_tests",
        bezent_store::INGRESS_ROLE,
    )
    .await;
    for (i, destination) in [
        SS58_TRAILING_BYTE.to_owned(),
        format!("0x{}", &ACCOUNT[..40]),
    ]
    .iter()
    .enumerate()
    {
        let reference = format!("bz-r-bad-{i}");
        sqlx::raw_sql(&unbound(&reference, destination))
            .execute(&ingress)
            .await
            .unwrap_or_else(|e| panic!("the refused request was not recorded: {e}"));
        sqlx::raw_sql(&transition(
            &reference,
            "Refused",
            "'ERR-InvalidDestination'",
        ))
        .execute(&ingress)
        .await
        .unwrap_or_else(|e| panic!("the refusal was not recorded: {e}"));
        assert_eq!(
            text(
                &pool,
                &format!(
                    "SELECT state::text FROM transfers WHERE bridge_reference = '{reference}'"
                )
            )
            .await,
            "Refused"
        );
        // A second one, still `Received`, is never accepted.
        let open = format!("bz-r-open-{i}");
        sqlx::raw_sql(&unbound(&open, destination))
            .execute(&ingress)
            .await
            .unwrap();
        let err = sqlx::raw_sql(&transition(&open, "Accepted", "NULL"))
            .execute(&ingress)
            .await
            .expect_err("accepted with a destination that does not decode");
        assert_refusal(&err, "ERR-InvalidDestination", "its acceptance");
    }
    // The same request with a destination that decodes is accepted, as before.
    sqlx::raw_sql(&unbound("bz-r-good", SS58_ONE_BYTE))
        .execute(&ingress)
        .await
        .unwrap();
    sqlx::raw_sql(&transition("bz-r-good", "Accepted", "NULL"))
        .execute(&ingress)
        .await
        .expect("a destination that decodes is accepted");
}

/// Only a deposit to the issuing network is read: a deposit to another network, withdrawals and
/// refunds (the destinations of which are on origin networks, or the redeemer's) are as before,
/// and so is a deposit on an issuing network that has no 32-byte account format.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_4_withdrawals_refunds_and_other_networks_are_unaffected(pool: PgPool) {
    declarations(&pool).await;
    let ingress = login(
        &pool,
        "deposit_dest_ingress_tests",
        bezent_store::INGRESS_ROLE,
    )
    .await;
    let core = login(&pool, "deposit_dest_core_tests", bezent_store::CORE_ROLE).await;

    // A deposit not to the issuing network (ss58 or opaque origin), even if the text would not
    // decode: the check is for the account a mint goes to.
    for (i, network) in ["other-ss58", "attested", "origin-a"].iter().enumerate() {
        let reference = format!("bz-d-net-{i}");
        sqlx::raw_sql(&transfer("deposit", &reference, network, "not-an-account"))
            .execute(&ingress)
            .await
            .unwrap_or_else(|e| panic!("deposit to {network}: {e}"));
    }
    // The refund of an unassociated arrival, to any destination on any network: the ingress.
    for (i, network) in ["issuing", "other-ss58", "attested", "origin-a"]
        .iter()
        .enumerate()
    {
        let reference = format!("bz-u-net-{i}");
        sqlx::raw_sql(&transfer("refund", &reference, network, "not-an-account"))
            .execute(&ingress)
            .await
            .unwrap_or_else(|e| panic!("refund to {network}: {e}"));
    }
    // A withdrawal, as the core makes it from a redemption on the issuing network, to a payee on
    // an origin network (an attested one included), whatever the payee's spelling.
    for (i, network) in ["attested", "origin-a", "other-ss58"].iter().enumerate() {
        let sql = format!(
            "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                 destination_network, destination, state, redeemer, on_chain_reference, \
                 source_network, source_block_number, source_block_hash, source_position_kind, \
                 source_position_index) \
             VALUES ('bz-w-net-{i}', 'redemption-{i}', 'withdrawal', 'asset-a', 1000, '{network}', \
                 'not-an-account', 'Received', 'redeemer-{i}', '\\x7{i}', 'issuing', {i}, \
                 '\\xaa', 'log', 0)"
        );
        sqlx::raw_sql(&sql)
            .execute(&core)
            .await
            .unwrap_or_else(|e| panic!("withdrawal to {network}: {e}"));
    }

    // A deployment whose issuing network is not `ss58` (a test network of `opaque` addresses, as
    // the other store tests declare): no 32-byte account to decode, so nothing to refuse.
    exec(
        &pool,
        "UPDATE networks SET address_format = 'opaque' WHERE network = 'issuing'",
    )
    .await;
    sqlx::raw_sql(&deposit("bz-d-opaque", "dest-1", 0))
        .execute(&ingress)
        .await
        .expect("a deposit to an issuing network of opaque addresses");
}

/// Rows that exist: the migration applies over a deposit with an undecodable destination, which
/// stays as it is (the triggers fire on insert only); the owner (drills, restores) may still insert
/// one; a deposit that is still `Received` is not accepted, as it could not be minted; and a
/// destination cannot be edited into one afterwards (`REQ-ST-7`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_4_existing_rows_are_untouched_and_the_owner_is_not_held_to_it(pool: PgPool) {
    declarations(&pool).await;
    // The owner's insert (a restore) is not refused.
    exec(&pool, &deposit("bz-d-owner", "dest-1", 0)).await;
    // A row from before 0080 (the owner's insert stands for it), still `Received`.
    exec(&pool, &deposit("bz-d-legacy", "dest-legacy", 1)).await;
    assert_eq!(count(&pool, "bz-d-legacy").await, 1);
    assert_eq!(
        text(
            &pool,
            "SELECT destination FROM transfers WHERE bridge_reference = 'bz-d-legacy'"
        )
        .await,
        "dest-legacy"
    );
    let ingress = login(
        &pool,
        "deposit_dest_ingress_tests",
        bezent_store::INGRESS_ROLE,
    )
    .await;
    let err = sqlx::raw_sql(&transition("bz-d-legacy", "Accepted", "NULL"))
        .execute(&ingress)
        .await
        .expect_err("a legacy deposit that cannot be minted was accepted");
    assert_refusal(&err, "ERR-InvalidDestination", "its acceptance");
    assert_refused(
        &pool,
        "UPDATE transfers SET destination = 'other' WHERE bridge_reference = 'bz-d-legacy'",
        "REQ-ST-7",
    )
    .await;
}
