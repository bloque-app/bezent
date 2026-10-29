// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-005 / issue #5 (DEC-11): the AWS KMS backend against a local KMS stand-in that implements
//! `GetPublicKey`, `ListResourceTags` and `Sign` (digest) with a real secp256k1 key. No test calls
//! AWS. The signatures are checked on both chains' formats (`REQ-CU-1`); the environment and tag
//! refusals close issue #25 (`REQ-KY-6`, `AC-I4.2`).

use bezent_signer::config::{Network, confirm_store_environment};
use bezent_signer::custody::Mixed;
use bezent_signer::keys::{
    Environment, FileBackend, KeyError, Scheme, SigningEngine, ecdsa_digest, evm_digest,
};
use bezent_signer::kms::{ENVIRONMENT_TAG, KmsBackend, KmsKey, evm_address, kreivo_account};
use k256::ecdsa::{RecoveryId, Signature, SigningKey, VerifyingKey};

mod conformance;

use conformance::doubles::LocalKms;

fn evm_entry(kms: &LocalKms) -> KmsKey {
    KmsKey {
        id: "polygon-payout".into(),
        key: "arn:aws:kms:us-east-2:111122223333:key/evm".into(),
        scheme: Scheme::Secp256k1Evm,
        address: Some(format!("0x{}", hex::encode(evm_address(&kms.public())))),
        account: None,
    }
}

fn kreivo_entry(kms: &LocalKms) -> KmsKey {
    KmsKey {
        id: "kreivo-mint".into(),
        key: "arn:aws:kms:us-east-2:111122223333:key/kreivo".into(),
        scheme: Scheme::Ecdsa,
        address: None,
        account: Some(format!("0x{}", hex::encode(kreivo_account(&kms.public())))),
    }
}

fn recover(digest: &[u8; 32], signature: &[u8]) -> (Signature, VerifyingKey) {
    assert_eq!(signature.len(), 65);
    let sig = Signature::from_slice(&signature[..64]).unwrap();
    assert!(sig.normalize_s().is_none(), "low s");
    let key = VerifyingKey::recover_from_prehash(
        digest,
        &sig,
        RecoveryId::from_byte(signature[64]).unwrap(),
    )
    .unwrap();
    (sig, key)
}

#[tokio::test]
async fn req_cu_1_evm_signature_recovers_the_configured_address() {
    for high_s in [false, true] {
        let mut kms = LocalKms::new(7, Some("production"));
        kms.high_s = high_s;
        let entry = evm_entry(&kms);
        let expected = *kms.key.verifying_key();
        let backend = KmsBackend::connect(kms, Environment::Production, &[entry])
            .await
            .unwrap();
        assert_eq!(backend.environment(), Environment::Production);
        assert_eq!(
            backend.scheme("polygon-payout").unwrap(),
            Scheme::Secp256k1Evm
        );
        let message = b"eip-1559 payload";
        let signature = backend.sign("polygon-payout", message).await.unwrap();
        let (_, recovered) = recover(&evm_digest(message), &signature);
        assert_eq!(recovered, expected);
        assert_eq!(
            backend.public_key("polygon-payout").unwrap(),
            k256::PublicKey::from(&expected).to_sec1_bytes().to_vec()
        );
        // The file backend would sign the same bytes with the same key: same digest, same format.
        let local = evm_digest(message);
        let (sig, _) = recover(&local, &signature);
        use k256::ecdsa::signature::hazmat::PrehashVerifier;
        expected.verify_prehash(&local, &sig).unwrap();
    }
}

#[tokio::test]
async fn req_cu_1_kreivo_ecdsa_signature_verifies_for_short_and_long_payloads() {
    let kms = LocalKms::new(9, Some("test"));
    let entry = kreivo_entry(&kms);
    let expected = *kms.key.verifying_key();
    let backend = KmsBackend::connect(kms, Environment::Test, &[entry])
        .await
        .unwrap();
    for len in [10usize, 256, 257, 600] {
        let payload = vec![0xab; len];
        let signature = backend.sign("kreivo-mint", &payload).await.unwrap();
        let (_, recovered) = recover(&ecdsa_digest(&payload), &signature);
        assert_eq!(recovered, expected, "payload of {len} bytes");
    }
    // Over 256 bytes the payload is hashed first (Substrate), so it differs from the short rule.
    assert_ne!(ecdsa_digest(&[1u8; 257]), {
        use blake2::Digest;
        let h: [u8; 32] = blake2::Blake2b::<blake2::digest::consts::U32>::digest([1u8; 257]).into();
        h
    });
}

#[tokio::test]
async fn req_ky_6_a_key_without_the_environment_tag_is_refused() {
    let kms = LocalKms::new(7, None);
    let entry = evm_entry(&kms);
    let err = KmsBackend::connect(kms, Environment::Test, &[entry])
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&err, KeyError::Refused(w) if w.contains(ENVIRONMENT_TAG)),
        "{err}"
    );
}

#[tokio::test]
async fn ac_i4_2_a_test_key_cannot_be_used_in_a_production_configuration() {
    let kms = LocalKms::new(7, Some("test"));
    let entry = evm_entry(&kms);
    let err = KmsBackend::connect(kms, Environment::Production, &[entry])
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&err, KeyError::Refused(w) if w.contains("tagged test")),
        "{err}"
    );
    // And a production key in a test configuration.
    let kms = LocalKms::new(7, Some("production"));
    let entry = evm_entry(&kms);
    assert!(
        KmsBackend::connect(kms, Environment::Test, &[entry])
            .await
            .is_err()
    );
    // A tag that is neither.
    let kms = LocalKms::new(7, Some("staging"));
    let entry = evm_entry(&kms);
    assert!(
        KmsBackend::connect(kms, Environment::Test, &[entry])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn ac_i4_2_a_published_test_account_is_refused_in_production() {
    // The published fixture's EVM and Kreivo ECDSA keys, wrongly tagged production in KMS.
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/test-keys.json")).unwrap();
    for (id, scheme) in [
        ("polygon-payout", "secp256k1-evm"),
        ("kreivo-destroy", "ecdsa"),
    ] {
        let seed = fixture["keys"]
            .as_array()
            .unwrap()
            .iter()
            .find(|k| k["id"] == id)
            .unwrap()["seed"]
            .as_str()
            .unwrap()
            .trim_start_matches("0x")
            .to_owned();
        let mut kms = LocalKms::new(1, Some("production"));
        kms.key = SigningKey::from_slice(&hex::decode(seed).unwrap()).unwrap();
        let entry = if scheme == "ecdsa" {
            kreivo_entry(&kms)
        } else {
            evm_entry(&kms)
        };
        let err = KmsBackend::connect(kms, Environment::Production, &[entry])
            .await
            .err()
            .unwrap();
        assert!(
            matches!(&err, KeyError::Refused(w) if w.contains("published test fixture")),
            "{id}: {err}"
        );
    }
}

#[tokio::test]
async fn req_ky_3_the_public_key_must_be_the_configured_address_or_account() {
    let kms = LocalKms::new(7, Some("test"));
    let mut evm = evm_entry(&kms);
    evm.address = Some(format!("0x{}", "11".repeat(20)));
    assert!(
        KmsBackend::connect(LocalKms::new(7, Some("test")), Environment::Test, &[evm])
            .await
            .is_err()
    );
    let mut kreivo = kreivo_entry(&kms);
    kreivo.account = Some(format!("0x{}", "22".repeat(32)));
    assert!(
        KmsBackend::connect(LocalKms::new(7, Some("test")), Environment::Test, &[kreivo])
            .await
            .is_err()
    );
    // An address for a Kreivo key, or both, is a misconfiguration too.
    let mut both = kreivo_entry(&kms);
    both.address = Some(format!("0x{}", "11".repeat(20)));
    assert!(
        KmsBackend::connect(LocalKms::new(7, Some("test")), Environment::Test, &[both])
            .await
            .is_err()
    );
    // Only secp256k1 schemes live in KMS.
    let mut sr = kreivo_entry(&kms);
    sr.scheme = Scheme::Sr25519;
    assert!(
        KmsBackend::connect(LocalKms::new(7, Some("test")), Environment::Test, &[sr])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn req_cu_1_a_signature_by_another_key_is_never_returned() {
    let mut kms = LocalKms::new(7, Some("test"));
    kms.wrong_signer = true;
    let entry = evm_entry(&kms);
    let backend = KmsBackend::connect(kms, Environment::Test, &[entry])
        .await
        .unwrap();
    assert!(matches!(
        backend.sign("polygon-payout", b"x").await,
        Err(KeyError::Backend(_))
    ));
    assert!(matches!(
        backend.sign("nope", b"x").await,
        Err(KeyError::UnknownKey(_))
    ));
}

#[tokio::test]
async fn req_ky_1_debug_prints_ids_and_public_keys_only() {
    let kms = LocalKms::new(7, Some("test"));
    let entry = evm_entry(&kms);
    let backend = KmsBackend::connect(kms, Environment::Test, &[entry])
        .await
        .unwrap();
    let text = format!("{backend:?}");
    assert!(text.contains("polygon-payout") && text.contains("secp256k1-evm"));
    assert!(!text.contains(&hex::encode([7u8; 32])));
    assert!(!text.contains("arn:"));
}

#[tokio::test]
async fn req_ky_6_kms_and_vault_must_share_an_environment() {
    let seed = format!("0x{}", hex::encode([3u8; 32]));
    let vault_of = |environment: Environment| {
        FileBackend::from_seeds(
            environment,
            vec![(
                "statement".to_owned(),
                Scheme::Ed25519,
                zeroize::Zeroizing::new(seed.clone()),
            )],
        )
        .unwrap()
    };
    let kms = || async {
        let kms = LocalKms::new(7, Some("test"));
        let entry = evm_entry(&kms);
        KmsBackend::connect(kms, Environment::Test, &[entry])
            .await
            .unwrap()
    };
    assert!(Mixed::new(kms().await, Some(vault_of(Environment::Production))).is_err());
    let mixed = Mixed::new(kms().await, Some(vault_of(Environment::Test))).unwrap();
    assert_eq!(mixed.scheme("statement").unwrap(), Scheme::Ed25519);
    assert_eq!(
        mixed.scheme("polygon-payout").unwrap(),
        Scheme::Secp256k1Evm
    );
    assert!(mixed.sign("statement", b"s").await.is_ok());
    assert!(mixed.sign("polygon-payout", b"s").await.is_ok());
}

fn network(id: &str, environment: Environment) -> Network {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "environment": match environment { Environment::Test => "test", Environment::Production => "production" },
    }))
    .unwrap()
}

#[test]
fn ac_i4_2_the_environment_comes_from_the_stores_networks_not_a_label() {
    let prod = || vec!["polygon".to_owned()];
    // Test keys on a store that declares a production network.
    assert!(
        confirm_store_environment(
            &[network("polygon-amoy", Environment::Test)],
            Environment::Test,
            &prod()
        )
        .is_err()
    );
    // A production network labelled test in the configuration.
    assert!(
        confirm_store_environment(
            &[network("polygon", Environment::Test)],
            Environment::Test,
            &[]
        )
        .is_err()
    );
    // A test network labelled production.
    assert!(
        confirm_store_environment(
            &[network("polygon-amoy", Environment::Production)],
            Environment::Production,
            &[]
        )
        .is_err()
    );
    // Consistent.
    assert!(
        confirm_store_environment(
            &[network("polygon-amoy", Environment::Test)],
            Environment::Test,
            &[]
        )
        .is_ok()
    );
    assert!(
        confirm_store_environment(
            &[network("polygon", Environment::Production)],
            Environment::Production,
            &prod()
        )
        .is_ok()
    );
}
