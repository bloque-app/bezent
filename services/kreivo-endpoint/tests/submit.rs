// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! T-007-03: mints and refund mints, from the core's order to submitted bytes: an era born at the
//! finalized head, a dry run before any signature, the exact bytes recorded, the signature from
//! the signer over its link and checked, persisted before submission, one declared lane
//! (`REQ-EO-1`, `REQ-EO-4`, `REQ-HN-3`, `AC-I1.2`, `AC-D2.1`, `INV-9`; PLAN `DEC-5`).

mod common;

use bezent_kreivo_endpoint::chain::fake::DryRunMode;
use bezent_kreivo_endpoint::kreivo::{
    self, AssetId, Backend, ContractEvent, MAX_ERA, Mortality, Payout, TxParams,
};
use bezent_kreivo_endpoint::observe::Step;
use bezent_kreivo_endpoint::proof::Disposition;
use bezent_kreivo_endpoint::submit::{Prepared, Submitted, signing_intent_payload};
use common::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

const TO: [u8; 32] = [0x77; 32];

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_eo_1_the_signed_bytes_are_persisted_before_they_are_submitted(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let s = f.submitter();
    let order = f.order_mint([1; 32], TO, 1_000_000).await;
    let finalized = f.chain.finalized_ref();
    assert_eq!(s.orders_to_prepare(&finalized).await.unwrap(), vec![order]);

    let prepared = s.prepare(order).await.unwrap();
    let Prepared::Ready { nonce, .. } = &prepared else {
        panic!("{prepared:?}");
    };
    assert_eq!(*nonce, 0);
    // The era is born at the finalized head and lasts MAX_ERA blocks (DEC-5, REQ-HN-3).
    let (from, until, checkpoint): (i64, i64, Vec<u8>) = sqlx::query_as(
        "SELECT valid_from_block, valid_until_block, checkpoint_hash FROM prepared_transactions",
    )
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(from as u64, finalized.number);
    assert_eq!(until as u64, finalized.number + MAX_ERA - 1);
    assert_eq!(checkpoint, finalized.hash.to_vec());

    // The node refuses the submission: the bytes are persisted all the same, and nothing else.
    f.chain.refuse_submissions(true);
    let signing = f
        .signing_intent("mint", [1; 32], &prepared, 1_000_000)
        .await;
    let submitted = s.sign_and_submit(signing).await.unwrap();
    let Submitted::Persisted { id, .. } = submitted else {
        panic!("{submitted:?}");
    };
    assert!(f.chain.submitted().is_empty());
    let (bytes, hash): (Vec<u8>, Vec<u8>) =
        sqlx::query_as("SELECT bytes, tx_hash FROM signed_transactions WHERE id = $1")
            .bind(id)
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert_eq!(hash, kreivo::blake2_256(&bytes).to_vec());

    // The node accepts again: the same bytes go out, and the chain includes them.
    f.chain.refuse_submissions(false);
    assert_eq!(s.resend().await.unwrap(), 1);
    assert_eq!(f.chain.submitted(), vec![bytes.clone()]);
    f.chain.advance(1);
    assert_eq!(f.chain.nonce(&f.lane.account), 1);

    // The mint is observed by its reference, and the transaction's inclusion and result too.
    while let Step::Read { .. } = f.follower().step().await.unwrap() {}
    assert_eq!(
        f.count(
            "SELECT count(*) FROM observations WHERE detail ->> 'event' = 'Minted' \
             AND on_chain_reference = '\\x0101010101010101010101010101010101010101010101010101010101010101'"
        )
        .await,
        1
    );
    let found = f.prover().step().await.unwrap();
    assert!(
        matches!(found.as_slice(), [(i, Disposition::Executed { .. })] if *i == id),
        "{found:?}"
    );
    assert_eq!(
        f.count(
            "SELECT count(*) FROM observations WHERE position_kind = 'transaction' AND executed"
        )
        .await,
        1
    );
    // Nothing is left to sign, send or prove.
    assert!(
        s.intents_to_sign(&f.chain.finalized_ref())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(s.resend().await.unwrap(), 0);
    assert!(f.prover().step().await.unwrap().is_empty());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_5_nothing_is_signed_without_a_passing_dry_run(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let s = f.submitter();
    // AboveWindow (index 6): the contract would refuse it.
    f.chain.set_dry_run(DryRunMode::Refuse(6));
    let order = f.order_mint([2; 32], TO, 5).await;
    let prepared = s.prepare(order).await.unwrap();
    let Prepared::DryRunFailed { id, reason } = &prepared else {
        panic!("{prepared:?}");
    };
    assert_eq!(reason, "AboveWindow");
    // It is recorded, and not prepared again: retrying is the core's to order.
    assert!(
        s.orders_to_prepare(&f.chain.finalized_ref())
            .await
            .unwrap()
            .is_empty()
    );
    // A signing intent that names it anyway is refused before the signer is asked.
    let signing = f
        .intent_past_the_store("mint", &[2; 32], signing_intent_payload(*id, &[1, 2, 3], 5))
        .await;
    let out = s.sign_and_submit(signing).await.unwrap();
    assert!(
        matches!(&out, Submitted::Refused(r) if r.contains("DEC-5")),
        "{out:?}"
    );
    // A dry run that traps is refused just the same.
    f.chain.set_dry_run(DryRunMode::Trap);
    let order = f.order_mint([3; 32], TO, 5).await;
    assert!(matches!(
        s.prepare(order).await.unwrap(),
        Prepared::DryRunFailed { .. }
    ));
    assert_eq!(f.signatures().await, 0);
    assert!(f.chain.submitted().is_empty());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_d2_1_after_a_restart_the_recorded_bytes_are_sent_again_and_nothing_is_signed_again(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let order = f.order_mint([4; 32], TO, 7).await;
    let prepared = f.submitter().prepare(order).await.unwrap();
    let signing = f.signing_intent("mint", [4; 32], &prepared, 7).await;
    f.chain.refuse_submissions(true);
    let out = f.submitter().sign_and_submit(signing).await.unwrap();
    assert!(matches!(out, Submitted::Persisted { .. }), "{out:?}");
    f.chain.refuse_submissions(false);

    // A new process: it finds nothing to sign, and sends the recorded bytes.
    let restarted = f.submitter();
    let head = f.chain.finalized_ref();
    assert!(restarted.intents_to_sign(&head).await.unwrap().is_empty());
    assert!(restarted.orders_to_prepare(&head).await.unwrap().is_empty());
    assert_eq!(restarted.resend().await.unwrap(), 1);
    let recorded: Vec<u8> = sqlx::query_scalar("SELECT bytes FROM signed_transactions")
        .fetch_one(&f.admin)
        .await
        .unwrap();
    assert_eq!(f.chain.submitted(), vec![recorded]);
    // Asked to sign it again anyway, it refuses before the signer is asked.
    let again = restarted.sign_and_submit(signing).await.unwrap();
    assert!(
        matches!(&again, Submitted::Refused(r) if r.contains("REQ-EO-1")),
        "{again:?}"
    );
    assert_eq!(f.signatures().await, 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_eo_1_no_new_attempt_for_a_reference_while_an_earlier_one_may_execute(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let s = f.submitter();
    let first = f.order_mint([5; 32], TO, 9).await;
    let prepared = s.prepare(first).await.unwrap();
    assert!(matches!(prepared, Prepared::Ready { .. }));
    // A second order for the same reference, while the first is live and unsigned.
    let second = f.order_mint([5; 32], TO, 9).await;
    assert_eq!(s.prepare(second).await.unwrap(), Prepared::Open);
    // And once the first is signed.
    let signing = f.signing_intent("mint", [5; 32], &prepared, 9).await;
    f.chain.refuse_submissions(true);
    s.sign_and_submit(signing).await.unwrap();
    f.chain.advance(MAX_ERA + 1);
    assert_eq!(s.prepare(second).await.unwrap(), Prepared::Open);
    // Its non-execution proven, a new order may be prepared.
    let found = f.prover().step().await.unwrap();
    assert!(
        matches!(found.as_slice(), [(_, Disposition::NotExecuted(_))]),
        "{found:?}"
    );
    assert!(matches!(
        s.prepare(second).await.unwrap(),
        Prepared::Ready { .. }
    ));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_hn_3_a_prepared_transaction_whose_era_passed_is_never_signed(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let s = f.submitter();
    let order = f.order_mint([6; 32], TO, 11).await;
    let prepared = s.prepare(order).await.unwrap();
    f.chain.advance(MAX_ERA);
    let signing = f.signing_intent("mint", [6; 32], &prepared, 11).await;
    let out = s.sign_and_submit(signing).await.unwrap();
    assert!(
        matches!(&out, Submitted::Refused(r) if r.contains("REQ-HN-3")),
        "{out:?}"
    );
    assert_eq!(f.signatures().await, 0);
    // The order is prepared again, as a new attempt with a fresh era.
    let head = f.chain.finalized_ref();
    assert_eq!(s.orders_to_prepare(&head).await.unwrap(), vec![order]);
    let again = s.prepare(order).await.unwrap();
    assert!(
        matches!(again, Prepared::Ready { attempt: 2, .. }),
        "{again:?}"
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_eo_4_one_lane_keeps_order_by_nonce(pool: PgPoolOptions, connect: PgConnectOptions) {
    let f = Fixture::new(pool, connect).await;
    let s = f.submitter();
    let a = f.order_mint([7; 32], TO, 1).await;
    let b = f.order_mint([8; 32], TO, 2).await;
    let pa = s.prepare(a).await.unwrap();
    let pb = s.prepare(b).await.unwrap();
    assert!(matches!(pa, Prepared::Ready { nonce: 0, .. }), "{pa:?}");
    assert!(matches!(pb, Prepared::Ready { nonce: 1, .. }), "{pb:?}");
    // Signed out of order: the second waits for the first.
    let sb = f.signing_intent("mint", [8; 32], &pb, 2).await;
    assert!(matches!(
        s.sign_and_submit(sb).await.unwrap(),
        Submitted::Submitted { .. }
    ));
    f.chain.advance(1);
    assert_eq!(f.chain.nonce(&f.lane.account), 0);
    let sa = f.signing_intent("mint", [7; 32], &pa, 1).await;
    assert!(matches!(
        s.sign_and_submit(sa).await.unwrap(),
        Submitted::Submitted { .. }
    ));
    f.chain.advance(1);
    assert_eq!(f.chain.nonce(&f.lane.account), 2);
    while let Step::Read { .. } = f.follower().step().await.unwrap() {}
    let minted: Vec<Vec<u8>> = sqlx::query_scalar(
        "SELECT on_chain_reference FROM observations WHERE detail ->> 'event' = 'Minted' \
         ORDER BY (detail ->> 'seq')::bigint",
    )
    .fetch_all(&f.admin)
    .await
    .unwrap();
    assert_eq!(minted, vec![vec![7; 32], vec![8; 32]]);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_9_a_signing_intent_that_does_not_answer_its_order_is_not_signed(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let s = f.submitter();
    // One prepared transaction per case: a prepared transaction has one signing intent (M1).
    let mut cases = Vec::new();
    for (n, case) in [9u8, 12, 13].into_iter().enumerate() {
        let order = f.order_mint([case; 32], TO, 100).await;
        let prepared = s.prepare(order).await.unwrap();
        let Prepared::Ready { id, message, .. } = prepared else {
            panic!("{prepared:?}");
        };
        cases.push(match n {
            // Another amount than the order's.
            0 => ([case; 32], signing_intent_payload(id, &message, 1)),
            // Other bytes than the prepared transaction's.
            1 => ([case; 32], signing_intent_payload(id, &[0xde, 0xad], 100)),
            // Another reference.
            _ => ([10u8; 32], signing_intent_payload(id, &message, 100)),
        });
    }
    for (reference, payload) in cases {
        let signing = f.intent_past_the_store("mint", &reference, payload).await;
        let out = s.sign_and_submit(signing).await.unwrap();
        assert!(
            matches!(&out, Submitted::Refused(r) if r.contains("INV-9")),
            "{out:?}"
        );
    }
    assert_eq!(f.signatures().await, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_eo_1_the_whole_signed_payload_is_checked_before_a_signature(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // Review M2: a prepared row whose columns say one thing (nonce, era) while its payload
    // carries another (a later nonce, an immortal era, a tip, a fee asset, PassAuthenticate) is
    // never signed, however well its call and message answer the order.
    let f = Fixture::new(pool, connect).await;
    let s = f.submitter();
    let order = f.order_mint([14; 32], TO, 100).await;
    let honest = s.prepare(order).await.unwrap();
    assert!(
        matches!(honest, Prepared::Ready { nonce: 0, .. }),
        "{honest:?}"
    );
    let (call, payload): (Vec<u8>, Vec<u8>) =
        sqlx::query_as("SELECT call, signing_payload FROM prepared_transactions")
            .fetch_one(&f.admin)
            .await
            .unwrap();
    let head = f.chain.block(3);
    let params = TxParams {
        genesis: GENESIS,
        spec_version: 134,
        transaction_version: 13,
        era: Mortality::born_at(head.number),
        birth_hash: head.hash,
        nonce: 0,
        tip: 0,
    };
    assert_eq!(kreivo::signing_payload(&call, &params), payload);
    let implicit = kreivo::implicit(&params);
    let with_extra = |extra: &[u8]| [call.as_slice(), extra, implicit.as_slice()].concat();
    let era = params.era.bytes();
    let variants: Vec<(&str, Vec<u8>)> = vec![
        // The row says nonce 0; the payload carries nonce 1.
        (
            "nonce",
            kreivo::signing_payload(&call, &TxParams { nonce: 1, ..params }),
        ),
        // A tip.
        (
            "tip",
            kreivo::signing_payload(
                &call,
                &TxParams {
                    tip: 1_000_000,
                    ..params
                },
            ),
        ),
        // An immortal era: valid forever, so never provably not executed.
        ("immortal", with_extra(&[0, 0, 0, 0, 0])),
        // A fee asset.
        ("fee asset", with_extra(&[0, era[0], era[1], 0, 0, 1, 0])),
        // PassAuthenticate: Some.
        ("pass", with_extra(&[1, era[0], era[1], 0, 0, 0])),
    ];
    for (attempt, (what, bad)) in variants.into_iter().enumerate() {
        let message = kreivo::message_to_sign(&bad);
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO prepared_transactions (order_intent_id, attempt, network, lane, key_id, \
               nonce, valid_from_block, valid_until_block, checkpoint_hash, call, signing_payload, \
               message, dry_run_block_number, dry_run_block_hash, dry_run_passed, dry_run) \
             SELECT order_intent_id, $2, network, lane, key_id, nonce, valid_from_block, \
               valid_until_block, checkpoint_hash, call, $3, $4, dry_run_block_number, \
               dry_run_block_hash, dry_run_passed, dry_run \
             FROM prepared_transactions WHERE attempt = 1 AND order_intent_id = $1 RETURNING id",
        )
        .bind(order)
        .bind(i32::try_from(attempt).unwrap() + 2)
        .bind(&bad)
        .bind(&message)
        .fetch_one(&f.endpoint)
        .await
        .unwrap();
        let signing = f
            .intent("mint", &[14; 32], signing_intent_payload(id, &message, 100))
            .await;
        let out = s.sign_and_submit(signing).await.unwrap();
        assert!(
            matches!(&out, Submitted::Refused(r) if r.contains("REQ-EO-1")),
            "{what}: {out:?}"
        );
    }
    assert_eq!(f.signatures().await, 0);
    // Each is in the event log, once (review S3).
    assert_eq!(
        f.count(&format!(
            "SELECT count(*) FROM event_log WHERE kind = '{}'",
            bezent_kreivo_endpoint::signal::ANOMALY
        ))
        .await,
        5
    );
    // The honest one is signed.
    let signing = f.signing_intent("mint", [14; 32], &honest, 100).await;
    assert!(matches!(
        s.sign_and_submit(signing).await.unwrap(),
        Submitted::Submitted { .. }
    ));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_the_delivery_key_used_while_the_lane_is_idle_starts_the_compromise_path(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // Review S4: a nonce past everything bezent signed is found at the next preparation.
    let f = Fixture::new(pool, connect).await;
    let s = f.submitter();
    let order = f.order_mint([15; 32], TO, 1).await;
    let prepared = s.prepare(order).await.unwrap();
    let signing = f.signing_intent("mint", [15; 32], &prepared, 1).await;
    s.sign_and_submit(signing).await.unwrap();
    f.chain.advance(1);
    f.chain.consume_nonce(f.lane.account);
    f.chain.advance(1);
    for reference in [16u8, 17] {
        let order = f.order_mint([reference; 32], TO, 1).await;
        assert!(matches!(
            s.prepare(order).await.unwrap(),
            Prepared::Ready { nonce: 2, .. } | Prepared::Ready { nonce: 3, .. }
        ));
    }
    assert_eq!(
        f.count("SELECT count(*) FROM event_log WHERE kind = 'key-compromise'")
            .await,
        1
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ot_6_a_refund_mint_goes_to_the_redeemer_of_the_redemption(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let redeemer = [0x55; 32];
    f.chain.emit(ContractEvent::Redeemed {
        seq: 0,
        reference: [11; 32],
        redeemer,
        asset: AssetId::Here(101),
        amount: 40,
        backend: Backend::Evm { chain_id: 137 },
        payout: Payout::Address([1; 20]),
    });
    f.chain.advance(1);
    let s = f.submitter();
    let order = f.order_refund([11; 32], redeemer, 40).await;
    let prepared = s.prepare(order).await.unwrap();
    let signing = f
        .signing_intent("refund-mint", [11; 32], &prepared, 40)
        .await;
    assert!(matches!(
        s.sign_and_submit(signing).await.unwrap(),
        Submitted::Submitted { .. }
    ));
    f.chain.advance(1);
    while let Step::Read { .. } = f.follower().step().await.unwrap() {}
    let (recipient, amount): (Option<String>, String) = sqlx::query_as(
        "SELECT recipient, amount::text FROM observations WHERE detail ->> 'event' = 'RefundMinted'",
    )
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(recipient, Some(format!("0x{}", hex::encode(redeemer))));
    assert_eq!(amount, "40");
}
