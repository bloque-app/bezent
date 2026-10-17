// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Migration 0036: the two steps of a delivery are paired in the store (`INV-9`; PLAN `DEC-3`).
//! PR #39 second review MF1, and issue #38. Every check logs in as a role of its own: one pool,
//! one login, no role switched on a pooled connection (#27).

mod common;

use sqlx::PgPool;

const LOGIN_PASSWORD: &str = "test-only-delivery-pairing-login";

/// A login on `pool`'s database named `name` holding `roles`, created once per server, with a
/// test-only password.
async fn login(pool: &PgPool, name: &str, roles: &[&str]) -> PgPool {
    let grants: String = roles
        .iter()
        .map(|r| format!("BEGIN GRANT {r} TO {name}; EXCEPTION WHEN OTHERS THEN NULL; END; "))
        .collect();
    common::exec(
        pool,
        &format!(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{name}') THEN \
                 BEGIN CREATE ROLE {name} LOGIN PASSWORD '{LOGIN_PASSWORD}'; \
                 EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END; \
               END IF; {grants} \
             END $$;"
        ),
    )
    .await;
    let options = (*pool.connect_options())
        .clone()
        .username(name)
        .password(LOGIN_PASSWORD);
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap_or_else(|e| panic!("log in as {name}: {e}"))
}

async fn declare(pool: &PgPool) {
    common::exec(
        pool,
        "INSERT INTO networks (network, role, address_format) VALUES \
           ('kreivo', 'issuing', 'ss58'), ('polygon', 'origin', 'evm')",
    )
    .await;
}

/// Records a delivery order on `network`, as the core would; returns its id.
async fn order(pool: &PgPool, network: &str, purpose: &str, reference: u8) -> i64 {
    sqlx::query_scalar(&format!(
        "INSERT INTO intents (network, purpose, on_chain_reference, payload) \
         VALUES ('{network}', '{purpose}', '\\x{reference:02x}', \
           '{{\"order\": {{}}, \"amount\": \"1\"}}') RETURNING id"
    ))
    .fetch_one(pool)
    .await
    .expect("record the order")
}

/// The insert of a prepared transaction for `order`, labelled `network`, with `message`.
fn prepared(order: i64, network: &str, message: u8) -> String {
    format!(
        "INSERT INTO prepared_transactions (order_intent_id, attempt, network, lane, key_id, \
           nonce, valid_from_block, valid_until_block, checkpoint_hash, call, signing_payload, \
           message, dry_run_block_number, dry_run_block_hash, dry_run_passed, dry_run) \
         VALUES ({order}, 1, '{network}', '\\x01', 'k', 0, 0, 9223372036854775807, '\\x01', \
           '\\x01', '\\x{message:02x}', '\\x{message:02x}', 0, '\\x01', true, '{{}}') RETURNING id"
    )
}

async fn insert(pool: &PgPool, sql: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(sql).fetch_one(pool).await
}

fn assert_refused<T: std::fmt::Debug>(out: Result<T, sqlx::Error>, what: &str) {
    let err = out.expect_err(&format!("the store accepted: {what}"));
    common::assert_refusal(&err, "INV-", what);
}

/// PR #39 second review MF1 (`INV-9`, `INV-8`): an EVM endpoint's login cannot write a Kreivo
/// order's prepared transaction, whatever network the row names, so the Kreivo key is never asked
/// to sign bytes the EVM endpoint chose. Nor can any login pair a prepared row with an order of
/// another network, and the Kreivo endpoint's login never prepares an EVM order.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn mf1_inv_9_an_evm_endpoint_login_cannot_prepare_a_kreivo_order(pool: PgPool) {
    declare(&pool).await;
    let kreivo_order = order(&pool, "kreivo", "mint", 1).await;
    let polygon_order = order(&pool, "polygon", "payout", 2).await;

    let evm = login(
        &pool,
        "evm_endpoint_pairing_for_tests",
        &[bezent_store::EVM_ENDPOINT_ROLE],
    )
    .await;
    // The reviewer's scenario: a `polygon` row for a Kreivo order.
    assert_refused(
        insert(&evm, &prepared(kreivo_order, "polygon", 0xaa)).await,
        "an EVM endpoint's polygon row for a Kreivo order",
    );
    assert_refused(
        insert(&evm, &prepared(kreivo_order, "kreivo", 0xaa)).await,
        "an EVM endpoint's kreivo row for a Kreivo order",
    );
    // Its own network's orders it prepares.
    insert(&evm, &prepared(polygon_order, "polygon", 0xbb))
        .await
        .expect("the EVM endpoint prepares a Polygon order");
    evm.close().await;

    let kreivo_order_2 = order(&pool, "kreivo", "mint", 3).await;
    let polygon_order_2 = order(&pool, "polygon", "payout", 4).await;
    let kreivo = login(
        &pool,
        "kreivo_endpoint_pairing_for_tests",
        &[bezent_store::KREIVO_ENDPOINT_ROLE],
    )
    .await;
    assert_refused(
        insert(&kreivo, &prepared(polygon_order_2, "polygon", 0xcc)).await,
        "the Kreivo endpoint preparing a Polygon order",
    );
    assert_refused(
        insert(&kreivo, &prepared(kreivo_order_2, "polygon", 0xcc)).await,
        "the Kreivo endpoint's polygon row for a Kreivo order",
    );
    insert(&kreivo, &prepared(kreivo_order_2, "kreivo", 0xcc))
        .await
        .expect("the Kreivo endpoint prepares a Kreivo order");
    kreivo.close().await;

    // The migration owner is held to the pairing too: a row on another network than its order's,
    // or for an intent that is not an order.
    assert_refused(
        insert(&pool, &prepared(polygon_order_2, "kreivo", 0xdd)).await,
        "the owner's kreivo row for a Polygon order",
    );
    let not_an_order: i64 = sqlx::query_scalar(
        "INSERT INTO intents (network, purpose, payload) \
         VALUES ('kreivo', 'mint', '{\"amount\": \"1\"}') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_refused(
        insert(&pool, &prepared(not_an_order, "kreivo", 0xdd)).await,
        "a prepared row for an intent that is not an order",
    );
}

/// A signing intent's insert: on `network`, of `purpose`, with on-chain reference `reference`,
/// carrying `payload` (JSON text).
fn signing(network: &str, purpose: &str, reference: u8, payload: &str) -> String {
    format!(
        "INSERT INTO intents (network, purpose, on_chain_reference, payload) \
         VALUES ('{network}', '{purpose}', '\\x{reference:02x}', '{payload}') RETURNING id"
    )
}

/// The payload of a signing intent naming `prepared`, with message byte `message` and `amount`.
fn over(prepared: i64, message: u8, amount: &str) -> String {
    format!(r#"{{"message": "0x{message:02x}", "amount": "{amount}", "prepared": {prepared}}}"#)
}

/// The core's login on `pool`'s database (only the core records intents).
async fn core(pool: &PgPool) -> PgPool {
    login(pool, "core_pairing_for_tests", &[bezent_store::CORE_ROLE]).await
}

/// Issue #38 (`INV-9`, PLAN `DEC-3`): a signing intent names a prepared transaction of its own
/// order. One that points at another order's prepared row, or at a prepared row of another
/// network or purpose, is refused to the core's login; the core's own signing intent is accepted.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_38_inv_9_a_forged_intent_naming_another_orders_bytes_is_refused(pool: PgPool) {
    declare(&pool).await;
    let order_a = order(&pool, "kreivo", "mint", 0x0a).await;
    let order_b = order(&pool, "kreivo", "mint", 0x0b).await;
    let polygon_order = order(&pool, "polygon", "payout", 0x0c).await;
    let prepared_a = insert(&pool, &prepared(order_a, "kreivo", 0xa1))
        .await
        .unwrap();
    let prepared_b = insert(&pool, &prepared(order_b, "kreivo", 0xb1))
        .await
        .unwrap();
    let prepared_polygon = insert(&pool, &prepared(polygon_order, "polygon", 0xc1))
        .await
        .unwrap();

    let core = core(&pool).await;
    // Order A's intent, naming order B's prepared bytes.
    assert_refused(
        insert(
            &core,
            &signing("kreivo", "mint", 0x0a, &over(prepared_b, 0xb1, "1")),
        )
        .await,
        "an intent for order A naming order B's prepared transaction",
    );
    // A Kreivo intent over a Polygon order's prepared bytes, and the other way round.
    assert_refused(
        insert(
            &core,
            &signing("kreivo", "mint", 0x0c, &over(prepared_polygon, 0xc1, "1")),
        )
        .await,
        "a Kreivo intent naming a Polygon prepared transaction",
    );
    assert_refused(
        insert(
            &core,
            &signing("polygon", "payout", 0x0a, &over(prepared_a, 0xa1, "1")),
        )
        .await,
        "a Polygon intent naming a Kreivo prepared transaction",
    );
    // Another purpose than its order's, and a prepared row that does not exist.
    assert_refused(
        insert(
            &core,
            &signing("kreivo", "refund-mint", 0x0a, &over(prepared_a, 0xa1, "1")),
        )
        .await,
        "an intent of another purpose than its order",
    );
    assert_refused(
        insert(
            &core,
            &signing("kreivo", "mint", 0x0a, &over(999_999, 0xa1, "1")),
        )
        .await,
        "an intent naming no prepared transaction",
    );
    // The core's intent over its own order's prepared bytes.
    insert(
        &core,
        &signing("kreivo", "mint", 0x0a, &over(prepared_a, 0xa1, "1")),
    )
    .await
    .expect("the core records order A's signing intent");
    core.close().await;
}

/// PR #39 third review MF-A (`INV-9`, issue #38, `AC-I1.2`): a compromised core records, for
/// order A, an intent carrying order B's prepared bytes, under A's own preparation or under none;
/// or A's bytes with another amount than A's order. The store refuses every one: a signing intent
/// carries exactly the bytes of the prepared transaction it names, and its order's amount, and
/// any intent carrying bytes to sign names its preparation. (The signer's own refusal, with this
/// check off, is `mf_a_inv_9_the_signer_refuses_bytes_that_are_not_its_prepared_transactions` in
/// services/signer/tests/guards.rs.)
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn mf_a_inv_9_an_intent_for_order_a_carrying_order_bs_bytes_is_refused(pool: PgPool) {
    declare(&pool).await;
    let order_a = order(&pool, "kreivo", "mint", 0x0a).await;
    let order_b = order(&pool, "kreivo", "mint", 0x0b).await;
    let prepared_a = insert(&pool, &prepared(order_a, "kreivo", 0xa1))
        .await
        .unwrap();
    insert(&pool, &prepared(order_b, "kreivo", 0xb1))
        .await
        .unwrap();
    // A preparation whose dry run failed.
    let order_c = order(&pool, "kreivo", "mint", 0x0d).await;
    let failed: i64 = insert(
        &pool,
        &prepared(order_c, "kreivo", 0xd1).replace(", true, '{}')", ", false, '{}')"),
    )
    .await
    .unwrap();

    let core = core(&pool).await;
    // The reviewer's scenario: A's preparation named, B's bytes carried.
    assert_refused(
        insert(
            &core,
            &signing("kreivo", "mint", 0x0a, &over(prepared_a, 0xb1, "1")),
        )
        .await,
        "an intent for order A naming A's preparation and carrying B's bytes",
    );
    // B's bytes and no preparation named at all; and bytes with no message to name them.
    assert_refused(
        insert(
            &core,
            &signing(
                "kreivo",
                "mint",
                0x0a,
                r#"{"message": "0xb1", "amount": "1"}"#,
            ),
        )
        .await,
        "an intent carrying bytes and naming no preparation",
    );
    assert_refused(
        insert(
            &core,
            &signing(
                "kreivo",
                "mint",
                0x0a,
                &format!(r#"{{"amount": "1", "prepared": {prepared_a}}}"#),
            ),
        )
        .await,
        "an intent naming a preparation and carrying no bytes",
    );
    // A's bytes, another amount than A's order.
    assert_refused(
        insert(
            &core,
            &signing("kreivo", "mint", 0x0a, &over(prepared_a, 0xa1, "2")),
        )
        .await,
        "an intent claiming another amount than its order",
    );
    assert_refused(
        insert(
            &core,
            &signing(
                "kreivo",
                "mint",
                0x0a,
                &format!(r#"{{"message": "0xa1", "amount": 1, "prepared": {prepared_a}}}"#),
            ),
        )
        .await,
        "an intent whose amount is not a decimal string",
    );
    // A preparation whose dry run failed is signed for by no intent.
    assert_refused(
        insert(
            &core,
            &signing("kreivo", "mint", 0x0d, &over(failed, 0xd1, "1")),
        )
        .await,
        "an intent over a preparation whose dry run failed",
    );
    // The exact bytes and amount of its own order's preparation.
    insert(
        &core,
        &signing("kreivo", "mint", 0x0a, &over(prepared_a, 0xa1, "1")),
    )
    .await
    .expect("the core records order A's signing intent");
    core.close().await;
}

/// PR #39 third review SF-A (`INV-9`, `NFR-7`): no session is exempt from the signing-intent
/// check, the migration owner's and a superuser's included (the test's own login is both). A
/// restore does not need the exemption: plain pg_dump creates the trigger after the data (the
/// restore drill restores signing intents with the check in force).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn sf_a_inv_9_the_owner_and_a_superuser_are_held_to_the_signing_intent_check(pool: PgPool) {
    declare(&pool).await;
    let superuser: bool =
        sqlx::query_scalar("SELECT rolsuper FROM pg_roles WHERE rolname = session_user")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(superuser, "the test login is the owner and a superuser");
    let order_a = order(&pool, "kreivo", "mint", 0x0a).await;
    let order_b = order(&pool, "kreivo", "mint", 0x0b).await;
    let prepared_a = insert(&pool, &prepared(order_a, "kreivo", 0xa1))
        .await
        .unwrap();
    let prepared_b = insert(&pool, &prepared(order_b, "kreivo", 0xb1))
        .await
        .unwrap();
    assert_refused(
        insert(
            &pool,
            &signing("kreivo", "mint", 0x0a, &over(prepared_b, 0xb1, "1")),
        )
        .await,
        "the owner's intent for order A naming B's preparation",
    );
    assert_refused(
        insert(
            &pool,
            &signing("kreivo", "mint", 0x0a, &over(prepared_a, 0xb1, "1")),
        )
        .await,
        "the owner's intent for order A carrying B's bytes",
    );
    assert_refused(
        insert(
            &pool,
            &signing(
                "kreivo",
                "mint",
                0x0a,
                r#"{"message": "0xa1", "amount": "1"}"#,
            ),
        )
        .await,
        "the owner's intent naming no preparation",
    );
    insert(
        &pool,
        &signing("kreivo", "mint", 0x0a, &over(prepared_a, 0xa1, "1")),
    )
    .await
    .expect("the owner records a signing intent of its own order's bytes");
}

/// PR #39 third review SF-B (`REQ-KY-4`, `INV-9`), migration 0038: one EVM key binds to exactly
/// one network. A lane key that signed for Polygon signs for Base under no key id and no public
/// key, and a lane that prepared for Polygon prepares nothing for Base; other keys and lanes do.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn sf_b_req_ky_4_one_evm_key_binds_to_exactly_one_network(pool: PgPool) {
    declare(&pool).await;
    common::exec(
        &pool,
        "INSERT INTO networks (network, role, address_format) VALUES ('base', 'origin', 'evm')",
    )
    .await;
    let on_base = |sql: String| sql.replace("'\\x01', 'k',", "'\\x02', 'k2',");
    let signed = |network: &'static str, reference: u8, message: u8, base: bool| {
        let pool = pool.clone();
        async move {
            let order = order(&pool, network, "payout", reference).await;
            let mut sql = prepared(order, network, message);
            if base {
                sql = on_base(sql);
            }
            let prepared = insert(&pool, &sql).await.expect("prepare");
            insert(
                &pool,
                &signing(network, "payout", reference, &over(prepared, message, "1")),
            )
            .await
            .expect("record the signing intent")
        }
    };
    let signature = |intent: i64, key: &str, public: u8| {
        format!(
            "INSERT INTO signatures (intent_id, key_id, scheme, public_key, signature) \
             VALUES ({intent}, '{key}', 'secp256k1-evm', '\\x{public:02x}', '\\x01') RETURNING id"
        )
    };

    let polygon = signed("polygon", 0x01, 0xa1, false).await;
    insert(&pool, &signature(polygon, "lane", 0x0a))
        .await
        .expect("the lane key signs for Polygon");
    let polygon_2 = signed("polygon", 0x02, 0xa2, false).await;
    insert(&pool, &signature(polygon_2, "lane", 0x0a))
        .await
        .expect("and again for Polygon");

    let base = signed("base", 0x03, 0xb1, true).await;
    assert_refused(
        insert(&pool, &signature(base, "lane", 0x0b)).await,
        "the Polygon lane key's id signing for Base",
    );
    assert_refused(
        insert(&pool, &signature(base, "base-lane", 0x0a)).await,
        "the Polygon lane key, under another id, signing for Base",
    );
    insert(&pool, &signature(base, "base-lane", 0x0b))
        .await
        .expect("Base's own key signs for Base");

    // The lane: Polygon's address or key id preparing for Base.
    let base_order = order(&pool, "base", "payout", 0x04).await;
    assert_refused(
        insert(&pool, &prepared(base_order, "base", 0xb2)).await,
        "the Polygon lane preparing for Base",
    );
    assert_refused(
        insert(
            &pool,
            &prepared(base_order, "base", 0xb2).replace("'\\x01', 'k',", "'\\x03', 'k',"),
        )
        .await,
        "the Polygon lane's key id preparing for Base",
    );
    insert(&pool, &on_base(prepared(base_order, "base", 0xb2)))
        .await
        .expect("Base's own lane prepares for Base");
}
