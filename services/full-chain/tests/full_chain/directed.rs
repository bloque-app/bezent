// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-018 on the full-chain bed: directed deposits end to end, and the exercises of the runbooks of
//! `T-018-19` (`deploy/runbooks/procedures/directed-return-failed.md`,
//! `procedures/directed-spam.md`, `pages/unpaired-direction.md`; `NFR-9`).
//!
//! What is real and what is not is `bed.rs`'s: Polygon is an anvil fork at the pinned block with the
//! real native USDC and the vault and `DirectedDeposit` the repository's scripts deploy; Kreivo is
//! the in-memory model. A redemption is an event the model is told to emit, as in `s2`.
//!
//! The directing contract here is the repository's own build, deployed by
//! `script/DeployDirected.s.sol` in front of the bed's vault, declared in the store's
//! `directing_code` and named in the EVM endpoint's `[[directing]]`, each from its deployment block
//! (`Bed::directed`). A user calls it as the web app does: `depositWithPermit` with an EIP-2612 permit
//! for exactly the amount, signed over the domain read from the token, or the fallback, `approve`
//! then `deposit`.

use alloy::primitives::{Address, B256, U256, keccak256};
use alloy::sol;
use alloy::sol_types::SolCall;
use bezent_core_service::directed_alerts::{self, Outcome, Thresholds};
use bezent_evm_endpoint::evm::{DIRECTED_TOPIC, TRANSFER_TOPIC};
use bezent_kreivo_endpoint::kreivo::{AssetId, ContractEvent};
use k256::ecdsa::SigningKey;
use serde_json::{Value, json};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use crate::bed::*;

sol! {
    function depositWithPermit(uint256 amount, bytes32 recipient, address refund, uint256 deadline, uint8 v, bytes32 r, bytes32 s) external;
    function deposit(uint256 amount, bytes32 recipient, address refund) external;
    function approve(address spender, uint256 amount) external returns (bool);
    function nonces(address owner) external view returns (uint256);
    function DOMAIN_SEPARATOR() external view returns (bytes32);
    function blacklister() external view returns (address);
    function blacklist(address account) external;
    function unBlacklist(address account) external;
    function isBlacklisted(address account) external view returns (bool);
    function token() external view returns (address);
    function reserve() external view returns (address);
    function minimum() external view returns (uint256);
    function maximum() external view returns (uint256);
}

/// Foundry's account 8 (anvil's published test key): the user who signs a permit.
const USER_KEY: &str = "dbda1821b80551c9d65939329250298aa3472ba22feea921c0cf5d620ea67b97";
/// A user with no key the bed knows, impersonated on the fork: the fallback path (`approve`, then
/// `deposit`), as a wallet that cannot sign typed data takes it.
const PLAIN_USER: Address = Address::repeat_byte(0xa7);
/// The original depositor each call names as `refund` (`d-refund-dest-swaps`): not the caller.
const REFUND: Address = Address::repeat_byte(0xaf);
/// A refund address the token refuses to pay (USDC-blacklisted in `s8`).
const REFUND_BLOCKED: Address = Address::repeat_byte(0xab);
/// What the scenarios direct, in the token's minor units (6 decimals).
const DIRECTED: u128 = 25_000_000;
const WITHDRAWAL: u128 = 10_000_000;
/// Bob, prefix 42, and his account: a recipient bezent refuses by rule once he is declared a
/// ledger's contract account (`REQ-IN-5`, PLAN `DEC-15`).
const BOB: &str = "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty";
const BOB_ACCOUNT: [u8; 32] = [
    0x8e, 0xaf, 0x04, 0x15, 0x16, 0x87, 0x73, 0x63, 0x26, 0xc9, 0xfe, 0xa1, 0x7e, 0x25, 0xfc, 0x52,
    0x87, 0x61, 0x36, 0x93, 0xc9, 0x12, 0x90, 0x9c, 0xb2, 0x26, 0xaa, 0x47, 0x94, 0xf2, 0x6a, 0x48,
];

fn user_key() -> SigningKey {
    SigningKey::from_slice(&hex::decode(USER_KEY).unwrap()).unwrap()
}

fn address_of(key: &SigningKey) -> Address {
    let point = key.verifying_key().to_encoded_point(false);
    Address::from_raw_public_key(&point.as_bytes()[1..])
}

fn word_hex(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

/// A read of `to` at the fork's latest block.
async fn call(bed: &Bed, to: Address, data: Vec<u8>) -> Vec<u8> {
    let out = bed
        .anvil
        .rpc(
            "eth_call",
            json!([{"to": to, "data": word_hex(&data)}, "latest"]),
        )
        .await;
    hex::decode(out.as_str().unwrap().trim_start_matches("0x")).unwrap()
}

fn usdc() -> Address {
    USDC.parse().unwrap()
}

/// `from` sends `data` to `to` on the fork, impersonated (anvil), with gas money; the receipt, which
/// must be a success.
async fn send(bed: &Bed, from: Address, to: Address, data: Vec<u8>) -> Value {
    bed.anvil
        .rpc("anvil_impersonateAccount", json!([from]))
        .await;
    bed.anvil
        .rpc("anvil_setBalance", json!([from, "0x56BC75E2D63100000"]))
        .await;
    let tx = bed
        .anvil
        .send(json!({"from": from, "to": to, "data": word_hex(&data)}))
        .await;
    let receipt = bed.anvil.receipt(tx).await;
    assert_eq!(receipt["status"], "0x1", "{receipt}");
    receipt
}

/// The user signs an EIP-2612 permit for exactly `amount` to the directing contract, over the
/// domain read from the token (`DOMAIN_SEPARATOR()`, as the web app reads it), and calls
/// `depositWithPermit` naming `recipient` and `refund`. Returns the receipt.
async fn direct_with_permit(
    bed: &Bed,
    key: &SigningKey,
    amount: u128,
    recipient: [u8; 32],
    refund: Address,
) -> Value {
    let owner = address_of(key);
    let directed = bed.directed.expect("a bed with directing code");
    // Foundry's accounts may carry an EIP-7702 delegation on Polygon at the pinned block, which
    // would make USDC check the permit as a contract's (EIP-1271); this user is a plain account.
    bed.anvil.rpc("anvil_setCode", json!([owner, "0x"])).await;
    bed.anvil.fund_usdc(owner, amount).await;
    let nonce = U256::from_be_slice(&call(bed, usdc(), noncesCall { owner }.abi_encode()).await);
    let domain = B256::from_slice(&call(bed, usdc(), DOMAIN_SEPARATORCall {}.abi_encode()).await);
    let deadline = U256::MAX;
    let typehash = keccak256(
        "Permit(address owner,address spender,uint256 value,uint256 nonce,uint256 deadline)",
    );
    let structured = keccak256(
        [
            typehash.as_slice(),
            owner.into_word().as_slice(),
            directed.into_word().as_slice(),
            &U256::from(amount).to_be_bytes::<32>(),
            &nonce.to_be_bytes::<32>(),
            &deadline.to_be_bytes::<32>(),
        ]
        .concat(),
    );
    let digest = keccak256([&[0x19, 0x01][..], domain.as_slice(), structured.as_slice()].concat());
    let (signature, recovery) = key.sign_prehash_recoverable(digest.as_slice()).unwrap();
    let data = depositWithPermitCall {
        amount: U256::from(amount),
        recipient: B256::from(recipient),
        refund,
        deadline,
        v: 27 + recovery.to_byte(),
        r: B256::from_slice(&signature.r().to_bytes()),
        s: B256::from_slice(&signature.s().to_bytes()),
    }
    .abi_encode();
    send(bed, owner, directed, data).await
}

/// The fallback, for a wallet that cannot sign typed data: `approve` exactly `amount`, then
/// `deposit`.
async fn direct_with_approval(
    bed: &Bed,
    from: Address,
    amount: u128,
    recipient: [u8; 32],
    refund: Address,
) -> Value {
    let directed = bed.directed.expect("a bed with directing code");
    bed.anvil.fund_usdc(from, amount).await;
    send(
        bed,
        from,
        usdc(),
        approveCall {
            spender: directed,
            amount: U256::from(amount),
        }
        .abi_encode(),
    )
    .await;
    send(
        bed,
        from,
        directed,
        depositCall {
            amount: U256::from(amount),
            recipient: B256::from(recipient),
            refund,
        }
        .abi_encode(),
    )
    .await
}

fn number(v: &Value) -> u64 {
    u64::from_str_radix(v.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}

/// The coordinate of the token's `Transfer` into the vault in `receipt` (the arrival's, `REQ-RN-3`),
/// after checking what the contract promises: it moves exactly `amount` from the caller to the
/// vault, and its `Directed` log, naming `recipient` and `refund`, is the very next log (`AC-A6.6`,
/// `REQ-RN-5`).
fn directed_coordinate(
    bed: &Bed,
    receipt: &Value,
    caller: Address,
    amount: u128,
    recipient: [u8; 32],
    refund: Address,
) -> Value {
    let logs = receipt["logs"].as_array().unwrap();
    let at = logs
        .iter()
        .position(|l| {
            l["address"].as_str().unwrap().eq_ignore_ascii_case(USDC)
                && l["topics"][0].as_str() == Some(&format!("{TRANSFER_TOPIC:#x}"))
        })
        .expect("the token's transfer");
    let transfer = &logs[at];
    assert_eq!(transfer["topics"][1], format!("{:#x}", caller.into_word()));
    assert_eq!(
        transfer["topics"][2],
        format!("{:#x}", bed.vault.safe.into_word())
    );
    assert_eq!(
        transfer["data"],
        format!("{:#x}", B256::from(U256::from(amount)))
    );
    let directed = &logs[at + 1];
    assert!(
        directed["address"]
            .as_str()
            .unwrap()
            .eq_ignore_ascii_case(&format!("{:#x}", bed.directed.unwrap()))
    );
    assert_eq!(directed["topics"][0], format!("{DIRECTED_TOPIC:#x}"));
    assert_eq!(directed["topics"][1], format!("{:#x}", caller.into_word()));
    assert_eq!(directed["topics"][2], word_hex(&recipient));
    assert_eq!(
        directed["data"],
        format!(
            "0x{}{}",
            hex::encode(U256::from(amount).to_be_bytes::<32>()),
            hex::encode(refund.into_word())
        )
    );
    assert_eq!(
        number(&directed["logIndex"]),
        number(&transfer["logIndex"]) + 1
    );
    json!({
        "network": POLYGON,
        "block_number": number(&receipt["blockNumber"]),
        "block_hash": receipt["blockHash"],
        "position": {"kind": "log", "index": number(&transfer["logIndex"])},
    })
}

/// The reserve check at zero divergence (`INV-1`): it holds, and what the networks show and what the
/// book holds do not differ. Unlike [`Bed::assert_balanced`] it says nothing of the terms, so it
/// also holds while a payout is owed.
async fn assert_zero_divergence(bed: &Bed, when: &str) {
    use bezent_core_service::reserve::check::Outcome;
    let e = bed.check_reserve().await;
    assert_eq!(e.outcome, Outcome::Holds, "{when}: {e:#?}");
    assert!(e.differences.is_empty(), "{when}: {e:#?}");
}

/// The transfers bezent made itself from directed arrivals (`directed:` request references), of
/// `kind`: (id, state, condition), in order.
async fn directed_transfers(bed: &Bed, kind: &str) -> Vec<(i64, String, Option<String>)> {
    sqlx::query_as(
        "SELECT id, state::text, condition::text FROM transfers \
         WHERE kind::text = $1 AND request_reference LIKE 'directed:%' ORDER BY id",
    )
    .bind(kind)
    .fetch_all(&bed.admin)
    .await
    .unwrap()
}

async fn minted(bed: &Bed) -> Vec<([u8; 32], u128)> {
    bed.kreivo_events()
        .await
        .into_iter()
        .filter_map(|e| match e {
            ContractEvent::Minted {
                to, amount, asset, ..
            } if asset == AssetId::Here(LOCAL_ASSET) => Some((to, amount)),
            _ => None,
        })
        .collect()
}

/// Declares Bob a ledger's contract account: a recipient bezent refuses by rule.
async fn refuse_bob(bed: &Bed) {
    sqlx::query(
        "INSERT INTO ledger_accounts (network, address, reason) \
         VALUES ($1, $2, 'a ledger''s contract account (full-chain bed)')",
    )
    .bind(KREIVO)
    .bind(BOB)
    .execute(&bed.admin)
    .await
    .unwrap();
}

/// USDC's blacklister, impersonated on the fork, blacklists `account` (or lifts it).
async fn set_blacklisted(bed: &Bed, account: Address, on: bool) {
    let blacklister =
        Address::from_slice(&call(bed, usdc(), blacklisterCall {}.abi_encode()).await[12..]);
    let data = if on {
        blacklistCall { account }.abi_encode()
    } else {
        unBlacklistCall { account }.abi_encode()
    };
    send(bed, blacklister, usdc(), data).await;
    assert_eq!(is_blacklisted(bed, account).await, on);
}

async fn is_blacklisted(bed: &Bed, account: Address) -> bool {
    call(bed, usdc(), isBlacklistedCall { account }.abi_encode()).await[31] == 1
}

/// The latest final head of Polygon the store holds: (number, hash, time).
async fn final_polygon_head(bed: &Bed) -> (i64, Vec<u8>, chrono::DateTime<chrono::Utc>) {
    sqlx::query_as(
        "SELECT block_number, block_hash, block_time FROM final_heads \
         WHERE network = $1 ORDER BY block_number DESC LIMIT 1",
    )
    .bind(POLYGON)
    .fetch_one(&bed.admin)
    .await
    .unwrap()
}

/// A directed round trip (`AC-A6.1`, `AC-A6.2`, `AC-A6.4`, `AC-A6.6`, `INV-1`, `INV-6`; F-018 plan
/// "Done when", endpoint and core): `DirectedDeposit` deployed on the fork in front of the vault by
/// its script, declared, and configured in the endpoint; a user's `depositWithPermit` naming a
/// Kreivo recipient and a refund address moves exactly the amount into the vault with `Directed`
/// right after the `Transfer`; nothing is recorded or minted before it is final; once final it is one
/// directed arrival at the `Transfer`'s coordinate and never an `Arrival`; a requester naming that
/// coordinate is refused `ERR-SourceNotFound`; the core makes the deposit with no request and mints
/// it on the Kreivo model, once, to the recipient; the recipient redeems and the core pays out from
/// the vault on the fork. Then a return: a directed deposit (the fallback path, `approve` then
/// `deposit`) naming a recipient bezent refuses is returned to the refund address its caller named,
/// once, and never minted. The reserve check stays at zero divergence throughout.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s7_ac_a6_1_ac_a6_4_a_directed_round_trip_and_a_return(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let bed = Bed::directed(pool, connect).await;
    let directed = bed.directed.unwrap();
    // The deployment is the configuration's: the token, the vault, the route's limits.
    assert_eq!(
        Address::from_slice(&call(&bed, directed, tokenCall {}.abi_encode()).await[12..]),
        usdc()
    );
    assert_eq!(
        Address::from_slice(&call(&bed, directed, reserveCall {}.abi_encode()).await[12..]),
        bed.vault.safe
    );
    assert_eq!(
        U256::from_be_slice(&call(&bed, directed, minimumCall {}.abi_encode()).await),
        U256::from(DIRECTED_MINIMUM)
    );
    assert_eq!(
        U256::from_be_slice(&call(&bed, directed, maximumCall {}.abi_encode()).await),
        U256::from(DIRECTED_MAXIMUM)
    );
    bed.turn(0).await;
    assert_zero_divergence(&bed, "before anything").await;

    // The user's deposit, with a permit for exactly the amount.
    let key = user_key();
    let user = address_of(&key);
    let receipt = direct_with_permit(&bed, &key, DIRECTED, ALICE_ACCOUNT, REFUND).await;
    let coordinate = directed_coordinate(&bed, &receipt, user, DIRECTED, ALICE_ACCOUNT, REFUND);
    assert_eq!(bed.anvil.usdc_balance(user).await, U256::ZERO);
    assert_eq!(bed.reserve_balance().await, DIRECTED);
    assert_eq!(
        bed.anvil.usdc_balance(directed).await,
        U256::ZERO,
        "the contract holds a balance"
    );

    // Not final: nothing recorded, ordered or minted (INV-6).
    bed.turn(0).await;
    assert_eq!(
        bed.count("SELECT count(*) FROM observations WHERE kind = 'directed_arrival'")
            .await,
        0,
        "a directed arrival is recorded before it is final"
    );
    assert!(bed.chain.submitted().is_empty());
    assert_zero_divergence(&bed, "the arrival not final").await;

    // Final: one directed arrival at the Transfer's coordinate, never also an Arrival (AC-A6.6).
    bed.anvil.finalize().await;
    bed.evm_pass().await;
    let rows: Vec<(String, i64, i64, Value)> = sqlx::query_as(
        "SELECT kind::text, block_number, position_index, detail FROM observations \
         WHERE detail ->> 'event' IN ('DirectedArrival', 'Arrival', 'UnpairedDirection')",
    )
    .fetch_all(&bed.admin)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (kind, block, index, detail) = &rows[0];
    assert_eq!(kind, "directed_arrival");
    assert_eq!(*block, coordinate["block_number"].as_i64().unwrap());
    assert_eq!(*index, coordinate["position"]["index"].as_i64().unwrap());
    assert_eq!(detail["recipient"], word_hex(&ALICE_ACCOUNT));
    assert_eq!(detail["refund"], REFUND.to_checksum(None));
    assert_eq!(detail["directing"], directed.to_checksum(None));

    // A requester naming the coordinate binds nothing (AC-A6.2).
    let (status, body) = bed
        .http(
            "POST",
            "/v0/deposits",
            Some(&Bed::deposit_body("req-directed", coordinate.clone())),
        )
        .await;
    crate::assert_source_not_found(status, &body);

    // The core makes the deposit with no request and mints it to the recipient (AC-A6.1).
    bed.until("the directed deposit is completed", 12, 0, || async {
        directed_transfers(&bed, "deposit")
            .await
            .first()
            .is_some_and(|(_, s, _)| s == "Completed")
    })
    .await;
    let (deposit, _, _) = directed_transfers(&bed, "deposit").await[0].clone();
    assert_eq!(
        bed.count(&format!(
            "SELECT count(*) FROM intake_answers WHERE transfer_id = {deposit}"
        ))
        .await,
        0,
        "a directed deposit has no request"
    );
    let (destination, refund_destination): (String, String) =
        sqlx::query_as("SELECT destination, refund_destination FROM transfers WHERE id = $1")
            .bind(deposit)
            .fetch_one(&bed.admin)
            .await
            .unwrap();
    assert!(destination.eq_ignore_ascii_case(&word_hex(&ALICE_ACCOUNT)));
    assert!(refund_destination.eq_ignore_ascii_case(&format!("{REFUND:#x}")));
    for _ in 0..3 {
        bed.turn(0).await;
    }
    assert_eq!(minted(&bed).await, vec![(ALICE_ACCOUNT, DIRECTED)]);
    assert_eq!(bed.submitted_once().await, 1);
    assert_eq!(bed.kreivo_supply().await, DIRECTED);
    bed.assert_balanced(DIRECTED, DIRECTED).await;

    // The recipient redeems; the core pays out from the vault on the fork.
    bed.redeem([0x71; 32], WITHDRAWAL, BENEFICIARY);
    bed.kreivo_blocks(1).await;
    bed.until("the withdrawal is completed", 20, 65, || async {
        bed.maybe_transfer("withdrawal")
            .await
            .is_some_and(|(_, state)| state == "Completed")
    })
    .await;
    assert_eq!(
        bed.anvil.usdc_balance(BENEFICIARY).await,
        U256::from(WITHDRAWAL)
    );
    for _ in 0..3 {
        bed.turn(65).await;
    }
    bed.assert_balanced(DIRECTED - WITHDRAWAL, DIRECTED - WITHDRAWAL)
        .await;

    // A return (AC-A6.4): a recipient bezent refuses, by the fallback path, refunding to REFUND.
    refuse_bob(&bed).await;
    let receipt = direct_with_approval(&bed, PLAIN_USER, DIRECTED, BOB_ACCOUNT, REFUND).await;
    directed_coordinate(&bed, &receipt, PLAIN_USER, DIRECTED, BOB_ACCOUNT, REFUND);
    assert_eq!(bed.reserve_balance().await, 2 * DIRECTED - WITHDRAWAL);
    assert_zero_divergence(&bed, "the second arrival not final").await;
    bed.anvil.finalize().await;
    bed.until("the return is completed", 20, 65, || async {
        directed_transfers(&bed, "refund")
            .await
            .first()
            .is_some_and(|(_, s, _)| s == "Completed")
    })
    .await;
    for _ in 0..3 {
        bed.turn(65).await;
    }
    let returns = directed_transfers(&bed, "refund").await;
    assert_eq!(returns.len(), 1, "returned more than once: {returns:?}");
    let why: String = sqlx::query_scalar(
        "SELECT payload ->> 'reason' FROM event_log WHERE kind = 'directed-return' AND transfer_id = $1",
    )
    .bind(returns[0].0)
    .fetch_one(&bed.admin)
    .await
    .unwrap();
    assert_eq!(why, "recipient-refused");
    assert_eq!(bed.anvil.usdc_balance(REFUND).await, U256::from(DIRECTED));
    assert_eq!(
        directed_transfers(&bed, "deposit").await.len(),
        1,
        "the refused recipient's arrival became a deposit"
    );
    assert_eq!(minted(&bed).await, vec![(ALICE_ACCOUNT, DIRECTED)]);
    bed.assert_balanced(DIRECTED - WITHDRAWAL, DIRECTED - WITHDRAWAL)
        .await;

    // The directed-returns page (deploy/runbooks/pages/directed-returns.md, its exercise): with the
    // threshold at 0, the one return pages, once.
    let zero = Thresholds {
        returns_per_day: 0,
        ..Thresholds::default()
    };
    let first = directed_alerts::check(&bed.core_pool, &zero).await.unwrap();
    assert!(
        first.iter().any(
            |o| matches!(o, Outcome::Paged { kind, value: 1, repeat: false, .. }
            if *kind == directed_alerts::RETURNS)
        ),
        "{first:?}"
    );
    directed_alerts::check(&bed.core_pool, &zero).await.unwrap();
    assert_eq!(
        bed.count("SELECT count(*) FROM event_log WHERE kind = 'directed-returns-alert'")
            .await,
        1
    );
}

/// `T-018-19`, a return that fails (`deploy/runbooks/procedures/directed-return-failed.md`;
/// `REQ-RF-1`, `REQ-RF-2`, `REQ-RV-1`, `REQ-RV-2`, `REQ-GV-5`, `INV-1`, `INV-8`): a directed deposit
/// that can never be minted (its recipient is refused) names a refund address the token will not pay
/// (USDC-blacklisted). The core orders its return; every preparation of the payout is refused by the
/// dry run, so nothing is signed or sent; the refusals page once; at the deadline the return goes to
/// review and nothing else happens: the value stays in the vault, the reserve check at zero
/// divergence, and no other address is ever paid. The runbook is then followed: the cause is
/// confirmed on chain; once the token's issuer lifts the blacklist, two operators resolve the review
/// into a new attempt, and the return is paid, once, to the same refund address.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s8_req_rf_2_a_return_that_fails_waits_in_review_and_is_paid_once_to_its_refund_address(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let bed = Bed::directed(pool, connect).await;
    refuse_bob(&bed).await;
    set_blacklisted(&bed, REFUND_BLOCKED, true).await;
    let receipt =
        direct_with_approval(&bed, PLAIN_USER, DIRECTED, BOB_ACCOUNT, REFUND_BLOCKED).await;
    directed_coordinate(
        &bed,
        &receipt,
        PLAIN_USER,
        DIRECTED,
        BOB_ACCOUNT,
        REFUND_BLOCKED,
    );
    bed.anvil.finalize().await;

    // The return is ordered, and its payout's dry run is refused, again and again: nothing signed.
    bed.until("the refusals page", 30, 0, || async {
        bed.count("SELECT count(*) FROM event_log WHERE kind = 'payout-refusal-alert'")
            .await
            > 0
    })
    .await;
    let returns = directed_transfers(&bed, "refund").await;
    assert_eq!(returns.len(), 1, "{returns:?}");
    let id = returns[0].0;
    assert_eq!(
        bed.count(
            "SELECT count(*) FROM signed_transactions s JOIN intents i ON i.id = s.intent_id \
             WHERE i.network = 'polygon'"
        )
        .await,
        0,
        "a payout the dry run refused was signed"
    );
    let reason: String = sqlx::query_scalar(
        "SELECT dry_run ->> 'reason' FROM prepared_transactions WHERE network = 'polygon' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&bed.admin)
    .await
    .unwrap();
    // What an operator sees: the vault's Roles modifier runs the payout with `shouldRevert`, so the
    // token's own revert ("Blacklistable: account is blacklisted") is not in it; the modifier's
    // `ModuleTransactionFailed()` is. The runbook reads the cause from the token itself.
    assert!(
        reason.contains("0xd27b44a9"),
        "the dry run's reason is not the Roles modifier's ModuleTransactionFailed(): {reason}"
    );
    // Once per attempt, however many refusals follow.
    for _ in 0..3 {
        bed.turn(0).await;
    }
    assert_eq!(
        bed.count("SELECT count(*) FROM event_log WHERE kind = 'payout-refusal-alert'")
            .await,
        1
    );
    assert_eq!(bed.reserve_balance().await, DIRECTED);
    assert_eq!(bed.anvil.usdc_balance(REFUND_BLOCKED).await, U256::ZERO);
    assert_zero_divergence(&bed, "the return refused").await;

    // Its deadline passes on the destination's final head: review, never a refund elsewhere and
    // never a mint (REQ-RV-1, REQ-RF-1). The clock starts at the first final head the core sees
    // after the return was ordered (§5.4), so one is made final first; then the fork's time moves
    // past the deadline (the bed's `default_deadline`, an hour: a refund has no route's budget).
    bed.anvil.finalize().await;
    bed.turn(0).await;
    bed.anvil.rpc("evm_increaseTime", json!([10_000])).await;
    bed.anvil.finalize().await;
    bed.until("the return is in review", 12, 0, || async {
        bed.state_of(id).await.0 == "Needs-Review"
    })
    .await;
    let before: String =
        sqlx::query_scalar("SELECT state_before_review::text FROM reviews WHERE transfer_id = $1")
            .bind(id)
            .fetch_one(&bed.admin)
            .await
            .unwrap();
    for _ in 0..3 {
        bed.turn(0).await;
    }
    assert_eq!(bed.state_of(id).await.0, "Needs-Review");
    assert_eq!(bed.reserve_balance().await, DIRECTED);
    assert!(bed.chain.submitted().is_empty(), "something was minted");
    assert_eq!(directed_transfers(&bed, "refund").await.len(), 1);
    assert_zero_divergence(&bed, "the return in review").await;

    // The runbook. Confirm: the refund address is blacklisted by the token, on chain.
    assert!(is_blacklisted(&bed, REFUND_BLOCKED).await);
    // The token's issuer lifts it (outside bezent). Two operators resolve the review into a new
    // attempt, on a final head of the destination network; one alone moves nothing (REQ-GV-5).
    set_blacklisted(&bed, REFUND_BLOCKED, false).await;
    bed.anvil.finalize().await;
    bed.evm_pass().await;
    let (number, hash, time) = final_polygon_head(&bed).await;
    let action: i64 = sqlx::query_scalar(
        "INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, \
             block_hash, block_time, justification) \
         VALUES ('review-resolution', $1, 'Delivering', 'polygon', $2, $3, $4, \
                 'the refund address is no longer blacklisted by the token') RETURNING id",
    )
    .bind(id)
    .bind(number)
    .bind(&hash)
    .bind(time)
    .fetch_one(&bed.ops[0])
    .await
    .unwrap();
    for _ in 0..2 {
        bed.turn(0).await;
    }
    assert_eq!(
        bed.state_of(id).await.0,
        "Needs-Review",
        "REQ-GV-5: one operator moved value"
    );
    assert_eq!(bed.anvil.usdc_balance(REFUND_BLOCKED).await, U256::ZERO);
    sqlx::query(
        "INSERT INTO manual_approvals (action_id, justification) \
         VALUES ($1, 'checked isBlacklisted on the token: false')",
    )
    .bind(action)
    .execute(&bed.ops[1])
    .await
    .unwrap();
    bed.until("the return is paid", 20, 65, || async {
        bed.state_of(id).await.0 == "Completed"
    })
    .await;
    for _ in 0..3 {
        bed.turn(65).await;
    }
    assert_eq!(before, "Source-Final");
    assert_eq!(
        bed.anvil.usdc_balance(REFUND_BLOCKED).await,
        U256::from(DIRECTED),
        "paid other than once, in full, to the refund address"
    );
    assert_eq!(bed.reserve_balance().await, 0);
    assert_eq!(
        bed.count("SELECT count(*) FROM observations WHERE detail ->> 'event' = 'Payout'")
            .await,
        1
    );
    assert!(bed.chain.submitted().is_empty());
    bed.assert_balanced(0, 0).await;
}

/// `T-018-19`, spam at the minimum (`deploy/runbooks/procedures/directed-spam.md`,
/// `pages/directed-mints.md`; amendment 0029 Open 6, ruled: alerts only, no count cap; `REQ-IC-2`,
/// `AC-A6.3`) and an unpaired direction (`pages/unpaired-direction.md`; `AC-A6.6`). One sender makes
/// many directed deposits at the contract's minimum: every one is minted, nothing is refused,
/// returned or held, and the mints page once on crossing the threshold and not again while it
/// stands. The runbook's lever is followed: an operator pauses the deposit route, the next one waits
/// in `CND-Paused` and mints nothing, two operators lift it and it mints; the threshold raised, the
/// alert clears. Then the declared contract's code is replaced on the fork by one that emits a
/// `Directed` log with no transfer (a fault of the declared code, as the runbook's exercise asks):
/// the endpoint records one `UnpairedDirection` at the log, nothing binds or mints it, and it pages
/// once. The reserve check stays at zero divergence throughout.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s9_ac_a6_6_spam_at_the_minimum_only_pages_and_an_unpaired_direction_pages_once(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    const SPAM: usize = 5;
    let bed = Bed::directed(pool, connect).await;
    let key = user_key();
    for _ in 0..SPAM {
        direct_with_permit(&bed, &key, DIRECTED_MINIMUM, ALICE_ACCOUNT, REFUND).await;
    }
    bed.anvil.finalize().await;
    bed.until("every deposit at the minimum is minted", 20, 0, || async {
        let d = directed_transfers(&bed, "deposit").await;
        d.len() == SPAM && d.iter().all(|(_, s, _)| s == "Completed")
    })
    .await;
    for _ in 0..2 {
        bed.turn(0).await;
    }
    let spam = (SPAM as u128) * DIRECTED_MINIMUM;
    assert_eq!(minted(&bed).await.len(), SPAM);
    assert!(directed_transfers(&bed, "refund").await.is_empty());
    bed.assert_balanced(spam, spam).await;

    // The page: once on crossing, then standing; nothing capped.
    let low = Thresholds {
        mints_per_day: 3,
        ..Thresholds::default()
    };
    let first = directed_alerts::check(&bed.core_pool, &low).await.unwrap();
    assert!(
        first.iter().any(
            |o| matches!(o, Outcome::Paged { kind, value, repeat: false, .. }
            if *kind == directed_alerts::MINTS && *value == SPAM as u128)
        ),
        "{first:?}"
    );
    let again = directed_alerts::check(&bed.core_pool, &low).await.unwrap();
    assert!(
        again.iter().any(
            |o| matches!(o, Outcome::Standing { kind, .. } if *kind == directed_alerts::MINTS)
        ),
        "{again:?}"
    );
    assert_eq!(
        bed.count("SELECT count(*) FROM event_log WHERE kind = 'directed-mints-alert'")
            .await,
        1
    );

    // The runbook's lever: pause the deposit route. The next deposit waits, minting nothing.
    let pause: i64 = sqlx::query_scalar(
        "SELECT operator_pause($1, $2, 'deposit', 'spam at the minimum: the float cannot keep up')",
    )
    .bind(ASSET)
    .bind(POLYGON)
    .fetch_one(&bed.ops[0])
    .await
    .unwrap();
    direct_with_permit(&bed, &key, DIRECTED_MINIMUM, ALICE_ACCOUNT, REFUND).await;
    bed.anvil.finalize().await;
    bed.until("the deposit under the pause is made", 8, 0, || async {
        directed_transfers(&bed, "deposit").await.len() == SPAM + 1
    })
    .await;
    for _ in 0..3 {
        bed.turn(0).await;
    }
    let (held, state, condition) = directed_transfers(&bed, "deposit").await[SPAM].clone();
    assert_eq!(
        (state.as_str(), condition.as_deref()),
        ("Source-Final", Some("CND-Paused"))
    );
    assert_eq!(minted(&bed).await.len(), SPAM, "minted under the pause");
    assert!(directed_transfers(&bed, "refund").await.is_empty());
    assert_zero_divergence(&bed, "a deposit under the pause").await;
    let request: i64 =
        sqlx::query_scalar("SELECT operator_request_unpause($1, 'the float is topped up')")
            .bind(pause)
            .fetch_one(&bed.ops[0])
            .await
            .unwrap();
    sqlx::query_scalar::<_, i64>("SELECT operator_approve_unpause($1, 'checked the float')")
        .bind(request)
        .fetch_one(&bed.ops[1])
        .await
        .unwrap();
    bed.until("the held deposit is minted", 12, 0, || async {
        bed.state_of(held).await.0 == "Completed"
    })
    .await;
    for _ in 0..2 {
        bed.turn(0).await;
    }
    let total = spam + DIRECTED_MINIMUM;
    assert_eq!(minted(&bed).await.len(), SPAM + 1);
    bed.assert_balanced(total, total).await;

    // The threshold raised to the route's real use: the alert clears.
    let raised = Thresholds {
        mints_per_day: 10,
        ..Thresholds::default()
    };
    let cleared = directed_alerts::check(&bed.core_pool, &raised)
        .await
        .unwrap();
    assert!(
        cleared
            .iter()
            .any(|o| matches!(o, Outcome::Cleared { kind, .. } if *kind == directed_alerts::MINTS)),
        "{cleared:?}"
    );

    // An unpaired direction: the declared contract's code, replaced for one call by a fault that
    // emits `Directed` with no transfer, then put back.
    let directed = bed.directed.unwrap();
    let code = bed
        .anvil
        .rpc("eth_getCode", json!([directed, "latest"]))
        .await;
    // Runtime: CALLDATACOPY the data words (calldata 96..160) to memory, then LOG3 with the three
    // topics from calldata 0..96.
    bed.anvil
        .rpc(
            "anvil_setCode",
            json!([directed, "0x6040606060003760403560203560003560406000a300"]),
        )
        .await;
    let sender = address_of(&key);
    let input = [
        DIRECTED_TOPIC.as_slice(),
        sender.into_word().as_slice(),
        &ALICE_ACCOUNT,
        &U256::from(DIRECTED).to_be_bytes::<32>(),
        REFUND.into_word().as_slice(),
    ]
    .concat();
    let receipt = send(&bed, sender, directed, input).await;
    bed.anvil
        .rpc("anvil_setCode", json!([directed, code]))
        .await;
    let logs = receipt["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 1, "{receipt}");
    let unpaired_index = number(&logs[0]["logIndex"]);
    let reserve = bed.reserve_balance().await;
    bed.anvil.finalize().await;
    bed.evm_pass().await;
    let rows: Vec<(i64, i64, i64)> = sqlx::query_as(
        "SELECT id, block_number, position_index FROM observations WHERE kind = 'unpaired_direction'",
    )
    .fetch_all(&bed.admin)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].1 as u64, number(&receipt["blockNumber"]));
    assert_eq!(rows[0].2 as u64, unpaired_index);
    let pages = directed_alerts::check(&bed.core_pool, &raised)
        .await
        .unwrap();
    assert!(
        pages
            .iter()
            .any(|o| matches!(o, Outcome::Observation { kind, id } if *kind == directed_alerts::UNPAIRED && *id == rows[0].0)),
        "{pages:?}"
    );
    directed_alerts::check(&bed.core_pool, &raised)
        .await
        .unwrap();
    assert_eq!(
        bed.count("SELECT count(*) FROM event_log WHERE kind = 'unpaired-direction'")
            .await,
        1
    );
    for _ in 0..3 {
        bed.turn(0).await;
    }
    // It binds nothing and mints nothing.
    assert_eq!(directed_transfers(&bed, "deposit").await.len(), SPAM + 1);
    assert!(directed_transfers(&bed, "refund").await.is_empty());
    assert_eq!(minted(&bed).await.len(), SPAM + 1);
    assert_eq!(bed.reserve_balance().await, reserve);
    bed.assert_balanced(total, total).await;
}
