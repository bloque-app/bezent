// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #42, migration 0039: the readings the endpoints record at final heads for the reserve
//! check (`REQ-BK-8`, `REQ-OB-5`, `REQ-OB-6`, `INV-8`), the shape of a statement intent (`REQ-PR-1`;
//! PLAN `DEC-16`), and the lag bound held to the route budgets when a network's role changes
//! (`NFR-5`). Each role check runs under `SET LOCAL ROLE` inside its own transaction, rolled back, so
//! no pooled connection keeps a role it was switched to (#27).

mod common;

use common::{assert_refused, exec};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

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

const REFUSED: Option<&str> = Some(bezent_store::REFUSAL_SQLSTATE);
/// Permission denied.
const DENIED: Option<&str> = Some("42501");

async fn declare(pool: &PgPool) {
    exec(
        pool,
        "INSERT INTO networks (network, role, address_format) VALUES \
           ('kreivo', 'issuing', 'ss58'), ('polygon', 'origin', 'evm'); \
         INSERT INTO final_heads (network, block_number, block_hash, block_time) VALUES \
           ('kreivo', 1, '\\x01', now()), ('polygon', 1, '\\x01', now()); \
         INSERT INTO assets (asset, precision, assurance_class, issuing_asset) \
           VALUES ('bUSDC', 6, 'public', 'Here(101)');",
    )
    .await;
}

fn supply(network: &str, block: i64) -> String {
    format!(
        "INSERT INTO reserve_readings (network, block_number, block_hash, kind, subject, amount) \
         VALUES ('{network}', {block}, '\\x01', 'supply', 'Here(101)', 5)"
    )
}

fn reserve(network: &str, block: i64) -> String {
    format!(
        "INSERT INTO reserve_readings (network, block_number, block_hash, kind, subject, holder, \
           amount) VALUES ('{network}', {block}, '\\x01', 'reserve', '0xusdc', '0xvault', 5)"
    )
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ob_6_inv_8_only_a_networks_endpoint_records_its_readings(pool: PgPool) {
    declare(&pool).await;
    let kreivo = bezent_store::KREIVO_ENDPOINT_ROLE;
    let evm = bezent_store::EVM_ENDPOINT_ROLE;
    // Each endpoint its own network's.
    assert_eq!(as_role(&pool, kreivo, &supply("kreivo", 1)).await, None);
    assert_eq!(as_role(&pool, evm, &reserve("polygon", 1)).await, None);
    // Never the other's.
    assert_eq!(
        as_role(&pool, kreivo, &reserve("polygon", 1))
            .await
            .as_deref(),
        REFUSED
    );
    assert_eq!(
        as_role(&pool, evm, &supply("kreivo", 1)).await.as_deref(),
        REFUSED
    );
    // The endpoints' mark alone, the ingress, the core, the signer and the operators may not
    // even insert (0045).
    for role in [
        bezent_store::ENDPOINT_ROLE,
        bezent_store::INGRESS_ROLE,
        bezent_store::SERVICE_ROLE,
        bezent_store::CORE_ROLE,
        bezent_store::SIGNER_ROLE,
        bezent_store::OPERATOR_ROLE,
    ] {
        assert_eq!(
            as_role(&pool, role, &supply("kreivo", 1)).await.as_deref(),
            DENIED,
            "{role}"
        );
    }
    // The core reads them, through its own role.
    exec(&pool, &supply("kreivo", 1)).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql("SET LOCAL ROLE bezent_core")
        .execute(&mut *tx)
        .await
        .unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM reserve_readings")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(n, 1);
    tx.rollback().await.unwrap();
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ob_5_req_bk_8_a_reading_rests_on_a_recorded_final_head_and_says_what_it_is(
    pool: PgPool,
) {
    declare(&pool).await;
    // Not at a recorded final head (another block, or another hash): refused.
    assert_refused(&pool, &supply("kreivo", 2), "REQ-OB-5").await;
    assert_refused(
        &pool,
        "INSERT INTO reserve_readings (network, block_number, block_hash, kind, subject, amount) \
         VALUES ('kreivo', 1, '\\x02', 'supply', 'Here(101)', 5)",
        "REQ-OB-5",
    )
    .await;
    // Supply on the issuing network only; reserve on an origin network only.
    assert_refused(&pool, &supply("polygon", 1), "REQ-BK-8").await;
    assert_refused(&pool, &reserve("kreivo", 1), "REQ-BK-8").await;
    // Unknown is NULL with its reason, never a guess; a known one has no reason.
    common::assert_sqlstate(
        &pool,
        "INSERT INTO reserve_readings (network, block_number, block_hash, kind, subject) \
         VALUES ('kreivo', 1, '\\x01', 'supply', 'Here(101)')",
        "23514",
    )
    .await;
    exec(
        &pool,
        "INSERT INTO reserve_readings (network, block_number, block_hash, kind, subject, holder, \
           unknown_reason) VALUES ('polygon', 1, '\\x01', 'reserve', '0xusdc', '0xvault', \
           'DEC-12: the providers disagree')",
    )
    .await;
    // Recorded once per point and subject, and never changed.
    exec(&pool, &supply("kreivo", 1)).await;
    common::assert_sqlstate(&pool, &supply("kreivo", 1), "23505").await;
    assert_refused(&pool, "UPDATE reserve_readings SET amount = 6", "REQ-BK-8").await;
    assert_refused(&pool, "DELETE FROM reserve_readings", "REQ-BK-8").await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ar_3_no_function_of_0039_is_public(pool: PgPool) {
    let names = "'reserve_readings_guard_insert', 'intents_guard_statement', \
                 'intents_guard_prepared', 'signatures_ed25519_statements_only'";
    let public: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT p.oid::regprocedure::text FROM pg_proc p WHERE p.proname IN ({names}) \
           AND has_function_privilege('public', p.oid, 'EXECUTE')"
    ))
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(public.is_empty(), "PUBLIC may run {public:?}");
    let n: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM pg_proc WHERE proname IN ({names})"
    ))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n, 4);
}

fn statement_intent(network: &str, doc: &[u8], sha: &str, extra: &str) -> String {
    format!(
        "INSERT INTO intents (network, purpose, payload{}) VALUES ('{network}', 'statement', \
           '{{\"message\": \"0x{}\", \"amount\": \"0\", \"statement\": {{\"asset\": \"bUSDC\", \
             \"sequence\": 1, \"sha256\": \"{sha}\"}}}}'{})",
        if extra.is_empty() {
            ""
        } else {
            ", reference_scope"
        },
        hex::encode(doc),
        if extra.is_empty() {
            String::new()
        } else {
            format!(", {extra}")
        },
    )
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pr_1_dec_16_a_statement_intent_carries_a_statement_and_nothing_of_a_transfer(
    pool: PgPool,
) {
    declare(&pool).await;
    let doc = br#"{"statement": "bezent reserve statement", "asset": "bUSDC", "sequence": 1}"#;
    let sha = format!("0x{}", hex::encode(Sha256::digest(doc)));
    // The core records one: its bytes, amount 0, and the asset, sequence and hash of the bytes.
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql("SET LOCAL ROLE bezent_core")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::raw_sql(&statement_intent("kreivo", doc, &sha, ""))
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    // A hash that is not the bytes', another network than the issuing one, a redeemer's scope: refused.
    let wrong = format!("0x{}", "00".repeat(32));
    assert_refused(
        &pool,
        &statement_intent("kreivo", doc, &wrong, ""),
        "REQ-PR-1",
    )
    .await;
    assert_refused(
        &pool,
        &statement_intent("polygon", doc, &sha, ""),
        "REQ-PR-1",
    )
    .await;
    assert_refused(
        &pool,
        &statement_intent("kreivo", doc, &sha, "'0xredeemer'"),
        "REQ-PR-1",
    )
    .await;
    // Nor does any other intent carry a statement.
    assert_refused(
        &pool,
        &format!(
            "INSERT INTO intents (network, purpose, payload) VALUES ('kreivo', 'mint', \
               '{{\"order\": {{}}, \"amount\": \"1\", \"statement\": {{\"asset\": \"bUSDC\", \
                 \"sequence\": 1, \"sha256\": \"{sha}\"}}}}')"
        ),
        "REQ-PR-1",
    )
    .await;
}

/// PR #40 second round, (2): a network's lag bound is held to the completion budgets of the routes
/// it serves when the bound, the finality time or the budget changes (0032, 0033), and now when its
/// role does: a network that becomes the issuing one serves every route (`NFR-5`, `NFR-6`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn nfr_5_a_network_that_becomes_issuing_is_held_to_every_routes_budget(pool: PgPool) {
    exec(
        &pool,
        "INSERT INTO networks (network, role, address_format, finality_seconds) VALUES \
           ('kreivo', 'issuing', 'ss58', 0), ('polygon', 'origin', 'evm', 0), \
           ('slow', 'origin', 'ss58', 1200); \
         INSERT INTO assets (asset, precision, assurance_class, issuing_asset) \
           VALUES ('bUSDC', 6, 'public', 'Here(101)'); \
         INSERT INTO routes (asset, network, direction, minimum, maximum, window_limit, \
             window_seconds, capacity, cutoffs, completion_budget_seconds) \
           VALUES ('bUSDC', 'polygon', 'deposit', 1, 1000, 10000, 86400, 10, '[]', 600);",
    )
    .await;
    // 'slow' serves no route: its bound (1260 s) is above the polygon route's budget, and stands.
    // Made the issuing network, it would serve that route too: refused.
    assert_refused(
        &pool,
        "UPDATE networks SET role = 'issuing' WHERE network = 'slow'",
        "NFR-6",
    )
    .await;
    // And a budget lowered below the issuing network's bound is refused (0033's rule).
    exec(
        &pool,
        "UPDATE networks SET reserve_point_lag_seconds = 300 WHERE network = 'kreivo'",
    )
    .await;
    assert_refused(
        &pool,
        "UPDATE routes SET completion_budget_seconds = 299",
        "NFR-6",
    )
    .await;
}

/// Migration 0040 (PR #50 review M1; PLAN DEC-16, 2026-10-15): the core may pause an asset under
/// INV-4 only on a supply reading of that asset, on the issuing network, flagged as a mismatch,
/// naming an evaluation of it; and, as every pause, never lift it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_4_the_core_pauses_on_a_flagged_supply_reading_only(pool: PgPool) {
    declare(&pool).await;
    exec(
        &pool,
        "INSERT INTO assets (asset, precision, assurance_class, issuing_asset) \
           VALUES ('bEURC', 6, 'public', 'Here(102)'); \
         INSERT INTO final_heads (network, block_number, block_hash, block_time) \
           VALUES ('kreivo', 2, '\\x02', now()); \
         INSERT INTO reserve_readings (network, block_number, block_hash, kind, subject, amount) \
           VALUES ('kreivo', 1, '\\x01', 'supply', 'Here(101)', 5); \
         INSERT INTO reserve_readings (network, block_number, block_hash, kind, subject, amount, \
             mismatch, detail) VALUES ('kreivo', 2, '\\x02', 'supply', 'Here(101)', 6, true, \
             '{\"contract_supply\": \"5\", \"issuance\": \"6\"}'); \
         INSERT INTO reserve_evaluations (asset, evaluated_at, outcome) VALUES \
           ('bUSDC', now(), 'holds'), ('bEURC', now(), 'holds');",
    )
    .await;
    let id = |sql: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    let clean = id("SELECT id FROM reserve_readings WHERE NOT mismatch").await;
    let flagged = id("SELECT id FROM reserve_readings WHERE mismatch").await;
    let busdc = id("SELECT id FROM reserve_evaluations WHERE asset = 'bUSDC'").await;
    let beurc = id("SELECT id FROM reserve_evaluations WHERE asset = 'bEURC'").await;
    let pause = |asset: &str, evaluation: i64, reading: i64| {
        format!(
            "INSERT INTO pauses (asset, cause, evaluation_id, reading_id) \
             VALUES ('{asset}', 'INV-4', {evaluation}, {reading})"
        )
    };
    // On the flagged reading of the asset's own issuing identity, naming its evaluation.
    assert_eq!(
        as_role(&pool, "bezent_core", &pause("bUSDC", busdc, flagged)).await,
        None
    );
    // Not on a reading that is not flagged, nor for another asset.
    assert_eq!(
        as_role(&pool, "bezent_core", &pause("bUSDC", busdc, clean))
            .await
            .as_deref(),
        REFUSED
    );
    assert_eq!(
        as_role(&pool, "bezent_core", &pause("bEURC", beurc, flagged))
            .await
            .as_deref(),
        REFUSED
    );
    assert_eq!(
        as_role(&pool, "bezent_core", &pause("bEURC", busdc, flagged))
            .await
            .as_deref(),
        REFUSED
    );
    // An INV-4 pause names its reading; no other cause carries one.
    assert_eq!(
        as_role(
            &pool,
            "bezent_core",
            &format!("INSERT INTO pauses (asset, cause, evaluation_id) VALUES ('bUSDC', 'INV-4', {busdc})")
        )
        .await
        .as_deref(),
        REFUSED
    );
    assert_eq!(
        sqlx::query(&format!(
            "INSERT INTO pauses (asset, cause, evaluation_id) VALUES ('bUSDC', 'INV-4', {busdc})"
        ))
        .execute(&pool)
        .await
        .err()
        .and_then(|e| e
            .as_database_error()
            .and_then(|d| d.code().map(|c| c.into_owned())))
        .as_deref(),
        Some("23514"),
        "the owner too: the constraint holds the shape"
    );
    // No other service pauses, and the core does not lift one.
    assert_eq!(
        as_role(&pool, "bezent_ingress", &pause("bUSDC", busdc, flagged))
            .await
            .as_deref(),
        DENIED
    );
    exec(&pool, &pause("bUSDC", busdc, flagged)).await;
    assert_eq!(
        as_role(&pool, "bezent_core", "DELETE FROM pauses")
            .await
            .as_deref(),
        DENIED
    );
    // No function of 0040 is PUBLIC's.
    for f in [
        "pauses_guard_insert()",
        "pause_reading_is_supply_mismatch(text, bigint)",
    ] {
        let public: bool =
            sqlx::query_scalar("SELECT has_function_privilege('public', $1, 'EXECUTE')")
                .bind(f)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(!public, "{f}");
    }
}

/// A statement intent for place `sequence` of bUSDC over `doc`, signed by `key` (as the signer
/// records it); returns the intent.
async fn signed_statement(pool: &PgPool, sequence: i64, doc: &[u8], key: u8) -> i64 {
    let sha = format!("0x{}", hex::encode(Sha256::digest(doc)));
    let id: i64 = sqlx::query_scalar(&format!(
        "INSERT INTO intents (network, purpose, payload) VALUES ('kreivo', 'statement', \
           '{{\"message\": \"0x{}\", \"amount\": \"0\", \"statement\": {{\"asset\": \"bUSDC\", \
             \"sequence\": {sequence}, \"sha256\": \"{sha}\"}}}}') RETURNING id",
        hex::encode(doc)
    ))
    .fetch_one(pool)
    .await
    .unwrap();
    exec(
        pool,
        &format!(
            "INSERT INTO signatures (intent_id, key_id, scheme, public_key, signature) \
             VALUES ({id}, 'statement', 'ed25519', '\\x{}', '\\x{}')",
            hex::encode([key; 32]),
            hex::encode([9u8; 64])
        ),
    )
    .await;
    id
}

/// PR #50 re-check S-1, S-3: a place whose signed statement never reached the store is abandoned
/// once, by the login that records it, only as the asset's next open place, with no published
/// statement and the statement key's signature over exactly that hash; it is never published
/// afterwards, the next statement names the last published one and skips only abandoned places,
/// and the signer reads that rule through one function, never PUBLIC's (`REQ-PR-2`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s_1_s_3_req_pr_2_an_abandoned_place_is_recorded_once_and_skipped_only_by_naming_it(
    pool: PgPool,
) {
    declare(&pool).await;
    let evaluation: i64 = sqlx::query_scalar(
        "INSERT INTO reserve_evaluations (asset, evaluated_at, outcome) \
         VALUES ('bUSDC', now(), 'holds') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let hash = |b: &[u8]| -> Vec<u8> { Sha256::digest(b).to_vec() };
    let publish = |sequence: i64, doc: &[u8], previous: Option<&[u8]>| {
        format!(
            "INSERT INTO reserve_statements (asset, sequence, evaluation_id, issued_at, name, \
                 sha256, previous_sha256, public_key, signature) \
             VALUES ('bUSDC', {sequence}, {evaluation}, now(), 'bUSDC/{sequence}', '\\x{}', {}, \
                     '\\x{}', '\\x{}')",
            hex::encode(hash(doc)),
            previous.map_or("NULL".into(), |p| format!("'\\x{}'", hex::encode(hash(p)))),
            hex::encode([1u8; 32]),
            hex::encode([9u8; 64]),
        )
    };
    let abandon = |sequence: i64, doc: &[u8], key: u8, intent: i64| {
        format!(
            "INSERT INTO reserve_statement_abandoned (asset, sequence, sha256, public_key, \
                 intent_id, reason, actor) \
             VALUES ('bUSDC', {sequence}, '\\x{}', '\\x{}', {intent}, 'it no longer re-derives', \
                     'mallory')",
            hex::encode(hash(doc)),
            hex::encode([key; 32]),
        )
    };
    let (one, two, three) = (&b"statement one"[..], &b"statement two"[..], &b"three"[..]);
    let i1 = signed_statement(&pool, 1, one, 1).await;
    exec(&pool, &publish(1, one, None)).await;
    let i2 = signed_statement(&pool, 2, two, 1).await;

    // Not the next open place, a published place, a hash or key that was not signed: refused.
    assert_refused(&pool, &abandon(1, one, 1, i1), "REQ-PR-2").await;
    assert_refused(&pool, &abandon(3, two, 1, i2), "REQ-PR-2").await;
    assert_refused(&pool, &abandon(2, three, 1, i2), "REQ-PR-2").await;
    assert_refused(&pool, &abandon(2, two, 2, i2), "REQ-PR-2").await;
    // Place 2 abandoned, by whoever records it, whatever actor it names.
    exec(&pool, &abandon(2, two, 1, i2)).await;
    let actor: String =
        sqlx::query_scalar("SELECT actor FROM reserve_statement_abandoned WHERE sequence = 2")
            .fetch_one(&pool)
            .await
            .unwrap();
    let me: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(actor, me);
    // Once, and for good.
    assert_refused(&pool, &abandon(2, two, 1, i2), "REQ-PR-2").await;
    assert_refused(
        &pool,
        "UPDATE reserve_statement_abandoned SET reason = 'other'",
        "REQ-PR-2",
    )
    .await;
    assert_refused(&pool, "DELETE FROM reserve_statement_abandoned", "REQ-PR-2").await;
    assert_refused(&pool, "TRUNCATE reserve_statement_abandoned", "REQ-PR-2").await;

    // Nothing is published at the abandoned place; the next names the last published statement.
    assert_refused(&pool, &publish(2, two, Some(one)), "REQ-PR-2").await;
    assert_refused(&pool, &publish(3, three, Some(two)), "REQ-PR-2").await;
    assert_refused(&pool, &publish(4, three, Some(one)), "REQ-PR-2").await;

    // The signer's view: place 3 may be signed naming statement 1 and abandoned place 2, exactly.
    let refusal = |place: i64, previous: Option<&[u8]>, abandoned: String| {
        let pool = pool.clone();
        let previous = previous.map(hash);
        async move {
            let mut tx = pool.begin().await.unwrap();
            sqlx::raw_sql("SET LOCAL ROLE bezent_signer")
                .execute(&mut *tx)
                .await
                .unwrap();
            let why: Option<String> = sqlx::query_scalar(
                "SELECT statement_place_refusal('bUSDC', $1, $2, $3::text::jsonb)",
            )
            .bind(place)
            .bind(previous)
            .bind(abandoned)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
            tx.rollback().await.unwrap();
            why
        }
    };
    let named = format!(
        "[{{\"sequence\": 2, \"sha256\": \"0x{}\", \"reason\": \"it no longer re-derives\"}}]",
        hex::encode(hash(two))
    );
    let none = "[]".to_owned();
    assert_eq!(refusal(3, Some(one), named.clone()).await, None);
    for (place, previous, abandoned) in [
        (2, Some(one), none.clone()),
        (3, Some(one), none.clone()),
        (3, None, named.clone()),
        (3, Some(three), named.clone()),
        (1, None, none.clone()),
        (
            3,
            Some(one),
            "[{\"sequence\": 2, \"sha256\": \"0x00\"}]".to_owned(),
        ),
    ] {
        assert!(
            refusal(place, previous, abandoned.clone()).await.is_some(),
            "{place} {abandoned}"
        );
    }
    exec(&pool, &publish(3, three, Some(one))).await;

    // No function of 0041 is PUBLIC's; the signer's is the signer's only.
    for f in [
        "reserve_statement_abandoned_guard_insert()",
        "statement_place_refusal(text, bigint, bytea, jsonb)",
    ] {
        let public: bool =
            sqlx::query_scalar("SELECT has_function_privilege('public', $1, 'EXECUTE')")
                .bind(f)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(!public, "{f}");
    }
    for role in ["bezent_service", "bezent_core", "bezent_operator"] {
        let may: bool = sqlx::query_scalar(
            "SELECT has_function_privilege($1, 'statement_place_refusal(text, bigint, bytea, jsonb)', 'EXECUTE')",
        )
        .bind(role)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!may, "{role}");
    }
}

/// Issue #58 (migration 0053): the receipt of each object the statement sink wrote is recorded
/// against a statement that is already recorded, by the login that records it, and never changed
/// (`REQ-PR-2`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_58_req_pr_2_statement_object_receipts_name_a_recorded_statement_and_are_append_only(
    pool: PgPool,
) {
    declare(&pool).await;
    let evaluation: i64 = sqlx::query_scalar(
        "INSERT INTO reserve_evaluations (asset, evaluated_at, outcome) \
         VALUES ('bUSDC', now(), 'holds') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let object = |sequence: i64, name: &str| {
        format!(
            "INSERT INTO reserve_statement_objects (asset, sequence, name, location, sha256, etag, \
                 version_id, actor) \
             VALUES ('bUSDC', {sequence}, '{name}', 'statements/{name}', '\\x{}', '\"abc\"', 'v1', \
                     'mallory')",
            hex::encode([7u8; 32])
        )
    };
    // No statement is recorded yet: refused.
    assert_refused(&pool, &object(1, "bUSDC/1.json"), "REQ-PR-2").await;
    exec(
        &pool,
        &format!(
            "INSERT INTO reserve_statements (asset, sequence, evaluation_id, issued_at, name, \
                 sha256, public_key, signature) \
             VALUES ('bUSDC', 1, {evaluation}, now(), 'bUSDC/1.json', '\\x{}', '\\x{}', '\\x{}')",
            hex::encode([1u8; 32]),
            hex::encode([1u8; 32]),
            hex::encode([9u8; 64]),
        ),
    )
    .await;
    exec(&pool, &object(1, "bUSDC/1.json")).await;
    exec(&pool, &object(1, "bUSDC/1.json.sig")).await;
    // Once per object; the actor is the login, whatever it names.
    assert!(
        sqlx::raw_sql(&object(1, "bUSDC/1.json"))
            .execute(&pool)
            .await
            .is_err()
    );
    let me: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&pool)
        .await
        .unwrap();
    let actors: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT actor FROM reserve_statement_objects")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(actors, [me]);
    for sql in [
        "UPDATE reserve_statement_objects SET etag = 'x'",
        "DELETE FROM reserve_statement_objects",
        "TRUNCATE reserve_statement_objects",
    ] {
        assert_refused(&pool, sql, "REQ-PR-2").await;
    }
    // The trigger function is no one's to call.
    let public: bool = sqlx::query_scalar(
        "SELECT has_function_privilege('public', 'reserve_statement_objects_guard_insert()', 'EXECUTE')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!public);
}
