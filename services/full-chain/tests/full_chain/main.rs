// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The full-chain suite (F-013, `T-013-08`; PLAN `DEC-19`, SPEC section 15): the ingress, the core,
//! the signer and both endpoints, running together on one store, against an anvil fork of Polygon
//! and an in-memory model of the issuing network. Read `bed.rs` first: it says what is real and
//! what is not.
//!
//! **Which chain each scenario runs against** (every scenario, without exception):
//!
//! | Network | What it is here |
//! |---|---|
//! | Polygon | an anvil fork of the real chain at the pinned block, real contracts, local execution |
//! | Kreivo | `FakeChain`, an in-memory model: not a chain, no runtime, no `pallet-assets` |
//!
//! No scenario runs on zombienet or on a real Kreivo.
//!
//! A scenario that cannot reach the fork **fails**; there is no skip.

mod bed;

use alloy::primitives::U256;
use bed::*;
use bezent_kreivo_endpoint::kreivo::{AssetId, ContractEvent};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// What the scenarios move, in the token's minor units (6 decimals).
const DEPOSIT: u128 = 123_450_000;
const WITHDRAWAL: u128 = 40_000_000;

/// A deposit end to end: the arrival at the vault on the fork, finality, the requester's request
/// over HTTP, the core's order, the signer, the mint on the Kreivo model, and the transfer completed.
/// Returns the transfer.
async fn deposit(bed: &Bed, reference: &str, amount: u128) -> i64 {
    let arrival = bed.arrive(amount).await;
    let receipt = bed.anvil.receipt(arrival).await;
    let coordinate = Bed::coordinate_of(&receipt);
    // Before finality the endpoint records nothing (AC-A2.1).
    bed.evm_pass().await;
    assert_eq!(
        bed.count("SELECT count(*) FROM observations WHERE detail ->> 'event' = 'Arrival'")
            .await,
        0,
        "an arrival is recorded before it is final"
    );
    bed.core_pass().await;
    assert_eq!(
        bed.count("SELECT count(*) FROM intents WHERE network = 'kreivo-test'")
            .await,
        0,
        "an order was recorded for an arrival that is not final"
    );
    assert!(
        bed.chain.submitted().is_empty(),
        "something was minted for an arrival that is not final"
    );
    bed.anvil.finalize().await;
    bed.evm_pass().await;
    let (status, body) = bed
        .http(
            "POST",
            "/v0/deposits",
            Some(&Bed::deposit_body(reference, coordinate.clone())),
        )
        .await;
    assert_eq!(status, 201, "{body}");
    let id = bed.transfer_of_request(reference).await;
    bed.until("the deposit is completed", 12, 0, || async {
        bed.state_of(id).await.0 == "Completed"
    })
    .await;
    // The recorded arrival is the one the receipt names (the coordinate a requester reads from the
    // chain is the coordinate bezent recorded).
    let (block, index): (i64, i64) = sqlx::query_as(
        "SELECT block_number, position_index FROM observations WHERE detail ->> 'event' = 'Arrival'",
    )
    .fetch_one(&bed.admin)
    .await
    .unwrap();
    assert_eq!(block, coordinate["block_number"].as_i64().unwrap());
    assert_eq!(index, coordinate["position"]["index"].as_i64().unwrap());
    id
}

/// A deposit (`AC-A2.1`, `AC-A2.2`, `INV-6`): an arrival at the vault on the fork is not
/// recorded, ordered or minted until it is final; once final and bound by the requester's request it
/// is minted on the Kreivo model, once, for exactly its amount, to the request's destination,
/// carrying the order's on-chain reference; the transfer completes; and the reserve check holds at
/// zero divergence (the vault's balance on the fork, the model's supply, the book).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s1_deposit(pool: PgPoolOptions, connect: PgConnectOptions) {
    let bed = Bed::new(pool, connect).await;
    let id = deposit(&bed, "req-1", DEPOSIT).await;
    // A few more turns for the supply and the reserve to be read at heads past the mint.
    for _ in 0..3 {
        bed.turn(0).await;
    }
    assert_eq!(bed.state_of(id).await.0, "Completed");
    // One mint, signed by the signer on a recorded intent (INV-9), minted once, for the amount.
    assert_eq!(bed.signatures().await, 1);
    // (The endpoint broadcasts the same bytes again until they are final; once is what was made.)
    assert_eq!(bed.submitted_once().await, 1);
    // Exactly the arrival's amount, for the request's destination, carrying the order's on-chain
    // reference: what the model's own events say, not what the store recorded.
    let reference: Vec<u8> = sqlx::query_scalar(
        "SELECT on_chain_reference FROM intents WHERE purpose = 'mint' AND network = 'kreivo-test'",
    )
    .fetch_one(&bed.admin)
    .await
    .unwrap();
    let minted: Vec<_> = bed
        .kreivo_events()
        .await
        .into_iter()
        .filter(|e| matches!(e, ContractEvent::Minted { .. }))
        .collect();
    assert_eq!(minted.len(), 1, "{minted:?}");
    let ContractEvent::Minted {
        reference: r,
        asset,
        to,
        amount,
        ..
    } = &minted[0]
    else {
        unreachable!()
    };
    assert_eq!(
        (r.to_vec(), *asset, *to, *amount),
        (
            reference,
            AssetId::Here(LOCAL_ASSET),
            ALICE_ACCOUNT,
            DEPOSIT
        )
    );
    assert_eq!(bed.kreivo_supply().await, DEPOSIT);
    assert_eq!(bed.reserve_balance().await, DEPOSIT);
    bed.assert_balanced(DEPOSIT, DEPOSIT).await;
}

/// A withdrawal (`AC-B1.1`, `AC-B1.2`, `INV-7`): a redemption on the Kreivo model pays nothing until
/// it is final; once final the core makes the withdrawal, the EVM endpoint prepares the payout, the
/// signer signs it, the vault pays through its delivery role on the fork, the payout is observed
/// carrying the transfer's reference, and the withdrawal completes. The reserve check holds at zero
/// divergence on what is left.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s2_withdrawal(pool: PgPoolOptions, connect: PgConnectOptions) {
    let bed = Bed::new(pool, connect).await;
    deposit(&bed, "req-1", DEPOSIT).await;
    for _ in 0..2 {
        bed.turn(0).await;
    }
    bed.assert_balanced(DEPOSIT, DEPOSIT).await;

    // The redemption is in a block that is not final: nothing is paid (INV-7).
    bed.redeem([0x21; 32], WITHDRAWAL, BENEFICIARY);
    bed.chain.produce();
    bed.kreivo_pass().await;
    bed.core_pass().await;
    bed.evm_pass().await;
    assert_eq!(
        bed.count("SELECT count(*) FROM transfers WHERE kind = 'withdrawal'")
            .await,
        0
    );
    assert_eq!(
        bed.count("SELECT count(*) FROM intents WHERE purpose = 'payout'")
            .await,
        0
    );
    assert_eq!(bed.reserve_balance().await, DEPOSIT);

    // Final: the core makes the withdrawal, the EVM endpoint prepares the payout, the signer signs
    // it, the vault pays through its delivery role, and the payout is observed with its reference.
    bed.chain.finalize(bed.chain.best());
    bed.until("the withdrawal is completed", 20, 65, || async {
        bed.maybe_transfer("withdrawal")
            .await
            .is_some_and(|(_, state)| state == "Completed")
    })
    .await;
    let (id, _) = bed.maybe_transfer("withdrawal").await.unwrap();
    assert_eq!(
        bed.anvil.usdc_balance(BENEFICIARY).await,
        U256::from(WITHDRAWAL)
    );
    // The payout leg: from the vault, to the beneficiary, carrying the transfer's payout reference.
    let (sender, recipient, amount, on_chain): (String, String, String, Vec<u8>) = sqlx::query_as(
        "SELECT sender, recipient, amount::text, on_chain_reference FROM observations \
         WHERE detail ->> 'event' = 'Payout'",
    )
    .fetch_one(&bed.admin)
    .await
    .unwrap();
    assert_eq!(sender, format!("{:#x}", bed.vault.safe));
    assert_eq!(recipient, BENEFICIARY.to_string());
    assert_eq!(amount, WITHDRAWAL.to_string());
    let want: Vec<u8> =
        sqlx::query_scalar("SELECT destination_reference FROM transfers WHERE id = $1")
            .bind(id)
            .fetch_one(&bed.admin)
            .await
            .unwrap();
    assert_eq!(on_chain, want);
    for _ in 0..3 {
        bed.turn(65).await;
    }
    assert_eq!(bed.reserve_balance().await, DEPOSIT - WITHDRAWAL);
    assert_eq!(bed.kreivo_supply().await, DEPOSIT - WITHDRAWAL);
    bed.assert_balanced(DEPOSIT - WITHDRAWAL, DEPOSIT - WITHDRAWAL)
        .await;
}

/// The payout's transaction and bytes, once the EVM endpoint has signed and sent it.
async fn payout_in_the_pool(bed: &Bed) -> Option<(Vec<u8>, Vec<u8>)> {
    sqlx::query_as(
        "SELECT s.tx_hash, s.bytes FROM signed_transactions s JOIN intents i ON i.id = s.intent_id \
         WHERE i.purpose = 'payout' AND i.network = 'polygon' ORDER BY s.id LIMIT 1",
    )
    .fetch_optional(&bed.admin)
    .await
    .unwrap()
}

/// A payout that is not executed (`AC-B1.3`, `AC-B1.4`, `INV-8`; REQ-NX-4, REQ-RV-1, REQ-RF-1): the network drops
/// the payout, time passes the route's deadline, the core sends the transfer to review and orders a
/// cancel, the cancel takes the payout's place and is final, the endpoint proves the payout not
/// executed, two operators resolve the review into a refund, and the refund is minted once. Nothing
/// is refunded before the proof, and the beneficiary is never paid.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s3_a_payout_not_executed_is_refunded_once_and_never_paid(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let bed = Bed::new(pool, connect).await;
    deposit(&bed, "req-1", DEPOSIT).await;
    for _ in 0..2 {
        bed.turn(0).await;
    }

    // Transactions wait in the pool until a block is mined: nothing the endpoint sends executes
    // until the scenario mines.
    bed.anvil
        .rpc("evm_setAutomine", serde_json::json!([false]))
        .await;
    bed.redeem([0x31; 32], WITHDRAWAL, BENEFICIARY);
    bed.kreivo_blocks(1).await;
    bed.until("the payout is signed and sent", 12, 0, || async {
        payout_in_the_pool(&bed).await.is_some()
    })
    .await;
    let (hash, raw) = payout_in_the_pool(&bed).await.unwrap();
    let (id, _) = bed.maybe_transfer("withdrawal").await.unwrap();
    assert_eq!(bed.state_of(id).await, ("Delivering".into(), None));

    // The network drops it. Time passes the deadline and the fork finalizes blocks without it.
    bed.anvil
        .rpc(
            "anvil_dropTransaction",
            serde_json::json!([format!("0x{}", hex::encode(&hash))]),
        )
        .await;
    bed.anvil
        .rpc("evm_increaseTime", serde_json::json!([10_000]))
        .await;
    bed.anvil.finalize().await;
    assert_eq!(bed.anvil.usdc_balance(BENEFICIARY).await, U256::ZERO);

    // Unknown at its deadline: review, never a refund (REQ-RV-1, REQ-RF-1), and the cancel is
    // ordered, signed (the signer reads its bytes) and sent in the payout's place.
    bed.until("the transfer is in review with its cancel sent", 12, 0, || async {
        bed.state_of(id).await.0 == "Needs-Review"
            && bed
                .count(
                    "SELECT count(*) FROM signed_transactions s JOIN intents i ON i.id = s.intent_id \
                     WHERE i.purpose = 'cancel'",
                )
                .await
                > 0
    })
    .await;
    assert_eq!(
        bed.count("SELECT count(*) FROM transfers WHERE kind = 'refund'")
            .await,
        0
    );
    assert_eq!(
        bed.count("SELECT count(*) FROM intents WHERE purpose = 'refund-mint'")
            .await,
        0,
        "REQ-RF-1: a refund is ordered on unknown"
    );

    // The cancel executes, in the payout's place, and the fork finalizes it.
    bed.anvil.mine(1).await;
    bed.anvil.finalize().await;
    bed.until("the payout is proven not executed", 12, 0, || async {
        bed.count("SELECT count(*) FROM non_execution_proofs").await == 1
    })
    .await;
    let (kind, final_block): (String, i64) =
        sqlx::query_as("SELECT kind, final_block_number FROM non_execution_proofs")
            .fetch_one(&bed.admin)
            .await
            .unwrap();
    assert_eq!(kind, "nonce-consumed-by-other");
    bed.until("the proof is attached to the review", 6, 0, || async {
        bed.count("SELECT count(*) FROM event_log WHERE kind = 'review-evidence'")
            .await
            == 1
    })
    .await;
    // A proof alone moves nothing.
    assert_eq!(bed.state_of(id).await.0, "Needs-Review");
    assert_eq!(
        bed.count("SELECT count(*) FROM transfers WHERE kind = 'refund'")
            .await,
        0
    );

    // The resolution moves value, so two operators, each on a login of their own, run it.
    let (hash, time): (Vec<u8>, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
        "SELECT block_hash, block_time FROM final_heads WHERE network = 'polygon' AND block_number = $1",
    )
    .bind(final_block)
    .fetch_one(&bed.admin)
    .await
    .unwrap();
    let action: i64 = sqlx::query_scalar(
        "INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, \
             block_hash, block_time, justification) \
         VALUES ('review-resolution', $1, 'Refunding', 'polygon', $2, $3, $4, \
                 'the cancel is final and the payout is proven not executed') RETURNING id",
    )
    .bind(id)
    .bind(final_block)
    .bind(&hash)
    .bind(time)
    .fetch_one(&bed.ops[0])
    .await
    .unwrap();
    bed.turn(0).await;
    assert_eq!(
        bed.state_of(id).await.0,
        "Needs-Review",
        "REQ-GV-5: one operator moved value"
    );
    let own =
        sqlx::query("INSERT INTO manual_approvals (action_id, justification) VALUES ($1, 'mine')")
            .bind(action)
            .execute(&bed.ops[0])
            .await;
    assert!(
        own.is_err(),
        "REQ-GV-5: the author approved their own resolution"
    );
    sqlx::query(
        "INSERT INTO manual_approvals (action_id, justification) VALUES ($1, 'checked the proof')",
    )
    .bind(action)
    .execute(&bed.ops[1])
    .await
    .unwrap();

    bed.until(
        "the refund is minted and the withdrawal refunded",
        16,
        0,
        || async { bed.state_of(id).await.0 == "Refunded" },
    )
    .await;
    for _ in 0..3 {
        bed.turn(65).await;
    }

    // Exactly one refund mint; the redemption's value is back on the issuing network.
    assert_eq!(
        bed.count("SELECT count(*) FROM transfers WHERE kind = 'refund'")
            .await,
        1
    );
    assert_eq!(
        bed.count("SELECT count(*) FROM observations WHERE detail ->> 'event' = 'RefundMinted'")
            .await,
        1
    );
    assert_eq!(bed.submitted_once().await, 2, "one mint, one refund mint");
    let refunded: Vec<_> = bed
        .kreivo_events()
        .await
        .into_iter()
        .filter(|e| matches!(e, ContractEvent::RefundMinted { .. }))
        .collect();
    assert_eq!(refunded.len(), 1, "{refunded:?}");
    let ContractEvent::RefundMinted {
        reference,
        to,
        asset,
        amount,
        ..
    } = &refunded[0]
    else {
        unreachable!()
    };
    assert_eq!(
        (*reference, *to, *asset, *amount),
        (
            [0x31; 32],
            ALICE_ACCOUNT,
            AssetId::Here(LOCAL_ASSET),
            WITHDRAWAL
        ),
        "the refund is the redemption's, to its redeemer, for what it destroyed"
    );
    assert_eq!(bed.kreivo_supply().await, DEPOSIT);

    // No double payment: the beneficiary was never paid and the vault never lost a unit; the
    // payout's bytes, sent again, are refused by the chain (its nonce is the cancel's).
    assert_eq!(bed.anvil.usdc_balance(BENEFICIARY).await, U256::ZERO);
    assert_eq!(bed.reserve_balance().await, DEPOSIT);
    assert_eq!(
        bed.count("SELECT count(*) FROM observations WHERE detail ->> 'event' = 'Payout'")
            .await,
        0
    );
    let again = bed
        .anvil
        .try_rpc(
            "eth_sendRawTransaction",
            serde_json::json!([format!("0x{}", hex::encode(&raw))]),
        )
        .await;
    assert!(
        again.is_err(),
        "the dropped payout was accepted again: {again:?}"
    );
    // A second refund of the same redemption is refused (INV-8): the transfer is terminal.
    let second = sqlx::query_scalar::<_, i64>(
        "INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, \
             block_hash, block_time, justification) \
         VALUES ('review-resolution', $1, 'Refunding', 'polygon', $2, $3, $4, 'again') RETURNING id",
    )
    .bind(id)
    .bind(final_block)
    .bind(&hash)
    .bind(time)
    .fetch_one(&bed.ops[0])
    .await;
    if let Ok(action) = second {
        let _ = sqlx::query(
            "INSERT INTO manual_approvals (action_id, justification) VALUES ($1, 'again')",
        )
        .bind(action)
        .execute(&bed.ops[1])
        .await;
    }
    for _ in 0..2 {
        bed.turn(0).await;
    }
    assert_eq!(
        bed.count("SELECT count(*) FROM transfers WHERE kind = 'refund'")
            .await,
        1
    );
    assert_eq!(
        bed.submitted_once().await,
        2,
        "INV-8: a second refund mint was made"
    );
    assert_eq!(bed.state_of(id).await.0, "Refunded");
    bed.assert_balanced(DEPOSIT, DEPOSIT).await;
}

/// The ingress took the request in and refused it: no movement of that kind at that coordinate.
fn assert_source_not_found(status: u16, body: &serde_json::Value) {
    // Taken in (201), or answered again from the record (200).
    assert!(status == 201 || status == 200, "{status} {body}");
    assert_eq!(body["state"], "Refused", "{body}");
    assert_eq!(body["error"], "ERR-SourceNotFound", "{body}");
}

/// What nothing was minted looks like: no order, no intent, no signature, nothing submitted to the
/// issuing network, no supply, no reserve, and a reserve check that holds at zero.
async fn assert_nothing_minted(bed: &Bed) {
    assert_eq!(
        bed.count("SELECT count(*) FROM intents WHERE network = 'kreivo-test'")
            .await,
        0
    );
    assert_eq!(bed.signatures().await, 0);
    assert!(bed.chain.submitted().is_empty(), "a mint was submitted");
    assert_eq!(bed.kreivo_supply().await, 0);
}

/// `INV-6`, `INV-16`: a reorganisation of the fork across an arrival, before it
/// is final. The arrival is in a block; the fork then abandons that block for another at the same
/// height; the requester points at the coordinate it read; the endpoint, which records only final
/// blocks, never saw it; nothing is minted, then or after finality, and the reserve equation holds
/// at zero.
///
/// The reorganisation is anvil's snapshot revert (`evm_snapshot`, `evm_revert`): the chain is rolled
/// back to the block before the arrival and mined forward with other blocks. `anvil_reorg` is not
/// used: on a fork, in anvil 1.7.1, it drops the transaction but leaves its effect in the token's
/// storage (the vault kept the balance of a transfer no block contained), which is not a reorg.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s4_a_reorg_across_an_arrival_before_finality_mints_nothing(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let bed = Bed::new(pool, connect).await;
    let snapshot = bed.anvil.rpc("evm_snapshot", serde_json::json!([])).await;
    let arrival = bed.arrive(DEPOSIT).await;
    let receipt = bed.anvil.receipt(arrival).await;
    let coordinate = Bed::coordinate_of(&receipt);
    let number = coordinate["block_number"].as_u64().unwrap();
    let old_hash = coordinate["block_hash"].as_str().unwrap().to_owned();
    bed.anvil.mine(3).await;
    assert_eq!(bed.reserve_balance().await, DEPOSIT);

    // The reorganisation: the chain goes back to before the arrival and grows other blocks.
    let reverted = bed
        .anvil
        .rpc("evm_revert", serde_json::json!([snapshot]))
        .await;
    assert_eq!(reverted, serde_json::json!(true));
    bed.anvil.mine(5).await;
    let (new_hash, _) = bed.anvil.block(number).await;
    assert_ne!(
        format!("{new_hash:#x}"),
        old_hash,
        "the block at that height is still the same: the reorganisation did nothing"
    );
    assert_eq!(
        bed.reserve_balance().await,
        0,
        "the arrival survived the reorganisation"
    );
    assert!(
        bed.anvil
            .rpc("eth_getTransactionReceipt", serde_json::json!([arrival]))
            .await
            .is_null()
    );

    // The requester's request for the coordinate it read: nothing is there to bind.
    let (status, body) = bed
        .http(
            "POST",
            "/v0/deposits",
            Some(&Bed::deposit_body("req-1", coordinate.clone())),
        )
        .await;
    assert_source_not_found(status, &body);
    bed.turn(0).await;
    bed.anvil.finalize().await;
    for _ in 0..4 {
        bed.turn(0).await;
    }
    let (status, body) = bed
        .http(
            "POST",
            "/v0/deposits",
            Some(&Bed::deposit_body("req-1", coordinate)),
        )
        .await;
    assert_source_not_found(status, &body);
    assert_eq!(
        bed.count("SELECT count(*) FROM observations WHERE detail ->> 'event' = 'Arrival'")
            .await,
        0
    );
    assert_eq!(
        bed.count("SELECT count(*) FROM transfers WHERE state <> 'Refused'")
            .await,
        0
    );
    for _ in 0..3 {
        bed.turn(0).await;
    }
    assert_nothing_minted(&bed).await;
    bed.assert_balanced(0, 0).await;
}

/// What the compromise left in the store: the alerts, and the pauses resting on them.
async fn compromise_pauses(bed: &Bed) -> Vec<i64> {
    sqlx::query_scalar(
        "SELECT p.id FROM pauses p JOIN event_log e ON e.seq = p.alert_seq \
         WHERE e.kind = 'key-compromise' AND p.cause = 'REQ-IC-4' AND p.asset IS NULL \
           AND p.network IS NULL AND p.direction IS NULL ORDER BY p.id",
    )
    .fetch_all(&bed.admin)
    .await
    .unwrap()
}

/// `INV-15`; REQ-IC-4, REQ-GV-2, REQ-GV-5: the delivery key is used for something
/// bezent did not record (here: the key spends the nonce of a payout in flight on a call of its own,
/// as a stolen key would). The EVM endpoint's prover sees it on the fork; the alert and the pause
/// are one commit; the core, with a new redemption waiting, orders nothing and the signer signs
/// nothing; the ingress takes no new request; observation goes on (a pause stops new work and
/// nothing else, `INV-15`); no service can lift it; two operators on logins of their own lift it by
/// the rule, and the held withdrawal then goes through.
///
/// Not asserted, deliberately: what becomes of a signing intent recorded before the pause. It is
/// still signed, with no bound, until amendment 0022 A1 is ruled (issue #77; the compromise
/// criterion stays in `e2e/criteria/known_unmet.toml`, and this scenario does not claim it).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s5_a_compromise_signal_pauses_stops_signing_and_is_lifted_by_the_rule(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    use alloy::primitives::{B256, U256};
    use alloy::sol_types::SolCall;

    let bed = Bed::new(pool, connect).await;
    deposit(&bed, "req-1", DEPOSIT).await;
    for _ in 0..2 {
        bed.turn(0).await;
    }
    assert_eq!(bed.signatures().await, 1);
    assert!(compromise_pauses(&bed).await.is_empty());

    // A payout in flight, and the key spends its nonce on a call of its own.
    bed.anvil
        .rpc("evm_setAutomine", serde_json::json!([false]))
        .await;
    bed.redeem([0x51; 32], WITHDRAWAL, BENEFICIARY);
    bed.kreivo_blocks(1).await;
    bed.until("the payout is signed and sent", 12, 0, || async {
        payout_in_the_pool(&bed).await.is_some()
    })
    .await;
    let signed_before = bed.signatures().await;
    let (_, raw) = payout_in_the_pool(&bed).await.unwrap();
    let (payout, _) = bezent_evm_endpoint::submit::decode_signed(&raw).unwrap();
    let (withdrawal, _) = bed.maybe_transfer("withdrawal").await.unwrap();
    let reference: Vec<u8> =
        sqlx::query_scalar("SELECT destination_reference FROM transfers WHERE id = $1")
            .bind(withdrawal)
            .fetch_one(&bed.admin)
            .await
            .unwrap();
    let emit = bezent_evm_endpoint::evm::emitReferenceCall {
        r#ref: B256::from_slice(&reference),
    }
    .abi_encode();
    bed.anvil
        .send(serde_json::json!({
            "from": bed.evm_lane_address(),
            "to": bed.vault.emitter,
            "data": format!("0x{}", hex::encode(emit)),
            "nonce": format!("0x{:x}", payout.nonce),
            "gas": "0x30000",
            "maxFeePerGas": format!("0x{:x}", payout.max_fee_per_gas * 2),
            "maxPriorityFeePerGas": format!("0x{:x}", payout.max_priority_fee_per_gas * 2),
        }))
        .await;
    bed.anvil.mine(1).await;
    bed.anvil.finalize().await;
    assert_eq!(bed.anvil.usdc_balance(BENEFICIARY).await, U256::ZERO);

    // The endpoint sees a use of the key that answers no recorded intent: one alert, one pause, in
    // the same commit, resting on it.
    bed.evm_pass().await;
    assert_eq!(
        bed.count("SELECT count(*) FROM event_log WHERE kind = 'key-compromise'")
            .await,
        1
    );
    let pauses = compromise_pauses(&bed).await;
    assert_eq!(pauses.len(), 1, "no pause applied");
    let pause = pauses[0];

    // Signing stopped: a redemption made after the pause is observed and made a withdrawal, and
    // waits: no order past the pause, no intent, no signature, no submission, however many turns
    // pass.
    bed.redeem([0x52; 32], WITHDRAWAL, BENEFICIARY);
    bed.kreivo_blocks(1).await;
    let submitted = bed.submitted_once().await;
    let intents = bed.count("SELECT count(*) FROM intents").await;
    let signatures = bed.signatures().await;
    for _ in 0..4 {
        bed.turn(0).await;
    }
    let held: i64 = sqlx::query_scalar(
        "SELECT id FROM transfers WHERE kind = 'withdrawal' AND on_chain_reference = $1",
    )
    .bind([0x52u8; 32].to_vec())
    .fetch_one(&bed.admin)
    .await
    .unwrap();
    assert_eq!(bed.state_of(held).await.1.as_deref(), Some("CND-Paused"));
    assert_eq!(
        bed.count("SELECT count(*) FROM intents").await,
        intents,
        "an intent was recorded under the pause"
    );
    assert_eq!(bed.signatures().await, signatures);
    assert_eq!(bed.signatures().await, signed_before);
    assert_eq!(bed.submitted_once().await, submitted);
    // New work is refused at the door; observation goes on (INV-15).
    let late = bed.arrive(DEPOSIT).await;
    bed.anvil.mine(1).await;
    let late_coordinate = Bed::coordinate_of(&bed.anvil.receipt(late).await);
    bed.anvil.finalize().await;
    bed.evm_pass().await;
    assert_eq!(
        bed.count("SELECT count(*) FROM observations WHERE detail ->> 'event' = 'Arrival'")
            .await,
        2,
        "INV-15: a pause stops observation"
    );
    let (status, body) = bed
        .http(
            "POST",
            "/v0/deposits",
            Some(&Bed::deposit_body("req-3", late_coordinate)),
        )
        .await;
    assert!(status == 201 || status == 200, "{status} {body}");
    assert_eq!(body["state"], "Refused", "{body}");
    assert_eq!(body["error"], "ERR-RoutePaused", "{body}");

    // No service lifts it, the one that raised it included.
    for pool in [&bed.evm_pool, &bed.core_pool] {
        assert!(
            sqlx::raw_sql("DELETE FROM pauses")
                .execute(pool)
                .await
                .is_err()
        );
    }
    assert_eq!(compromise_pauses(&bed).await, vec![pause]);

    // Lifted by the rule (REQ-GV-2): a request by one operator, an approval by another. The author
    // cannot approve their own request.
    let request: i64 = sqlx::query_scalar(
        "SELECT operator_request_unpause($1, 'the key is rotated and the vault checked')",
    )
    .bind(pause)
    .fetch_one(&bed.ops[0])
    .await
    .unwrap();
    assert!(
        sqlx::query_scalar::<_, i64>("SELECT operator_approve_unpause($1, 'mine')")
            .bind(request)
            .fetch_one(&bed.ops[0])
            .await
            .is_err(),
        "REQ-GV-5: an operator approved their own request to lift"
    );
    assert_eq!(
        compromise_pauses(&bed).await,
        vec![pause],
        "lifted by one person"
    );
    sqlx::query_scalar::<_, i64>(
        "SELECT operator_approve_unpause($1, 'checked the key and the vault')",
    )
    .bind(request)
    .fetch_one(&bed.ops[1])
    .await
    .unwrap();
    assert!(compromise_pauses(&bed).await.is_empty());

    // The held withdrawal resumes by itself and pays out, its payout signed on a recorded intent.
    bed.anvil
        .rpc("evm_setAutomine", serde_json::json!([true]))
        .await;
    bed.until("the held withdrawal is completed", 24, 65, || async {
        bed.state_of(held).await.0 == "Completed"
    })
    .await;
    assert_eq!(bed.signatures().await, signed_before + 1);
    assert_eq!(
        bed.anvil.usdc_balance(BENEFICIARY).await,
        U256::from(WITHDRAWAL)
    );
}

/// The block and hash this suite forks at are the ones `e2e/evm/forks.env` pins (that file is the
/// one place a pin changes).
#[test]
fn the_fork_is_at_the_pinned_block() {
    let pins = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../e2e/evm/forks.env"
    ))
    .expect("e2e/evm/forks.env");
    let block: u64 = pins
        .lines()
        .find_map(|l| l.strip_prefix("POLYGON_BLOCK="))
        .and_then(|v| v.trim().parse().ok())
        .expect("POLYGON_BLOCK in forks.env");
    assert_eq!(
        block, PINNED_BLOCK,
        "update PINNED_BLOCK and PINNED_HASH in bed.rs"
    );
}
