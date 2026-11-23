// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #154: a provider's passing failure no longer makes a reading at a final head unknown.
//! Each provider is asked up to three times at the same block. Only its failure is asked again,
//! never its answer, and a reading is still known only when both providers agree (`DEC-12`).

use alloy::primitives::{Address, U256, address};
use bezent_evm_endpoint::fee;
use bezent_evm_endpoint::pair::{Pair, READ_RETRY_PAUSES};
use bezent_evm_endpoint::reserve::{self, balance_of};
use bezent_evm_endpoint::rpc::mock::{MockChain, MockRpc};

const USDC: Address = address!("3c499c542cef5e3811e1192ce70d8cc03d5c3359");
const VAULT: Address = address!("584a9e72ebc1cdba2f20233764eef08d528792ca");
const DELIVERY: Address = address!("00000000000000000000000000000000000d311e");

fn word(v: u64) -> Vec<u8> {
    U256::from(v).to_be_bytes::<32>().to_vec()
}

/// Two providers on two chains with the same blocks and balances, so a fault injected in one
/// reaches only that provider.
fn pair() -> (MockChain, MockChain, Pair<MockRpc>) {
    let a = MockChain::new(137, 5);
    let b = a.fork();
    for c in [&a, &b] {
        c.set_view(USDC, balance_of(VAULT), word(3_000_000));
        c.set_balance(DELIVERY, U256::from(7u64));
    }
    let p = Pair {
        a: MockRpc::new("tenderly", &a),
        b: MockRpc::new("drpc", &b),
    };
    (a, b, p)
}

async fn reading(p: &Pair<MockRpc>) -> reserve::Reading {
    reserve::read(
        p,
        "reserve",
        USDC,
        USDC.to_string(),
        VAULT,
        VAULT.to_checksum(None),
        5,
    )
    .await
}

#[tokio::test]
async fn issue_154_a_rate_limit_that_passes_within_the_retries_leaves_the_reading_known() {
    let (a, _b, p) = pair();
    // Two failures, then an answer: the third try is the last one.
    assert_eq!(READ_RETRY_PAUSES.len(), 2);
    a.fail_next_reads(2, "eth_call: HTTP 429");
    let r = reading(&p).await;
    assert_eq!(r.amount, Some(3_000_000), "{r:?}");
    assert!(r.unknown_reason.is_none());
}

#[tokio::test]
async fn issue_154_a_provider_that_keeps_failing_still_makes_the_reading_unknown() {
    let (_a, b, p) = pair();
    b.fail_next_reads(3, "eth_call: HTTP 429");
    let r = reading(&p).await;
    assert!(r.amount.is_none());
    assert!(
        r.unknown_reason
            .as_deref()
            .is_some_and(|u| u.contains("unavailable") && u.contains("drpc") && u.contains("429")),
        "{r:?}"
    );
}

#[tokio::test]
async fn issue_154_dec_12_an_answer_is_never_asked_again() {
    // A disagreement stays a disagreement: no retry turns it into agreement.
    let (_a, b, p) = pair();
    b.set_view(USDC, balance_of(VAULT), word(3_000_001));
    let r = reading(&p).await;
    assert!(r.amount.is_none());
    assert!(
        r.unknown_reason
            .as_deref()
            .is_some_and(|u| u.contains("DEC-12")),
        "{r:?}"
    );
    // A malformed answer is not retried either: it is unknown at once.
    let (a, _b, p) = pair();
    a.set_view(USDC, balance_of(VAULT), vec![0u8; 3]);
    let started = std::time::Instant::now();
    let r = reading(&p).await;
    assert!(
        r.unknown_reason
            .as_deref()
            .is_some_and(|u| u.contains("not 32")),
        "{r:?}"
    );
    assert!(
        started.elapsed() < READ_RETRY_PAUSES[0],
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn issue_154_req_st_6_the_fee_position_is_retried_the_same_way() {
    let (a, b, p) = pair();
    a.fail_next_reads(1, "eth_getBalance: HTTP 429");
    b.fail_next_reads(2, "eth_getBalance: HTTP 429");
    let r = fee::read(&p, DELIVERY, 5).await;
    assert_eq!(r.amount, Some(7), "{r:?}");
    // Both providers were still asked at the same block, and both answered.
    assert_eq!(r.detail["providers"][0]["answer"]["balance"], "7");
    assert_eq!(r.detail["providers"][1]["answer"]["balance"], "7");
}
