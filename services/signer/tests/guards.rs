// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! T-005-03, T-005-04, T-005-05: the signer signs only recorded intents, behind its guards
//! (`INV-9`, PLAN `DEC-3`); a test key holds no production authority (`AC-I4.2`); a restart
//! returns the recorded signature (`AC-D2.1`); an unrecorded signature starts the compromise path
//! (`AC-I1.2`).

mod common;

use bezent_signer::compromise::{self, ObservedSignature, Verdict};
use bezent_signer::config::Config;
use bezent_signer::keys::KeyBackend;
use bezent_signer::{Outcome, Refusal, Signed};
use common::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

fn signed(outcome: Outcome) -> Signed {
    match outcome {
        Outcome::Signed(s) => s,
        other => panic!("not signed: {other:?}"),
    }
}

fn refused(outcome: Outcome) -> Refusal {
    match outcome {
        Outcome::Refused(r) => r,
        other => panic!("not refused: {other:?}"),
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_9_unrecorded_intent_is_refused(pool: PgPoolOptions, connect: PgConnectOptions) {
    let f = Fixture::new(pool, connect).await;
    assert_eq!(refused(f.signer.sign(4242).await), Refusal::Unrecorded);
    assert_eq!(f.signatures().await, 0);
    assert!(
        f.log.text().contains("refused intent=4242 INV-9"),
        "{}",
        f.log.text()
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_9_recorded_intent_is_signed_over_its_exact_bytes(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let id = f
        .intent("kreivo-test", "mint", Some(b"\x01"), "10", None)
        .await;
    let s = signed(f.signer.sign(id).await);
    assert_eq!(
        (s.key_id.as_str(), s.scheme.as_str()),
        ("kreivo-mint", "sr25519")
    );
    assert!(!s.replayed);
    let public = schnorrkel::PublicKey::from_bytes(&s.public_key).unwrap();
    let message: String =
        sqlx::query_scalar("SELECT payload ->> 'message' FROM intents WHERE id = $1")
            .bind(id)
            .fetch_one(&f.admin)
            .await
            .unwrap();
    let message = hex::decode(message.strip_prefix("0x").unwrap()).unwrap();
    public
        .verify(
            schnorrkel::signing_context(b"substrate").bytes(&message),
            &schnorrkel::Signature::from_bytes(&s.signature).unwrap(),
        )
        .expect("the signature is over the intent's recorded bytes");
    let recorded: Vec<u8> =
        sqlx::query_scalar("SELECT signature FROM signatures WHERE intent_id = $1")
            .bind(id)
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert_eq!(recorded, s.signature);
    // REQ-AR-3: the signer logs every signature, with the intent id.
    assert!(
        f.log
            .text()
            .contains(&format!("signed intent={id} key=kreivo-mint"))
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_9_intent_without_exact_minor_units_is_refused(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let id = f.intent("kreivo-test", "mint", None, "1.5", None).await;
    assert_eq!(
        refused(f.signer.sign(id).await),
        Refusal::Malformed("amount")
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_3_reused_one_time_reference_is_refused(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let first = f
        .intent("kreivo-test", "mint", Some(b"\x0a\x0b"), "1", None)
        .await;
    let second = f
        .intent("kreivo-test", "mint", Some(b"\x0a\x0b"), "1", None)
        .await;
    signed(f.signer.sign(first).await);
    assert_eq!(
        refused(f.signer.sign(second).await),
        Refusal::ReferenceReused
    );
    assert_eq!(f.signatures().await, 1);
    assert!(
        f.log
            .text()
            .contains(&format!("refused intent={second} INV-2"))
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_3_amount_above_the_window_cap_is_refused(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    // The cap is 1000 per hour for kreivo-mint, across its purposes.
    let a = f.intent("kreivo-test", "mint", None, "600", None).await;
    let b = f
        .intent("kreivo-test", "refund-mint", None, "500", None)
        .await;
    let c = f.intent("kreivo-test", "mint", None, "400", None).await;
    let d = f.intent("kreivo-test", "mint", None, "1", None).await;
    signed(f.signer.sign(a).await);
    assert_eq!(
        refused(f.signer.sign(b).await),
        Refusal::WindowCap {
            cap: 1000,
            signed: 600,
            amount: 500
        }
    );
    signed(f.signer.sign(c).await);
    assert!(matches!(
        refused(f.signer.sign(d).await),
        Refusal::WindowCap { .. }
    ));
    // Another key's window is its own.
    let e = f.intent("polygon-test", "payout", None, "1000", None).await;
    signed(f.signer.sign(e).await);
    assert!(
        f.log
            .text()
            .contains(&format!("refused intent={b} REQ-KY-3"))
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_3_kreivo_destroy_from_another_account_is_refused(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let other = f
        .intent("kreivo-test", "destroy", None, "1", Some("0xdead"))
        .await;
    let unnamed = f.intent("kreivo-test", "destroy", None, "1", None).await;
    let own = f
        .intent("kreivo-test", "destroy", None, "1", Some(CONTRACT))
        .await;
    assert_eq!(refused(f.signer.sign(other).await), Refusal::DestroyNotOwn);
    assert_eq!(
        refused(f.signer.sign(unnamed).await),
        Refusal::DestroyNotOwn
    );
    // From the component's own account it passes this guard, and the signer still signs no
    // destroy: it decodes no such transaction, so it signs none (audit must-fix 2).
    assert_eq!(
        refused(f.signer.sign(own).await),
        Refusal::NotADelivery {
            purpose: "destroy".into()
        }
    );
    assert!(
        f.log
            .text()
            .contains(&format!("refused intent={other} INV-4"))
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i4_2_test_key_refuses_a_production_network_id(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    // At sign time: an intent for the production network is refused, whatever its purpose.
    for purpose in ["mint", "destroy"] {
        let id = f.intent("kreivo", purpose, None, "1", Some("0xc1c1")).await;
        assert_eq!(
            refused(f.signer.sign(id).await),
            Refusal::Environment {
                network: "kreivo".into(),
                network_environment: bezent_signer::keys::Environment::Production,
                key_environment: bezent_signer::keys::Environment::Test,
            }
        );
    }
    assert_eq!(f.signatures().await, 0);
    assert!(f.log.text().contains("REQ-KY-6 (AC-I4.2)"));

    // At start: a test key bound to the production network fails the configuration.
    let mut json = config_json(&f.dir, my_uid(), TOKEN);
    json["bindings"][0]["network"] = "kreivo".into();
    let config = Config::parse(json.to_string().as_bytes()).unwrap();
    let err = config.policy(&f.backend()).unwrap_err();
    assert!(err.0.contains("AC-I4.2"), "{err}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ky_4_intent_with_no_bound_key_is_refused(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let unbound = f.intent("polygon-test", "mint", None, "1", None).await;
    let undeclared = f.intent("base-test", "payout", None, "1", None).await;
    assert!(matches!(
        refused(f.signer.sign(unbound).await),
        Refusal::NoKey { .. }
    ));
    assert!(matches!(
        refused(f.signer.sign(undeclared).await),
        Refusal::UnknownNetwork(_)
    ));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_d2_1_restart_returns_the_recorded_signature(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let id = f
        .intent("kreivo-test", "mint", Some(b"\x01"), "10", None)
        .await;
    let first = signed(f.signer.sign(id).await);

    // A restart: nothing survives but the configuration, the key file and the store.
    let Fixture {
        admin,
        connect,
        signer,
        config,
        ..
    } = f;
    drop(signer);
    let (restarted, log, _) = Fixture::start(&config, &connect).await;
    let again = signed(restarted.sign(id).await);
    assert!(again.replayed);
    // sr25519 signatures are randomised: equal bytes mean nothing was signed again.
    assert_eq!(again.signature, first.signature);
    assert_eq!(again.public_key, first.public_key);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM signatures")
        .fetch_one(&admin)
        .await
        .unwrap();
    assert_eq!(rows, 1);
    // And a repeat on the same process is the same.
    assert_eq!(signed(restarted.sign(id).await).signature, first.signature);
    assert!(log.text().contains("replayed (AC-D2.1"));
    assert!(!log.text().contains(&format!("signed intent={id}")));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_unrecorded_signature_starts_the_compromise_path(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let core = f.core_pool().await;
    let id = f
        .intent("polygon-test", "payout", Some(b"\x07"), "10", None)
        .await;
    let s = signed(f.signer.sign(id).await);

    // bezent's own signature, with the recovery byte as an EVM transaction writes it.
    let mut on_chain = s.signature.clone();
    on_chain[64] += 27;
    let seen = |signature: Vec<u8>| ObservedSignature {
        network: "polygon-test".into(),
        public_key: s.public_key.clone(),
        signature,
        block_number: 100,
        block_hash: vec![0xbb],
        position_index: 3,
    };
    assert_eq!(
        compromise::check(&core, &seen(on_chain)).await.unwrap(),
        Verdict::Recorded { intent_id: id }
    );
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM event_log")
        .fetch_one(&f.admin)
        .await
        .unwrap();
    assert_eq!(events, 0);

    // The same key used outside the signer, over bytes no intent recorded.
    let rogue = f.backend().sign("polygon-payout", b"rogue").await.unwrap();
    let verdict = compromise::check(&core, &seen(rogue)).await.unwrap();
    let Verdict::Compromise {
        alert_seq,
        pause_seq,
    } = verdict
    else {
        panic!("not a compromise: {verdict:?}");
    };
    let rows: Vec<(i64, String, String)> =
        sqlx::query_as("SELECT seq, kind, payload::text FROM event_log ORDER BY seq")
            .fetch_all(&f.admin)
            .await
            .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (rows[0].0, rows[0].1.as_str()),
        (alert_seq, "key-compromise")
    );
    assert!(rows[0].2.contains(&hex::encode(&s.public_key)));
    assert!(rows[0].2.contains("\"block_number\": 100"));
    assert_eq!(
        (rows[1].0, rows[1].1.as_str()),
        (pause_seq, "pause-requested")
    );
    assert!(rows[1].2.contains("\"scope\": \"global\""));
}

/// Review M1's probe: the attacker holds the signer's host, so the key file and the signer's store
/// login. They sign bytes no intent recorded and, as `bezent_signer`, record that signature against
/// an intent the core recorded but has not had signed yet.
async fn plant_rogue_signature(f: &Fixture, intent: i64) -> (Vec<u8>, Vec<u8>) {
    let backend = f.backend();
    let public_key = backend.public_key("polygon-payout").unwrap();
    let rogue = backend
        .sign("polygon-payout", b"rogue drain")
        .await
        .unwrap();
    // SET LOCAL ROLE, in a transaction: the pooled connection never keeps the role (#27).
    let mut conn = f.admin.begin().await.unwrap();
    sqlx::raw_sql("SET LOCAL ROLE bezent_signer")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO signatures (intent_id, key_id, scheme, public_key, signature) \
         VALUES ($1, 'polygon-payout', 'secp256k1-evm', $2, $3)",
    )
    .bind(intent)
    .bind(&public_key)
    .bind(&rogue)
    .execute(&mut *conn)
    .await
    .expect("the signer role records a row");
    conn.commit().await.unwrap();
    (public_key, rogue)
}

/// The event log, in order.
pub async fn events(f: &Fixture) -> Vec<(i64, String, String)> {
    sqlx::query_as("SELECT seq, kind, payload::text FROM event_log ORDER BY seq")
        .fetch_all(&f.admin)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_row_the_signer_wrote_does_not_hide_a_rogue_signature(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let core = f.core_pool().await;
    let id = f
        .intent("polygon-test", "payout", Some(b"\x08"), "10", None)
        .await;
    let (public_key, rogue) = plant_rogue_signature(&f, id).await;

    // Seen on chain: the row names it, but it is not over the intent's recorded bytes.
    let mut on_chain = rogue.clone();
    on_chain[64] += 27;
    let verdict = compromise::check(
        &core,
        &ObservedSignature {
            network: "polygon-test".into(),
            public_key: public_key.clone(),
            signature: on_chain,
            block_number: 200,
            block_hash: vec![0xcc],
            position_index: 1,
        },
    )
    .await
    .unwrap();
    let Verdict::Compromise {
        alert_seq,
        pause_seq,
    } = verdict
    else {
        panic!("a rogue signature passed as bezent's own: {verdict:?}");
    };
    let rows = events(&f).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (rows[0].0, rows[0].1.as_str()),
        (alert_seq, "key-compromise")
    );
    assert!(
        rows[0]
            .2
            .contains(&format!("\"unverified_records\": [{id}]")),
        "{}",
        rows[0].2
    );
    assert_eq!(
        (rows[1].0, rows[1].1.as_str()),
        (pause_seq, "pause-requested")
    );
    assert!(rows[1].2.contains("\"scope\": \"global\""));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_malleated_signature_of_bezents_own_is_no_compromise(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let core = f.core_pool().await;
    let id = f
        .intent("polygon-test", "payout", Some(b"\x09"), "10", None)
        .await;
    let s = signed(f.signer.sign(id).await);
    // A third party flips s to n − s in the pending transaction (review N5).
    let sig = k256::ecdsa::Signature::from_slice(&s.signature[..64]).unwrap();
    let (r, low) = sig.split_scalars();
    let high = k256::ecdsa::Signature::from_scalars(r, -*low).unwrap();
    let mut on_chain = high.to_bytes().to_vec();
    on_chain.push(28 - s.signature[64]);
    let verdict = compromise::check(
        &core,
        &ObservedSignature {
            network: "polygon-test".into(),
            public_key: s.public_key.clone(),
            signature: on_chain,
            block_number: 201,
            block_hash: vec![0xcd],
            position_index: 0,
        },
    )
    .await
    .unwrap();
    assert_eq!(verdict, Verdict::Recorded { intent_id: id });
    assert!(events(&f).await.is_empty());
}

// ---------------------------------------------------------------------------------------------
// F-003, issue #24: one signature per (reference, attempt), attempt n+1 only after attempt n is
// proven not executed (REQ-EO-6, amendment 0009); a redemption's reference is its own namespace
// (amendment 0008), so the refund mint of a withdrawal is signed beside its payout.

/// Records the two steps of an attempt (PLAN DEC-3), as the core and the Kreivo endpoint would:
/// the order, the endpoint's prepared transaction (lane `\x01`, nonce 0, valid in blocks 0 to 5,
/// as `prove` describes it) over the exact bytes of the mint or refund mint at lane nonce `nonce`,
/// and the signing intent over it. Returns the signing intent.
async fn attempt_intent(
    f: &Fixture,
    purpose: &str,
    reference: &[u8],
    attempt: i32,
    nonce: u64,
) -> i64 {
    let d = delivery(
        "kreivo-test",
        purpose,
        reference32(Some(reference)),
        "1",
        nonce,
    );
    f.deliver(
        "kreivo-test",
        purpose,
        Some(reference),
        attempt,
        &d,
        "1",
        None,
    )
    .await
}

/// Proves intent `intent`'s signed transaction not executed, as the Kreivo endpoint would (F-007).
async fn prove(f: &Fixture, intent: i64) {
    sqlx::raw_sql(&format!(
        "INSERT INTO signed_transactions (intent_id, attempt, bytes) VALUES ({intent}, 1, '\\xbeef'); \
         INSERT INTO non_execution_proofs (signed_transaction_id, network, signer, nonce, \
             valid_from_block, valid_until_block, final_block_number, final_block_hash, \
             nonce_at_final, kind) \
         SELECT id, 'kreivo-test', '\\x01', 0, 0, 5, 10, '\\x0a', 0, 'nonce-unconsumed' \
         FROM signed_transactions WHERE intent_id = {intent}"
    ))
    .execute(&f.admin)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_eo_6_attempt_2_is_signed_once_attempt_1_is_proven(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let reference = reference32(Some(b"\x0a\x0b"));
    let first = attempt_intent(&f, "mint", &reference, 1, 900).await;
    signed(f.signer.sign(first).await);
    // A store that let attempt 2 be recorded unproven (its gate lifted here, as the owner) still
    // gets no signature: the signer refuses it.
    sqlx::raw_sql("ALTER TABLE intents DISABLE TRIGGER intents_guard_attempt")
        .execute(&f.admin)
        .await
        .unwrap();
    // Attempt 2 takes the lane's nonce again: attempt 1 expired with it unconsumed.
    let early = attempt_intent(&f, "mint", &reference, 2, 900).await;
    sqlx::raw_sql("ALTER TABLE intents ENABLE TRIGGER intents_guard_attempt")
        .execute(&f.admin)
        .await
        .unwrap();
    assert_eq!(
        refused(f.signer.sign(early).await),
        Refusal::AttemptNotProven { attempt: 2 }
    );
    // Once attempt 1 is proven, attempt 2 carries the same reference and is signed.
    prove(&f, first).await;
    signed(f.signer.sign(early).await);
    assert_eq!(f.signatures().await, 2);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_2_a_refund_mint_is_signed_beside_a_mint_with_the_same_reference(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    // The same 32-byte reference in another movement is another lane.
    let reference = [7u8; 32];
    let mint = attempt_intent(&f, "mint", &reference, 1, next_nonce()).await;
    let refund = attempt_intent(&f, "refund-mint", &reference, 1, next_nonce()).await;
    signed(f.signer.sign(mint).await);
    signed(f.signer.sign(refund).await);
    // The same movement and attempt twice is still refused.
    let again = attempt_intent(&f, "refund-mint", &reference, 1, next_nonce()).await;
    assert_eq!(
        refused(f.signer.sign(again).await),
        Refusal::ReferenceReused
    );
}

/// An EIP-1559 signing payload on chain `chain_id`, at `nonce`, of nothing to `to` with `data`,
/// with gas `gas` and fees 1 and 2 (the bytes a cancel, or something labelled one, carries).
fn evm_tx(chain_id: u64, nonce: u64, gas: u64, to: [u8; 20], data: &[u8]) -> Vec<u8> {
    bezent_signer::delivery::evm_signing_payload(&bezent_signer::delivery::EvmTx {
        chain_id,
        nonce,
        priority_fee: 1,
        max_fee: 2,
        gas,
        to,
        value: 0,
        data: data.to_vec(),
    })
}

/// The id of the signed transaction the endpoint persisted for signing intent `intent`.
async fn persist_signed(f: &Fixture, intent: i64) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO signed_transactions (intent_id, attempt, bytes) VALUES ($1, 1, '\\xbeef') \
         RETURNING id",
    )
    .bind(intent)
    .fetch_one(&f.admin)
    .await
    .unwrap()
}

/// PR #39 review S6 (`REQ-NX-4`, `INV-9`): a cancel carries no reference and moves nothing, so the
/// reference guard and the window cap pass it. A compromised core or endpoint labels a delivery
/// call `cancel`: the signer reads the bytes and refuses anything but a transfer of nothing from
/// the key to itself. Since the audit's must-fix 2, the cancel also answers the core's order: it
/// takes exactly the nonce of the signed transaction the order names, of the same key, and one at
/// another nonce or on another chain raises the alert.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s6_req_nx_4_a_delivery_labelled_cancel_is_refused(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    use k256::elliptic_curve::sec1::ToEncodedPoint;
    use sha3::{Digest, Keccak256};

    let f = Fixture::new(pool, connect).await;
    let public = f.backend().public_key("polygon-payout").unwrap();
    let point = k256::PublicKey::from_sec1_bytes(&public)
        .unwrap()
        .to_encoded_point(false);
    let own: [u8; 20] = Keccak256::digest(&point.as_bytes()[1..])[12..]
        .try_into()
        .unwrap();
    // The payout the cancel forces out: signed, persisted.
    let nonce = next_nonce();
    let reference = reference32(None);
    let payout = delivery("polygon-test", "payout", reference, "10", nonce);
    let payout = f
        .deliver(
            "polygon-test",
            "payout",
            Some(&reference),
            1,
            &payout,
            "10",
            None,
        )
        .await;
    signed(f.signer.sign(payout).await);
    let target = persist_signed(&f, payout).await;

    let fixture = &f;
    let cancel = |message: Vec<u8>| async move {
        let d = Delivery {
            order: serde_json::json!({"cancel": target}),
            payload: message.clone(),
            message,
        };
        fixture
            .deliver("polygon-test", "cancel", None, 1, &d, "0", None)
            .await
    };
    // A delivery call to the Roles modifier, labelled cancel.
    let delivery_call = cancel(evm_payout(10, reference, nonce)).await;
    assert_eq!(
        refused(f.signer.sign(delivery_call).await),
        Refusal::NotACancel
    );
    // A transfer of nothing to another address, or to itself with calldata.
    let elsewhere = cancel(evm_tx(137, nonce, 21_000, ROLES, &[])).await;
    assert_eq!(refused(f.signer.sign(elsewhere).await), Refusal::NotACancel);
    let with_data = cancel(evm_tx(137, nonce, 21_000, own, &[0x01])).await;
    assert_eq!(refused(f.signer.sign(with_data).await), Refusal::NotACancel);
    // SF2 (second review): with more gas than a bare transfer.
    let more_gas = cancel(evm_tx(137, nonce, 21_001, own, &[])).await;
    assert_eq!(refused(f.signer.sign(more_gas).await), Refusal::NotACancel);
    assert_eq!(f.signatures().await, 1);
    // Another chain id, or another nonce than the cancelled payout's: not the core's order.
    for (what, bytes) in [
        ("another chain", evm_tx(0xa0, nonce, 21_000, own, &[])),
        ("another nonce", evm_tx(137, nonce + 1, 21_000, own, &[])),
    ] {
        let id = cancel(bytes).await;
        assert!(
            matches!(
                refused(f.signer.sign(id).await),
                Refusal::NotTheOrder { .. }
            ),
            "{what}"
        );
    }
    assert_eq!(f.signatures().await, 1);
    // The cancel the endpoint builds is signed, at the payout's nonce.
    let real = cancel(evm_tx(137, nonce, 21_000, own, &[])).await;
    signed(f.signer.sign(real).await);
    assert_eq!(f.signatures().await, 2);
}

/// PR #39 second review MF1 (`INV-9`, `REQ-KY-4`): a Polygon intent never reaches the Kreivo key,
/// nor a Kreivo intent the Polygon key. A Kreivo key bound to an EVM network, or an EVM key bound
/// to a network that is not one, fails the configuration at start; so does an issuing network
/// that declares a chain id, and a cancel on a network with no fee cap (SF2).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn mf1_inv_9_a_key_signs_only_its_own_kind_of_network(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let refused_config = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut json = config_json(&f.dir, my_uid(), TOKEN);
        edit(&mut json);
        let config = Config::parse(json.to_string().as_bytes()).unwrap();
        config.policy(&f.backend()).unwrap_err()
    };
    // The Kreivo mint key bound to the Polygon network.
    let err = refused_config(&|j| j["bindings"][0]["network"] = "polygon-test".into());
    assert!(err.0.contains("INV-9"), "{err}");
    // The Polygon key bound to the Kreivo network.
    let err = refused_config(&|j| {
        j["bindings"][2]["network"] = "kreivo-test".into();
        j["bindings"][2]["purposes"] = serde_json::json!(["payout"]);
    });
    assert!(err.0.contains("INV-9"), "{err}");
    // An issuing network with a chain id.
    let err = refused_config(&|j| j["networks"][0]["chain_id"] = 1.into());
    assert!(err.0.contains("chain_id"), "{err}");
    // A cancel with no fee cap for its network.
    let err = refused_config(&|j| {
        j["networks"][1]
            .as_object_mut()
            .unwrap()
            .remove("cancel_max_fee_per_gas");
    });
    assert!(err.0.contains("REQ-NX-4"), "{err}");

    // At sign time: a Polygon intent of a Kreivo purpose finds no key, and a Kreivo intent of a
    // Polygon purpose neither; nothing is signed.
    let polygon_mint = f.intent("polygon-test", "mint", None, "1", None).await;
    let kreivo_payout = f.intent("kreivo-test", "payout", None, "1", None).await;
    for id in [polygon_mint, kreivo_payout] {
        assert!(matches!(
            refused(f.signer.sign(id).await),
            Refusal::NoKey { .. }
        ));
    }
    assert_eq!(f.signatures().await, 0);
}

/// PR #39 third review MF-A (`INV-9`, issue #38, `AC-I1.2`): a compromised core records, for
/// order A, an intent carrying order B's prepared bytes. The store refuses it (0037); with that
/// check off, as a core that wrote past the store would, the signer reads the prepared transaction
/// itself and refuses unless bytes, amount, order and network all match: B's bytes under A's
/// preparation, B's preparation under A's order, B's bytes naming no preparation, A's bytes with
/// another amount, or a preparation of another network. Only A's own intent is signed.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn mf_a_inv_9_the_signer_refuses_bytes_that_are_not_its_prepared_transactions(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    // A's bytes are the mint its order asks for (the signer decodes them, must-fix 2).
    let a = delivery(
        "kreivo-test",
        "mint",
        reference32(Some(b"\x0a")),
        "10",
        next_nonce(),
    );
    let (a_bytes, b_bytes) = (a.message.clone(), vec![0xb1, 0xb2, 0xb3]);
    let prepared_a = f
        .prepare_with(
            "kreivo-test",
            "mint",
            Some(&reference32(Some(b"\x0a"))),
            1,
            &a,
            "10",
        )
        .await;
    let prepared_b = f
        .prepare("kreivo-test", "mint", Some(b"\x0b"), 1, &b_bytes, "10")
        .await;
    let prepared_polygon = f
        .prepare(
            "polygon-test",
            "mint",
            Some(b"\x0a"),
            1,
            &evm_payload(137),
            "10",
        )
        .await;
    let payload = |message: &[u8], amount: &str, prepared: Option<i64>| {
        let mut p = serde_json::json!({
            "message": format!("0x{}", hex::encode(message)),
            "amount": amount,
        });
        if let Some(prepared) = prepared {
            p["prepared"] = prepared.into();
        }
        p
    };

    // The store refuses the reviewer's scenario outright.
    let err = sqlx::query(
        "INSERT INTO intents (network, purpose, on_chain_reference, payload) \
         VALUES ('kreivo-test', 'mint', '\\x0a', $1::text::jsonb)",
    )
    .bind(payload(&b_bytes, "10", Some(prepared_a)).to_string())
    .execute(&f.admin)
    .await
    .expect_err("the store recorded order A's intent over order B's bytes");
    assert_eq!(
        err.as_database_error().and_then(|d| d.code()).as_deref(),
        Some(bezent_store::REFUSAL_SQLSTATE),
        "{err}"
    );

    // Past the store, the signer refuses each forgery. One signing intent per prepared
    // transaction is the store's rule (0019), so each forgery under A's order names a preparation
    // of A's of its own.
    let again = f
        .prepare_with(
            "kreivo-test",
            "mint",
            Some(&reference32(Some(b"\x0a"))),
            1,
            &a,
            "10",
        )
        .await;
    let once_more = f
        .prepare_with(
            "kreivo-test",
            "mint",
            Some(&reference32(Some(b"\x0a"))),
            1,
            &a,
            "10",
        )
        .await;
    let forged = [
        (
            payload(&b_bytes, "10", Some(again)),
            "B's bytes under A's preparation",
        ),
        (
            payload(&b_bytes, "10", Some(prepared_b)),
            "B's preparation under A's order",
        ),
        (
            payload(&b_bytes, "10", None),
            "B's bytes naming no preparation",
        ),
        (
            payload(&a_bytes, "1", Some(once_more)),
            "A's bytes, another amount",
        ),
        (
            payload(&evm_payload(137), "10", Some(prepared_polygon)),
            "a Kreivo intent naming a Polygon preparation",
        ),
    ];
    for (body, what) in forged {
        let id = f
            .intent_past_the_store(
                "kreivo-test",
                "mint",
                Some(&reference32(Some(b"\x0a"))),
                &body,
            )
            .await;
        assert!(
            matches!(
                refused(f.signer.sign(id).await),
                Refusal::NotPrepared { .. }
            ),
            "{what}"
        );
    }
    assert_eq!(f.signatures().await, 0);

    // A's own intent: A's preparation, A's bytes, A's amount.
    let own = f
        .intent_past_the_store(
            "kreivo-test",
            "mint",
            Some(&reference32(Some(b"\x0a"))),
            &payload(&a_bytes, "10", Some(prepared_a)),
        )
        .await;
    assert_eq!(signed(f.signer.sign(own).await).intent_id, own);
}

/// PR #39 third review SF-B (`REQ-KY-4`, `INV-9`): one EVM key binds to one network only, by its
/// id and by its public key, and every EVM intent's bytes carry the chain id the policy declares
/// for its network. A compromised EVM endpoint that prepares a Polygon payout whose bytes carry
/// Base's chain id gets no signature for it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn sf_b_req_ky_4_an_evm_key_signs_for_one_network_and_its_chain_id_only(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let with_base = |j: &mut serde_json::Value, key: &str| {
        j["networks"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!(
                {"id": "base-test", "environment": "test", "chain_id": 8453}
            ));
        j["bindings"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!(
                {"key": key, "network": "base-test", "purposes": ["payout"],
                 "window_cap": "1000", "window_seconds": 3600}
            ));
    };
    // The Polygon key bound to Base as well.
    let mut json = config_json(&f.dir, my_uid(), TOKEN);
    with_base(&mut json, "polygon-payout");
    let err = Config::parse(json.to_string().as_bytes())
        .unwrap()
        .policy(&f.backend())
        .unwrap_err();
    assert!(err.0.contains("SF-B"), "{err}");
    // The same key under another id, bound to Base.
    let dir = temp_dir();
    let path = write_keys(&dir);
    let mut keys: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    keys["keys"].as_array_mut().unwrap().push(serde_json::json!(
        {"id": "base-payout", "scheme": "secp256k1-evm",
         "seed": format!("0x{}", hex::encode(seed("polygon-payout")))}
    ));
    std::fs::write(&path, keys.to_string()).unwrap();
    let twin = bezent_signer::keys::FileBackend::load(&path).unwrap();
    let mut json = config_json(&f.dir, my_uid(), TOKEN);
    with_base(&mut json, "base-payout");
    let err = Config::parse(json.to_string().as_bytes())
        .unwrap()
        .policy(&twin)
        .unwrap_err();
    assert!(err.0.contains("SF-B"), "{err}");

    // At sign time: a Polygon payout over bytes on Base's chain id, or over bytes that are no
    // transaction, is refused, its preparation and amount matching.
    let on_base = f
        .signing_intent(
            "polygon-test",
            "payout",
            Some(b"\x0b"),
            1,
            &evm_payload(8453),
            "10",
            None,
        )
        .await;
    // Since the audit's must-fix 2 the signer decodes the payout first: bytes on another chain
    // id are not the core's order, and raise the alert.
    assert!(matches!(
        refused(f.signer.sign(on_base).await),
        Refusal::NotTheOrder { .. }
    ));
    let no_tx = f
        .signing_intent(
            "polygon-test",
            "payout",
            Some(b"\x0c"),
            1,
            &[1, 2, 3, 4, 5],
            "10",
            None,
        )
        .await;
    assert!(matches!(
        refused(f.signer.sign(no_tx).await),
        Refusal::NotTheOrder { .. }
    ));
    assert_eq!(f.signatures().await, 0);
    // The payout its order asks for, on its own chain id, is signed.
    let on_polygon = f
        .intent("polygon-test", "payout", Some(b"\x0d"), "10", None)
        .await;
    signed(f.signer.sign(on_polygon).await);
}

// ---------------------------------------------------------------------------------------------
// The audit of PR #56, must-fix 2 (REQ-AR-3, INV-9, issue #23).

/// The key-compromise alerts the signer raised, with their detection keys.
async fn signer_alerts(f: &Fixture) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT payload ->> 'detection_key' FROM event_log \
         WHERE kind = 'key-compromise' AND payload ->> 'detector' = 'signer' ORDER BY seq",
    )
    .fetch_all(&f.admin)
    .await
    .unwrap()
}

/// Must-fix 2 (`REQ-AR-3`, `INV-9`, issue #23): the signer checked the bytes only against the
/// endpoint's own preparation record, and its window cap counted the order's amount. A compromised
/// endpoint prepares, for the core's order of a payout of 10 to the beneficiary (or a mint of 10 to
/// the recipient), bytes that pay or mint another amount, to another account, in another token,
/// under another reference, or on another network; the core records the signing intent over them
/// (the store sees only that they are the preparation's). The signer decodes the bytes itself,
/// compares them with the core's order, refuses every one and raises the key-compromise alert (and
/// its pause request) for each; nothing is signed. The window cap counts what the bytes move, per
/// key, network and asset.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn must_fix_2_the_signer_signs_only_bytes_that_answer_the_cores_order(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    use bezent_signer::delivery::{self, EvmPayout, EvmTx, KreivoMessage, KreivoTx};

    let f = Fixture::new(pool, connect).await;
    let payout_order = serde_json::json!({
        "to": format!("0x{}", hex::encode(BENEFICIARY)), "asset": ASSET
    });
    let evm = |token: [u8; 20], to: [u8; 20], amount: u128, reference: [u8; 32], chain: u64| {
        delivery::evm_signing_payload(&EvmTx {
            chain_id: chain,
            nonce: next_nonce(),
            priority_fee: 1,
            max_fee: 2,
            gas: 200_000,
            to: ROLES,
            value: 0,
            data: delivery::evm_payout_call(
                &contracts(),
                &EvmPayout {
                    token,
                    to,
                    amount,
                    reference,
                },
            ),
        })
    };
    let fixture = &f;
    let forged_payout = |bytes: Vec<u8>, reference: [u8; 32]| {
        let order = payout_order.clone();
        async move {
            let d = Delivery {
                order,
                payload: bytes.clone(),
                message: bytes,
            };
            fixture
                .deliver(
                    "polygon-test",
                    "payout",
                    Some(&reference),
                    1,
                    &d,
                    "10",
                    None,
                )
                .await
        }
    };
    let mut forged = Vec::new();
    for (what, make) in [
        ("an attacker's address", ([0x44; 20], [0x66; 20], 10, 137)),
        ("10^9 for 10", ([0x44; 20], BENEFICIARY, 1_000_000_000, 137)),
        ("another token", ([0x45; 20], BENEFICIARY, 10, 137)),
        ("another chain id", ([0x44; 20], BENEFICIARY, 10, 8453)),
    ] {
        let reference = reference32(None);
        let (token, to, amount, chain) = make;
        forged.push((
            what,
            forged_payout(evm(token, to, amount, reference, chain), reference).await,
        ));
    }
    // Another reference than the order's.
    let reference = reference32(None);
    forged.push((
        "another reference",
        forged_payout(
            evm([0x44; 20], BENEFICIARY, 10, reference32(None), 137),
            reference,
        )
        .await,
    ));

    // On the issuing network: a mint of the order's 10 to the recipient, forged.
    let mint_order = serde_json::json!({
        "asset": ISSUING_ASSET, "to": format!("0x{}", hex::encode(RECIPIENT))
    });
    let kreivo = |message: KreivoMessage, genesis: [u8; 32], contract: [u8; 32]| {
        delivery::kreivo_signing_payload(&KreivoTx {
            contract,
            value: 0,
            ref_time: 1_000_000_000,
            proof_size: 100_000,
            storage_deposit_limit: Some(10_000_000_000),
            data: message.data(),
            era: [0x05, 0x00],
            nonce: u32::try_from(next_nonce()).unwrap(),
            tip: 0,
            spec_version: 134,
            transaction_version: 13,
            genesis,
            birth_hash: [8; 32],
        })
    };
    let own_contract: [u8; 32] = hex::decode(CONTRACT.strip_prefix("0x").unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    for (what, to, amount, asset, genesis, contract) in [
        (
            "a mint to an attacker",
            [0x66; 32],
            10,
            101,
            GENESIS,
            own_contract,
        ),
        (
            "a mint of 10^9",
            RECIPIENT,
            1_000_000_000,
            101,
            GENESIS,
            own_contract,
        ),
        (
            "a mint of another asset",
            RECIPIENT,
            10,
            102,
            GENESIS,
            own_contract,
        ),
        (
            "a mint on another network",
            RECIPIENT,
            10,
            101,
            [0x11; 32],
            own_contract,
        ),
        (
            "a mint through another contract",
            RECIPIENT,
            10,
            101,
            GENESIS,
            [0x77; 32],
        ),
    ] {
        let reference = reference32(None);
        let payload = kreivo(
            KreivoMessage::Mint {
                asset,
                to,
                amount,
                reference,
            },
            genesis,
            contract,
        );
        let d = Delivery {
            order: mint_order.clone(),
            message: delivery::kreivo_message_to_sign(&payload),
            payload,
        };
        forged.push((
            what,
            f.deliver("kreivo-test", "mint", Some(&reference), 1, &d, "10", None)
                .await,
        ));
    }

    for (what, id) in &forged {
        let r = refused(f.signer.sign(*id).await);
        assert!(matches!(r, Refusal::NotTheOrder { .. }), "{what}: {r:?}");
        assert_eq!(r.spec(), "REQ-AR-3");
    }
    assert_eq!(f.signatures().await, 0, "nothing forged was signed");
    // One alert per forged intent, the signer's own, each with its pause request.
    let alerts = signer_alerts(&f).await;
    assert_eq!(
        alerts,
        forged
            .iter()
            .map(|(_, id)| format!("intent:{id}"))
            .collect::<Vec<_>>()
    );
    let pauses: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_log WHERE kind = 'pause-requested'")
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert_eq!(pauses, i64::try_from(forged.len()).unwrap());
    // Asked again, the same refusal, and no second alert.
    let (_, first) = forged[0];
    assert!(matches!(
        refused(f.signer.sign(first).await),
        Refusal::NotTheOrder { .. }
    ));
    assert_eq!(signer_alerts(&f).await.len(), forged.len());

    // What answers the order is signed, and the cap counts what the bytes move, per asset: the
    // payout key's 1000 per hour of `usd` takes 10 and then 990, and not 1 more.
    let ten = f.intent("polygon-test", "payout", None, "10", None).await;
    signed(f.signer.sign(ten).await);
    let rest = f.intent("polygon-test", "payout", None, "990", None).await;
    signed(f.signer.sign(rest).await);
    let over = f.intent("polygon-test", "payout", None, "1", None).await;
    assert_eq!(
        refused(f.signer.sign(over).await),
        Refusal::WindowCap {
            cap: 1000,
            signed: 1000,
            amount: 1
        }
    );
    let recorded: Vec<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT asset, nonce::text FROM signatures ORDER BY id")
            .fetch_all(&f.admin)
            .await
            .unwrap();
    assert!(
        recorded
            .iter()
            .all(|(asset, nonce)| asset.as_deref() == Some(ASSET) && nonce.is_some()),
        "{recorded:?}"
    );
}
