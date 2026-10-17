// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Issue #42: the vault's balance of each admitted token, and of each convertible, recorded at each
//! final head the endpoint records, from both providers; providers that disagree make the reading
//! unknown (`REQ-BK-8`, `REQ-CV-4`, `NFR-5`, `INV-1`; PLAN `DEC-12`).

mod common;

use alloy::primitives::U256;
use bezent_evm_endpoint::reserve::{self, balance_of};
use common::{Fixture, NETWORK, usdc, usdt0, vault, vault_text};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

fn word(v: u64) -> Vec<u8> {
    U256::from(v).to_be_bytes::<32>().to_vec()
}

type Row = (
    i64,
    Vec<u8>,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    serde_json::Value,
);

async fn readings(f: &Fixture) -> Vec<Row> {
    sqlx::query_as(
        "SELECT block_number, block_hash, kind, subject, holder, amount::text, unknown_reason, \
             detail FROM reserve_readings WHERE network = $1 ORDER BY kind DESC, subject",
    )
    .bind(NETWORK)
    .fetch_all(&f.admin)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_8_the_vault_balance_of_each_admitted_token_is_recorded_at_the_final_head_from_both_providers(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    f.chain.set_view(usdc(), balance_of(vault()), word(1_234));
    f.chain.set_view(usdt0(), balance_of(vault()), word(77));
    f.follower().step().await.unwrap();

    let (head_number, head_hash): (i64, Vec<u8>) = sqlx::query_as(
        "SELECT block_number, block_hash FROM final_heads WHERE network = $1 \
         ORDER BY block_number DESC LIMIT 1",
    )
    .bind(NETWORK)
    .fetch_one(&f.admin)
    .await
    .unwrap();
    let got = readings(&f).await;
    assert_eq!(got.len(), 2, "{got:?}");
    let (block, hash, kind, subject, holder, amount, unknown, detail) = &got[0];
    // At the final head, with its hash: the point the reserve check reads the network at.
    assert_eq!((*block, hash), (head_number, &head_hash));
    assert_eq!(kind, "reserve");
    assert_eq!(subject, &usdc().to_string());
    assert_eq!(holder, &vault_text());
    assert_eq!(amount.as_deref(), Some("1234"));
    assert!(unknown.is_none());
    // Both providers' answers are kept.
    let answers: Vec<_> = detail["providers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["answer"]["balance"].clone())
        .collect();
    assert_eq!(answers, vec!["1234", "1234"]);
    // The convertible, itemised outside the equation (REQ-CV-4).
    let (_, _, kind, subject, _, amount, _, _) = &got[1];
    assert_eq!(kind, "convertible");
    assert_eq!(subject, &usdt0().to_checksum(None));
    assert_eq!(amount.as_deref(), Some("77"));

    // A head already recorded is not read again.
    f.follower().step().await.unwrap();
    assert_eq!(readings(&f).await.len(), 2);
    // The next head is.
    f.advance(1);
    f.follower().step().await.unwrap();
    assert_eq!(readings(&f).await.len(), 4);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn dec_12_providers_that_disagree_on_a_balance_make_it_unknown(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // Two providers on the same blocks, answering different balances.
    let chain = bezent_evm_endpoint::rpc::mock::MockChain::new(common::CHAIN_ID, 5);
    let other = chain.fork();
    let f = Fixture::with_chains(pool, connect, chain, other).await;
    f.chain.set_view(usdc(), balance_of(vault()), word(1_000));
    f.chain_b.set_view(usdc(), balance_of(vault()), word(1_001));
    f.chain.set_view(usdt0(), balance_of(vault()), word(5));
    f.chain_b.set_view(usdt0(), balance_of(vault()), word(5));
    f.follower().step().await.unwrap();

    let got = readings(&f).await;
    let (_, _, kind, _, _, amount, unknown, detail) = &got[0];
    assert_eq!(kind, "reserve");
    // Never the higher, never the lower: unknown, with both answers kept.
    assert!(amount.is_none());
    let unknown = unknown.as_deref().unwrap();
    assert!(
        unknown.contains("DEC-12") && unknown.contains("1000") && unknown.contains("1001"),
        "{unknown}"
    );
    assert_eq!(detail["providers"][0]["answer"]["balance"], "1000");
    assert_eq!(detail["providers"][1]["answer"]["balance"], "1001");
    // Where they agree the reading stands.
    assert_eq!(got[1].5.as_deref(), Some("5"));

    // A provider that cannot answer is unknown too.
    f.chain_b
        .set_view(usdc(), balance_of(vault()), vec![0u8; 3]);
    let r = reserve::read(
        &f.pair,
        "reserve",
        usdc(),
        usdc().to_string(),
        vault(),
        vault_text(),
        5,
    )
    .await;
    assert!(r.amount.is_none());
    assert!(
        r.unknown_reason
            .as_deref()
            .is_some_and(|u| u.contains("unavailable") && u.contains("not 32")),
        "{r:?}"
    );
}

/// PR #50 review M4: the payout prover records the final head it proves at (`proof.rs`,
/// `store::final_head`) and reads no balance. When it sees a new finalized head before the
/// follower does, that head is recorded first, without readings; the follower still reads the
/// vault's balances there, or the reserve check would find no reading at the latest final point
/// for as long as the head stays the latest, and pause the asset during an ordinary payout.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_8_nfr_5_a_final_head_the_prover_recorded_first_still_gets_its_readings(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect, 5).await;
    f.chain.set_view(usdc(), balance_of(vault()), word(1_234));
    f.chain.set_view(usdt0(), balance_of(vault()), word(77));

    // The prover sees the finalized head first and records it, as `Prover::step` does.
    let head = f.pair.final_point().await.unwrap();
    let mut conn = f.endpoint.acquire().await.unwrap();
    bezent_evm_endpoint::store::final_head(&mut conn, NETWORK, &head)
        .await
        .unwrap();
    drop(conn);
    assert!(readings(&f).await.is_empty());

    // The follower's next step, at the same final head, reads the balances there.
    f.follower().step().await.unwrap();
    let got = readings(&f).await;
    assert_eq!(got.len(), 2, "{got:?}");
    let number = i64::try_from(head.number).unwrap();
    assert!(
        got.iter()
            .all(|(b, h, ..)| *b == number && h.as_slice() == head.hash.as_slice()),
        "{got:?}"
    );
    assert_eq!(got[0].5.as_deref(), Some("1234"));
    // And not again once read.
    f.follower().step().await.unwrap();
    assert_eq!(readings(&f).await.len(), 2);
}
