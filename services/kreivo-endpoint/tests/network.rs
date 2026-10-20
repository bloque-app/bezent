// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! T-007-01: the endpoint refuses the wrong network at start-up (`AC-I4.1`, `REQ-IC-1`; PLAN
//! `DEC-5`). Against the in-memory chain and the committed fixtures; no network is reached.

use std::process::Command;

use bezent_kreivo_endpoint::chain::fake::FakeChain;
use bezent_kreivo_endpoint::network::{self, IdentityError, RelayPin};

const BIN: &str = env!("CARGO_BIN_EXE_bezent-kreivo-endpoint");

fn fixture(name: &str) -> String {
    format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[tokio::test]
async fn ac_i4_1_a_chain_that_is_not_the_declared_network_fails_closed() {
    let declared = [1; 32];
    let chain = FakeChain::new([2; 32], [0; 32]);
    let err = network::check_chain(&chain, declared).await.unwrap_err();
    assert_eq!(
        err,
        IdentityError::ChainMismatch {
            declared,
            chain: [2; 32]
        }
    );
    assert!(err.to_string().starts_with("AC-I4.1"), "{err}");

    // A chain that cannot say what it is is refused too: unknown is not the declared network.
    let chain = FakeChain::new(declared, [0; 32]);
    chain.set_unavailable(true);
    assert!(matches!(
        network::check_chain(&chain, declared).await,
        Err(IdentityError::Unknown(_))
    ));

    chain.set_unavailable(false);
    assert_eq!(network::check_chain(&chain, declared).await, Ok(()));
}

#[test]
fn ac_i4_1_the_binary_refuses_a_chain_spec_of_another_network_at_start_up() {
    // The pinned Kreivo spec, declared as Kreivo: the check passes.
    let ok = Command::new(BIN)
        .args(["--check", &fixture("endpoint.toml")])
        .output()
        .unwrap();
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );

    // The same spec declared as another network: the binary exits 1 before reading anything.
    let wrong = Command::new(BIN)
        .args(["--check", &fixture("wrong-network.toml")])
        .output()
        .unwrap();
    assert_eq!(wrong.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&wrong.stderr);
    assert!(
        stderr.contains("AC-I4.1") && stderr.contains("refusing to start"),
        "{stderr}"
    );

    // And without --check: the same refusal, before the light client starts.
    let run = Command::new(BIN)
        .arg(fixture("wrong-network.toml"))
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(1));
}

#[test]
fn ac_i4_1_the_relay_chain_is_pinned_with_the_network() {
    // Review S5: a relay chain spec whose genesis is not the declared relay genesis stops the
    // start, with or without --check.
    for args in [
        vec!["--check".to_owned(), fixture("wrong-relay.toml")],
        vec![fixture("wrong-relay.toml")],
    ] {
        let out = Command::new(BIN).args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("AC-I4.1") && stderr.contains("relay chain"),
            "{stderr}"
        );
    }

    let spec = std::fs::read_to_string(format!(
        "{}/chain-specs/kreivo.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let relay = std::fs::read_to_string(fixture("ksmcc3-identity.json")).unwrap();
    let kusama = network::chain_spec_genesis(&relay).unwrap();
    assert_eq!(
        hex::encode(kusama),
        "b0a8d493285c2df73290dfb7e61f870f17b41801197a149ca93654499ea3dafe"
    );
    let pin = RelayPin {
        genesis: Some(kusama),
        checkpoint: None,
        para_id: Some(2281),
    };
    assert_eq!(network::check_relay(&pin, &spec, Some(&relay)), Ok(()));
    // Kreivo is a parachain: without its relay chain pinned, nothing starts.
    assert!(matches!(
        network::check_relay(&RelayPin::default(), &spec, None),
        Err(IdentityError::ChainSpec(_))
    ));
    // A relay spec whose id is not the chain spec's relay chain is refused too.
    let other = relay.replace("\"ksmcc3\"", "\"polkadot\"");
    assert!(matches!(
        network::check_relay(&pin, &spec, Some(&other)),
        Err(IdentityError::ChainSpec(_))
    ));
}

fn kreivo_spec() -> String {
    std::fs::read_to_string(format!(
        "{}/chain-specs/kreivo.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// The identity fixture with a checkpoint: `lightSyncState` as Substrate writes it.
fn relay_with_checkpoint(grandpa_set: &str) -> String {
    let relay = std::fs::read_to_string(fixture("ksmcc3-identity.json")).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&relay).unwrap();
    v["lightSyncState"] = serde_json::json!({
        "finalizedBlockHeader": "0x00",
        "babeEpochChanges": "0x00",
        "babeFinalizedBlockWeight": 1,
        "grandpaAuthoritySet": grandpa_set,
    });
    v.to_string()
}

#[test]
fn s_c_the_relay_chain_specs_checkpoint_is_pinned() {
    // Review S-c: a relay spec with Kusama's genesis and id, but a forged checkpoint, is refused;
    // smoldot would trust the forged checkpoint's GRANDPA set.
    let spec = kreivo_spec();
    let real = relay_with_checkpoint("0x01");
    let forged = relay_with_checkpoint("0x02");
    let pinned = network::relay_checkpoint(&real).unwrap();
    assert!(pinned.is_some());
    let relay = std::fs::read_to_string(fixture("ksmcc3-identity.json")).unwrap();
    let kusama = network::chain_spec_genesis(&relay).unwrap();
    let pin = RelayPin {
        genesis: Some(kusama),
        checkpoint: pinned,
        para_id: Some(2281),
    };
    assert_eq!(network::check_relay(&pin, &spec, Some(&real)), Ok(()));
    let err = network::check_relay(&pin, &spec, Some(&forged)).unwrap_err();
    assert!(
        matches!(err, IdentityError::CheckpointMismatch { .. }),
        "{err}"
    );
    assert!(err.to_string().contains("refusing to start"), "{err}");
    // A checkpoint the configuration does not pin is refused; so is a pin with no checkpoint.
    let unpinned = RelayPin {
        checkpoint: None,
        ..pin
    };
    assert!(matches!(
        network::check_relay(&unpinned, &spec, Some(&real)),
        Err(IdentityError::CheckpointMismatch { .. })
    ));
    assert!(matches!(
        network::check_relay(&pin, &spec, Some(&relay)),
        Err(IdentityError::CheckpointMismatch { .. })
    ));
    // The hash is of the checkpoint's value, not of how the file orders its keys.
    let reordered: serde_json::Value = serde_json::from_str(&real).unwrap();
    let reordered = serde_json::to_string_pretty(&reordered).unwrap();
    assert_eq!(network::relay_checkpoint(&reordered).unwrap(), pinned);
}

#[test]
fn s_d_kreivos_para_id_is_pinned() {
    // Review S-d: a Kreivo spec with the right state root but another para_id would make smoldot
    // follow another parachain's heads.
    let spec = kreivo_spec();
    let relay = std::fs::read_to_string(fixture("ksmcc3-identity.json")).unwrap();
    let kusama = network::chain_spec_genesis(&relay).unwrap();
    let pin = RelayPin {
        genesis: Some(kusama),
        checkpoint: None,
        para_id: Some(2281),
    };
    let other = spec.replace("\"para_id\": 2281", "\"para_id\": 2000");
    assert_ne!(other, spec);
    assert_eq!(
        network::check_spec(network::chain_spec_genesis(&spec).unwrap(), &other),
        Ok(()),
        "the state root alone does not tell them apart"
    );
    assert_eq!(
        network::check_relay(&pin, &other, Some(&relay)),
        Err(IdentityError::ParaMismatch {
            declared: Some(2281),
            spec: Some(2000)
        })
    );
    // No para_id declared: refused, not assumed.
    let undeclared = RelayPin {
        para_id: None,
        ..pin
    };
    assert!(matches!(
        network::check_relay(&undeclared, &spec, Some(&relay)),
        Err(IdentityError::ParaMismatch { .. })
    ));
    // And the binary refuses another declared para_id at start-up, with or without --check.
    for args in [
        vec!["--check".to_owned(), fixture("wrong-para.toml")],
        vec![fixture("wrong-para.toml")],
    ] {
        let out = Command::new(BIN).args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("AC-I4.1") && stderr.contains("para_id"),
            "{stderr}"
        );
    }
}

#[tokio::test]
async fn ac_i4_1_a_contract_of_another_build_or_none_fails_closed() {
    // Issue #24: the right network can still hold other code at the contract's account, or none.
    let contract = [7; 32];
    let declared = [0xc0; 32];
    let chain = FakeChain::new([1; 32], contract);

    // No contract instantiated there.
    let err = network::check_contract(&chain, &contract, declared)
        .await
        .unwrap_err();
    assert_eq!(
        err,
        IdentityError::ContractMismatch {
            declared,
            chain: None
        }
    );
    assert!(
        err.to_string().starts_with("AC-I4.1") && err.to_string().contains("refusing to start"),
        "{err}"
    );

    // Another build.
    chain.set_code_hash(Some([0xc1; 32]));
    assert_eq!(
        network::check_contract(&chain, &contract, declared).await,
        Err(IdentityError::ContractMismatch {
            declared,
            chain: Some([0xc1; 32])
        })
    );
    // The declared build, at another account than the one declared: none there.
    chain.set_code_hash(Some(declared));
    assert!(matches!(
        network::check_contract(&chain, &[8; 32], declared).await,
        Err(IdentityError::ContractMismatch { chain: None, .. })
    ));

    // A chain that cannot say is unknown, and unknown is not the declared build.
    chain.set_unavailable(true);
    assert!(matches!(
        network::check_contract(&chain, &contract, declared).await,
        Err(IdentityError::Unknown(_))
    ));

    chain.set_unavailable(false);
    assert_eq!(
        network::check_contract(&chain, &contract, declared).await,
        Ok(())
    );
}

#[test]
fn ac_i4_1_the_binary_refuses_a_configuration_that_pins_no_code_hash() {
    // Required like the genesis: a configuration without it is refused, with or without --check.
    for args in [
        vec!["--check".to_owned(), fixture("no-code-hash.toml")],
        vec![fixture("no-code-hash.toml")],
    ] {
        let out = Command::new(BIN).args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("contract_code_hash"), "{stderr}");
    }
}
