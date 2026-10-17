// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! T-009-01: the network's identity at start-up, on both providers (`AC-I4.1`, `REQ-IC-1`;
//! PLAN `DEC-8`, `DEC-12`). Any mismatch fails closed.

mod common;

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use bezent_evm_endpoint::evm;
use bezent_evm_endpoint::network::{self, Identity};
use bezent_evm_endpoint::pair::Pair;
use bezent_evm_endpoint::rpc::mock::{MockChain, MockRpc};
use common::{CHAIN_ID, NETWORK, vault};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

fn identity(chain: &MockChain, pinned: u64) -> Identity {
    Identity {
        chain_id: CHAIN_ID,
        pinned_number: pinned,
        pinned_hash: chain.header_at(pinned).hash,
    }
}

fn pair(a: &MockChain, b: &MockChain) -> Pair<MockRpc> {
    Pair {
        a: MockRpc::new("a", a),
        b: MockRpc::new("b", b),
    }
}

#[tokio::test]
async fn ac_i4_1_the_declared_network_on_both_providers_starts() {
    let chain = MockChain::new(CHAIN_ID, 20);
    let head = network::check(&pair(&chain, &chain), &identity(&chain, 5))
        .await
        .unwrap();
    assert_eq!(head.number, 19);
}

#[tokio::test]
async fn ac_i4_1_another_chain_id_is_refused() {
    let chain = MockChain::new(CHAIN_ID, 20);
    let other = MockChain::new(8453, 20);
    let e = network::check(&pair(&chain, &other), &identity(&chain, 5))
        .await
        .unwrap_err();
    assert!(e.contains("AC-I4.1") && e.contains("8453"), "{e}");
}

#[tokio::test]
async fn ac_i4_1_another_pinned_hash_is_refused() {
    let chain = MockChain::new(CHAIN_ID, 20);
    let mut id = identity(&chain, 5);
    id.pinned_hash = chain.header_at(6).hash;
    let e = network::check(&pair(&chain, &chain), &id)
        .await
        .unwrap_err();
    assert!(e.contains("AC-I4.1") && e.contains("not the pinned"), "{e}");
    // A provider whose history at the pinned height is another one.
    let forked = chain.fork();
    forked.replace(5);
    let e = network::check(&pair(&chain, &forked), &identity(&chain, 5))
        .await
        .unwrap_err();
    assert!(e.contains("provider b"), "{e}");
}

#[tokio::test]
async fn dec_12_providers_that_disagree_at_the_finalized_head_are_refused() {
    let chain = MockChain::new(CHAIN_ID, 20);
    let forked = chain.fork();
    forked.replace(19);
    let e = network::check(&pair(&chain, &forked), &identity(&chain, 5))
        .await
        .unwrap_err();
    assert!(e.contains("DEC-12"), "{e}");
}

#[tokio::test]
async fn ac_i4_1_a_provider_that_fails_is_refused() {
    let chain = MockChain::new(CHAIN_ID, 20);
    let failing = chain.fork();
    failing.fail_all(Some("connection refused"));
    let e = network::check(&pair(&chain, &failing), &identity(&chain, 5))
        .await
        .unwrap_err();
    assert!(e.contains("cannot be checked"), "{e}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i4_1_the_store_must_declare_the_configured_vault(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let admin = pool.connect_with(connect).await.unwrap();
    let e = network::declarations(&admin, NETWORK, vault())
        .await
        .unwrap_err();
    assert!(e.contains("AC-I4.1"), "{e}");
    common::declare(&admin).await;
    let d = network::declarations(&admin, NETWORK, vault())
        .await
        .unwrap();
    assert_eq!(d.vault_text, common::vault_text());
    assert_eq!(d.token_of(common::ASSET).unwrap().0, common::usdc());
    let e = network::declarations(&admin, NETWORK, common::beneficiary())
        .await
        .unwrap_err();
    assert!(e.contains("not the configured vault"), "{e}");
    let e = network::declarations(&admin, common::ISSUING, vault())
        .await
        .unwrap_err();
    assert!(e.contains("not declared as an EVM origin network"), "{e}");
}

/// The delivery key's address in these checks.
fn lane() -> Address {
    Address::repeat_byte(0x44)
}

/// A chain where the Roles modifier answers `avatar` and `target` and the vault answers whether
/// it enables it, as F-008's deployment leaves them.
fn vault_chain(avatar: Address, target: Address, enabled: bool) -> MockChain {
    let chain = MockChain::new(CHAIN_ID, 20);
    let roles = common::contracts().roles;
    chain.set_view(
        roles,
        evm::avatarCall {}.abi_encode(),
        avatar.into_word().to_vec(),
    );
    chain.set_view(
        roles,
        evm::targetCall {}.abi_encode(),
        target.into_word().to_vec(),
    );
    chain.set_view(
        vault(),
        evm::isModuleEnabledCall { module: roles }.abi_encode(),
        U256::from(u8::from(enabled)).to_be_bytes::<32>().to_vec(),
    );
    chain
}

async fn check_vault(a: &MockChain, b: &MockChain) -> Result<(), String> {
    let p = pair(a, b);
    let head = network::check(&p, &identity(a, 5)).await?;
    network::check_vault(&p, &head, common::contracts().roles, vault(), lane()).await
}

/// PR #39 review S4 (`AC-I4.1`): at start-up, on both providers, the configured Roles modifier's
/// avatar and target are this vault and the vault enables it as a module; otherwise the endpoint
/// fails closed. And (S5) a delivery key that holds code is refused.
#[tokio::test]
async fn s4_ac_i4_1_the_roles_modifier_must_be_this_vaults_on_both_providers() {
    let good = vault_chain(vault(), vault(), true);
    check_vault(&good, &good).await.unwrap();

    let other = Address::repeat_byte(0x77);
    for (chain, words) in [
        (vault_chain(other, vault(), true), "avatar"),
        (vault_chain(vault(), other, true), "target"),
        (vault_chain(vault(), vault(), false), "enabled as a module"),
    ] {
        let e = check_vault(&chain, &chain).await.unwrap_err();
        assert!(e.contains("AC-I4.1") && e.contains(words), "{e}");
    }
    // One provider that answers otherwise: the providers disagree, nothing starts.
    let lying = good.fork();
    lying.set_view(
        common::contracts().roles,
        evm::avatarCall {}.abi_encode(),
        other.into_word().to_vec(),
    );
    let e = check_vault(&good, &lying).await.unwrap_err();
    assert!(e.contains("DEC-12"), "{e}");
    // A Roles address with no such function (the call answers nothing): refused.
    let empty = MockChain::new(CHAIN_ID, 20);
    assert!(check_vault(&empty, &empty).await.is_err());
    // S5: a delivery key that holds code (an EIP-7702 delegation) is refused at start-up.
    let delegated = vault_chain(vault(), vault(), true);
    let mut code = vec![0xef, 0x01, 0x00];
    code.extend_from_slice(other.as_slice());
    delegated.set_code(lane(), code);
    let e = check_vault(&delegated, &delegated).await.unwrap_err();
    assert!(e.contains("holds code"), "{e}");
}
