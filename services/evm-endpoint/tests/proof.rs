// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! T-009-06: the outcome of every signed transaction from chain data; non-execution proven only
//! by an identified consumer of the nonce, cross-checked; re-signed and wrapped copies never prove
//! (`REQ-NX-1` to `REQ-NX-4`, `REQ-OB-6`, `AC-C2.1`, `INV-8`; PLAN `DEC-8`, `DEC-12`).

mod common;

use alloy::primitives::{Address, B256, U256, keccak256};
use alloy::sol_types::SolCall;
use bezent_evm_endpoint::evm;
use bezent_evm_endpoint::observe::Step;
use bezent_evm_endpoint::proof::Outcome;
use bezent_evm_endpoint::rpc::mock::MockChain;
use bezent_evm_endpoint::submit::{Prepared, Submitted, decode_signed};
use common::{CHAIN_ID, Fixture, block_tx, lane_tx, sign_outside};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

fn reference(n: u8) -> B256 {
    B256::repeat_byte(n)
}

async fn follow(f: &Fixture) {
    while let Step::Read { .. } = f.follower().step().await.unwrap() {}
}

async fn outcome(f: &Fixture, signed: i64) -> Outcome {
    follow(f).await;
    f.prover()
        .step()
        .await
        .unwrap()
        .into_iter()
        .find(|(id, _)| *id == signed)
        .map(|(_, o)| o)
        .expect("the prover looked at it")
}

async fn proofs(f: &Fixture) -> i64 {
    f.count("SELECT count(*) FROM non_execution_proofs").await
}

/// Orders, prepares, has signed and submits a cancel of `signed`, as the core and the endpoint
/// would (`REQ-NX-4`). Returns the cancel's signed bytes.
async fn cancel(f: &Fixture, signed: i64) -> Vec<u8> {
    let order = f.order_cancel(signed).await;
    let prepared = f.submitter().prepare(order).await.unwrap();
    assert!(matches!(prepared, Prepared::Ready { .. }), "{prepared:?}");
    let intent = f.signing_intent("cancel", None, &prepared, 0).await;
    let Submitted::Submitted { id, .. } = f.submitter().sign_and_submit(intent).await.unwrap()
    else {
        panic!("the cancel was not submitted");
    };
    sqlx::query_scalar("SELECT bytes FROM signed_transactions WHERE id = $1")
        .bind(id)
        .fetch_one(&f.admin)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ob_6_an_executed_payout_is_recorded_executed_and_never_proven(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    let (signed, bytes) = f.signed_payout(reference(1), 700).await;
    // Not included yet: nothing is known.
    assert!(matches!(outcome(&f, signed).await, Outcome::Pending(_)));
    let block = f.mine(&bytes, true);
    f.advance(1);
    assert_eq!(outcome(&f, signed).await, Outcome::Executed { block });
    assert_eq!(proofs(&f).await, 0);
    let executed = f
        .count(
            "SELECT count(*) FROM observations WHERE position_kind = 'transaction' AND executed \
             AND kind = 'transaction'",
        )
        .await;
    assert_eq!(executed, 1);
    // And the payout leg is observed at the vault, with its reference (INV-14).
    let legs = f
        .count(
            "SELECT count(*) FROM observations WHERE detail ->> 'event' = 'Payout' \
             AND kind = 'payout_leg'",
        )
        .await;
    assert_eq!(legs, 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_nx_1_a_payout_refused_on_chain_is_proven_once_final(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    let (signed, bytes) = f.signed_payout(reference(1), 700).await;
    let block = f.mine(&bytes, false);
    f.advance(1);
    assert_eq!(
        outcome(&f, signed).await,
        Outcome::NotExecuted {
            kind: "refused",
            block
        }
    );
    let (kind, at, index): (String, i64, i64) = sqlx::query_as(
        "SELECT kind, at_block_number, at_position_index FROM non_execution_proofs \
         WHERE signed_transaction_id = $1",
    )
    .bind(signed)
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!((kind.as_str(), at, index), ("refused", block as i64, 0));
    let refused = f
        .count("SELECT count(*) FROM observations WHERE position_kind = 'transaction' AND NOT executed")
        .await;
    assert_eq!(refused, 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_c2_1_req_nx_4_a_forced_non_execution_is_proven_from_chain_data(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    let (signed, payout) = f.signed_payout(reference(1), 700).await;
    let cancel_bytes = cancel(&f, signed).await;
    let (cancel_tx, _) = decode_signed(&cancel_bytes).unwrap();
    let (payout_tx, _) = decode_signed(&payout).unwrap();
    // The cancel takes the payout's nonce, moves nothing, and pays more for gas.
    assert_eq!(cancel_tx.nonce, payout_tx.nonce);
    assert_eq!(cancel_tx.to.to(), Some(&f.lane.address));
    assert!(cancel_tx.value.is_zero() && cancel_tx.input.is_empty());
    assert!(cancel_tx.max_priority_fee_per_gas > payout_tx.max_priority_fee_per_gas);
    let block = f.mine(&cancel_bytes, true);
    f.advance(2);
    assert_eq!(
        outcome(&f, signed).await,
        Outcome::NotExecuted {
            kind: "nonce-consumed-by-other",
            block
        }
    );
    // What the proof names is checkable from chain data alone (REQ-NX-2).
    type Row = (
        String,
        Vec<u8>,
        i64,
        i64,
        i64,
        i64,
        Vec<u8>,
        i64,
        serde_json::Value,
    );
    let (network, signer, nonce, valid_until, final_block, at, at_hash, index, detail): Row =
        sqlx::query_as(
            "SELECT network, signer, nonce, valid_until_block, final_block_number, \
               at_block_number, at_block_hash, at_position_index, detail \
             FROM non_execution_proofs WHERE signed_transaction_id = $1",
        )
        .bind(signed)
        .fetch_one(&f.admin)
        .await
        .unwrap();
    assert_eq!(network, common::NETWORK);
    assert_eq!(signer, f.lane.address.to_vec());
    assert_eq!(valid_until, i64::MAX);
    let (header, txs) = f
        .pair
        .block_transactions(u64::try_from(at).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(header.hash.as_slice(), at_hash.as_slice());
    let consumer = &txs[usize::try_from(index).unwrap()];
    assert_eq!(consumer.from, f.lane.address);
    assert_eq!(consumer.nonce, u64::try_from(nonce).unwrap());
    assert_ne!(consumer.hash, keccak256(&payout));
    assert_eq!(detail["consumer"], serde_json::json!(consumer.hash));
    let after = f
        .pair
        .nonce_at(f.lane.address, u64::try_from(final_block).unwrap())
        .await
        .unwrap();
    assert!(after > u64::try_from(nonce).unwrap());
    // The cancel itself executed.
    assert_eq!(proofs(&f).await, 1);
}

/// The other valid signature over the same bytes (`s` replaced by `n − s`): a re-signed copy, as
/// a stolen key or a second signing would make one.
fn twin(signature: &[u8]) -> Vec<u8> {
    let n = U256::from_str_radix(
        "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141",
        16,
    )
    .unwrap();
    let s = U256::from_be_slice(&signature[32..64]);
    let mut out = signature[..32].to_vec();
    out.extend_from_slice(&(n - s).to_be_bytes::<32>());
    out.push(signature[64] ^ 1);
    out
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_c2_1_a_re_signed_copy_never_proves_and_executing_counts(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    // Refused: unknown, never proof.
    let (signed, bytes) = f.signed_payout(reference(1), 700).await;
    let (tx, signature) = decode_signed(&bytes).unwrap();
    let copy = block_tx(&tx, twin(&signature), keccak256(b"a re-signed copy"), 0);
    assert_eq!(copy.from, f.lane.address);
    f.mine_tx(copy, false);
    f.advance(1);
    let Outcome::Unknown(why) = outcome(&f, signed).await else {
        panic!("a refused copy decided something");
    };
    assert!(why.contains("re-signed copy"), "{why}");
    assert_eq!(proofs(&f).await, 0);
    // Executed: the payout executed, whoever's bytes carried it.
    let (signed2, bytes2) = f.signed_payout(reference(2), 700).await;
    let (tx2, signature2) = decode_signed(&bytes2).unwrap();
    let copy2 = block_tx(&tx2, twin(&signature2), keccak256(b"another copy"), 0);
    let block = f.mine_tx(copy2, true);
    f.advance(1);
    assert_eq!(outcome(&f, signed2).await, Outcome::Executed { block });
    assert_eq!(proofs(&f).await, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_c2_1_a_wrapped_copy_never_proves(pool: PgPoolOptions, connect: PgConnectOptions) {
    let f = Fixture::new(pool, connect, 5).await;
    let (signed, _) = f.signed_payout(reference(1), 700).await;
    // The delivery key, at the payout's nonce, calls another contract with the reference inside.
    let wrapper = Address::repeat_byte(0x99);
    let inner = evm::emitReferenceCall {
        r#ref: reference(1),
    }
    .abi_encode();
    let wrapped = sign_outside(&lane_tx(0, wrapper, [vec![0xab; 4], inner].concat()));
    f.mine_tx(wrapped, true);
    f.advance(1);
    let Outcome::Unknown(why) = outcome(&f, signed).await else {
        panic!("a wrapped copy proved something");
    };
    assert!(why.contains("carries this reference"), "{why}");
    assert_eq!(proofs(&f).await, 0);
    // bezent did not sign it: the compromise path (S2), not only an anomaly.
    assert_eq!(
        f.count("SELECT count(*) FROM event_log WHERE kind = 'key-compromise'")
            .await,
        1
    );
}

/// PR #39 review S5 (`AC-I1.2`, `REQ-NX-4`): a lane that holds code (an EIP-7702 delegation)
/// would run it on a cancel, a transfer of nothing to itself. No cancel is prepared or signed from
/// it, and the compromise path starts.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s5_ac_i1_2_no_cancel_from_a_lane_that_holds_code(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    let (signed, _) = f.signed_payout(reference(1), 700).await;
    let mut delegation = vec![0xef, 0x01, 0x00];
    delegation.extend_from_slice(Address::repeat_byte(0x77).as_slice());
    let set_code = |code: &[u8]| {
        for c in f.chains() {
            c.set_code(f.lane.address, code.to_vec());
        }
    };
    let compromises = "SELECT count(*) FROM event_log WHERE kind = 'key-compromise'";

    // Prepared: refused.
    set_code(&delegation);
    let order = f.order_cancel(signed).await;
    let Prepared::Refused(why) = f.submitter().prepare(order).await.unwrap() else {
        panic!("a cancel was prepared from a lane that holds code");
    };
    assert!(why.contains("holds code"), "{why}");
    assert_eq!(f.count(compromises).await, 1);
    assert_eq!(
        f.count("SELECT count(*) FROM event_log WHERE kind = 'pause-requested'")
            .await,
        1
    );

    // The code appears after the cancel is prepared: it is not signed.
    set_code(&[]);
    let order = f.order_cancel(signed).await;
    let prepared = f.submitter().prepare(order).await.unwrap();
    assert!(matches!(prepared, Prepared::Ready { .. }), "{prepared:?}");
    let intent = f.signing_intent("cancel", None, &prepared, 0).await;
    set_code(&delegation);
    let signatures = f.count("SELECT count(*) FROM signatures").await;
    let Submitted::Refused(why) = f.submitter().sign_and_submit(intent).await.unwrap() else {
        panic!("a cancel was signed from a lane that holds code");
    };
    assert!(why.contains("holds code"), "{why}");
    assert_eq!(f.count("SELECT count(*) FROM signatures").await, signatures);
    assert_eq!(f.count(compromises).await, 1, "raised once per lane");
}

/// PR #39 review S2 (`AC-I1.2`, `INV-8`): the delivery key consumes the payout's nonce with the
/// same delivery and other gas, signed outside bezent. bezent never signs a second payload for one
/// reference and attempt, so this is the key used by someone else: alert and pause, unknown, and
/// never a proof.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s2_ac_i1_2_a_lane_transaction_carrying_the_reference_raises_compromise_and_pause(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    let (signed, bytes) = f.signed_payout(reference(1), 700).await;
    let (ours, _) = decode_signed(&bytes).unwrap();
    let call = evm::delivery_call(&common::contracts(), &common::delivery(reference(1), 700));
    let copy = lane_tx(ours.nonce, common::contracts().roles, call);
    assert_ne!(evm::signing_payload(&copy), evm::signing_payload(&ours));
    f.mine_tx(sign_outside(&copy), true);
    f.advance(1);
    let Outcome::Unknown(why) = outcome(&f, signed).await else {
        panic!("a copy with other gas decided something");
    };
    assert!(why.contains("carries this reference"), "{why}");
    assert_eq!(proofs(&f).await, 0);
    for kind in ["key-compromise", "pause-requested"] {
        assert_eq!(
            f.count(&format!(
                "SELECT count(*) FROM event_log WHERE kind = '{kind}'"
            ))
            .await,
            1,
            "{kind}"
        );
    }
    // A second look raises nothing new.
    let _ = outcome(&f, signed).await;
    assert_eq!(
        f.count("SELECT count(*) FROM event_log WHERE kind = 'key-compromise'")
            .await,
        1
    );
}

/// PR #39 review S1 (`INV-8`, `INV-14`): anyone can call the emitter. A bare `Reference` of the
/// payout's reference, in a transaction where the vault moves nothing, is not a payout and never
/// blocks the proof of a forced non-execution; a reference beside a movement of the vault, in the
/// same transaction, still does.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s1_inv_8_a_bare_emit_of_the_reference_never_blocks_the_proof(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    let emitter = common::contracts().emitter;
    // Someone reads the reference from the mempool and emits it first.
    let (signed, _) = f.signed_payout(reference(1), 700).await;
    let h = f.chain.mine();
    f.chain.log(
        h.number,
        0,
        B256::repeat_byte(0x41),
        0,
        emitter,
        vec![evm::REFERENCE_TOPIC, reference(1)],
        vec![],
    );
    let cancel_bytes = cancel(&f, signed).await;
    let block = f.mine(&cancel_bytes, true);
    f.advance(1);
    assert_eq!(
        outcome(&f, signed).await,
        Outcome::NotExecuted {
            kind: "nonce-consumed-by-other",
            block
        }
    );
    assert_eq!(
        f.count("SELECT count(*) FROM observations WHERE detail ->> 'event' = 'UnpairedReference'")
            .await,
        1
    );

    // The reference beside the vault's own transfer, not adjacent to it: it blocks.
    let (signed2, _) = f.signed_payout(reference(2), 700).await;
    let h = f.chain.mine();
    let tx = B256::repeat_byte(0x42);
    f.chain.log(
        h.number,
        0,
        tx,
        0,
        emitter,
        vec![evm::REFERENCE_TOPIC, reference(2)],
        vec![],
    );
    common::transfer_log(
        &f.chain,
        h.number,
        1,
        tx,
        common::usdc(),
        common::vault(),
        common::beneficiary(),
        U256::from(700),
    );
    let cancel_bytes = cancel(&f, signed2).await;
    f.mine(&cancel_bytes, true);
    f.advance(1);
    let Outcome::Unknown(why) = outcome(&f, signed2).await else {
        panic!("proven though the vault moved value beside the reference");
    };
    assert!(why.contains("on chain"), "{why}");
    assert_eq!(proofs(&f).await, 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_nx_3_a_consumer_bezent_did_not_sign_proves_nothing_and_raises_compromise(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    let (signed, _) = f.signed_payout(reference(1), 700).await;
    f.mine_tx(
        sign_outside(&lane_tx(0, Address::repeat_byte(0x42), vec![])),
        true,
    );
    f.advance(1);
    let Outcome::Unknown(why) = outcome(&f, signed).await else {
        panic!("an unidentified consumer decided something");
    };
    assert!(why.contains("no recorded intent signed"), "{why}");
    assert_eq!(proofs(&f).await, 0);
    assert_eq!(
        f.count("SELECT count(*) FROM event_log WHERE kind = 'key-compromise'")
            .await,
        1
    );
    assert_eq!(
        f.count("SELECT count(*) FROM event_log WHERE kind = 'pause-requested'")
            .await,
        1
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_8_a_reference_already_on_chain_blocks_the_proof(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    let (signed, _) = f.signed_payout(reference(1), 700).await;
    // The reference reached the vault some other way (a copy at another nonce).
    let h = f.chain.mine();
    common::payout_logs(
        &f.chain,
        h.number,
        B256::repeat_byte(0x31),
        &common::delivery(reference(1), 700),
    );
    let cancel_bytes = cancel(&f, signed).await;
    f.mine(&cancel_bytes, true);
    f.advance(1);
    let Outcome::Unknown(why) = outcome(&f, signed).await else {
        panic!("proven though the reference is on chain");
    };
    assert!(why.contains("on chain"), "{why}");
    assert_eq!(proofs(&f).await, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_12_req_nx_3_providers_that_disagree_prove_nothing(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let a = MockChain::new(CHAIN_ID, 5);
    let b = a.fork();
    let f = Fixture::with_chains(pool, connect, a, b).await;
    let (signed, _) = f.signed_payout(reference(1), 700).await;
    let cancel_bytes = cancel(&f, signed).await;
    f.mine(&cancel_bytes, true);
    f.advance(1);
    let Outcome::Pending(why) = outcome(&f, signed).await else {
        panic!("decided on providers that disagree");
    };
    assert!(why.contains("DEC-12"), "{why}");
    assert_eq!(proofs(&f).await, 0);
}
