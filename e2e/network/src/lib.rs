// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! bezent's zombienet test bed (F-013, plan `DEC-19`): a relay chain and Kreivo, with the bridged
//! assets in genesis at their ruled ids (`DEC-7`).
//!
//! Forked from aledgent's `e2e/network` (the same zombienet-sdk, collator flags and release
//! artifacts). What differs:
//!
//! - **Kreivo 0.17.1** (spec 134), the first runtime that lets a contract mint and burn a local
//!   asset its community administers (kreivo#512). aledgent's CI pins 0.17.0.
//! - **One Kreivo node**, the collator. aledgent's pruned full node serves its fault cases; bezent
//!   adds nodes when its own fault cases need them.
//! - **The six assets** of `DEC-7` in the genesis override, instead of aledgent's three test assets.
//!
//! Kreivo's genesis comes from the released runtime's `development` preset (balances, collators and
//! session keys; no communities and no assets; see aledgent's notes on why not `local`). Assets
//! cannot be created by a signed origin on Kreivo (only Root creates them), so they are added here as
//! a genesis override on a key the preset does not have. The dedicated community is registered after
//! spawn, as aledgent registers its own (`e2e/kreivo/setup.mjs`); its account is derived from its id,
//! so genesis can already name it as Issuer and Admin of `Here(101)`–`Here(103)` (`REQ-HN-10`), which
//! is where the Root referendum puts it on mainnet.

use zombienet_sdk::subxt::utils::AccountId32;
use zombienet_sdk::{NetworkConfig, NetworkConfigBuilder, RegistrationStrategy};

/// Kreivo's para id, as its presets hard-code it.
pub const PARA_ID: u32 = 2281;
/// The Kreivo collator. Everything submits through it.
pub const COLLATOR: &str = "kreivo01";

/// The dedicated community's id on the test bed. Its account is Issuer and Admin of the three
/// bridged assets bezent mints (`REQ-HN-10`). The mainnet id is not ruled; this one is the test
/// bed's own (aledgent's test community is 4242).
pub const COMMUNITY_ID: u16 = 4243;

/// What Alice holds of each asset she owns at genesis.
pub const GENESIS_ASSET_BALANCE: u128 = 100_000_000_000;

/// Who holds an asset's roles at genesis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Roles {
    /// The dedicated community's account: owner, Issuer, Admin and Freezer.
    Community,
    /// Alice (dev chains only): a stand-in for the account that holds the roles on mainnet.
    Alice,
}

/// One asset of the genesis override.
#[derive(Clone, Copy, Debug)]
pub struct Asset {
    pub id: u32,
    pub name: &'static str,
    pub symbol: &'static str,
    pub decimals: u8,
    pub is_sufficient: bool,
    pub min_balance: u128,
    pub roles: Roles,
}

/// The six assets (`DEC-7`, ruled `pl-asset-ids` 2026-10-13).
///
/// - `Here(101)`–`Here(103)`: bUSDC, bEURC, bBTC. Metadata, sufficiency and minimum balance as
///   Kreivo mainnet carries them (read 2026-10-13 with `e2e/kreivo/mainnet-metadata.mjs`): "not
///   sufficient", minimum 1. Roles with the dedicated community. No supply: the contract mints.
/// - `Here(100)` COPM, `Here(104)` MXNB, `Here(105)` BRLB: metadata copied from mainnet. COPM from
///   `Here(100)`; MXNB and BRLB from `Here(101)` and `Here(102)` before the referendum gave those ids
///   to bUSDC and bEURC (mainnet has no `Here(104)` or `Here(105)` yet). Roles with Alice, standing in
///   for Bloque's issuer account; COPM's moves to the community at `M-3`.
pub const ASSETS: [Asset; 6] = [
    Asset { id: 100, name: "COPM/2", symbol: "COPM", decimals: 2, is_sufficient: true, min_balance: 1, roles: Roles::Alice },
    Asset { id: 101, name: "Bloque USDC", symbol: "bUSDC", decimals: 6, is_sufficient: false, min_balance: 1, roles: Roles::Community },
    Asset { id: 102, name: "Bloque EURC", symbol: "bEURC", decimals: 6, is_sufficient: false, min_balance: 1, roles: Roles::Community },
    Asset { id: 103, name: "Bloque BTC", symbol: "bBTC", decimals: 8, is_sufficient: false, min_balance: 1, roles: Roles::Community },
    Asset { id: 104, name: "MXNB/2", symbol: "MXNB", decimals: 2, is_sufficient: false, min_balance: 1, roles: Roles::Alice },
    Asset { id: 105, name: "BRLB/2", symbol: "BRLB", decimals: 2, is_sufficient: false, min_balance: 1, roles: Roles::Alice },
];

/// An environment variable, or a default.
pub fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

/// Alice's account, SS58 with the generic prefix, which Kreivo's genesis builder accepts.
pub fn alice() -> String {
    zombienet_sdk::subxt_signer::sr25519::dev::alice()
        .public_key()
        .to_account_id()
        .to_string()
}

/// Kreivo's `Communities::community_account(id)`:
/// `PalletId(*b"kv/cmtys").into_sub_account_truncating(id)`, i.e. `b"modl" ++ b"kv/cmtys" ++ id (u16
/// LE)`, zero-padded to 32 bytes. SS58 with the generic prefix.
pub fn community_account(id: u16) -> String {
    let mut raw = [0u8; 32];
    raw[..4].copy_from_slice(b"modl");
    raw[4..12].copy_from_slice(b"kv/cmtys");
    raw[12..14].copy_from_slice(&id.to_le_bytes());
    AccountId32::from(raw).to_string()
}

/// The genesis override for Kreivo: the six assets with their metadata and roles; Alice holds a
/// balance of the three she owns. The three bridged assets start with no supply.
pub fn kreivo_genesis_overrides() -> serde_json::Value {
    let alice = alice();
    let community = community_account(COMMUNITY_ID);
    let owner = |a: &Asset| match a.roles {
        Roles::Community => community.clone(),
        Roles::Alice => alice.clone(),
    };
    let assets: Vec<_> = ASSETS
        .iter()
        .map(|a| serde_json::json!([{ "Here": a.id }, owner(a), a.is_sufficient, a.min_balance]))
        .collect();
    let metadata: Vec<_> = ASSETS
        .iter()
        .map(|a| {
            serde_json::json!([
                { "Here": a.id },
                a.name.as_bytes().to_vec(),
                a.symbol.as_bytes().to_vec(),
                a.decimals
            ])
        })
        .collect();
    let accounts: Vec<_> = ASSETS
        .iter()
        .filter(|a| a.roles == Roles::Alice)
        .map(|a| serde_json::json!([{ "Here": a.id }, alice.clone(), GENESIS_ASSET_BALANCE]))
        .collect();
    serde_json::json!({
        "assets": { "assets": assets, "metadata": metadata, "accounts": accounts }
    })
}

/// The network configuration.
///
/// Inputs, from the environment (`e2e/scripts/prepare.sh` fetches them):
///
/// - `ZOMBIENET_KREIVO_WASM`: the released Kreivo runtime (`kreivo_runtime.compact.compressed.wasm`).
/// - `ZOMBIENET_POLKADOT_BIN`, `ZOMBIENET_POLKADOT_PARACHAIN_BIN`: polkadot-sdk release binaries.
/// - `ZOMBIENET_RELAY_CHAIN` (default `rococo-local`, built into the polkadot binary).
/// - `ZOMBIENET_KREIVO_PRESET` (optional): a preset name for the runtime.
pub fn network_config() -> NetworkConfig {
    let wasm = std::env::var("ZOMBIENET_KREIVO_WASM").expect(
        "ZOMBIENET_KREIVO_WASM must point at a released Kreivo runtime wasm; \
         run e2e/scripts/prepare.sh",
    );
    let preset = std::env::var("ZOMBIENET_KREIVO_PRESET").ok();
    let polkadot_bin = env_or("ZOMBIENET_POLKADOT_BIN", "polkadot");
    let parachain_bin = env_or("ZOMBIENET_POLKADOT_PARACHAIN_BIN", "polkadot-parachain");
    let relay_chain = env_or("ZOMBIENET_RELAY_CHAIN", "rococo-local");

    NetworkConfigBuilder::new()
        .with_relaychain(|r| {
            r.with_chain(relay_chain.as_str())
                .with_default_command(polkadot_bin.as_str())
                .with_validator(|n| n.with_name("alice"))
                .with_validator(|n| n.with_name("bob"))
        })
        .with_parachain(|p| {
            p.with_id(PARA_ID)
                .with_chain("kreivo-local")
                .with_chain_spec_runtime(wasm.as_str(), preset.as_deref())
                .with_genesis_overrides(kreivo_genesis_overrides())
                .cumulus_based(true)
                // Onboarded at relay genesis: substrate-rail found the post-launch registration
                // path left the para without a core and the collator idle at block 0.
                .with_registration_strategy(RegistrationStrategy::InGenesis)
                .with_default_command(parachain_bin.as_str())
                // Kreivo sets `RelayParentOffset = 1`, which needs the slot-based collator: the
                // default lookahead collator supplies no relay-parent descendant proofs and the
                // runtime panics every block with `InvalidNumberOfDescendants`.
                .with_collator(|c| {
                    c.with_name(COLLATOR).with_args(vec![
                        "--force-authoring".into(),
                        ("--authoring", "slot-based").into(),
                        ("--state-pruning", "archive-canonical").into(),
                    ])
                })
        })
        .build()
        .unwrap_or_else(|errs| panic!("invalid zombienet config: {errs:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ruled ids and decimals (`DEC-7`).
    #[test]
    fn assets_at_ruled_ids() {
        let got: Vec<_> = ASSETS.iter().map(|a| (a.id, a.symbol, a.decimals)).collect();
        assert_eq!(
            got,
            vec![
                (100, "COPM", 2),
                (101, "bUSDC", 6),
                (102, "bEURC", 6),
                (103, "bBTC", 8),
                (104, "MXNB", 2),
                (105, "BRLB", 2),
            ]
        );
        for a in &ASSETS {
            assert_eq!(a.roles == Roles::Community, (101..=103).contains(&a.id), "Here({})", a.id);
        }
    }
}
