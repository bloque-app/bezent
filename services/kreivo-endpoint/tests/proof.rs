// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! T-007-04: a signed transaction proven not executed from chain data anyone can check
//! (`REQ-NX-1`, `REQ-NX-2`, `REQ-NX-3`, `AC-C2.1`), an on-chain refusal counted once final, a
//! transaction of the delivery key bezent did not record caught (`AC-I1.2`), and no proof unless
//! the nonce's consumer is identified and is not this transaction (review M1, `INV-8`).

mod common;

use bezent_kreivo_endpoint::chain::{BlockRef, Chain};
use bezent_kreivo_endpoint::kreivo::{self, MAX_ERA, Mortality, TxParams};
use bezent_kreivo_endpoint::proof::{Disposition, ProofKind};
use bezent_kreivo_endpoint::signal::ANOMALY;
use bezent_kreivo_endpoint::submit::{Prepared, Submitted, extrinsic_from_payload};
use bezent_signer::keys::SigningEngine;
use common::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

const TO: [u8; 32] = [0x77; 32];

/// Prepares, signs and persists a mint whose submission the node refuses. Returns the signed
/// transaction's id.
async fn signed_but_not_sent(f: &Fixture, reference: [u8; 32]) -> i64 {
    let s = f.submitter();
    let order = f.order_mint(reference, TO, 3).await;
    let prepared = s.prepare(order).await.unwrap();
    assert!(matches!(prepared, Prepared::Ready { .. }), "{prepared:?}");
    let signing = f.signing_intent("mint", reference, &prepared, 3).await;
    f.chain.refuse_submissions(true);
    let out = s.sign_and_submit(signing).await.unwrap();
    f.chain.refuse_submissions(false);
    let Submitted::Persisted { id, .. } = out else {
        panic!("{out:?}");
    };
    id
}

/// A transaction of the delivery key made outside the signer, at `nonce`: a mint of `amount` to
/// `to` with `reference`, born at the finalized head.
async fn outside_the_signer(
    f: &Fixture,
    nonce: u32,
    reference: [u8; 32],
    to: [u8; 32],
    amount: u128,
) -> Vec<u8> {
    let head = f.chain.finalized_ref();
    let call = kreivo::contracts_call(
        &CONTRACT,
        &kreivo::CallLimits {
            gas_limit: kreivo::Weight::default(),
            storage_deposit_limit: None,
        },
        &kreivo::mint_data(kreivo::AssetId::Here(101), &to, amount, &reference),
    );
    let p = TxParams {
        genesis: GENESIS,
        spec_version: 134,
        transaction_version: 13,
        era: Mortality::born_at(head.number),
        birth_hash: head.hash,
        nonce,
        tip: 0,
    };
    let message = kreivo::message_to_sign(&kreivo::signing_payload(&call, &p));
    let signature = f.keys.sign("kreivo-mint", &message).await.unwrap();
    kreivo::extrinsic(
        &f.lane.account,
        kreivo::Scheme::Sr25519,
        &signature,
        &p,
        &call,
    )
}

async fn compromises(f: &Fixture) -> i64 {
    f.count("SELECT count(*) FROM event_log WHERE kind = 'key-compromise'")
        .await
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_c2_1_an_expired_transaction_whose_nonce_was_never_consumed_is_proven_not_executed(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let id = signed_but_not_sent(&f, [1; 32]).await;
    // While the era is open, it is unknown (REQ-NX-3): nothing is recorded.
    f.chain.advance(MAX_ERA - 2);
    let found = f.prover().step().await.unwrap();
    assert!(
        matches!(found.as_slice(), [(_, Disposition::Unknown(_))]),
        "{found:?}"
    );
    assert_eq!(
        f.count("SELECT count(*) FROM non_execution_proofs").await,
        0
    );

    f.chain.advance(2);
    let found = f.prover().step().await.unwrap();
    let [(i, Disposition::NotExecuted(p))] = found.as_slice() else {
        panic!("{found:?}");
    };
    assert_eq!(*i, id);
    assert_eq!(p.kind, ProofKind::NonceUnconsumed);

    // AC-C2.1: the record names network, signer, nonce, expiry and the final block...
    type Row = (String, Vec<u8>, i64, i64, i64, i64, Vec<u8>, i64, String);
    let r: Row = sqlx::query_as(
        "SELECT network, signer, nonce, valid_from_block, valid_until_block, final_block_number, \
           final_block_hash, nonce_at_final, kind FROM non_execution_proofs",
    )
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(r.0, NETWORK);
    assert_eq!(r.1, f.lane.account.to_vec());
    assert_eq!(r.8, "nonce-unconsumed");
    // ...and anyone can check it from chain data alone: the final block is the chain's, at or
    // past the era's last valid block, and the signer's nonce there has not passed the nonce.
    let final_block = BlockRef {
        number: r.5 as u64,
        hash: r.6.clone().try_into().unwrap(),
    };
    assert_eq!(f.chain.block(final_block.number), final_block);
    assert!(r.5 >= r.4);
    let nonce = f
        .chain
        .account_nonce(&final_block, &f.lane.account)
        .await
        .unwrap();
    assert_eq!(nonce as i64, r.7);
    assert!(nonce <= r.2 as u64);
    // The era in the signed bytes is the one the proof names.
    let bytes: Vec<u8> = sqlx::query_scalar("SELECT bytes FROM signed_transactions")
        .fetch_one(&f.admin)
        .await
        .unwrap();
    let d = kreivo::decode_extrinsic(&bytes).unwrap();
    let birth = Mortality::birth_at(d.era, r.3 as u64);
    assert_eq!(birth as i64, r.3);
    assert_eq!(Mortality::period_phase(d.era).0, MAX_ERA);
    assert_eq!(i64::from(d.nonce), r.2);

    // And it is recorded once.
    assert!(f.prover().step().await.unwrap().is_empty());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_nx_1_a_nonce_consumed_by_another_recorded_transaction_proves_non_execution(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let a = signed_but_not_sent(&f, [2; 32]).await;
    // A's era ends unsent; the lane takes nonce 0 again for another order, B, which is included.
    f.chain.advance(MAX_ERA);
    let s = f.submitter();
    let order = f.order_mint([0x22; 32], TO, 5).await;
    let prepared = s.prepare(order).await.unwrap();
    assert!(
        matches!(prepared, Prepared::Ready { nonce: 0, .. }),
        "{prepared:?}"
    );
    let signing = f.signing_intent("mint", [0x22; 32], &prepared, 5).await;
    let Submitted::Submitted { id: b, .. } = s.sign_and_submit(signing).await.unwrap() else {
        panic!("B is not submitted");
    };
    f.chain.advance(1);
    let consumed_at = f.chain.finalized_ref();

    let found = f.prover().step().await.unwrap();
    let [
        (i, Disposition::NotExecuted(p)),
        (j, Disposition::Executed { .. }),
    ] = found.as_slice()
    else {
        panic!("{found:?}");
    };
    assert_eq!((*i, *j), (a, b));
    assert!(
        matches!(p.kind, ProofKind::NonceConsumedByOther { at, index: 0, .. } if at == consumed_at),
        "{p:?}"
    );
    // Checkable from chain data: the nonce passes N at that block and not before, and the
    // extrinsic there is B, another payload with another reference.
    let parent = f.chain.block(consumed_at.number - 1);
    assert!(
        f.chain
            .account_nonce(&parent, &f.lane.account)
            .await
            .unwrap()
            <= p.nonce
    );
    let consumer = &f.chain.extrinsics(&consumed_at).await.unwrap()[0];
    let ours: Vec<u8> = sqlx::query_scalar("SELECT bytes FROM signed_transactions WHERE id = $1")
        .bind(a)
        .fetch_one(&f.admin)
        .await
        .unwrap();
    let (ours, consumer) = (
        kreivo::decode_extrinsic(&ours).unwrap(),
        kreivo::decode_extrinsic(consumer).unwrap(),
    );
    assert_eq!(consumer.nonce, ours.nonce);
    assert!(!consumer.same_payload(&ours));
    let (kind, at, index): (String, i64, i64) =
        sqlx::query_as("SELECT kind, at_block_number, at_position_index FROM non_execution_proofs")
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert_eq!(
        (kind.as_str(), at as u64, index),
        ("nonce-consumed-by-other", consumed_at.number, 0)
    );
    // The consumer is bezent's own: no alarm.
    assert_eq!(compromises(&f).await, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_nx_1_a_re_signed_copy_of_the_same_payload_is_this_transaction(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // Review M1: two signatures over one payload are one mint. sr25519 signatures are randomised,
    // so the copy hashes differently; it must never count as "another transaction".
    let f = Fixture::new(pool, connect).await;
    let id = signed_but_not_sent(&f, [3; 32]).await;
    let (call, payload, ours): (Vec<u8>, Vec<u8>, Vec<u8>) = sqlx::query_as(
        "SELECT p.call, p.signing_payload, st.bytes FROM prepared_transactions p, signed_transactions st",
    )
    .fetch_one(&f.admin)
    .await
    .unwrap();
    let message = kreivo::message_to_sign(&payload);
    let signature = f.keys.sign("kreivo-mint", &message).await.unwrap();
    let copy = extrinsic_from_payload(&f.lane, &signature, &call, &payload).unwrap();
    assert_ne!(copy, ours);
    f.chain.submit(&copy).await.unwrap();
    f.chain.advance(1);
    assert_eq!(f.chain.nonce(&f.lane.account), 1);

    let found = f.prover().step().await.unwrap();
    let [(i, Disposition::Executed { hash, index: 0, .. })] = found.as_slice() else {
        panic!("{found:?}");
    };
    assert_eq!(*i, id);
    assert_eq!(*hash, kreivo::blake2_256(&copy));
    // No proof: the mint executed. Its inclusion is this transaction's observation.
    assert_eq!(
        f.count("SELECT count(*) FROM non_execution_proofs").await,
        0
    );
    assert_eq!(
        f.count(
            "SELECT count(*) FROM observations WHERE position_kind = 'transaction' AND executed \
             AND (detail ->> 'resigned')::boolean"
        )
        .await,
        1
    );
    // AC-I1.2: the copy's signature is not one the signer recorded.
    assert_eq!(compromises(&f).await, 1);
    // Nothing is left to prove.
    assert!(f.prover().step().await.unwrap().is_empty());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_nx_3_a_nonce_consumed_by_an_unidentified_extrinsic_proves_nothing(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    signed_but_not_sent(&f, [4; 32]).await;
    // The lane's nonce is consumed by something that is no extrinsic of the key the endpoint
    // can decode.
    f.chain.consume_nonce(f.lane.account);
    f.chain.advance(1);
    for _ in 0..2 {
        let found = f.prover().step().await.unwrap();
        assert!(
            matches!(found.as_slice(), [(_, Disposition::Unknown(_))]),
            "{found:?}"
        );
    }
    // It stays unknown after the era ends: the consumer is still not identified.
    f.chain.advance(MAX_ERA);
    let found = f.prover().step().await.unwrap();
    assert!(
        matches!(found.as_slice(), [(_, Disposition::Unknown(_))]),
        "{found:?}"
    );
    assert_eq!(
        f.count("SELECT count(*) FROM non_execution_proofs").await,
        0
    );
    // AC-I1.2: the compromise path, once however many looks.
    assert_eq!(compromises(&f).await, 1);
    assert_eq!(
        f.count("SELECT count(*) FROM event_log WHERE kind = 'pause-requested'")
            .await,
        1
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_8_another_payload_for_the_same_reference_is_never_a_proof(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    signed_but_not_sent(&f, [5; 32]).await;
    // The same reference at the same nonce, in another payload made outside the signer.
    let twin = outside_the_signer(&f, 0, [5; 32], TO, 3).await;
    f.chain.include_raw(twin);
    f.chain.advance(MAX_ERA + 1);
    let found = f.prover().step().await.unwrap();
    assert!(
        matches!(found.as_slice(), [(_, Disposition::Unknown(_))]),
        "{found:?}"
    );
    assert_eq!(
        f.count("SELECT count(*) FROM non_execution_proofs").await,
        0
    );
    assert_eq!(
        f.count(&format!(
            "SELECT count(*) FROM event_log WHERE kind = '{ANOMALY}'"
        ))
        .await,
        1
    );
    assert_eq!(compromises(&f).await, 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_nx_1_an_on_chain_refusal_counts_as_not_executed_once_final(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let s = f.submitter();
    let order = f.order_mint([6; 32], TO, 3).await;
    let prepared = s.prepare(order).await.unwrap();
    let signing = f.signing_intent("mint", [6; 32], &prepared, 3).await;
    f.chain.refuse_on_chain(true);
    assert!(matches!(
        s.sign_and_submit(signing).await.unwrap(),
        Submitted::Submitted { .. }
    ));
    // Included, refused, but not final yet: unknown.
    f.chain.produce();
    let found = f.prover().step().await.unwrap();
    assert!(
        matches!(found.as_slice(), [(_, Disposition::Unknown(_))]),
        "{found:?}"
    );
    f.chain.advance(1);
    let found = f.prover().step().await.unwrap();
    let [(_, Disposition::NotExecuted(p))] = found.as_slice() else {
        panic!("{found:?}");
    };
    assert!(
        matches!(&p.kind, ProofKind::Refused { at, index: 0, error: Some(_), .. } if at.number == 4),
        "{p:?}"
    );
    // The inclusion is an observation with its result (REQ-OB-6), and the refusal a proof.
    assert_eq!(
        f.count(
            "SELECT count(*) FROM observations WHERE position_kind = 'transaction' \
             AND block_number = 4 AND NOT executed"
        )
        .await,
        1
    );
    assert_eq!(
        f.count("SELECT count(*) FROM non_execution_proofs WHERE kind = 'refused'")
            .await,
        1
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_transaction_of_the_delivery_key_bezent_did_not_record_starts_the_compromise_path(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    signed_but_not_sent(&f, [7; 32]).await;
    // The delivery key signs something outside the signer, at the same nonce, for another
    // reference.
    let rogue = outside_the_signer(&f, 0, [0xee; 32], [0xee; 32], 1_000_000).await;
    f.chain.include_raw(rogue);
    f.chain.advance(1);

    // Review M1-bis: a consumer bezent did not sign proves nothing, even with another reference,
    // even once the era is over.
    for _ in 0..2 {
        let found = f.prover().step().await.unwrap();
        assert!(
            matches!(found.as_slice(), [(_, Disposition::Unknown(_))]),
            "{found:?}"
        );
        f.chain.advance(MAX_ERA);
    }
    assert_eq!(
        f.count("SELECT count(*) FROM non_execution_proofs").await,
        0
    );
    assert_eq!(compromises(&f).await, 1);
    assert_eq!(
        f.count("SELECT count(*) FROM event_log WHERE kind = 'pause-requested'")
            .await,
        1
    );
}

/// A call of `pallet-utility` (`batch` 0, `batch_all` 2, `force_batch` 4) wrapping `calls`. The
/// pallet's index does not matter: the endpoint reads into no wrapper.
fn utility(call_index: u8, calls: &[Vec<u8>]) -> Vec<u8> {
    use parity_scale_codec::{Compact, Encode};
    let mut out = vec![40, call_index];
    Compact(calls.len() as u32).encode_to(&mut out);
    for c in calls {
        out.extend_from_slice(c);
    }
    out
}

/// The delivery key's signature, made outside the signer, over `call` at `nonce`, born at the
/// finalized head.
async fn outside_the_signer_with(f: &Fixture, nonce: u32, call: Vec<u8>) -> Vec<u8> {
    let head = f.chain.finalized_ref();
    let p = TxParams {
        genesis: GENESIS,
        spec_version: 134,
        transaction_version: 13,
        era: Mortality::born_at(head.number),
        birth_hash: head.hash,
        nonce,
        tip: 0,
    };
    let message = kreivo::message_to_sign(&kreivo::signing_payload(&call, &p));
    let signature = f.keys.sign("kreivo-mint", &message).await.unwrap();
    kreivo::extrinsic(
        &f.lane.account,
        kreivo::Scheme::Sr25519,
        &signature,
        &p,
        &call,
    )
}

fn mint_call(reference: [u8; 32]) -> Vec<u8> {
    kreivo::contracts_call(
        &CONTRACT,
        &kreivo::CallLimits {
            gas_limit: kreivo::Weight::default(),
            storage_deposit_limit: None,
        },
        &kreivo::mint_data(
            kreivo::AssetId::Here(101),
            &[0xee; 32],
            1_000_000,
            &reference,
        ),
    )
}

async fn anomalies(f: &Fixture) -> i64 {
    f.count(&format!(
        "SELECT count(*) FROM event_log WHERE kind = '{ANOMALY}'"
    ))
    .await
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m1_bis_a_batch_all_wrapper_of_this_reference_is_never_a_proof(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // Review M1-bis, scenario 1: the leaked key wraps the mint for our reference in
    // `Utility::batch_all` at our nonce. The lane stays the origin, the contract mints and marks
    // the reference used; the wrapper hides the reference from a top-level decode.
    let f = Fixture::new(pool, connect).await;
    signed_but_not_sent(&f, [9; 32]).await;
    let wrapped = outside_the_signer_with(&f, 0, utility(2, &[mint_call([9; 32])])).await;
    f.chain.include_raw(wrapped);
    f.chain.use_reference_outside([9; 32]);
    f.chain.advance(MAX_ERA + 1);

    let found = f.prover().step().await.unwrap();
    assert!(
        matches!(found.as_slice(), [(_, Disposition::Unknown(_))]),
        "{found:?}"
    );
    assert_eq!(
        f.count("SELECT count(*) FROM non_execution_proofs").await,
        0
    );
    assert_eq!(compromises(&f).await, 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m1_bis_a_mint_under_another_nonce_is_never_a_proof(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // Review M1-bis, scenario 2: the leaked key consumes our nonce with something else, then
    // mints our reference at the next nonce.
    let f = Fixture::new(pool, connect).await;
    signed_but_not_sent(&f, [10; 32]).await;
    let transfer = outside_the_signer_with(&f, 0, vec![10, 3, 0xaa, 0xbb]).await;
    let mint = outside_the_signer_with(&f, 1, mint_call([10; 32])).await;
    f.chain.include_raw(transfer);
    f.chain.include_raw(mint);
    f.chain.use_reference_outside([10; 32]);
    f.chain.advance(MAX_ERA + 1);

    let found = f.prover().step().await.unwrap();
    assert!(
        matches!(found.as_slice(), [(_, Disposition::Unknown(_))]),
        "{found:?}"
    );
    assert_eq!(
        f.count("SELECT count(*) FROM non_execution_proofs").await,
        0
    );
    assert!(compromises(&f).await >= 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m1_bis_a_used_reference_is_never_proven_not_executed(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // Review M1-bis: the chain's nonces say "not executed" (the era ended, the lane's nonce
    // unconsumed), but the contract says the reference was used, by a call that does not consume
    // the lane's nonce (a proxy, a multisig, a `dispatch_as`). The contract wins: no proof.
    let f = Fixture::new(pool, connect).await;
    signed_but_not_sent(&f, [12; 32]).await;
    f.chain.use_reference_outside([12; 32]);
    f.chain.advance(MAX_ERA + 1);
    assert_eq!(f.chain.nonce(&f.lane.account), 0);
    for _ in 0..2 {
        let found = f.prover().step().await.unwrap();
        assert!(
            matches!(found.as_slice(), [(_, Disposition::Unknown(why))] if why.contains("was used")),
            "{found:?}"
        );
    }
    assert_eq!(
        f.count("SELECT count(*) FROM non_execution_proofs").await,
        0
    );
    // A delivery anomaly, once however many looks.
    assert_eq!(anomalies(&f).await, 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m1_bis_a_refunded_redemption_is_never_proven_not_executed(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    use bezent_kreivo_endpoint::kreivo::{AssetId, Backend, ContractEvent, Payout};
    // The same for a refund mint: the redemption's `refunded` flag is the contract's single-use
    // state (INV-8: at most one refund mint per redemption).
    let f = Fixture::new(pool, connect).await;
    let redeemer = [0x56; 32];
    f.chain.emit(ContractEvent::Redeemed {
        seq: 0,
        reference: [13; 32],
        redeemer,
        asset: AssetId::Here(101),
        amount: 40,
        backend: Backend::Evm { chain_id: 137 },
        payout: Payout::Address([1; 20]),
    });
    f.chain.advance(1);
    let s = f.submitter();
    let order = f.order_refund([13; 32], redeemer, 40).await;
    let prepared = s.prepare(order).await.unwrap();
    let signing = f
        .signing_intent("refund-mint", [13; 32], &prepared, 40)
        .await;
    f.chain.refuse_submissions(true);
    assert!(matches!(
        s.sign_and_submit(signing).await.unwrap(),
        Submitted::Persisted { .. }
    ));
    f.chain.refuse_submissions(false);
    f.chain.refund_outside(redeemer, [13; 32]);
    f.chain.advance(MAX_ERA + 1);
    let found = f.prover().step().await.unwrap();
    assert!(
        matches!(found.as_slice(), [(_, Disposition::Unknown(why))] if why.contains("was used")),
        "{found:?}"
    );
    assert_eq!(
        f.count("SELECT count(*) FROM non_execution_proofs").await,
        0
    );
    assert_eq!(anomalies(&f).await, 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn m1_bis_an_unrefunded_redemption_is_proven_not_executed(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    use bezent_kreivo_endpoint::kreivo::{AssetId, Backend, ContractEvent, Payout};
    // The control: the contract says the redemption was not refunded, so the proof stands.
    let f = Fixture::new(pool, connect).await;
    let redeemer = [0x57; 32];
    f.chain.emit(ContractEvent::Redeemed {
        seq: 0,
        reference: [14; 32],
        redeemer,
        asset: AssetId::Here(101),
        amount: 40,
        backend: Backend::Evm { chain_id: 137 },
        payout: Payout::Address([1; 20]),
    });
    f.chain.advance(1);
    let s = f.submitter();
    let order = f.order_refund([14; 32], redeemer, 40).await;
    let prepared = s.prepare(order).await.unwrap();
    let signing = f
        .signing_intent("refund-mint", [14; 32], &prepared, 40)
        .await;
    f.chain.refuse_submissions(true);
    s.sign_and_submit(signing).await.unwrap();
    f.chain.refuse_submissions(false);
    f.chain.advance(MAX_ERA + 1);
    let found = f.prover().step().await.unwrap();
    assert!(
        matches!(found.as_slice(), [(_, Disposition::NotExecuted(p))] if p.kind == ProofKind::NonceUnconsumed),
        "{found:?}"
    );
    assert_eq!(anomalies(&f).await, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_nx_2_proofs_prepared_transactions_and_ranges_are_append_only(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    signed_but_not_sent(&f, [8; 32]).await;
    f.chain.advance(MAX_ERA);
    f.prover().step().await.unwrap();
    while let bezent_kreivo_endpoint::observe::Step::Read { .. } =
        f.follower().step().await.unwrap()
    {}
    for sql in [
        "UPDATE non_execution_proofs SET nonce = nonce + 1",
        "DELETE FROM non_execution_proofs",
        "UPDATE prepared_transactions SET dry_run_passed = true",
        "DELETE FROM prepared_transactions",
        "UPDATE observation_ranges SET last_block = last_block + 1",
        "DELETE FROM observation_ranges",
        "TRUNCATE observation_ranges",
    ] {
        let err = sqlx::raw_sql(sql).execute(&f.admin).await.unwrap_err();
        let db = err.as_database_error().unwrap();
        assert_eq!(
            db.code().as_deref(),
            Some(bezent_store::REFUSAL_SQLSTATE),
            "{sql}: {}",
            db.message()
        );
    }
    // A proof for a signed transaction that does not exist is refused.
    let insert = "INSERT INTO non_execution_proofs (signed_transaction_id, network, signer, nonce, \
           valid_from_block, valid_until_block, final_block_number, final_block_hash, \
           nonce_at_final, kind) \
         VALUES (999, $1, '\\x01', 0, 0, 1, 1, '\\x01', 0, 'nonce-unconsumed')";
    let err = sqlx::query(insert)
        .bind(NETWORK)
        .execute(&f.endpoint)
        .await
        .unwrap_err();
    let message = err.as_database_error().unwrap().message().to_owned();
    assert!(message.contains("REQ-NX-2"), "{message}");
    // Review S1: and the ingress's role may not record one at all.
    let err = sqlx::query(insert)
        .bind(NETWORK)
        .execute(&f.service)
        .await
        .unwrap_err();
    assert_eq!(
        err.as_database_error().and_then(|d| d.code()).as_deref(),
        Some("42501")
    );
}
