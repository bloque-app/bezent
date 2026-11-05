// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Amendment 0023 (form item `d-evm-submit-public`): signed transactions are sent to the public
//! broadcast nodes, all at once, bounded; a send is taken only when a node returns the hash of the
//! bytes that were signed; the paid pair never sends (`REQ-EO-1`, `AC-D2.1`, `DEC-12`).

mod common;

use std::time::{Duration, Instant};

use alloy::primitives::B256;
use bezent_evm_endpoint::broadcast::Broadcast;
use bezent_evm_endpoint::rpc::mock::{MockChain, MockRpc, Send};
use bezent_evm_endpoint::submit::Submitted;
use common::{CHAIN_ID, Fixture, beneficiary};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

fn reference(n: u8) -> B256 {
    B256::repeat_byte(n)
}

fn nodes(chain: &MockChain, modes: &[Send]) -> Broadcast<MockRpc> {
    Broadcast::new(
        modes
            .iter()
            .enumerate()
            .map(|(i, m)| MockRpc::new(&format!("node-{i}"), chain).sending(*m))
            .collect(),
    )
}

/// Orders, prepares and signs one payout, sending it through `broadcast`.
async fn sign_through(f: &Fixture, broadcast: &Broadcast<MockRpc>, n: u8) -> Submitted {
    let order = f.order_payout(reference(n), beneficiary(), 700).await;
    let prepared = f.submitter_via(broadcast).prepare(order).await.unwrap();
    let intent = f
        .signing_intent("payout", Some(reference(n).as_slice()), &prepared, 700)
        .await;
    f.submitter_via(broadcast)
        .sign_and_submit(intent)
        .await
        .unwrap()
}

async fn stored_hash(f: &Fixture, id: i64) -> Vec<u8> {
    sqlx::query_scalar("SELECT tx_hash FROM signed_transactions WHERE id = $1")
        .bind(id)
        .fetch_one(&f.admin)
        .await
        .unwrap()
}

/// A node that answers with a hash that is not the signed bytes' is a failed send for that node
/// (`REQ-EO-1`): alone, the send is not taken, the bytes are kept for the next pass, and the hash
/// stored is the one computed here, not the node's.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_eo_1_amendment_0023_a_node_returning_another_hash_has_not_taken_the_send(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    let lying = nodes(&f.broadcast_chain, &[Send::WrongHash]);
    let Submitted::Persisted { id, reason } = sign_through(&f, &lying, 1).await else {
        panic!("a mismatched hash counted as taken");
    };
    assert!(
        reason.contains("node-0") && reason.contains("returned"),
        "{reason}"
    );
    // The bytes are stored, with the hash computed from them.
    let bytes: Vec<u8> = sqlx::query_scalar("SELECT bytes FROM signed_transactions WHERE id = $1")
        .bind(id)
        .fetch_one(&f.admin)
        .await
        .unwrap();
    assert_eq!(
        stored_hash(&f, id).await,
        alloy::primitives::keccak256(&bytes).to_vec()
    );
    // The next pass, through honest nodes, sends the same bytes and counts.
    assert_eq!(f.submitter().resend().await.unwrap(), 1);
    // One lying node beside an honest one: the honest one is enough.
    let mixed = nodes(&f.broadcast_chain, &[Send::WrongHash, Send::Honest]);
    assert!(matches!(
        sign_through(&f, &mixed, 2).await,
        Submitted::Submitted { .. }
    ));
    // Every node failing, whichever way, is not taken either.
    let all_bad = nodes(&f.broadcast_chain, &[Send::WrongHash, Send::Fail]);
    assert!(matches!(
        sign_through(&f, &all_bad, 3).await,
        Submitted::Persisted { .. }
    ));
    // And the paid pair was never sent anything (no fallback).
    assert!(f.chain.sent().is_empty() && f.chain_b.sent().is_empty());
}

/// A node that says it took the transaction, which the paid pair never sees: nothing records an
/// outcome, so the next pass sends the same bytes again, and never signs again (`AC-D2.1`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_d2_1_amendment_0023_a_send_the_pair_never_sees_is_sent_again_as_the_same_bytes(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    let dropping = nodes(&f.broadcast_chain, &[Send::Honest]);
    let Submitted::Submitted { id, hash } = sign_through(&f, &dropping, 1).await else {
        panic!("not submitted");
    };
    assert_eq!(stored_hash(&f, id).await, hash.to_vec());
    let first = f.broadcast_chain.sent();
    assert_eq!(first.len(), 1);
    // The pair shows nothing of it: no observation, no proof. Two more passes.
    for pass in 2..=3 {
        assert_eq!(f.submitter_via(&dropping).resend().await.unwrap(), 1);
        assert_eq!(f.broadcast_chain.sent().len(), pass);
    }
    assert!(f.broadcast_chain.sent().iter().all(|b| *b == first[0]));
    assert!(f.chain.sent().is_empty() && f.chain_b.sent().is_empty());
    assert_eq!(f.count("SELECT count(*) FROM signatures").await, 1);
}

/// One node hangs past the bound: the other's success still counts, and the pass takes the bound,
/// not the hang. A hung node alone is a failed send after the bound, not a stall.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn amendment_0023_a_hung_node_does_not_stop_another_nodes_success_and_the_pass_is_bounded(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    let mut hung = nodes(&f.broadcast_chain, &[Send::Hang, Send::Honest]);
    hung.timeout = Duration::from_millis(300);
    let start = Instant::now();
    let Submitted::Submitted { id, .. } = sign_through(&f, &hung, 1).await else {
        panic!("the honest node's success did not count");
    };
    // Prepare, sign and send all ran inside it: the hang cost at most the bound.
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(f.broadcast_chain.sent().len(), 1);
    // The resend loop is bounded the same way.
    let start = Instant::now();
    assert_eq!(f.submitter_via(&hung).resend().await.unwrap(), 1);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
    // A hung node alone: not taken, after the bound.
    let mut only = nodes(&f.broadcast_chain, &[Send::Hang]);
    only.timeout = Duration::from_millis(300);
    let err = only.send_raw(b"x").await.unwrap_err();
    assert!(err.contains("no answer"), "{err}");
    let _ = id;
}

/// Start-up: a node on another chain refuses start; one that cannot be reached does not.
#[tokio::test]
async fn amendment_0023_start_up_refuses_a_wrong_chain_and_tolerates_an_unreachable_node() {
    let right = MockChain::new(CHAIN_ID, 1);
    let wrong = MockChain::new(CHAIN_ID + 1, 1);
    let down = MockChain::new(CHAIN_ID, 1);
    down.fail_all(Some("connection refused"));

    let ok = Broadcast::new(vec![MockRpc::new("a", &right), MockRpc::new("b", &right)]);
    ok.check_chain(CHAIN_ID).await.unwrap();

    let bad = Broadcast::new(vec![MockRpc::new("a", &right), MockRpc::new("b", &wrong)]);
    let err = bad.check_chain(CHAIN_ID).await.unwrap_err();
    assert!(err.contains("broadcast node b serves chain"), "{err}");

    let unreachable = Broadcast::new(vec![MockRpc::new("a", &right), MockRpc::new("b", &down)]);
    unreachable.check_chain(CHAIN_ID).await.unwrap();
    // Even all of them down starts: it is the send that fails, and is tried again.
    let all_down = Broadcast::new(vec![MockRpc::new("b", &down)]);
    all_down.check_chain(CHAIN_ID).await.unwrap();
}

/// Every broadcast node hangs and many rows are unsettled: a re-send pass stops at its budget
/// instead of costing the node bound per row, leaves the rest stored, and a later pass, once a
/// node answers, sends all of them as the same bytes, in nonce order.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn amendment_0023_a_resend_pass_is_bounded_when_every_node_hangs_and_the_rest_goes_next_pass(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    let honest = nodes(&f.broadcast_chain, &[Send::Honest]);
    for n in 1..=8 {
        assert!(matches!(
            sign_through(&f, &honest, n).await,
            Submitted::Submitted { .. }
        ));
    }
    let first = f.broadcast_chain.sent();
    assert_eq!(first.len(), 8);

    // Each send would cost 400 ms; eight of them, 3.2 s. The budget is 600 ms.
    let mut hung = nodes(&f.broadcast_chain, &[Send::Hang, Send::Hang]);
    hung.timeout = Duration::from_millis(400);
    let start = Instant::now();
    let taken = f
        .submitter_via(&hung)
        .resend_within(Duration::from_millis(600))
        .await
        .unwrap();
    let took = start.elapsed();
    assert_eq!(taken, 0);
    assert!(took < Duration::from_millis(1500), "{took:?}");
    assert_eq!(
        f.broadcast_chain.sent().len(),
        8,
        "a hung node recorded a send"
    );

    // A later pass, a node answering: every row goes out again, same bytes, nonce order.
    assert_eq!(f.submitter_via(&honest).resend().await.unwrap(), 8);
    let sent = f.broadcast_chain.sent();
    assert_eq!(sent.len(), 16);
    assert_eq!(&sent[8..], &first[..]);
    assert_eq!(f.count("SELECT count(*) FROM signatures").await, 8);
}
