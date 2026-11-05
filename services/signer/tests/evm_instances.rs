// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `M-2` (issue #30; PLAN `DEC-8`): one evm-endpoint instance per network, and the signer's
//! per-caller allow list stays exact per instance (`REQ-AR-3`). The central signer's fixture is
//! extended in memory with Base Sepolia and BNB Chain testnet, each with its own caller (its own
//! certificate name), its own key and its own binding; no code of the signer changes for a network.
//!
//! No database, no key, no network.

use bezent_signer::config::Config;
use bezent_signer::link::Callers;
use serde_json::{Value, json};

const CENTRAL: &str = include_str!("../fixtures/signer-central.json");

/// The two token hashes the instances present are distinct here, so a token cannot stand in for
/// another instance's even with the right name.
const TOKEN_BASE: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const TOKEN_BSC: &str = "2222222222222222222222222222222222222222222222222222222222222222";

fn evm_caller(name: &str, token: &str, network: &str) -> Value {
    json!({
        "name": name,
        "dns_name": format!("{name}.link.bezent.prod.bloque.network"),
        "token_sha256": [token],
        "allow": [{ "network": network, "purposes": ["payout", "cancel"] }]
    })
}

fn evm_network(id: &str, chain_id: u64, roles: &str, emitter: &str) -> Value {
    json!({
        "id": id,
        "environment": "production",
        "chain_id": chain_id,
        "cancel_max_fee_per_gas": "1000000000000",
        "delivery": {
            "roles": roles,
            "multisend": "0x9641d764fc13c8b624c04430c7356c1c7c8102e2",
            "emitter": emitter,
            "role": "bezent.delivery"
        }
    })
}

/// The central fixture with Base Sepolia and BNB Chain testnet beside Polygon.
fn three_instances() -> Value {
    let mut j: Value = serde_json::from_str(CENTRAL).unwrap();
    let callers = j["callers"].as_array_mut().unwrap();
    callers.push(evm_caller("evm-endpoint-base", TOKEN_BASE, "base-sepolia"));
    callers.push(evm_caller("evm-endpoint-bsc", TOKEN_BSC, "bsc-testnet"));
    let networks = j["networks"].as_array_mut().unwrap();
    networks.push(evm_network(
        "base-sepolia",
        84532,
        "0xb2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2",
        "0xb3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3",
    ));
    networks.push(evm_network(
        "bsc-testnet",
        97,
        "0xc2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2",
        "0xc3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3",
    ));
    let keys = j["keys"]["engines"][1]["keys"].as_array_mut().unwrap();
    keys.push(json!({
        "id": "base-sepolia-delivery",
        "key": "bezent-base-sepolia-delivery",
        "address": "0x3333333333333333333333333333333333333333",
        "public_key": "0x023333333333333333333333333333333333333333333333333333333333333333"
    }));
    keys.push(json!({
        "id": "bsc-testnet-delivery",
        "key": "bezent-bsc-testnet-delivery",
        "address": "0x4444444444444444444444444444444444444444",
        "public_key": "0x024444444444444444444444444444444444444444444444444444444444444444"
    }));
    // A payout intent's amount is in the origin token's minor units, so a window cap is too: 1 BTCB
    // is 10^18 (18 decimals), not 10^8 (bBTC's 8). 1 EURC is 10^6.
    let bindings = j["bindings"].as_array_mut().unwrap();
    bindings.push(json!({
        "key": "base-sepolia-delivery",
        "network": "base-sepolia",
        "purposes": ["payout", "cancel"],
        "window_cap": "1000000000",
        "window_seconds": 86400
    }));
    bindings.push(json!({
        "key": "bsc-testnet-delivery",
        "network": "bsc-testnet",
        "purposes": ["payout", "cancel"],
        "window_cap": "1000000000000000000",
        "window_seconds": 86400
    }));
    j
}

fn parse(j: &Value) -> Result<Config, String> {
    Config::parse(j.to_string().as_bytes()).map_err(|e| e.to_string())
}

#[test]
fn dec_8_a_new_evm_network_is_configuration_only() {
    let config = parse(&three_instances()).unwrap();
    assert_eq!(config.callers.len(), 5);
    assert_eq!(config.networks.len(), 4);
    // Each instance is a caller of its own, recognised by its own certificate name.
    let mut names: Vec<_> = config
        .callers
        .iter()
        .filter_map(|c| c.dns_name.clone())
        .collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 5);
}

#[test]
fn req_ar_3_each_instance_is_allowed_its_own_network_and_no_other() {
    let config = parse(&three_instances()).unwrap();
    let callers = Callers::new(&config.callers, 4_000_000_000).unwrap();
    let instances = [
        ("evm-endpoint", "polygon"),
        ("evm-endpoint-base", "base-sepolia"),
        ("evm-endpoint-bsc", "bsc-testnet"),
    ];
    for (caller, own) in instances {
        for (_, network) in instances {
            for purpose in ["payout", "cancel"] {
                assert_eq!(
                    callers.permits(caller, network, purpose),
                    network == own,
                    "{caller} {purpose} on {network}"
                );
            }
            // No instance asks for a mint, a refund mint or a statement, on any network.
            for purpose in ["mint", "refund-mint", "statement"] {
                assert!(
                    !callers.permits(caller, network, purpose),
                    "{caller} {purpose}"
                );
            }
        }
        assert!(!callers.permits(caller, "kreivo", "payout"));
    }
    // And nobody else asks for a payout on the new networks.
    for caller in ["core", "kreivo-endpoint"] {
        for network in ["base-sepolia", "bsc-testnet"] {
            assert!(!callers.permits(caller, network, "payout"), "{caller}");
        }
    }
}

#[test]
fn req_ar_3_no_evm_instance_holds_a_wildcard_network() {
    let config = parse(&three_instances()).unwrap();
    for caller in config
        .callers
        .iter()
        .filter(|c| c.name.starts_with("evm-endpoint"))
    {
        let allow = caller.allow.as_ref().unwrap();
        assert_eq!(allow.len(), 1, "{}", caller.name);
        assert_ne!(allow[0].network, "*", "{}", caller.name);
    }
}

#[test]
fn req_ar_3_an_instance_is_recognised_by_its_own_name_and_its_own_token() {
    // The name and the token must both be the instance's (a certificate name alone, or a token
    // alone, is not enough).
    use sha2::{Digest, Sha256};
    let mut j = three_instances();
    let hash = |t: &str| hex::encode(Sha256::digest(t.as_bytes()));
    j["callers"][3]["token_sha256"] = json!([hash("base-token")]);
    j["callers"][4]["token_sha256"] = json!([hash("bsc-token")]);
    let config = parse(&j).unwrap();
    let callers = Callers::new(&config.callers, 4_000_000_000).unwrap();
    let base = "evm-endpoint-base.link.bezent.prod.bloque.network";
    let bsc = "evm-endpoint-bsc.link.bezent.prod.bloque.network";
    assert_eq!(
        callers.authenticate_dns(Some(base), b"base-token"),
        Some("evm-endpoint-base")
    );
    assert_eq!(callers.authenticate_dns(Some(base), b"bsc-token"), None);
    assert_eq!(callers.authenticate_dns(Some(bsc), b"base-token"), None);
    assert_eq!(
        callers.authenticate_dns(Some(bsc), b"bsc-token"),
        Some("evm-endpoint-bsc")
    );
}
