// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `M-2` (issue #30): one evm-endpoint instance per network (`DEC-8`), each with its own
//! configuration, key lane and signer binding. The three example configurations (Polygon, Base
//! Sepolia, BNB Chain testnet) are checked together: they must share nothing that would let one
//! instance act for another (`REQ-KY-1`, `REQ-AR-3`, `AC-I4.1`). No database, no network.

use std::collections::BTreeSet;

use bezent_evm_endpoint::config::Config;

const FIXTURES: [(&str, &str, u64); 3] = [
    (include_str!("../fixtures/endpoint.toml"), "polygon", 137),
    (
        include_str!("../fixtures/base-sepolia.toml"),
        "base-sepolia",
        84532,
    ),
    (
        include_str!("../fixtures/bsc-testnet.toml"),
        "bsc-testnet",
        97,
    ),
];

fn configs() -> Vec<Config> {
    FIXTURES
        .iter()
        .map(|(text, network, chain_id)| {
            let c = Config::parse(text).unwrap_or_else(|e| panic!("{network}: {e}"));
            assert_eq!(&c.network, network);
            assert_eq!(c.chain_id, *chain_id, "{network}");
            c
        })
        .collect()
}

/// Asserts `values` holds no repeat, one value per instance.
fn all_distinct(what: &str, values: Vec<String>) {
    let set: BTreeSet<&String> = values.iter().collect();
    assert_eq!(set.len(), values.len(), "{what} is shared: {values:?}");
}

#[test]
fn dec_8_every_instance_checks_and_names_its_own_network_and_chain() {
    let c = configs();
    all_distinct("network", c.iter().map(|c| c.network.clone()).collect());
    all_distinct(
        "chain id",
        c.iter().map(|c| c.chain_id.to_string()).collect(),
    );
    all_distinct(
        "pinned hash",
        c.iter().map(|c| c.pinned_hash.clone()).collect(),
    );
}

#[test]
fn dec_8_no_two_instances_share_a_lane_key_or_a_delivery_address() {
    let c = configs();
    all_distinct(
        "lane key id",
        c.iter().map(|c| c.lane.key.clone()).collect(),
    );
    all_distinct(
        "lane public key",
        c.iter().map(|c| c.lane.public_key.clone()).collect(),
    );
    let checked: Vec<_> = c.iter().map(|c| c.check().unwrap()).collect();
    all_distinct(
        "delivery address",
        checked.iter().map(|k| k.lane_address.to_string()).collect(),
    );
    all_distinct(
        "vault",
        c.iter().map(|c| c.vault.to_ascii_lowercase()).collect(),
    );
}

#[test]
fn req_ky_1_no_two_instances_share_a_secret_path() {
    let c = configs();
    let mut paths = Vec::new();
    for c in &c {
        paths.push(c.database_url_file.clone().unwrap().display().to_string());
        paths.push(c.signer.token_file.clone().unwrap().display().to_string());
        for p in &c.providers {
            paths.push(p.url_file.clone().unwrap().display().to_string());
        }
    }
    // Four secrets an instance: its database login, its signer token, its two providers' URLs.
    assert_eq!(paths.len(), 4 * c.len());
    all_distinct("secret path", paths);
}

#[test]
fn amendment_0023_every_instance_has_public_broadcast_nodes() {
    for c in configs() {
        assert!(c.broadcast.len() >= 2, "{}", c.network);
    }
}

#[test]
fn dec_12_every_instance_has_two_providers_of_different_names() {
    for c in configs() {
        assert_eq!(c.providers.len(), 2, "{}", c.network);
        assert_ne!(c.providers[0].name, c.providers[1].name, "{}", c.network);
    }
}
