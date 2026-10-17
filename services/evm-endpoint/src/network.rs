// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Network identity at start-up (`T-009-01`; `AC-I4.1`, `REQ-IC-1`; PLAN `DEC-8`, `DEC-12`).
//!
//! Before anything is read or built, both providers must serve the declared chain id and hold
//! the pinned block at its pinned hash, and they must agree on the finalized head they both
//! stand on. Any mismatch fails closed: the endpoint does not start. What the store declares for
//! the network (the reserve, the admitted tokens) must match the configured vault, or the
//! endpoint does not start either.

use std::collections::BTreeMap;

use alloy::primitives::{Address, B256, Bytes};
use alloy::sol_types::SolCall;
use sqlx::PgPool;

use crate::evm;
use crate::pair::{Pair, Read};
use crate::rpc::{At, Call, Header, Rpc};

/// The declared identity of a network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The chain id.
    pub chain_id: u64,
    /// A block of its history.
    pub pinned_number: u64,
    /// That block's hash.
    pub pinned_hash: B256,
}

/// Checks both providers against `id`, and that they agree at the finalized head; returns that
/// head. Every error names the spec id that refuses.
pub async fn check<R: Rpc>(pair: &Pair<R>, id: &Identity) -> Result<Header, String> {
    for p in [&pair.a, &pair.b] {
        let chain_id = p
            .chain_id()
            .await
            .map_err(|e| format!("AC-I4.1: provider {} cannot be checked: {e}", p.name()))?;
        if chain_id != id.chain_id {
            return Err(format!(
                "AC-I4.1: provider {} serves chain {chain_id}, not the declared {}",
                p.name(),
                id.chain_id
            ));
        }
        let pinned = p
            .header(At::Number(id.pinned_number))
            .await
            .map_err(|e| format!("AC-I4.1: provider {} cannot be checked: {e}", p.name()))?;
        match pinned {
            Some(h) if h.hash == id.pinned_hash => {}
            Some(h) => {
                return Err(format!(
                    "AC-I4.1: provider {}: block {} is {}, not the pinned {}",
                    p.name(),
                    id.pinned_number,
                    h.hash,
                    id.pinned_hash
                ));
            }
            None => {
                return Err(format!(
                    "AC-I4.1: provider {} has no block {}",
                    p.name(),
                    id.pinned_number
                ));
            }
        }
    }
    let head = pair.final_point().await.map_err(|e| match e {
        Read::Disagree(d) => format!("DEC-12: the providers disagree at the finalized head: {d}"),
        Read::Unavailable(u) => format!("AC-I4.1: the finalized head cannot be read: {u}"),
    })?;
    if head.number < id.pinned_number {
        return Err(format!(
            "AC-I4.1: the finalized head {} is below the pinned block {}",
            head.number, id.pinned_number
        ));
    }
    Ok(head)
}

/// Checks the vault's contracts on both providers at the final point `head` (PR #39 review S4,
/// `AC-I4.1`): the configured Roles modifier's avatar and target are the configured vault, the
/// vault has it enabled as a module, and the delivery key holds no code (an EIP-7702 delegation
/// would run code on a cancel, S5). Any mismatch, disagreement or failed read fails closed.
pub async fn check_vault<R: Rpc>(
    pair: &Pair<R>,
    head: &Header,
    roles: Address,
    vault: Address,
    lane: Address,
) -> Result<(), String> {
    let view = |to: Address, data: Vec<u8>| Call {
        from: Address::ZERO,
        to,
        data: data.into(),
    };
    let address = |what: &str, out: Bytes| {
        evm::avatarCall::abi_decode_returns(&out)
            .map_err(|_| format!("AC-I4.1: the Roles modifier {roles}'s {what} does not decode"))
    };
    for (what, data) in [
        ("avatar", evm::avatarCall {}.abi_encode()),
        ("target", evm::targetCall {}.abi_encode()),
    ] {
        let out = pair
            .view_at(&view(roles, data), head.number)
            .await
            .map_err(|e| format!("AC-I4.1: the Roles modifier {roles}'s {what}: {e}"))?;
        let got = address(what, out)?;
        if got != vault {
            return Err(format!(
                "AC-I4.1: the Roles modifier {roles}'s {what} is {got}, not the vault {vault}"
            ));
        }
    }
    let enabled = pair
        .view_at(
            &view(
                vault,
                evm::isModuleEnabledCall { module: roles }.abi_encode(),
            ),
            head.number,
        )
        .await
        .map_err(|e| format!("AC-I4.1: whether the vault enables {roles}: {e}"))?;
    if evm::isModuleEnabledCall::abi_decode_returns(&enabled) != Ok(true) {
        return Err(format!(
            "AC-I4.1: the vault {vault} does not have the Roles modifier {roles} enabled as a module"
        ));
    }
    match pair.has_code(lane).await {
        Ok(false) => Ok(()),
        Ok(true) => Err(format!(
            "AC-I4.1, S5: the delivery key {lane} holds code (an EIP-7702 delegation?)"
        )),
        Err(e) => Err(format!("AC-I4.1: the delivery key's code: {e}")),
    }
}

/// An origin token the store admits on this network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    /// Its address as the store spells it (`admitted_tokens.token`).
    pub text: String,
    /// The asset it is admitted for.
    pub asset: String,
}

/// What the store declares for this network, checked against the configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declarations {
    /// The vault's address as the store spells it (`reserves.address`).
    pub vault_text: String,
    /// The admitted origin tokens, by address.
    pub tokens: BTreeMap<Address, Token>,
}

impl Declarations {
    /// The origin token of `asset` on this network: the one token admitted for it.
    pub fn token_of(&self, asset: &str) -> Option<(Address, &Token)> {
        let mut it = self.tokens.iter().filter(|(_, t)| t.asset == asset);
        let (a, t) = it.next()?;
        it.next().is_none().then_some((*a, t))
    }
}

/// Reads the declarations of `network` and checks them: an origin network with EVM addresses,
/// every reserve on it the configured vault, and the admitted tokens well formed.
pub async fn declarations(
    pool: &PgPool,
    network: &str,
    vault: Address,
) -> Result<Declarations, String> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT role, address_format FROM networks WHERE network = $1")
            .bind(network)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("the store: {e}"))?;
    match row.as_ref().map(|(r, f)| (r.as_str(), f.as_str())) {
        Some(("origin", "evm")) => {}
        other => {
            return Err(format!(
                "AC-I4.1: network {network} is not declared as an EVM origin network ({other:?})"
            ));
        }
    }
    let reserves: Vec<(String, String)> =
        sqlx::query_as("SELECT asset, address FROM reserves WHERE network = $1 ORDER BY asset")
            .bind(network)
            .fetch_all(pool)
            .await
            .map_err(|e| format!("the store: {e}"))?;
    let mut vault_text = None;
    for (asset, address) in &reserves {
        if address.parse::<Address>().ok() != Some(vault) {
            return Err(format!(
                "AC-I4.1: the reserve of {asset} on {network} is {address}, not the configured vault {vault}"
            ));
        }
        vault_text.get_or_insert_with(|| address.clone());
    }
    let vault_text = vault_text.ok_or_else(|| {
        format!("AC-I4.1: no reserve is declared on {network}; the vault watches nothing")
    })?;
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT token, asset FROM admitted_tokens WHERE network = $1 ORDER BY token",
    )
    .bind(network)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("the store: {e}"))?;
    let mut tokens = BTreeMap::new();
    for (text, asset) in rows {
        let address: Address = text
            .parse()
            .map_err(|_| format!("admitted token {text} on {network} is not an address"))?;
        tokens.insert(address, Token { text, asset });
    }
    Ok(Declarations { vault_text, tokens })
}
