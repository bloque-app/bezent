// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-018, T-018-06 (migration 0144, amendment 0029 A2, A3, A5; `d-refund-dest-swaps`): the store's
//! side of a directed deposit. A directed arrival names its own destination and its own refund
//! address in the transaction that moved its value, so no request binds it: only the core's deposit
//! (`directed_deposit()`) and the core's return do, each with the observed recipient or refund and
//! nothing else, whoever writes (`REQ-IN-10`, `REQ-IN-11`, `REQ-RF-2`, `AC-A6.1`, `AC-A6.2`,
//! `AC-A6.4`, `AC-A6.6`, `INV-2`).
//!
//! Everything but the logins' own checks is written as the migration owner, whom these guards do
//! not exempt. The ingress's refusal by name (`ERR-SourceNotFound`) is
//! `services/ingress/tests/directed.rs`.

mod common;

use bezent_store::login::{as_login, ensure_login, pool_options};
use common::*;
use sqlx::PgPool;

const PASSWORD: &str = "test-only-directed-deposits-password";
const RESERVE: &str = "0xA0b86991c6218b36C1d19D4a2e9Eb0cE3606eB48";
const DIRECTING: &str = "0xD1Ec7ed00000000000000000000000000000d1ec";
const REFUND: &str = "0x52908400098527886E0F7030069857D2E4169EE7";
const RECIPIENT: &str = "0x8eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a48";
const OTHER_RECIPIENT: &str = "0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d";
const OTHER_ADDRESS: &str = "0x00000000000000000000000000000000000000ee";
const ZERO: &str = "0x0000000000000000000000000000000000000000";
const AMOUNT: i64 = 5_000_000;
/// Block 7 of `origin-a` with hash `\xaa`, log 1: the vector `refs.rs` pins.
const VECTOR_REFERENCE_7: &str = "bz-d-071b8b19756a50ad5929639577cdea067cc85a88";

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
               ('issuing', 'issuing', 'ss58'), ('origin-a', 'origin', 'evm'); \
             INSERT INTO assets (asset, precision, assurance_class) VALUES ('asset-a', 6, 'public'); \
             INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
               ('origin-a', 'token-a', 'asset-a', 6); \
             INSERT INTO reserves (asset, network, address) VALUES \
               ('asset-a', 'origin-a', '{RESERVE}')"
        ),
    )
    .await;
}

/// What the EVM endpoint records for a directed arrival: one observation at the `Transfer` log's
/// coordinate (`origin-a`, `block`, `\xaa`, log 1), at the reserve, with the issuing network's
/// recipient and the refund address in its detail.
async fn directed_with(pool: &PgPool, block: i64, kind: &str, refund: &str, token: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, token, amount, sender, recipient, executed, kind, detail) \
         VALUES ('origin-a', $1, '\\xaa', $2::timestamptz, 'log', 1, $3, $4, '0xfunder', $5, TRUE, \
             $6, jsonb_build_object('event', \
               CASE $6 WHEN 'directed_arrival' THEN 'DirectedArrival' ELSE 'UnpairedDirection' END, \
               'directing', $7::text, 'direction_log', 2, 'recipient', $8::text, \
               'refund', $9::text)) \
         RETURNING id",
    )
    .bind(block)
    .bind(BLOCK_TIME)
    .bind(token)
    .bind(AMOUNT)
    .bind(RESERVE)
    .bind(kind)
    .bind(DIRECTING)
    .bind(RECIPIENT)
    .bind(refund)
    .fetch_one(pool)
    .await
    .expect("observe a directed arrival")
}

async fn directed(pool: &PgPool, block: i64) -> i64 {
    directed_with(pool, block, "directed_arrival", REFUND, "token-a").await
}

/// Makes the observation `id` final at its block + 20, with that final head recorded.
async fn make_final(pool: &PgPool, id: i64, block: i64) {
    exec(
        pool,
        &format!(
            "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
               VALUES ('origin-a', {block} + 20, '\\xff', '{BLOCK_TIME}') ON CONFLICT DO NOTHING; \
             INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
               final_block_hash) VALUES ({id}, 'final', {block} + 20, '\\xff')"
        ),
    )
    .await;
}

async fn final_directed(pool: &PgPool, block: i64) -> i64 {
    let id = directed(pool, block).await;
    make_final(pool, id, block).await;
    id
}

/// The core's call, as the core's login.
async fn directed_deposit(core: &PgPool, block: i64) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar("SELECT directed_deposit('origin-a', $1, '\\xaa', 'log', 1)")
        .bind(block)
        .fetch_one(core)
        .await
}

async fn count_transfers(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM transfers")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A deposit row naming the directed arrival at `block`, in `Received`, as a writer that is not
/// the core would: the destination and the refund are the caller's.
fn deposit_sql(reference: &str, block: i64, destination: &str, refund: &str) -> String {
    format!(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, refund_destination_network, refund_destination, \
             state, source_network, source_block_number, source_block_hash, source_position_kind, \
             source_position_index) \
         VALUES ('{reference}', 'req-{reference}', 'deposit', 'asset-a', {AMOUNT}, 'issuing', \
             '{destination}', 'origin-a', '{refund}', 'Received', 'origin-a', {block}, '\\xaa', \
             'log', 1)"
    )
}

/// A return row (the refund of an unassociated arrival) naming the directed arrival at `block`.
fn return_sql(reference: &str, block: i64, destination: &str) -> String {
    format!(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, source_network, source_block_number, \
             source_block_hash, source_position_kind, source_position_index) \
         VALUES ('{reference}', 'req-{reference}', 'refund', 'asset-a', {AMOUNT}, 'origin-a', \
             '{destination}', 'Received', 'origin-a', {block}, '\\xaa', 'log', 1)"
    )
}

/// The derived `bz-u-` reference of the return of the directed arrival at `block`.
async fn return_reference(pool: &PgPool, block: i64) -> String {
    sqlx::query_scalar("SELECT refund_reference('origin-a', $1, '\\xaa', 'log', 1)")
        .bind(block)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// `REQ-IN-3`, `REQ-IN-10`: `deposit_reference()` derives the bridge reference as
/// `services/ingress/src/refs.rs::deposit` does, byte for byte. The three vectors are pinned in
/// `refs.rs` as well (`req_in_10_deposit_reference_vectors_the_store_derives_the_same`): a block
/// number of eight bytes, a hash of any length, the log index as a `u32`'s four bytes even above
/// `2^31`, and the two position kinds.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_10_the_bridge_reference_the_store_derives_is_the_one_the_ingress_derives(
    pool: PgPool,
) {
    for (network, block, hash, kind, index, expected) in [
        (
            "origin-a",
            7_i64,
            "\\xaa".to_owned(),
            "log",
            0_i64,
            "bz-d-f974333c8ffbef8f6e0e9b3bd970def281dd7767",
        ),
        (
            "origin-a",
            7,
            "\\xaa".to_owned(),
            "log",
            1,
            "bz-d-071b8b19756a50ad5929639577cdea067cc85a88",
        ),
        (
            "polygon",
            96_983_980,
            format!("\\x{}", "11".repeat(32)),
            "log",
            4_294_967_295,
            "bz-d-668330a314e9d999ee65c91f51fb6dedd62becfe",
        ),
        (
            "origin-a",
            1,
            "\\xab01".to_owned(),
            "transaction",
            3,
            "bz-d-d1321a13639f570b72f526e3b141f8770dbcd76b",
        ),
    ] {
        let derived: String =
            sqlx::query_scalar("SELECT deposit_reference($1, $2, $3::text::bytea, $4, $5)")
                .bind(network)
                .bind(block)
                .bind(&hash)
                .bind(kind)
                .bind(index)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(derived, expected, "{network} {block} {hash} {kind} {index}");
    }
}

/// `REQ-RQ-6`, `REQ-IN-11`: `refund_reference()` derives the return's bridge reference as
/// `services/ingress/src/refs.rs::unassociated_refund` does; the same four coordinates are pinned in
/// `refs.rs` (`req_in_11_refund_reference_vectors_the_store_derives_the_same`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_11_the_return_reference_the_store_derives_is_the_one_the_ingress_derives(
    pool: PgPool,
) {
    for (network, block, hash, kind, index, expected) in [
        (
            "origin-a",
            7_i64,
            "\\xaa".to_owned(),
            "log",
            0_i64,
            "bz-u-785300f9639f00608a29c3f84a185865db732118",
        ),
        (
            "origin-a",
            7,
            "\\xaa".to_owned(),
            "log",
            1,
            "bz-u-38661c655e8905bb6befb155263552f8bd0d1000",
        ),
        (
            "polygon",
            96_983_980,
            format!("\\x{}", "11".repeat(32)),
            "log",
            4_294_967_295,
            "bz-u-32a768ecd707102dccbf4e9ddc6156624b235afe",
        ),
        (
            "origin-a",
            1,
            "\\xab01".to_owned(),
            "transaction",
            3,
            "bz-u-00c2a6caea37e16941afee596e77c4dc6dee27a5",
        ),
    ] {
        let derived: String =
            sqlx::query_scalar("SELECT refund_reference($1, $2, $3::text::bytea, $4, $5)")
                .bind(network)
                .bind(block)
                .bind(&hash)
                .bind(kind)
                .bind(index)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(derived, expected, "{network} {block} {hash} {kind} {index}");
    }
}

/// `REQ-IN-10`, `AC-A6.1`, `INV-2`: the core's deposit for a final directed arrival begins at
/// `Source-Final` with the observed recipient as destination, the observed refund address as refund
/// destination on the arrival's network, the observed amount and its asset, the milestone of the
/// final head its observation was decided at, and no request; a second call makes nothing more.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_1_directed_deposit_makes_the_deposit_with_the_observed_recipient_and_refund(
    pool: PgPool,
) {
    declarations(&pool).await;
    final_directed(&pool, 7).await;
    let core = login(&pool, "dd_core_a_for_tests", bezent_store::CORE_ROLE).await;

    let id = directed_deposit(&core, 7)
        .await
        .expect("the core makes it")
        .expect("a deposit");
    assert_eq!(count_transfers(&pool).await, 1);

    let row: (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
        String,
    ) = sqlx::query_as(
        "SELECT bridge_reference, kind, state, asset, destination_network || ' ' || destination, \
                    refund_destination_network || ' ' || refund_destination, request_reference, \
                    source_final_network, source_final_block_number, \
                    amount::text \
             FROM transfers WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.0, VECTOR_REFERENCE_7);
    assert_eq!(
        (row.1.as_str(), row.2.as_str(), row.3.as_str()),
        ("deposit", "Source-Final", "asset-a")
    );
    assert_eq!(row.4, format!("issuing {RECIPIENT}"));
    assert_eq!(row.5, format!("origin-a {REFUND}"));
    assert!(
        row.6.starts_with("directed:origin-a:7:0xaa:log:1"),
        "the coordinate stands in for the request reference: {}",
        row.6
    );
    assert_eq!(
        (row.7.as_str(), row.8, row.9.as_str()),
        ("origin-a", 27, "5000000")
    );
    // The final head's own block time is the milestone's.
    assert!(
        flag(
            &pool,
            "SELECT source_final_block_time = '2026-10-13T00:00:00Z'::timestamptz \
               AND source_final_seen_at IS NOT NULL FROM transfers"
        )
        .await
    );
    // No request made or reached it (REQ-ID-4, A2).
    assert_eq!(
        text(&pool, "SELECT count(*)::text FROM intake_answers").await,
        "0"
    );
    // The creation is in the sequenced log (REQ-ST-4) and the view derives its state.
    assert_eq!(
        text(
            &pool,
            "SELECT count(*)::text FROM event_log WHERE kind = 'transfer' AND transfer_id IS NOT NULL"
        )
        .await,
        "1"
    );

    // INV-2: a second call is a no-op, and answers the same deposit.
    assert_eq!(directed_deposit(&core, 7).await.unwrap(), Some(id));
    assert_eq!(count_transfers(&pool).await, 1);
}

/// `REQ-IN-10`: it begins at `Source-Final` and so needs the observation final, executed and of an
/// admitted token; a coordinate that is not a directed arrival has no directed deposit.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_10_directed_deposit_refuses_what_is_not_a_final_directed_arrival_of_an_admitted_token(
    pool: PgPool,
) {
    declarations(&pool).await;
    let core = login(&pool, "dd_core_b_for_tests", bezent_store::CORE_ROLE).await;
    // Not final yet (INV-6).
    directed(&pool, 7).await;
    let err = directed_deposit(&core, 7).await.expect_err("not final");
    assert_refusal(&err, "REQ-IN-10", "a directed arrival that is not final");
    // A plain arrival at the coordinate is not a directed arrival.
    exec(
        &pool,
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, token, amount, sender, recipient, executed, kind) \
         VALUES ('origin-a', 8, '\\xaa', '2026-10-13T00:00:00Z', 'log', 1, 'token-a', 1000, \
             '0xfunder', '0xA0b86991c6218b36C1d19D4a2e9Eb0cE3606eB48', TRUE, 'arrival')",
    )
    .await;
    assert_refusal(
        &directed_deposit(&core, 8).await.expect_err("not directed"),
        "REQ-IN-10",
        "a plain arrival",
    );
    // A token no asset admits is returned, never minted (REQ-IN-4, REQ-IN-11).
    let id = directed_with(&pool, 9, "directed_arrival", REFUND, "token-unadmitted").await;
    make_final(&pool, id, 9).await;
    assert_refusal(
        &directed_deposit(&core, 9).await.expect_err("not admitted"),
        "REQ-IN-4",
        "a token no asset admits",
    );
    // Nothing was made, and nothing is at a coordinate that was never observed.
    assert_refusal(
        &directed_deposit(&core, 99).await.expect_err("unobserved"),
        "REQ-IN-10",
        "an unobserved coordinate",
    );
    assert_eq!(count_transfers(&pool).await, 0);
}

/// `REQ-IN-10`: only the core (and the migration owner) calls it; no other login holds `EXECUTE`.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_10_only_the_core_calls_directed_deposit(pool: PgPool) {
    declarations(&pool).await;
    final_directed(&pool, 7).await;
    for (name, role) in [
        ("dd_ingress_c_for_tests", bezent_store::INGRESS_ROLE),
        ("dd_evm_c_for_tests", bezent_store::EVM_ENDPOINT_ROLE),
        ("dd_signer_c_for_tests", bezent_store::SIGNER_ROLE),
        ("dd_hooks_c_for_tests", bezent_store::WEBHOOKS_ROLE),
    ] {
        let login = login(&pool, name, role).await;
        let err = directed_deposit(&login, 7).await.expect_err(role);
        assert_error_sqlstate(&err, "42501", role);
    }
    assert!(
        !flag(
            &pool,
            "SELECT has_function_privilege('public', \
               'directed_deposit(text,bigint,bytea,position_kind,bigint)', 'EXECUTE')"
        )
        .await
    );
    assert_eq!(count_transfers(&pool).await, 0);
}

/// `AC-A6.2`, `REQ-IN-10`, `REQ-RQ-3`, `REQ-RQ-6`: `source_bindable()` offers a directed arrival to
/// the core's two purposes and to no other, and offers the core's two purposes nothing else. The
/// ingress asks for `deposit` and `refund` alone (`intake::source`), so a requester's deposit or
/// refund naming one finds nothing there: `ERR-SourceNotFound`.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_2_source_bindable_has_exactly_two_carve_outs_for_a_directed_arrival(pool: PgPool) {
    declarations(&pool).await;
    final_directed(&pool, 7).await;
    let bindable = |purpose: &'static str, block: i64| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, bool>(
                "SELECT source_bindable($1, 'origin-a', $2, '\\xaa', 'log', 1)",
            )
            .bind(purpose)
            .bind(block)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    assert!(!bindable("deposit", 7).await, "a requester's deposit");
    assert!(!bindable("refund", 7).await, "a requester's refund");
    assert!(bindable("directed_deposit", 7).await, "the core's deposit");
    assert!(bindable("directed_return", 7).await, "the core's return");

    // Nothing else is bindable under them: every other kind, at the reserve, final.
    for (i, kind) in [
        "arrival",
        "other_arrival",
        "convertible_arrival",
        "conversion_output",
        "conversion_input",
        "payout_leg",
        "outflow",
        "self_transfer",
        "unpaired_direction",
        "transaction",
        "other",
    ]
    .into_iter()
    .enumerate()
    {
        let block = 100 + i64::try_from(i).unwrap();
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO observations (network, block_number, block_hash, block_time, \
                 position_kind, position_index, token, amount, sender, recipient, executed, kind) \
             VALUES ('origin-a', $1, '\\xaa', $2::timestamptz, 'log', 1, 'token-a', $3, '0xfunder', \
                 $4, TRUE, $5) RETURNING id",
        )
        .bind(block)
        .bind(BLOCK_TIME)
        .bind(AMOUNT)
        .bind(RESERVE)
        .bind(kind)
        .fetch_one(&pool)
        .await
        .unwrap();
        make_final(&pool, id, block).await;
        for purpose in ["directed_deposit", "directed_return"] {
            assert!(!bindable(purpose, block).await, "{kind} under {purpose}");
        }
    }
    // An unknown purpose is false, as before.
    assert!(!bindable("anything", 7).await);
}

/// `AC-A6.2`, `REQ-IN-10`: no login that makes requests binds a directed arrival: the ingress's
/// deposit or refund naming it is refused by the store, and no request is answered with a transfer
/// bound to one, whoever writes the answer (the migration owner included).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_2_a_requesters_deposit_or_refund_naming_a_directed_arrival_binds_nothing(
    pool: PgPool,
) {
    declarations(&pool).await;
    final_directed(&pool, 7).await;
    let ingress = login(&pool, "dd_ingress_d_for_tests", bezent_store::INGRESS_ROLE).await;
    let core = login(&pool, "dd_core_d_for_tests", bezent_store::CORE_ROLE).await;

    // The ingress cannot write either row, even with every field the observation records.
    for sql in [
        deposit_sql("bz-d-forged", 7, RECIPIENT, REFUND),
        return_sql("bz-u-forged", 7, REFUND),
    ] {
        let err = sqlx::raw_sql(&sql)
            .execute(&ingress)
            .await
            .expect_err("the ingress binds a directed arrival");
        assert_refusal(&err, "REQ-IN-10", &sql);
    }
    assert_eq!(count_transfers(&pool).await, 0);

    // It left nothing in the way: the core's deposit is made, and the coordinate is its.
    let id = directed_deposit(&core, 7).await.unwrap().expect("deposit");

    // A request answered with that deposit is refused whoever writes the answer.
    exec(
        &pool,
        "INSERT INTO requesters (requester) VALUES ('alpha'); \
         INSERT INTO intake (request_reference, requester, kind, body, body_digest) \
           VALUES ('ref-1', 'alpha', 'deposit', '{}', '\\x01')",
    )
    .await;
    assert_refused(
        &pool,
        &format!(
            "INSERT INTO intake_answers (intake_id, transfer_id, bridge_reference) \
             SELECT i.id, t.id, t.bridge_reference FROM intake i, transfers t \
             WHERE i.request_reference = 'ref-1' AND t.id = {id}"
        ),
        "REQ-IN-10",
    )
    .await;
    // A refusal names no transfer, and is not stopped (ERR-SourceNotFound answers this way).
    exec(
        &pool,
        "INSERT INTO intake_answers (intake_id, bridge_reference, error) \
         SELECT i.id, 'bz-r-refused', 'ERR-SourceNotFound' FROM intake i \
         WHERE i.request_reference = 'ref-1'",
    )
    .await;
    assert_eq!(
        text(
            &pool,
            "SELECT count(*)::text FROM intake_answers WHERE transfer_id IS NOT NULL"
        )
        .await,
        "0"
    );
}

/// `REQ-IN-10`, `REQ-RF-2`, `INV-2`: a transfer bound to a directed arrival carries the observed
/// recipient as destination and the observed refund as refund destination, and nothing else,
/// whoever writes it: the owner and the core, by insert, and by the update that binds a source.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_10_a_deposit_bound_to_a_directed_arrival_takes_the_observed_destination_and_refund(
    pool: PgPool,
) {
    declarations(&pool).await;
    final_directed(&pool, 7).await;
    final_directed(&pool, 8).await;
    let core = login(&pool, "dd_core_e_for_tests", bezent_store::CORE_ROLE).await;

    let refund_elsewhere = OTHER_ADDRESS;
    for (what, sql) in [
        (
            "another destination",
            deposit_sql(VECTOR_REFERENCE_7, 7, OTHER_RECIPIENT, REFUND),
        ),
        (
            "another refund destination",
            deposit_sql(VECTOR_REFERENCE_7, 7, RECIPIENT, refund_elsewhere),
        ),
        (
            "the funder as the refund destination",
            deposit_sql(VECTOR_REFERENCE_7, 7, RECIPIENT, "0xfunder"),
        ),
        (
            "a bridge reference that is not the coordinate's",
            deposit_sql("bz-d-0000", 7, RECIPIENT, REFUND),
        ),
    ] {
        assert_refused(&pool, &sql, "REQ-IN-10").await;
        // The core's own login is held to it too (and its writer guard admits the row's shape).
        let err = sqlx::raw_sql(&sql.replace("'Received'", "'Source-Final'"))
            .execute(&core)
            .await
            .expect_err(what);
        assert_refusal(&err, "REQ-IN-10", what);
    }
    // No refund destination at all.
    assert_refused(
        &pool,
        &format!(
            "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                 destination_network, destination, state, source_network, source_block_number, \
                 source_block_hash, source_position_kind, source_position_index) \
             VALUES ('{VECTOR_REFERENCE_7}', 'r', 'deposit', 'asset-a', {AMOUNT}, 'issuing', \
                 '{RECIPIENT}', 'Received', 'origin-a', 7, '\\xaa', 'log', 1)"
        ),
        "REQ-RF-2",
    )
    .await;
    // The refund on another network than the arrival's is refused (0049, now and for the owner).
    assert_eq!(count_transfers(&pool).await, 0);

    // The right row is the one that is accepted, as the owner would carry it (a restore).
    exec(
        &pool,
        &deposit_sql(VECTOR_REFERENCE_7, 7, RECIPIENT, REFUND),
    )
    .await;
    assert_eq!(count_transfers(&pool).await, 1);

    // The update that binds a source is held to it too: an unbound deposit moved onto the directed
    // arrival at block 8 with another destination is refused; with its own it is not.
    exec(
        &pool,
        &format!(
            "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                 destination_network, destination, refund_destination_network, refund_destination, \
                 state) \
             VALUES ('bz-d-unbound', 'req-unbound', 'deposit', 'asset-a', {AMOUNT}, 'issuing', \
                 '{OTHER_RECIPIENT}', 'origin-a', '{REFUND}', 'Received')"
        ),
    )
    .await;
    assert_refused(
        &pool,
        "UPDATE transfers SET source_network = 'origin-a', source_block_number = 8, \
             source_block_hash = '\\xaa', source_position_kind = 'log', source_position_index = 1 \
         WHERE bridge_reference = 'bz-d-unbound'",
        "REQ-IN-10",
    )
    .await;
}

/// `REQ-IN-11`, `REQ-RF-2`, `AC-A6.4`: the return is the core's refund of an unassociated arrival,
/// to the observed refund address on the arrival's network and nowhere else; the coordinate is
/// consumed once, by its deposit or by its return (`INV-2`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_4_the_core_returns_a_directed_arrival_only_to_the_observed_refund_address(
    pool: PgPool,
) {
    declarations(&pool).await;
    final_directed(&pool, 7).await;
    final_directed(&pool, 8).await;
    let core = login(&pool, "dd_core_f_for_tests", bezent_store::CORE_ROLE).await;

    for (what, sql) in [
        ("another address", return_sql("bz-u-a", 7, OTHER_ADDRESS)),
        ("the funder", return_sql("bz-u-b", 7, "0xfunder")),
        (
            "the destination of the deposit",
            return_sql("bz-u-c", 7, RECIPIENT),
        ),
    ] {
        let err = sqlx::raw_sql(&sql).execute(&core).await.expect_err(what);
        assert_refusal(&err, "REQ-IN-11", what);
        assert_refused(&pool, &sql, "REQ-IN-11").await;
    }
    // A return that names a refund destination of its own is not a return either.
    assert_refused(
        &pool,
        &format!(
            "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                 destination_network, destination, refund_destination_network, refund_destination, \
                 state, source_network, source_block_number, source_block_hash, \
                 source_position_kind, source_position_index) \
             VALUES ('bz-u-d', 'r', 'refund', 'asset-a', {AMOUNT}, 'origin-a', '{REFUND}', \
                 'origin-a', '{REFUND}', 'Received', 'origin-a', 7, '\\xaa', 'log', 1)"
        ),
        "REQ-IN-11",
    )
    .await;
    assert_eq!(count_transfers(&pool).await, 0);

    // A return whose bridge reference is not the one derived from the coordinate is refused, for the
    // core's login and the owner (a `bz-u-` of another coordinate included).
    let of_block_8 = return_reference(&pool, 8).await;
    for (what, reference) in [
        ("a made-up reference", "bz-u-0000".to_owned()),
        ("the reference of another coordinate", of_block_8),
        ("a deposit's reference", VECTOR_REFERENCE_7.to_owned()),
    ] {
        let sql = return_sql(&reference, 7, REFUND);
        let err = sqlx::raw_sql(&sql).execute(&core).await.expect_err(what);
        assert_refusal(&err, "REQ-IN-11", what);
        assert_refused(&pool, &sql, "REQ-IN-11").await;
    }
    assert_eq!(count_transfers(&pool).await, 0);

    // To the observed refund address, by the core's own login (no function: T-018-08 writes it).
    let reference_7 = return_reference(&pool, 7).await;
    let reference_8 = return_reference(&pool, 8).await;
    exec_as(&core, &return_sql(&reference_7, 7, REFUND)).await;
    assert_eq!(count_transfers(&pool).await, 1);
    // The coordinate has its return: its deposit is not made (a no-op), and a second return is
    // refused as a duplicate (INV-2).
    assert_eq!(directed_deposit(&core, 7).await.unwrap(), None);
    let err = sqlx::raw_sql(&return_sql(&reference_7, 7, REFUND))
        .execute(&core)
        .await
        .expect_err("a second return");
    assert_error_sqlstate(&err, "23505", "a second return");

    // The other way: with a deposit at the coordinate, no return is made.
    directed_deposit(&core, 8).await.unwrap().expect("deposit");
    let err = sqlx::raw_sql(&return_sql(&reference_8, 8, REFUND))
        .execute(&core)
        .await
        .expect_err("a return of a deposited arrival");
    assert_error_sqlstate(&err, "23505", "a return of a deposited arrival");
}

async fn exec_as(pool: &PgPool, sql: &str) {
    sqlx::raw_sql(sql)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// `REQ-IN-10`, `REQ-RF-2`: a refund of the zero address, of the vault or of the directing contract
/// is refused, as the contract refuses it, for the deposit and for the return, whoever writes, and
/// the address is compared as an `evm` network spells it (without case).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_rf_2_a_zero_vault_or_directing_refund_is_refused(pool: PgPool) {
    declarations(&pool).await;
    let core = login(&pool, "dd_core_g_for_tests", bezent_store::CORE_ROLE).await;
    let lower_reserve = RESERVE.to_lowercase();
    let upper_directing = DIRECTING.to_uppercase().replace("0X", "0x");
    for (i, (what, refund)) in [
        ("the zero address", ZERO.to_owned()),
        ("the vault", RESERVE.to_owned()),
        ("the vault, spelled without case", lower_reserve),
        ("the directing contract", DIRECTING.to_owned()),
        (
            "the directing contract, spelled without case",
            upper_directing,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let block = 10 + i64::try_from(i).unwrap();
        let id = directed_with(&pool, block, "directed_arrival", &refund, "token-a").await;
        make_final(&pool, id, block).await;
        let err = directed_deposit(&core, block).await.expect_err(what);
        assert_refusal(&err, "REQ-RF-2", what);
        assert_refused(
            &pool,
            &deposit_sql("bz-d-x", block, RECIPIENT, &refund),
            "REQ-RF-2",
        )
        .await;
        assert_refused(&pool, &return_sql("bz-u-x", block, &refund), "REQ-RF-2").await;
    }
    // A directed arrival with no refund recorded is never bound either.
    let id = directed_with(&pool, 30, "directed_arrival", "", "token-a").await;
    make_final(&pool, id, 30).await;
    assert_refusal(
        &directed_deposit(&core, 30).await.expect_err("no refund"),
        "REQ-RF-2",
        "no refund",
    );
    assert_eq!(count_transfers(&pool).await, 0);
}

/// `REQ-IN-10`, `REQ-RF-2`: an observation whose detail lacks the refund address, the recipient or
/// the directing contract (the key missing, or null) binds nothing: the directing check compares
/// nothing when its key is missing, so the guard refuses the arrival instead of passing it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_in_10_a_detail_missing_refund_recipient_or_directing_is_refused(pool: PgPool) {
    declarations(&pool).await;
    let core = login(&pool, "dd_core_j_for_tests", bezent_store::CORE_ROLE).await;
    let mut block = 40_i64;
    for key in ["refund", "recipient", "directing"] {
        for how in ["missing", "null"] {
            block += 1;
            let mut keys = vec![
                ("directing", DIRECTING),
                ("recipient", RECIPIENT),
                ("refund", REFUND),
            ];
            keys.retain(|(k, _)| *k != key || how == "null");
            let pairs: String = keys
                .iter()
                .map(|(k, v)| {
                    if *k == key {
                        format!(", '{k}', NULL::text")
                    } else {
                        format!(", '{k}', '{v}'::text")
                    }
                })
                .collect();
            // The observations are append-only: the detail is written as it should be read.
            let id: i64 = sqlx::query_scalar(&format!(
                "INSERT INTO observations (network, block_number, block_hash, block_time, \
                     position_kind, position_index, token, amount, sender, recipient, executed, \
                     kind, detail) \
                 VALUES ('origin-a', {block}, '\\xaa', '{BLOCK_TIME}'::timestamptz, 'log', 1, \
                     'token-a', {AMOUNT}, '0xfunder', '{RESERVE}', TRUE, 'directed_arrival', \
                     jsonb_build_object('event', 'DirectedArrival', 'direction_log', 2{pairs})) \
                 RETURNING id"
            ))
            .fetch_one(&pool)
            .await
            .unwrap();
            make_final(&pool, id, block).await;
            let what = format!("{key} {how}");
            let err = directed_deposit(&core, block).await.expect_err(&what);
            assert_refusal(&err, "REQ-IN-10", &what);
            assert_refused(
                &pool,
                &deposit_sql(&deposit_ref(&pool, block).await, block, RECIPIENT, REFUND),
                "REQ-IN-10",
            )
            .await;
            assert_refused(
                &pool,
                &return_sql(&return_reference(&pool, block).await, block, REFUND),
                "REQ-IN-10",
            )
            .await;
        }
    }
    assert_eq!(count_transfers(&pool).await, 0);
}

async fn deposit_ref(pool: &PgPool, block: i64) -> String {
    sqlx::query_scalar("SELECT deposit_reference('origin-a', $1, '\\xaa', 'log', 1)")
        .bind(block)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// `INV-2`: two connections binding the same coordinate at once, the core's deposit against the
/// core's return: the coordinate's lock serialises them, exactly one binds, and the other finds it
/// consumed (the deposit call answers nothing; a second return is a duplicate).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_2_a_concurrent_double_bind_on_the_coordinate_lock_binds_exactly_once(pool: PgPool) {
    declarations(&pool).await;
    final_directed(&pool, 7).await;
    let first = login(&pool, "dd_core_k1_for_tests", bezent_store::CORE_ROLE).await;
    let second = login(&pool, "dd_core_k2_for_tests", bezent_store::CORE_ROLE).await;
    let reference = return_reference(&pool, 7).await;

    // The first holds the coordinate (its return inserted, not yet committed) while the second
    // asks for the deposit: that call waits on the lock, and finds the return when it is committed.
    let mut tx = first.begin().await.unwrap();
    exec_tx(&mut tx, &return_sql(&reference, 7, REFUND)).await;
    let waiting = tokio::spawn(async move { directed_deposit(&second, 7).await });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(
        !waiting.is_finished(),
        "the second did not wait on the coordinate's lock"
    );
    tx.commit().await.unwrap();
    assert_eq!(waiting.await.unwrap().unwrap(), None);
    assert_eq!(count_transfers(&pool).await, 1);
    assert_eq!(
        text(&pool, "SELECT kind::text FROM transfers").await,
        "refund"
    );

    // And the plain race: both ask for the deposit at once on another coordinate; one makes it
    // and the other answers the same id, so there is exactly one.
    final_directed(&pool, 8).await;
    let (a, b) = tokio::join!(directed_deposit(&first, 8), {
        let pool = pool.clone();
        async move {
            let late = login(&pool, "dd_core_k3_for_tests", bezent_store::CORE_ROLE).await;
            directed_deposit(&late, 8).await
        }
    });
    let (a, b) = (
        a.unwrap().expect("a deposit"),
        b.unwrap().expect("a deposit"),
    );
    assert_eq!(a, b);
    assert_eq!(
        text(
            &pool,
            "SELECT count(*)::text FROM transfers WHERE kind = 'deposit'"
        )
        .await,
        "1"
    );
}

/// `REQ-IN-10`, `AC-A6.2`: the answer guard sees inserts only, so the binding guard refuses the
/// owner's update that would move an already-answered deposit onto a directed arrival.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_2_an_update_binding_an_answered_deposit_to_a_directed_arrival_is_refused(
    pool: PgPool,
) {
    declarations(&pool).await;
    final_directed(&pool, 7).await;
    let reference = deposit_ref(&pool, 7).await;
    exec(
        &pool,
        &format!(
            "INSERT INTO requesters (requester) VALUES ('alpha'); \
             INSERT INTO intake (request_reference, requester, kind, body, body_digest) \
               VALUES ('ref-1', 'alpha', 'deposit', '{{}}', '\\x01'); \
             INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                 destination_network, destination, refund_destination_network, \
                 refund_destination, state) \
               VALUES ('{reference}', 'ref-1', 'deposit', 'asset-a', {AMOUNT}, 'issuing', \
                 '{RECIPIENT}', 'origin-a', '{REFUND}', 'Received'); \
             INSERT INTO intake_answers (intake_id, transfer_id, bridge_reference) \
               SELECT i.id, t.id, t.bridge_reference FROM intake i, transfers t \
               WHERE i.request_reference = 'ref-1' AND t.bridge_reference = '{reference}'"
        ),
    )
    .await;
    // Every field the observation records: only the answer stands in the way.
    assert_refused(
        &pool,
        &format!(
            "UPDATE transfers SET source_network = 'origin-a', source_block_number = 7, \
                 source_block_hash = '\\xaa', source_position_kind = 'log', \
                 source_position_index = 1 WHERE bridge_reference = '{reference}'"
        ),
        "REQ-IN-10",
    )
    .await;
    assert!(
        flag(
            &pool,
            "SELECT source_network IS NULL FROM transfers WHERE kind = 'deposit'"
        )
        .await
    );
}

/// `INV-2`, `REQ-IN-10`: when the coordinate's deposit exists but is no longer live (Refused,
/// Withdrawn, Expired, Refunded), `directed_deposit()` does not answer its id as if it were the
/// arrival's live deposit: it refuses by name. A live one (here, `Source-Final`) still answers.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_2_directed_deposit_answers_a_live_deposit_and_refuses_a_dead_one(pool: PgPool) {
    declarations(&pool).await;
    final_directed(&pool, 7).await;
    let core = login(&pool, "dd_core_l_for_tests", bezent_store::CORE_ROLE).await;
    let id = directed_deposit(&core, 7).await.unwrap().expect("deposit");
    assert_eq!(directed_deposit(&core, 7).await.unwrap(), Some(id));
    for state in ["Refused", "Withdrawn", "Expired", "Refunded"] {
        exec(
            &pool,
            &format!(
                "ALTER TABLE transfers DISABLE TRIGGER USER; \
                 UPDATE transfers SET state = '{state}' WHERE id = {id}; \
                 ALTER TABLE transfers ENABLE TRIGGER USER"
            ),
        )
        .await;
        let err = directed_deposit(&core, 7).await.expect_err(state);
        assert_refusal(&err, "REQ-IN-10", state);
        assert!(
            err.as_database_error().unwrap().message().contains(state),
            "{err}"
        );
    }
    assert_eq!(count_transfers(&pool).await, 1);
}

/// `AC-A6.6`: an `unpaired_direction` is bindable by nothing: not by a deposit, not by a refund, not
/// by the core's two purposes, not by `directed_deposit()`, and no request is answered with a
/// transfer bound to one.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_6_an_unpaired_direction_is_bindable_by_nothing(pool: PgPool) {
    declarations(&pool).await;
    let id = directed_with(&pool, 7, "unpaired_direction", REFUND, "token-a").await;
    make_final(&pool, id, 7).await;
    let core = login(&pool, "dd_core_h_for_tests", bezent_store::CORE_ROLE).await;
    for purpose in ["deposit", "refund", "directed_deposit", "directed_return"] {
        assert!(
            !flag(
                &pool,
                &format!("SELECT source_bindable('{purpose}', 'origin-a', 7, '\\xaa', 'log', 1)")
            )
            .await,
            "{purpose}"
        );
    }
    assert_refused(
        &pool,
        &deposit_sql("bz-d-u", 7, RECIPIENT, REFUND),
        "REQ-RQ-3",
    )
    .await;
    assert_refused(&pool, &return_sql("bz-u-u", 7, REFUND), "REQ-RQ-6").await;
    assert_refusal(
        &directed_deposit(&core, 7).await.expect_err("unpaired"),
        "REQ-IN-10",
        "an unpaired direction",
    );
    assert_eq!(count_transfers(&pool).await, 0);
}

/// `REQ-ST-1`, `REQ-ST-4`, `INV-2`: nothing in the store that reads a deposit fails on one with no
/// answer. The views derive its state, its creation is in the sequenced log, and a final directed
/// arrival nothing binds yet is what an operator sees as unassociated until the core's deposit
/// consumes it. (The requests' readers, the webhooks' fan-out and the log are
/// `services/ingress/tests/directed.rs`.)
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_1_the_views_read_a_deposit_without_an_answer(pool: PgPool) {
    declarations(&pool).await;
    final_directed(&pool, 7).await;
    let core = login(&pool, "dd_core_i_for_tests", bezent_store::CORE_ROLE).await;
    assert_eq!(
        text(&pool, "SELECT count(*)::text FROM unassociated_arrivals").await,
        "1",
        "a final directed arrival no transfer binds yet"
    );
    directed_deposit(&core, 7).await.unwrap().expect("deposit");
    assert_eq!(
        text(&pool, "SELECT count(*)::text FROM unassociated_arrivals").await,
        "0",
        "the deposit consumed it"
    );
    assert_eq!(
        text(
            &pool,
            "SELECT state || '/' || derived_state FROM transfer_status"
        )
        .await,
        "Source-Final/Source-Final"
    );
    assert_eq!(text(&pool, "SELECT count(*)::text FROM reviews").await, "0");
    // The core reads it as it reads any transfer, through its own login.
    assert_eq!(
        text_as(
            &core,
            "SELECT count(*)::text FROM transfers WHERE kind = 'deposit'"
        )
        .await,
        "1"
    );
}

async fn text_as(pool: &PgPool, sql: &str) -> String {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}
