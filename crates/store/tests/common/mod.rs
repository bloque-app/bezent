// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Helpers shared by the store tests. Every test runs in a fresh database that `#[sqlx::test]`
//! creates on the server `DATABASE_URL` names, with bezent's migrations applied.

#![allow(dead_code)]

use sqlx::migrate::Migrator;
use sqlx::{PgPool, Postgres, Transaction};

/// The same up migrations as [`bezent_store::MIGRATOR`] (symbolic links to them), with their down
/// migrations: for the reversibility test only. No service binary carries these.
pub static REVERSIBLE_MIGRATOR: Migrator = sqlx::migrate!("./tests/migrations");

/// Test operators, each a login of their own and a member of `bezent_operator` (REQ-GV-5; review
/// M3 of PR #32). Created once per server, with a test-only password.
pub const OPERATORS: [&str; 2] = ["ops_alice_for_tests", "ops_bob_for_tests"];
const OPERATOR_PASSWORD: &str = "test-only-operator-login";

/// A pool on `pool`'s database that logs in as operator `name`: one pool, one login (issue #27).
pub async fn operator_login(pool: &PgPool, name: &str) -> PgPool {
    exec(
        pool,
        &format!(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{name}') THEN \
                 BEGIN CREATE ROLE {name} LOGIN PASSWORD '{OPERATOR_PASSWORD}'; \
                 EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END; \
               END IF; \
               BEGIN GRANT bezent_operator TO {name}; \
               EXCEPTION WHEN OTHERS THEN NULL; END; \
             END $$;"
        ),
    )
    .await;
    let options = (*pool.connect_options())
        .clone()
        .username(name)
        .password(OPERATOR_PASSWORD);
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap_or_else(|e| panic!("log in as {name}: {e}"))
}

/// A pool on `pool`'s database that logs in as `name`, a login that is a member of no bezent role
/// (F1 of the verification review of PR #32). Created once per server, with a test-only password.
pub async fn unrelated_login(pool: &PgPool, name: &str) -> PgPool {
    exec(
        pool,
        &format!(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{name}') THEN \
                 BEGIN CREATE ROLE {name} LOGIN PASSWORD '{OPERATOR_PASSWORD}'; \
                 EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END; \
               END IF; \
             END $$;"
        ),
    )
    .await;
    let options = (*pool.connect_options())
        .clone()
        .username(name)
        .password(OPERATOR_PASSWORD);
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap_or_else(|e| panic!("log in as {name}: {e}"))
}

/// The block time every helper coordinate carries.
pub const BLOCK_TIME: &str = "2026-10-13T00:00:00Z";

/// Runs `sql` and asserts the store refuses it with bezent's refusal SQLSTATE and a message that
/// names `spec_id`.
pub async fn assert_refused(pool: &PgPool, sql: &str, spec_id: &str) {
    let err = sqlx::raw_sql(sql)
        .execute(pool)
        .await
        .expect_err(&format!("the store accepted: {sql}"));
    assert_refusal(&err, spec_id, sql);
}

/// Asserts `err` is a store-rule refusal naming `spec_id`.
pub fn assert_refusal(err: &sqlx::Error, spec_id: &str, what: &str) {
    let db = err
        .as_database_error()
        .unwrap_or_else(|| panic!("not a database error for {what}: {err}"));
    assert_eq!(
        db.code().as_deref(),
        Some(bezent_store::REFUSAL_SQLSTATE),
        "wrong SQLSTATE for {what}: {}",
        db.message()
    );
    assert!(
        db.message().contains(spec_id),
        "refusal for {what} does not name {spec_id}: {}",
        db.message()
    );
}

/// Runs `sql` and asserts it fails with `sqlstate` (a constraint, not a trigger).
pub async fn assert_sqlstate(pool: &PgPool, sql: &str, sqlstate: &str) {
    let err = sqlx::raw_sql(sql)
        .execute(pool)
        .await
        .expect_err(&format!("the store accepted: {sql}"));
    assert_error_sqlstate(&err, sqlstate, sql);
}

/// Asserts `err` is a database error with `sqlstate`.
pub fn assert_error_sqlstate(err: &sqlx::Error, sqlstate: &str, what: &str) {
    let db = err
        .as_database_error()
        .unwrap_or_else(|| panic!("not a database error for {what}: {err}"));
    assert_eq!(
        db.code().as_deref(),
        Some(sqlstate),
        "{what}: {}",
        db.message()
    );
}

/// Runs `sql`, which must succeed.
pub async fn exec(pool: &PgPool, sql: &str) {
    sqlx::raw_sql(sql)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// Runs `sql` inside `tx`, which must succeed.
pub async fn exec_tx(tx: &mut Transaction<'_, Postgres>, sql: &str) {
    sqlx::raw_sql(sql)
        .execute(&mut **tx)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// Reads one text value.
pub async fn text(pool: &PgPool, sql: &str) -> String {
    sqlx::query_scalar(sql)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

/// Reads one boolean.
pub async fn flag(pool: &PgPool, sql: &str) -> bool {
    sqlx::query_scalar(sql)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

/// Inserts a deposit transfer in `Received`, its only initial state but one, and returns its id.
pub async fn transfer(pool: &PgPool, bridge_reference: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state) \
         VALUES ($1, 'req-' || $1, 'deposit', 'asset-a', 1000, 'issuing', 'dest-1', 'Received') \
         RETURNING id",
    )
    .bind(bridge_reference)
    .fetch_one(pool)
    .await
    .expect("insert transfer")
}

/// Inserts a deposit that begins at `Source-Final` (REQ-IN-3): bound to the arrival at
/// (`origin-a`, `block`, log 0), final at block `block + 20`.
pub async fn bound_deposit(pool: &PgPool, bridge_reference: &str, block: i64) -> i64 {
    finalise_coordinate(pool, "origin-a", block, "aa", "log", 0, "asset-a", "1000").await;
    sqlx::query_scalar(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, \
             source_network, source_block_number, source_block_hash, source_position_kind, \
             source_position_index, \
             source_final_network, source_final_block_number, source_final_block_hash, \
             source_final_block_time, source_final_seen_at) \
         VALUES ($1, 'req-' || $1, 'deposit', 'asset-a', 1000, 'issuing', 'dest-1', 'Source-Final', \
             'origin-a', $2, '\\xaa', 'log', 0, \
             'origin-a', $2 + 20, '\\xbb', '2026-10-13T00:00:00Z', '2026-10-13T00:00:05Z') \
         RETURNING id",
    )
    .bind(bridge_reference)
    .bind(block)
    .fetch_one(pool)
    .await
    .expect("insert bound deposit")
}

/// Binds transfer `id` to the source coordinate (`origin-a`, `block`, `\xaa`, log 0).
pub async fn bind_source(pool: &PgPool, id: i64, block: i64) {
    exec(
        pool,
        &format!(
            "UPDATE transfers SET source_network = 'origin-a', source_block_number = {block}, \
               source_block_hash = '\\xaa', source_position_kind = 'log', \
               source_position_index = 0 WHERE id = {id}"
        ),
    )
    .await;
}

/// Appends a transition of transfer `id` from `from` to `to`, resting on block 10 (`\x0a`) of the
/// network the edge needs: the source network (`origin-a`) for `Source-Final`, the issuing network
/// otherwise. Into `Source-Final` it first makes the bound source final ([`finalise_source`]).
///
/// Into `Delivering` from another state it first records the next attempt's signing intent, its
/// signature and its signed bytes, and carries the attempt number (`REQ-EO-6`, `INV-9`); a new
/// attempt rests on the previous one's proof, which the caller records first
/// ([`prove_not_executed`]). Into `Refunding` it proves the current attempt not executed first.
pub async fn transition(pool: &PgPool, id: i64, from: &str, to: &str) -> Result<(), sqlx::Error> {
    let network = if to == "Source-Final" {
        "origin-a"
    } else {
        "issuing"
    };
    let mut attempt = None;
    if to == "Delivering" && from != "Delivering" {
        let next = current_attempt(pool, id).await + 1;
        // A refunding transfer takes no further attempt, so none can be signed for it (review M2
        // of PR #32); its edge into Delivering is refused all the same.
        let closed: bool = sqlx::query_scalar(
            "SELECT state IN ('Refunding', 'Refunded') \
                 OR EXISTS (SELECT 1 FROM transfers r WHERE r.refund_of = $1) \
             FROM transfers WHERE id = $1",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("closed");
        if !closed {
            signed_attempt(pool, id, next).await;
        }
        attempt = Some(next);
    }
    if to == "Refunding" && from != "Refunding" && !proven(pool, id).await {
        prove_not_executed(pool, id).await;
    }
    if to == "Source-Final" && from != "Source-Final" {
        finalise_source(pool, id).await;
    }
    sqlx::query(
        "INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
             block_hash, block_time, attempt) \
         VALUES ($1, $2, $3, $4, 10, '\\x0a', '2026-10-13T00:00:00Z', $5)",
    )
    .bind(id)
    .bind(from)
    .bind(to)
    .bind(network)
    .bind(attempt)
    .execute(pool)
    .await
    .map(|_| ())
}

/// The highest delivery attempt transfer `id` has reached (0 for none).
pub async fn current_attempt(pool: &PgPool, id: i64) -> i32 {
    sqlx::query_scalar("SELECT COALESCE(max(attempt), 0) FROM transitions WHERE transfer_id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("current attempt")
}

/// Prepares order intent `order` again, as the Kreivo endpoint would (F-007, PLAN DEC-3): its
/// next attempt in `prepared_transactions`, whose dry run passed, in lane `\x01` at nonce 0 and
/// valid in blocks 0 to 5, the transaction every proof below describes (0019, review S2).
/// Returns the prepared transaction's id.
pub async fn prepare_order(pool: &PgPool, order: i64) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO prepared_transactions (order_intent_id, attempt, network, lane, key_id, nonce, \
             valid_from_block, valid_until_block, checkpoint_hash, call, signing_payload, message, \
             dry_run_block_number, dry_run_block_hash, dry_run_passed, dry_run) \
         SELECT $1, COALESCE((SELECT max(attempt) FROM prepared_transactions \
                              WHERE order_intent_id = $1), 0) + 1, \
             network, '\\x01', 'mint-key', 0, 0, 5, '\\x02', '\\x03', '\\x04', '\\x01', 1, \
             '\\x05', true, '{\"outcome\": \"passed\"}' \
         FROM intents WHERE id = $1 RETURNING id",
    )
    .bind(order)
    .fetch_one(pool)
    .await
    .expect("prepare the order")
}

/// Records, as the core and an endpoint would (PLAN DEC-3), a delivery order with no transfer on
/// `network` (`purpose`, `reference` as SQL: `NULL` or a bytea literal, attempt `attempt`), its
/// prepared transaction whose `message` is the hex `message`, and the signing intent over it,
/// carrying `amount` (a JSON string). Every signing intent names the prepared transaction of its
/// own order, with its exact bytes and its order's amount (0037, MF-A). Returns the signing intent.
pub async fn signing_intent(
    pool: &PgPool,
    network: &str,
    purpose: &str,
    reference: &str,
    attempt: i32,
    amount: &str,
    message: &str,
) -> i64 {
    let order: i64 = sqlx::query_scalar(&format!(
        "INSERT INTO intents (network, purpose, on_chain_reference, attempt, payload) \
         VALUES ('{network}', '{purpose}', {reference}, {attempt}, \
                 jsonb_build_object('order', '{{}}'::jsonb, 'amount', '{amount}'::jsonb)) \
         RETURNING id"
    ))
    .fetch_one(pool)
    .await
    .expect("record the order");
    let prepared: i64 = sqlx::query_scalar(&format!(
        "INSERT INTO prepared_transactions (order_intent_id, attempt, network, lane, key_id, nonce, \
             valid_from_block, valid_until_block, checkpoint_hash, call, signing_payload, message, \
             dry_run_block_number, dry_run_block_hash, dry_run_passed, dry_run) \
         VALUES ({order}, 1, '{network}', '\\x01', 'mint-key', 0, 0, 5, '\\x02', '\\x03', \
             '\\x{message}', '\\x{message}', 1, '\\x05', true, '{{\"outcome\": \"passed\"}}') \
         RETURNING id"
    ))
    .fetch_one(pool)
    .await
    .expect("prepare the order");
    sqlx::query_scalar(&format!(
        "INSERT INTO intents (network, purpose, on_chain_reference, attempt, payload) \
         VALUES ('{network}', '{purpose}', {reference}, {attempt}, \
                 jsonb_build_object('message', '0x{message}', 'amount', '{amount}'::jsonb, \
                                    'prepared', {prepared})) \
         RETURNING id"
    ))
    .fetch_one(pool)
    .await
    .expect("record the signing intent")
}

/// Runs `sql` with 0037's signing-intent check off, in one transaction, as the migration owner:
/// how a test records what a compromised core would, to check the layer behind the store.
pub async fn without_intent_check(pool: &PgPool, sql: &str) -> i64 {
    let mut tx = pool.begin().await.unwrap();
    exec_tx(
        &mut tx,
        "ALTER TABLE intents DISABLE TRIGGER intents_guard_prepared",
    )
    .await;
    let id: i64 = sqlx::query_scalar(sql)
        .fetch_one(&mut *tx)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
    exec_tx(
        &mut tx,
        "ALTER TABLE intents ENABLE TRIGGER intents_guard_prepared",
    )
    .await;
    tx.commit().await.unwrap();
    id
}

/// The order intent of transfer `id`'s attempt `attempt`.
pub async fn order_of(pool: &PgPool, id: i64, attempt: i32) -> i64 {
    sqlx::query_scalar(
        "SELECT id FROM intents WHERE transfer_id = $1 AND attempt = $2 AND payload ? 'order' \
         ORDER BY id LIMIT 1",
    )
    .bind(id)
    .bind(attempt)
    .fetch_one(pool)
    .await
    .expect("the order")
}

/// Records, as the core, the endpoint, the signer and the endpoint again would, the two steps of
/// transfer `id`'s attempt `attempt` on its destination network (PLAN DEC-3): the order, the
/// endpoint's prepared transaction, the signing intent over it, its signature and its signed
/// bytes. Returns the signing intent's id.
pub async fn signed_attempt(pool: &PgPool, id: i64, attempt: i32) -> i64 {
    let order: i64 = sqlx::query_scalar(
        "INSERT INTO intents (transfer_id, network, purpose, on_chain_reference, reference_scope, \
             attempt, payload) \
         SELECT id, destination_network, 'mint', on_chain_reference, redeemer, $2, \
             '{\"order\": {}, \"amount\": \"1\"}' FROM transfers WHERE id = $1 \
         RETURNING id",
    )
    .bind(id)
    .bind(attempt)
    .fetch_one(pool)
    .await
    .expect("record the order");
    let prepared = prepare_order(pool, order).await;
    let intent: i64 = sqlx::query_scalar(
        "INSERT INTO intents (transfer_id, network, purpose, on_chain_reference, reference_scope, \
             attempt, payload) \
         SELECT id, destination_network, 'mint', on_chain_reference, redeemer, $2, \
             jsonb_build_object('message', '0x01', 'amount', '1', 'prepared', $3::bigint) \
         FROM transfers WHERE id = $1 RETURNING id",
    )
    .bind(id)
    .bind(attempt)
    .bind(prepared)
    .fetch_one(pool)
    .await
    .expect("record the signing intent");
    exec(
        pool,
        &format!(
            "INSERT INTO signatures (intent_id, key_id, scheme, public_key, signature) \
             VALUES ({intent}, 'mint-key', 'sr25519', '\\x01', '\\x02'); \
             INSERT INTO signed_transactions (intent_id, attempt, bytes) \
             VALUES ({intent}, 1, '\\xbeef')"
        ),
    )
    .await;
    intent
}

/// Whether transfer `id`'s current attempt is proven not executed.
pub async fn proven(pool: &PgPool, id: i64) -> bool {
    sqlx::query_scalar(
        "SELECT network IS NOT NULL FROM attempt_proof($1, \
             (SELECT COALESCE(max(attempt), 0) FROM transitions WHERE transfer_id = $1))",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .expect("read the proof")
}

/// Proves transfer `id`'s current attempt not executed, as an endpoint would (F-007): the window of
/// its signed transaction ended at the final block (`issuing`, 10, `\x0a`) with its nonce
/// unconsumed.
pub async fn prove_not_executed(pool: &PgPool, id: i64) {
    exec(
        pool,
        &format!(
            "INSERT INTO non_execution_proofs (signed_transaction_id, network, signer, nonce, \
                 valid_from_block, valid_until_block, final_block_number, final_block_hash, \
                 nonce_at_final, kind) \
             SELECT st.id, i.network, '\\x01', 0, 0, 5, 10, '\\x0a', 0, 'nonce-unconsumed' \
             FROM signed_transactions st JOIN intents i ON i.id = st.intent_id \
             WHERE i.transfer_id = {id} AND i.attempt = \
                 (SELECT max(attempt) FROM transitions WHERE transfer_id = {id}) \
             ORDER BY st.id DESC LIMIT 1"
        ),
    )
    .await;
}

/// Appends a transition that records no coordinate.
pub async fn bare_transition(
    pool: &PgPool,
    id: i64,
    from: &str,
    to: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO transitions (transfer_id, from_state, to_state) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(from)
        .bind(to)
        .execute(pool)
        .await
        .map(|_| ())
}

/// Walks transfer `id` along `path`, each step with a coordinate.
pub async fn walk(pool: &PgPool, id: i64, path: &[&str]) {
    for pair in path.windows(2) {
        transition(pool, id, pair[0], pair[1])
            .await
            .unwrap_or_else(|e| panic!("{} -> {}: {e}", pair[0], pair[1]));
    }
}

/// Takes a `Received` transfer to `Accepted`, binds its source at `block`, and makes it
/// `Source-Final`.
pub async fn to_source_final(pool: &PgPool, id: i64, block: i64) {
    bare_transition(pool, id, "Received", "Accepted")
        .await
        .expect("Received -> Accepted");
    bind_source(pool, id, block).await;
    walk(pool, id, &["Accepted", "Source-Final"]).await;
}

/// A deposit taken through every state to `Completed`; its source binds block `block`.
pub async fn completed(pool: &PgPool, bridge_reference: &str, block: i64) -> i64 {
    let id = transfer(pool, bridge_reference).await;
    to_source_final(pool, id, block).await;
    walk(
        pool,
        id,
        &["Source-Final", "Delivering", "Delivered", "Completed"],
    )
    .await;
    id
}

/// A transfer brought to the terminal state `state` along the edges of §5.4; its source, if it has
/// one, binds block `block`.
pub async fn terminal(pool: &PgPool, bridge_reference: &str, state: &str, block: i64) -> i64 {
    match state {
        "Completed" => completed(pool, bridge_reference, block).await,
        "Refused" => {
            let id = transfer(pool, bridge_reference).await;
            exec(
                pool,
                &format!(
                    "INSERT INTO transitions (transfer_id, from_state, to_state, error) \
                     VALUES ({id}, 'Received', 'Refused', 'ERR-BelowMinimum')"
                ),
            )
            .await;
            id
        }
        "Expired" | "Withdrawn" => {
            let id = transfer(pool, bridge_reference).await;
            bare_transition(pool, id, "Received", "Accepted")
                .await
                .unwrap();
            exec(
                pool,
                &format!(
                    "UPDATE transfers SET expires_at = '2026-10-12T00:00:00Z' WHERE id = {id}"
                ),
            )
            .await;
            walk(pool, id, &["Accepted", state]).await;
            id
        }
        "Refunded" => {
            let id = transfer(pool, bridge_reference).await;
            to_source_final(pool, id, block).await;
            walk(pool, id, &["Source-Final", "Delivering"]).await;
            let refund = refund_of(pool, id, &format!("{bridge_reference}-refund")).await;
            to_source_final(pool, refund, block).await;
            walk(
                pool,
                refund,
                &["Source-Final", "Delivering", "Delivered", "Completed"],
            )
            .await;
            // The original rests on the refund's final coordinate (issuing, 10, \x0a).
            walk(pool, id, &["Delivering", "Refunding", "Refunded"]).await;
            id
        }
        other => panic!("{other} is not terminal"),
    }
}

/// Inserts the refund transfer of transfer `original` (REQ-RF-3) and returns its id.
pub async fn refund_of(pool: &PgPool, original: i64, bridge_reference: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, state, refund_of) \
         VALUES ($1, 'req-' || $1, 'refund', 'asset-a', 1000, 'issuing', 'redeemer-1', \
             'Received', $2) \
         RETURNING id",
    )
    .bind(bridge_reference)
    .bind(original)
    .fetch_one(pool)
    .await
    .expect("insert refund")
}

/// Inserts an observation at a coordinate on `origin-a` and returns its id.
pub async fn observation(pool: &PgPool, block_number: i64, position_index: i64) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, token, amount, sender, recipient, on_chain_reference, executed) \
         VALUES ('origin-a', $1, '\\xaa', '2026-10-13T00:00:00Z', 'log', $2, 'token-a', 1000, \
             'sender-1', 'reserve-1', '\\x01', TRUE) \
         RETURNING id",
    )
    .bind(block_number)
    .bind(position_index)
    .fetch_one(pool)
    .await
    .expect("insert observation")
}

/// Makes the observation at a source coordinate final, as the endpoint of its network would, so a
/// transfer bound to it may reach `Source-Final` (review M1 of PR #56: the store refuses
/// Source-Final unless the bound observation is final; `INV-6`, `REQ-IN-2`). An observation already
/// there is kept as it is: it only gains its outcome, `final` at its block + 20, and that final
/// head (with the observed block's hash, unless one is recorded at that height already). With none there and the network undeclared (a test that never declared it), the network is
/// declared an origin network with a reserve, and an executed arrival of `asset` is recorded there.
/// An arrival's (or a conversion output's) token is admitted for `asset` when it is not admitted
/// already, so a deposit bound to it carries the observed token's family (review M1c, REQ-IN-4).
#[allow(clippy::too_many_arguments)]
pub async fn finalise_coordinate(
    pool: &PgPool,
    network: &str,
    block: i64,
    hash_hex: &str,
    position_kind: &str,
    position_index: i64,
    asset: &str,
    amount: &str,
) {
    exec(
        pool,
        &format!(
            "DO $$ \
             DECLARE o observations%ROWTYPE; head_hash BYTEA; reserve TEXT; \
             BEGIN \
               SELECT * INTO o FROM observations WHERE network = '{network}' \
                 AND block_number = {block} AND block_hash = '\\x{hash_hex}' \
                 AND position_kind = '{position_kind}' AND position_index = {position_index}; \
               IF NOT FOUND THEN \
                 reserve := '0xreserve-for-tests-' || '{network}'; \
                 IF EXISTS (SELECT 1 FROM networks n WHERE n.network = '{network}') \
                    AND NOT EXISTS (SELECT 1 FROM reserves r \
                                    WHERE r.network = '{network}' AND r.address = reserve) THEN \
                   RETURN; \
                 END IF; \
                 INSERT INTO networks (network, role, address_format) \
                   VALUES ('{network}', 'origin', 'opaque') ON CONFLICT DO NOTHING; \
                 INSERT INTO assets (asset, precision, assurance_class) \
                   VALUES ('{asset}', 6, 'public') ON CONFLICT DO NOTHING; \
                 INSERT INTO reserves (asset, network, address) \
                   VALUES ('{asset}', '{network}', reserve) ON CONFLICT DO NOTHING; \
                 INSERT INTO observations (network, block_number, block_hash, block_time, \
                     position_kind, position_index, token, amount, sender, recipient, executed, \
                     kind) \
                 VALUES ('{network}', {block}, '\\x{hash_hex}', '{BLOCK_TIME}', '{position_kind}', \
                     {position_index}, 'token-a', {amount}, '0xsender-for-tests', reserve, TRUE, \
                     'arrival') \
                 RETURNING * INTO o; \
               END IF; \
               IF o.kind IN ('arrival', 'conversion_output') AND o.token IS NOT NULL \
                  AND EXISTS (SELECT 1 FROM networks n WHERE n.network = o.network) THEN \
                 INSERT INTO assets (asset, precision, assurance_class) \
                   VALUES ('{asset}', 6, 'public') ON CONFLICT DO NOTHING; \
                 INSERT INTO admitted_tokens (network, token, asset, precision) \
                   SELECT o.network, o.token, a.asset, a.precision FROM assets a \
                   WHERE a.asset = '{asset}' ON CONFLICT DO NOTHING; \
               END IF; \
               IF NOT EXISTS (SELECT 1 FROM observation_outcomes WHERE observation_id = o.id) THEN \
                 SELECT block_hash INTO head_hash FROM final_heads \
                   WHERE network = o.network AND block_number = o.block_number + 20; \
                 IF head_hash IS NULL THEN \
                   head_hash := o.block_hash; \
                   INSERT INTO final_heads (network, block_number, block_hash, block_time) \
                     VALUES (o.network, o.block_number + 20, head_hash, '{BLOCK_TIME}'); \
                 END IF; \
                 INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
                     final_block_hash) \
                 VALUES (o.id, 'final', o.block_number + 20, head_hash); \
               ELSE \
                 INSERT INTO final_heads (network, block_number, block_hash, block_time) \
                 SELECT o.network, x.final_block_number, x.final_block_hash, '{BLOCK_TIME}' \
                 FROM observation_outcomes x WHERE x.observation_id = o.id AND x.outcome = 'final' \
                 ON CONFLICT DO NOTHING; \
               END IF; \
             END $$"
        ),
    )
    .await;
}

/// [`finalise_coordinate`] for the source transfer `id` is bound to; nothing when it is unbound.
pub async fn finalise_source(pool: &PgPool, id: i64) {
    let source: Option<(String, i64, String, String, i64, String, String)> = sqlx::query_as(
        "SELECT source_network, source_block_number, encode(source_block_hash, 'hex'), \
             source_position_kind::text, source_position_index, asset, amount::text \
         FROM transfers WHERE id = $1 AND source_network IS NOT NULL",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .expect("read the source binding");
    if let Some((network, block, hash, kind, index, asset, amount)) = source {
        finalise_coordinate(pool, &network, block, &hash, &kind, index, &asset, &amount).await;
    }
}

/// Records a balanced book entry: `amount` of `asset-a` from owed-mint to supply, on a reserve
/// arrival.
pub async fn balanced_entry(pool: &PgPool, reference: &str, amount: u64) -> i64 {
    let mut tx = pool.begin().await.expect("begin");
    let id: i64 =
        sqlx::query_scalar("INSERT INTO book_entries (reference) VALUES ($1) RETURNING id")
            .bind(reference)
            .fetch_one(&mut *tx)
            .await
            .expect("insert entry");
    exec_tx(
        &mut tx,
        &format!(
            "INSERT INTO book_postings (entry_id, asset, account, network, side, amount) VALUES \
               ({id}, 'asset-a', 'reserve', 'origin-a', 'debit', {amount}), \
               ({id}, 'asset-a', 'owed-mint', NULL, 'credit', {amount})"
        ),
    )
    .await;
    tx.commit().await.expect("commit balanced entry");
    id
}
