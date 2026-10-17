// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! T-009-04: payout preparation in two steps, the full decode-and-compare before any signature,
//! the persisted bytes and the nonce lane (`DEC-3`, `REQ-EO-1`, `REQ-EO-4`, `INV-9`, `INV-14`,
//! `AC-D2.1`).

mod common;

use alloy::primitives::{Address, B256, Bytes};
use bezent_evm_endpoint::evm;
use bezent_evm_endpoint::submit::{NO_EXPIRY, Prepared, Submitted, signing_intent_payload};
use common::{ASSET, Fixture, beneficiary, contracts, delivery, usdc, vault};
use serde_json::json;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

fn reference(n: u8) -> B256 {
    B256::repeat_byte(n)
}

async fn anomalies(f: &Fixture) -> i64 {
    f.count("SELECT count(*) FROM event_log WHERE kind = 'delivery-anomaly'")
        .await
}

async fn signatures(f: &Fixture) -> i64 {
    f.count("SELECT count(*) FROM signatures").await
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_3_a_payout_is_prepared_with_its_dry_run_and_its_exact_bytes(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    let order = f.order_payout(reference(1), beneficiary(), 700).await;
    assert_eq!(f.submitter().orders_to_prepare().await.unwrap(), [order]);
    let Prepared::Ready { id, message, nonce } = f.submitter().prepare(order).await.unwrap() else {
        panic!("not ready");
    };
    assert_eq!(nonce, 0);
    type Row = (Vec<u8>, i64, i64, Vec<u8>, Vec<u8>, bool, serde_json::Value);
    let (lane, valid_until, dry_block, call, stored, passed, dry_run): Row = sqlx::query_as(
        "SELECT lane, valid_until_block, dry_run_block_number, call, message, dry_run_passed, \
           dry_run FROM prepared_transactions WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(lane, f.lane.address.to_vec());
    assert_eq!(valid_until, NO_EXPIRY, "an EVM transaction does not expire");
    assert_eq!(dry_block, 4);
    assert!(passed);
    assert_eq!(dry_run["runs"].as_array().unwrap().len(), 2);
    assert_eq!(stored, message);
    // The stored bytes are exactly the delivery the order asks for.
    let tx = evm::decode_signing_payload(&stored).unwrap();
    assert_eq!(tx.chain_id, common::CHAIN_ID);
    assert_eq!(tx.to.to(), Some(&contracts().roles));
    assert_eq!(
        evm::decode_delivery_call(&contracts(), &tx.input),
        Some(delivery(reference(1), 700))
    );
    assert_eq!(call, [contracts().roles.as_slice(), &tx.input].concat());
    // Prepared once: a retry is the core's, as a new order.
    assert!(f.submitter().orders_to_prepare().await.unwrap().is_empty());
    assert!(matches!(
        f.submitter().prepare(order).await.unwrap(),
        Prepared::Refused(_)
    ));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_3_a_failed_dry_run_is_recorded_and_never_signed(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    // The Roles modifier refuses: the allowance is spent.
    let mut data = alloy::primitives::keccak256(b"ConditionViolation(uint8,bytes32)")[..4].to_vec();
    data.extend_from_slice(&[0u8; 64]);
    f.chain
        .set_call(Err(("execution reverted".into(), Some(Bytes::from(data)))));
    let order = f.order_payout(reference(1), beneficiary(), 700).await;
    let Prepared::DryRunFailed { id, reason } = f.submitter().prepare(order).await.unwrap() else {
        panic!("not failed");
    };
    assert!(reason.contains("ConditionViolation"), "{reason}");
    let message: Vec<u8> =
        sqlx::query_scalar("SELECT message FROM prepared_transactions WHERE id = $1")
            .bind(id)
            .fetch_one(&f.admin)
            .await
            .unwrap();
    let intent = f
        .intent_past_the_store(
            "payout",
            Some(reference(1).as_slice()),
            signing_intent_payload(id, &message, 700),
        )
        .await;
    let Submitted::Refused(why) = f.submitter().sign_and_submit(intent).await.unwrap() else {
        panic!("signed after a failed dry run");
    };
    assert!(why.contains("dry run"), "{why}");
    assert_eq!(signatures(&f).await, 0);
}

/// `ConditionViolation(status, key)` revert data, as the Roles modifier returns it.
fn violation(status: u8, key: B256) -> Bytes {
    let mut data = alloy::primitives::keccak256(b"ConditionViolation(uint8,bytes32)")[..4].to_vec();
    data.extend_from_slice(&[0u8; 31]);
    data.push(status);
    data.extend_from_slice(key.as_slice());
    Bytes::from(data)
}

/// The dry run recorded for prepared transaction `id`.
async fn dry_run(f: &Fixture, id: i64) -> serde_json::Value {
    sqlx::query_scalar("SELECT dry_run FROM prepared_transactions WHERE id = $1")
        .bind(id)
        .fetch_one(&f.admin)
        .await
        .unwrap()
}

/// The allowance key the S-2 tests refuse over.
fn spent_key() -> B256 {
    B256::repeat_byte(0x5a)
}

/// Prepares a payout whose dry run provider `a` refuses with `ConditionViolation(17,
/// spent_key())` and provider `b` with `b` (the same, when `None`, on one chain); returns the
/// dry run recorded.
async fn refused_by(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
    b: Option<Bytes>,
) -> serde_json::Value {
    let f = match &b {
        None => Fixture::new(pool, connect, 5).await,
        Some(_) => {
            let a = bezent_evm_endpoint::rpc::mock::MockChain::new(common::CHAIN_ID, 5);
            let fork = a.fork();
            Fixture::with_chains(pool, connect, a, fork).await
        }
    };
    let reverted = |data: Bytes| Err(("execution reverted".into(), Some(data)));
    f.chain.set_call(reverted(violation(17, spent_key())));
    if let Some(data) = b {
        f.chain_b.set_call(reverted(data));
    }
    let order = f.order_payout(reference(1), beneficiary(), 700).await;
    let Prepared::DryRunFailed { id, .. } = f.submitter().prepare(order).await.unwrap() else {
        panic!("not a failed dry run");
    };
    dry_run(&f, id).await
}

/// CND-PayoutWindow, PR #52 review S-2: the endpoint records the Roles refusal as a structured
/// `violation` (status and key) when both providers reverted with the same `ConditionViolation`.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_payout_window_s2_a_refusal_both_providers_agree_on_is_recorded_structured(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let recorded = refused_by(pool, connect, None).await;
    assert_eq!(
        recorded["violation"],
        json!({"status": 17, "key": alloy::hex::encode_prefixed(spent_key())})
    );
    assert_eq!(recorded["passed"], json!(false));
}

/// CND-PayoutWindow, PR #52 review S-2: two providers that refuse over different keys record no
/// structured refusal; the core then names no window hold.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_payout_window_s2_providers_that_disagree_record_no_refusal(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let other = violation(17, B256::repeat_byte(0x5b));
    let recorded = refused_by(pool, connect, Some(other)).await;
    assert_eq!(recorded["violation"], json!(null), "{recorded}");
    assert_eq!(recorded["passed"], json!(false));
}

/// CND-PayoutWindow, PR #52 review S-2: an `Error(string)` that spells the refusal's words is not
/// the refusal: no structured refusal is recorded, whatever the reason's text says.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_payout_window_s2_words_alone_record_no_refusal(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let text = format!(
        "Roles: ConditionViolation(status 17, {}) (execution reverted)",
        alloy::hex::encode_prefixed(spent_key())
    );
    let error: Bytes = [
        &[0x08, 0xc3, 0x79, 0xa0][..],
        &<alloy::sol_types::sol_data::String as alloy::sol_types::SolType>::abi_encode(&text),
    ]
    .concat()
    .into();
    let recorded = refused_by(pool, connect, Some(error)).await;
    assert_eq!(recorded["violation"], json!(null), "{recorded}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn t_009_04_no_signature_without_a_payload_that_decodes_to_the_order(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    // (a) A signing intent whose message is not the prepared bytes.
    let order = f.order_payout(reference(1), beneficiary(), 700).await;
    let prepared = f.submitter().prepare(order).await.unwrap();
    let Prepared::Ready { id, message, .. } = &prepared else {
        panic!("not ready");
    };
    let mut other = message.clone();
    *other.last_mut().unwrap() ^= 1;
    let intent = f
        .intent_past_the_store(
            "payout",
            Some(reference(1).as_slice()),
            signing_intent_payload(*id, &other, 700),
        )
        .await;
    assert!(matches!(
        f.submitter().sign_and_submit(intent).await.unwrap(),
        Submitted::Refused(_)
    ));

    // (b) A prepared transaction whose call pays someone else than the order names, recorded by
    // the endpoint's role (a bug, or a stolen login): its dry run passed, its bytes do not answer.
    let order2 = f.order_payout(reference(2), beneficiary(), 700).await;
    let mut theirs = delivery(reference(2), 700);
    theirs.to = Address::repeat_byte(0x66);
    let tx = evm::transaction(
        common::CHAIN_ID,
        1,
        contracts().roles,
        evm::delivery_call(&contracts(), &theirs),
        evm::Gas {
            limit: 260_000,
            max_fee: 90_000_000_000,
            priority_fee: 30_000_000_000,
        },
    );
    let payload = evm::signing_payload(&tx);
    let forged: i64 = sqlx::query_scalar(
        "INSERT INTO prepared_transactions (order_intent_id, attempt, network, lane, key_id, \
           nonce, valid_from_block, valid_until_block, checkpoint_hash, call, signing_payload, \
           message, dry_run_block_number, dry_run_block_hash, dry_run_passed, dry_run) \
         VALUES ($1, 1, 'polygon', $2, $3, 1, 4, $4, '\\x01', $5, $6, $6, 4, '\\x01', true, $7::jsonb) \
         RETURNING id",
    )
    .bind(order2)
    .bind(f.lane.address.to_vec())
    .bind(common::KEY)
    .bind(NO_EXPIRY)
    .bind([contracts().roles.as_slice(), &tx.input].concat())
    .bind(&payload)
    .bind(
        json!({"gas_limit": 260_000, "max_fee_per_gas": "90000000000",
               "max_priority_fee_per_gas": "30000000000"})
        .to_string(),
    )
    .fetch_one(&f.endpoint)
    .await
    .unwrap();
    let intent = f
        .intent(
            "payout",
            Some(reference(2).as_slice()),
            signing_intent_payload(forged, &payload, 700),
        )
        .await;
    let Submitted::Refused(why) = f.submitter().sign_and_submit(intent).await.unwrap() else {
        panic!("signed a payout to someone else");
    };
    assert!(why.contains("T-009-04"), "{why}");

    // (c) A signing intent whose amount is not its order's (one signing intent per prepared
    // transaction is the store's rule, so a fresh order).
    let order3 = f.order_payout(reference(3), beneficiary(), 700).await;
    let Prepared::Ready { id, message, .. } = f.submitter().prepare(order3).await.unwrap() else {
        panic!("not ready");
    };
    let intent = f
        .intent_past_the_store(
            "payout",
            Some(reference(3).as_slice()),
            signing_intent_payload(id, &message, 701),
        )
        .await;
    let Submitted::Refused(why) = f.submitter().sign_and_submit(intent).await.unwrap() else {
        panic!("signed another amount");
    };
    assert!(why.contains("INV-9"), "{why}");

    assert_eq!(signatures(&f).await, 0);
    assert_eq!(anomalies(&f).await, 3);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_eo_1_ac_d2_1_signed_bytes_are_persisted_then_sent_and_only_ever_re_sent(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    let (id, bytes) = f.signed_payout(reference(1), 700).await;
    // The bytes recover to the lane and are what was sent, to both providers.
    let (tx, _) = bezent_evm_endpoint::submit::decode_signed(&bytes).unwrap();
    assert_eq!(
        evm::decode_delivery_call(&contracts(), &tx.input),
        Some(delivery(reference(1), 700))
    );
    assert_eq!(f.chain.sent(), [bytes.clone(), bytes.clone()]);
    // A restart: the same bytes again, never a second signature (AC-D2.1).
    assert_eq!(f.submitter().resend().await.unwrap(), 1);
    assert_eq!(f.chain.sent().len(), 4);
    assert!(f.chain.sent().iter().all(|b| *b == bytes));
    let intent: i64 = sqlx::query_scalar("SELECT intent_id FROM signed_transactions WHERE id = $1")
        .bind(id)
        .fetch_one(&f.admin)
        .await
        .unwrap();
    assert!(matches!(
        f.submitter().sign_and_submit(intent).await.unwrap(),
        Submitted::Refused(_)
    ));
    assert_eq!(signatures(&f).await, 1);
    assert!(f.submitter().intents_to_sign().await.unwrap().is_empty());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_eo_4_the_lane_takes_consecutive_nonces(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    let mut nonces = Vec::new();
    for n in 1..=2u8 {
        let order = f.order_payout(reference(n), beneficiary(), 700).await;
        let Prepared::Ready { nonce, .. } = f.submitter().prepare(order).await.unwrap() else {
            panic!("not ready");
        };
        nonces.push(nonce);
    }
    assert_eq!(nonces, [0, 1]);
    // The chain moves past both (whoever consumed them): the next is the chain's.
    f.chain.set_nonce(f.lane.address, 4, 7);
    let order = f.order_payout(reference(3), beneficiary(), 700).await;
    let Prepared::Ready { nonce, .. } = f.submitter().prepare(order).await.unwrap() else {
        panic!("not ready");
    };
    assert_eq!(nonce, 7);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn t_009_04_fees_above_the_bound_or_an_empty_lane_fail_the_dry_run_as_fee_short(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    let mut s = f.submitter();
    s.gas.max_fee = 1_000;
    let order = f.order_payout(reference(1), beneficiary(), 700).await;
    let Prepared::DryRunFailed { reason, .. } = s.prepare(order).await.unwrap() else {
        panic!("prepared above the fee bound");
    };
    assert!(reason.starts_with("fee:"), "{reason}");
    f.chain
        .set_balance(f.lane.address, alloy::primitives::U256::from(1));
    let order = f.order_payout(reference(2), beneficiary(), 700).await;
    let Prepared::DryRunFailed { reason, .. } = f.submitter().prepare(order).await.unwrap() else {
        panic!("prepared with an empty lane");
    };
    assert!(
        reason.contains("InsufficientBalance") && reason.contains("fee"),
        "{reason}"
    );
    // A failed preparation holds no nonce.
    f.chain.set_balance(
        f.lane.address,
        alloy::primitives::U256::from(10u128.pow(21)),
    );
    let order = f.order_payout(reference(3), beneficiary(), 700).await;
    let Prepared::Ready { nonce, .. } = f.submitter().prepare(order).await.unwrap() else {
        panic!("not ready");
    };
    assert_eq!(nonce, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_14_a_payout_to_the_vault_or_of_another_token_is_not_prepared(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    let to_vault = f.order_payout(reference(1), vault(), 700).await;
    assert!(matches!(
        f.submitter().prepare(to_vault).await.unwrap(),
        Prepared::Refused(e) if e.contains("INV-14")
    ));
    let other_token = f
        .intent(
            "payout",
            Some(reference(2).as_slice()),
            json!({"order": {"to": beneficiary().to_string(), "asset": ASSET,
                             "token": common::usdt0().to_string()}, "amount": "700"}),
        )
        .await;
    assert!(matches!(
        f.submitter().prepare(other_token).await.unwrap(),
        Prepared::Refused(e) if e.contains("REQ-AS-3")
    ));
    let no_reference = f
        .intent(
            "payout",
            None,
            json!({"order": {"to": beneficiary().to_string(), "asset": ASSET,
                             "token": usdc().to_string()}, "amount": "700"}),
        )
        .await;
    assert!(matches!(
        f.submitter().prepare(no_reference).await.unwrap(),
        Prepared::Refused(e) if e.contains("REQ-ID-2")
    ));
    assert_eq!(
        f.count("SELECT count(*) FROM prepared_transactions").await,
        0
    );
}

/// PR #39 second review SF1 (`AC-I1.2`): the delivery key used while bezent had nothing in flight
/// (its nonce on chain moves past every nonce bezent signed, plus one) raises the compromise path,
/// an alert and a global pause request, before the next preparation or signature, and nothing is
/// prepared or signed from the lane.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn sf1_ac_i1_2_the_lane_used_while_idle_raises_compromise_and_pause(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    let compromises = "SELECT count(*) FROM event_log WHERE kind = 'key-compromise'";
    let pauses = "SELECT count(*) FROM event_log WHERE kind = 'pause-requested'";
    // bezent signs nonce 0, and it is included: the lane is at 1, nothing in flight.
    let (_, bytes) = f.signed_payout(reference(1), 700).await;
    let block = f.mine(&bytes, true);
    let order = f.order_payout(reference(2), beneficiary(), 700).await;
    let prepared = f.submitter().prepare(order).await.unwrap();
    let Prepared::Ready { nonce: 1, .. } = prepared else {
        panic!("not ready at nonce 1: {prepared:?}");
    };
    let intent = f
        .signing_intent("payout", Some(reference(2).as_slice()), &prepared, 700)
        .await;
    assert_eq!(f.count(compromises).await, 0);

    // The key is used outside bezent, twice: the lane's nonce is 3.
    for c in f.chains() {
        c.set_nonce(f.lane.address, block, 3);
    }
    let before = signatures(&f).await;
    let Submitted::Refused(why) = f.submitter().sign_and_submit(intent).await.unwrap() else {
        panic!("signed from a lane the key was used on outside bezent");
    };
    assert!(why.contains("SF1"), "{why}");
    assert_eq!(signatures(&f).await, before);
    assert_eq!(f.count(compromises).await, 1);
    assert_eq!(f.count(pauses).await, 1);

    // Nor is anything prepared from it; the alert is raised once for that nonce.
    let order = f.order_payout(reference(3), beneficiary(), 700).await;
    let Prepared::Refused(why) = f.submitter().prepare(order).await.unwrap() else {
        panic!("prepared from a lane the key was used on outside bezent");
    };
    assert!(why.contains("SF1"), "{why}");
    assert_eq!(f.count(compromises).await, 1);
}

/// PR #39 third review SF-B (`REQ-KY-4`, `INV-9`): a payout's decoded chain id is its order's
/// network's. A prepared transaction for a Polygon order whose bytes carry Base's chain id,
/// recorded by the endpoint's role (a bug, or a stolen login) with a passing dry run and a signing
/// intent over exactly those bytes, is never sent to the signer: the endpoint decodes the payload,
/// finds another chain id, records a delivery anomaly and signs nothing. (The signer refuses the
/// same bytes on its own: services/signer/tests/guards.rs,
/// `sf_b_req_ky_4_an_evm_key_signs_for_one_network_and_its_chain_id_only`.)
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn sf_b_req_ky_4_a_payout_on_another_chain_id_is_not_signed(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    let order = f.order_payout(reference(1), beneficiary(), 700).await;
    let tx = evm::transaction(
        8_453,
        0,
        contracts().roles,
        evm::delivery_call(&contracts(), &delivery(reference(1), 700)),
        evm::Gas {
            limit: 260_000,
            max_fee: 90_000_000_000,
            priority_fee: 30_000_000_000,
        },
    );
    let payload = evm::signing_payload(&tx);
    let forged: i64 = sqlx::query_scalar(
        "INSERT INTO prepared_transactions (order_intent_id, attempt, network, lane, key_id, \
           nonce, valid_from_block, valid_until_block, checkpoint_hash, call, signing_payload, \
           message, dry_run_block_number, dry_run_block_hash, dry_run_passed, dry_run) \
         VALUES ($1, 1, 'polygon', $2, $3, 0, 4, $4, '\\x01', $5, $6, $6, 4, '\\x01', true, $7::jsonb) \
         RETURNING id",
    )
    .bind(order)
    .bind(f.lane.address.to_vec())
    .bind(common::KEY)
    .bind(NO_EXPIRY)
    .bind([contracts().roles.as_slice(), &tx.input].concat())
    .bind(&payload)
    .bind(
        json!({"gas_limit": 260_000, "max_fee_per_gas": "90000000000",
               "max_priority_fee_per_gas": "30000000000"})
        .to_string(),
    )
    .fetch_one(&f.endpoint)
    .await
    .unwrap();
    let intent = f
        .intent(
            "payout",
            Some(reference(1).as_slice()),
            signing_intent_payload(forged, &payload, 700),
        )
        .await;
    let Submitted::Refused(why) = f.submitter().sign_and_submit(intent).await.unwrap() else {
        panic!("signed a payout on another chain");
    };
    assert!(why.contains("chain id 8453"), "{why}");
    assert_eq!(signatures(&f).await, 0);
    assert_eq!(anomalies(&f).await, 1);
}
