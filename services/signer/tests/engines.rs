// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The conformance suite (`tests/conformance`) against every signing engine, each with a local
//! stand-in for its service, and the checks of the Vault signing-plugin engine that are its own:
//! the signer hashes, only the digest reaches the plugin, `s` is made low, `v` is found by
//! recovering the declared address, and nothing that does not verify is returned.

mod conformance;

use bezent_signer::custody::Mixed;
use bezent_signer::engine::{Signable, SigningEngine, prepare};
use bezent_signer::keys::{Environment, FileBackend, KeyError, Scheme, evm_digest};
use bezent_signer::kms::{KmsBackend, KmsKey, evm_address, kreivo_account};
use bezent_signer::transit::{TransitBackend, TransitKey};
use bezent_signer::vault_plugin::{PluginBackend, PluginKey};
use conformance::doubles::{LocalKms, LocalPlugin, LocalTransit, secp_key};
use conformance::{Case, PAYLOAD, Setup, fails_closed_when_down, refuses_a_rogue, run};
use k256::PublicKey;
use zeroize::Zeroizing;

fn seed(n: u8) -> Vec<u8> {
    vec![n; 32]
}

fn seed_hex(n: u8) -> String {
    format!("0x{}", hex::encode(seed(n)))
}

fn secp_public(key: &k256::ecdsa::SigningKey) -> Vec<u8> {
    key.verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .to_vec()
}

/// The four schemes in one engine of the signer's memory.
fn file_engine() -> (FileBackend, Vec<Case>) {
    let file = format!(
        r#"{{"environment": "test", "keys": [
            {{"id": "sr", "scheme": "sr25519", "seed": "{}"}},
            {{"id": "ed", "scheme": "ed25519", "seed": "{}"}},
            {{"id": "kreivo-ecdsa", "scheme": "ecdsa", "seed": "{}"}},
            {{"id": "evm", "scheme": "secp256k1-evm", "seed": "{}"}}]}}"#,
        seed_hex(1),
        seed_hex(2),
        seed_hex(3),
        seed_hex(4)
    );
    let backend = FileBackend::parse(file.as_bytes()).unwrap();
    let sr = schnorrkel::MiniSecretKey::from_bytes(&seed(1))
        .unwrap()
        .expand_to_keypair(schnorrkel::ExpansionMode::Ed25519);
    let cases = vec![
        Case {
            id: "sr",
            scheme: Scheme::Sr25519,
            public_key: sr.public.to_bytes().to_vec(),
            non_exportable: false,
        },
        Case {
            id: "ed",
            scheme: Scheme::Ed25519,
            public_key: ed25519_dalek::SigningKey::from_bytes(&[2; 32])
                .verifying_key()
                .to_bytes()
                .to_vec(),
            non_exportable: false,
        },
        Case {
            id: "kreivo-ecdsa",
            scheme: Scheme::Ecdsa,
            public_key: secp_public(&secp_key(3)),
            non_exportable: false,
        },
        Case {
            id: "evm",
            scheme: Scheme::Secp256k1Evm,
            public_key: secp_public(&secp_key(4)),
            non_exportable: false,
        },
    ];
    (backend, cases)
}

#[tokio::test]
async fn req_cu_1_the_file_engine_passes_the_conformance_suite() {
    let (engine, cases) = file_engine();
    run(Setup {
        engine: &engine,
        cases,
        secrets: (1..=4).map(seed).collect(),
    })
    .await;
}

#[tokio::test]
async fn req_ky_1_the_vault_kv_engine_passes_the_conformance_suite() {
    let engine = FileBackend::from_seeds(
        Environment::Production,
        vec![(
            "statement".into(),
            Scheme::Ed25519,
            Zeroizing::new(seed_hex(5)),
        )],
    )
    .unwrap();
    assert_eq!(engine.name(), "Vault KV");
    run(Setup {
        engine: &engine,
        cases: vec![Case {
            id: "statement",
            scheme: Scheme::Ed25519,
            public_key: ed25519_dalek::SigningKey::from_bytes(&[5; 32])
                .verifying_key()
                .to_bytes()
                .to_vec(),
            non_exportable: false,
        }],
        secrets: vec![seed(5)],
    })
    .await;
}

async fn kms_engine(kms: LocalKms) -> KmsBackend<LocalKms> {
    let public = kms.public();
    let entries = [
        KmsKey {
            id: "polygon-payout".into(),
            key: "arn:aws:kms:us-east-2:111122223333:key/evm".into(),
            scheme: Scheme::Secp256k1Evm,
            address: Some(format!("0x{}", hex::encode(evm_address(&public)))),
            account: None,
        },
        KmsKey {
            id: "kreivo-mint".into(),
            key: "arn:aws:kms:us-east-2:111122223333:key/kreivo".into(),
            scheme: Scheme::Ecdsa,
            address: None,
            account: Some(format!("0x{}", hex::encode(kreivo_account(&public)))),
        },
    ];
    KmsBackend::connect(kms, Environment::Test, &entries)
        .await
        .unwrap()
}

#[tokio::test]
async fn req_cu_1_the_kms_engine_passes_the_conformance_suite() {
    for high_s in [false, true] {
        let mut kms = LocalKms::new(7, Some("test"));
        kms.high_s = high_s;
        let public = secp_public(&kms.key);
        let engine = kms_engine(kms).await;
        run(Setup {
            engine: &engine,
            cases: vec![
                Case {
                    id: "polygon-payout",
                    scheme: Scheme::Secp256k1Evm,
                    public_key: public.clone(),
                    non_exportable: true,
                },
                Case {
                    id: "kreivo-mint",
                    scheme: Scheme::Ecdsa,
                    public_key: public,
                    non_exportable: true,
                },
            ],
            secrets: vec![seed(7)],
        })
        .await;
    }
    let mut rogue = LocalKms::new(7, Some("test"));
    rogue.wrong_signer = true;
    refuses_a_rogue(&kms_engine(rogue).await, "polygon-payout").await;
    let engine = kms_engine(LocalKms::new(7, Some("test"))).await;
    *engine_api_down(&engine) = true;
    fails_closed_when_down(&engine, "polygon-payout").await;
}

fn engine_api_down(engine: &KmsBackend<LocalKms>) -> std::sync::MutexGuard<'_, bool> {
    engine.api().down.lock().unwrap()
}

fn ed_key(n: u8) -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[n; 32])
}

async fn transit_engine(api: LocalTransit) -> TransitBackend<LocalTransit> {
    let account = format!("0x{}", hex::encode(api.key.verifying_key().to_bytes()));
    TransitBackend::connect(
        api,
        Environment::Test,
        &[TransitKey {
            id: "kreivo-delivery".into(),
            key: "kreivo-delivery".into(),
            account,
        }],
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn req_cu_1_the_transit_engine_passes_the_conformance_suite() {
    let engine = transit_engine(LocalTransit::new(ed_key(8))).await;
    let case = || Case {
        id: "kreivo-delivery",
        scheme: Scheme::Ed25519,
        public_key: ed_key(8).verifying_key().to_bytes().to_vec(),
        non_exportable: true,
    };
    run(Setup {
        engine: &engine,
        cases: vec![case()],
        secrets: vec![seed(8)],
    })
    .await;

    // Joined with Vault KV, the custody passes too, and each key keeps its own custody.
    let kv = FileBackend::from_seeds(
        Environment::Test,
        vec![(
            "statement".into(),
            Scheme::Ed25519,
            Zeroizing::new(seed_hex(5)),
        )],
    )
    .unwrap();
    let mixed = Mixed::new(transit_engine(LocalTransit::new(ed_key(8))).await, Some(kv)).unwrap();
    run(Setup {
        engine: &mixed,
        cases: vec![
            case(),
            Case {
                id: "statement",
                scheme: Scheme::Ed25519,
                public_key: ed_key(5).verifying_key().to_bytes().to_vec(),
                non_exportable: false,
            },
        ],
        secrets: vec![seed(8), seed(5)],
    })
    .await;

    let mut rogue = LocalTransit::new(ed_key(8));
    rogue.rogue = Some(ed_key(9));
    refuses_a_rogue(&transit_engine(rogue).await, "kreivo-delivery").await;
    let engine = transit_engine(LocalTransit::new(ed_key(8))).await;
    *engine.api().down.lock().unwrap() = true;
    fails_closed_when_down(&engine, "kreivo-delivery").await;
}

// ------------------------------------------------------------ the Vault signing plugin

fn plugin_key(key: &k256::ecdsa::SigningKey) -> PluginKey {
    let public = PublicKey::from(key.verifying_key());
    PluginKey {
        id: "polygon-delivery".into(),
        key: "bezent-polygon-delivery".into(),
        address: format!("0x{}", hex::encode(evm_address(&public))),
        public_key: format!("0x{}", hex::encode(secp_public(key))),
    }
}

fn plugin_engine(api: LocalPlugin) -> PluginBackend<LocalPlugin> {
    let entry = plugin_key(&api.key);
    PluginBackend::new(api, Environment::Test, &[entry]).unwrap()
}

#[tokio::test]
async fn req_cu_1_the_vault_plugin_engine_passes_the_conformance_suite() {
    // The plugin signs low-s; a high s is normalised all the same.
    for high_s in [false, true] {
        let mut api = LocalPlugin::new(secp_key(11));
        api.high_s = high_s;
        let engine = plugin_engine(api);
        run(Setup {
            engine: &engine,
            cases: vec![Case {
                id: "polygon-delivery",
                scheme: Scheme::Secp256k1Evm,
                public_key: secp_public(&secp_key(11)),
                non_exportable: true,
            }],
            secrets: vec![seed(11)],
        })
        .await;
    }
    let kv = FileBackend::from_seeds(
        Environment::Test,
        vec![(
            "statement".into(),
            Scheme::Ed25519,
            Zeroizing::new(seed_hex(5)),
        )],
    )
    .unwrap();
    let mixed = Mixed::new(plugin_engine(LocalPlugin::new(secp_key(11))), Some(kv)).unwrap();
    assert!(mixed.non_exportable("polygon-delivery") && !mixed.non_exportable("statement"));
    run(Setup {
        engine: &mixed,
        cases: vec![
            Case {
                id: "polygon-delivery",
                scheme: Scheme::Secp256k1Evm,
                public_key: secp_public(&secp_key(11)),
                non_exportable: true,
            },
            Case {
                id: "statement",
                scheme: Scheme::Ed25519,
                public_key: ed_key(5).verifying_key().to_bytes().to_vec(),
                non_exportable: false,
            },
        ],
        secrets: vec![seed(11), seed(5)],
    })
    .await;
}

#[tokio::test]
async fn req_ky_1_the_signer_hashes_and_only_the_32_byte_digest_reaches_the_plugin() {
    let engine = plugin_engine(LocalPlugin::new(secp_key(11)));
    engine.sign("polygon-delivery", PAYLOAD).await.unwrap();
    assert_eq!(
        engine.api().asked.lock().unwrap().as_slice(),
        [("bezent-polygon-delivery".to_owned(), evm_digest(PAYLOAD))],
        "Keccak-256 of the payload, computed by the signer"
    );
    // A message, or a digest for another scheme, never reaches it.
    for (scheme, input) in [
        (Scheme::Secp256k1Evm, Signable::Message(PAYLOAD)),
        (Scheme::Ecdsa, prepare(Scheme::Ecdsa, PAYLOAD)),
        (Scheme::Ed25519, Signable::Message(PAYLOAD)),
    ] {
        let err = engine
            .sign_prepared("polygon-delivery", scheme, input)
            .await
            .unwrap_err();
        assert!(matches!(err, KeyError::Refused(_)), "{err}");
    }
    assert_eq!(
        engine.api().asked.lock().unwrap().len(),
        1,
        "nothing else sent"
    );
}

#[tokio::test]
async fn inv_9_v_is_found_by_recovering_the_declared_address_and_a_rogue_is_refused() {
    // Both recovery ids occur over a few digests, and each recovers the declared address.
    let engine = plugin_engine(LocalPlugin::new(secp_key(11)));
    let address = evm_address(&PublicKey::from(secp_key(11).verifying_key()));
    let mut seen = [false; 2];
    for i in 0..32u8 {
        let message = [b'm', i];
        let signature = engine.sign("polygon-delivery", &message).await.unwrap();
        seen[usize::from(signature[64])] = true;
        let sig = k256::ecdsa::Signature::from_slice(&signature[..64]).unwrap();
        let recovered = k256::ecdsa::VerifyingKey::recover_from_prehash(
            &evm_digest(&message),
            &sig,
            k256::ecdsa::RecoveryId::from_byte(signature[64]).unwrap(),
        )
        .unwrap();
        assert_eq!(evm_address(&PublicKey::from(&recovered)), address);
    }
    assert_eq!(seen, [true, true], "v = 0 and v = 1 both occur");

    let mut api = LocalPlugin::new(secp_key(11));
    api.rogue = Some(secp_key(12));
    refuses_a_rogue(&plugin_engine(api), "polygon-delivery").await;
    let engine = plugin_engine(LocalPlugin::new(secp_key(11)));
    *engine.api().down.lock().unwrap() = true;
    fails_closed_when_down(&engine, "polygon-delivery").await;
}

#[test]
fn req_ky_3_a_public_key_that_is_not_the_declared_address_is_refused_at_start() {
    let key = secp_key(11);
    let other = plugin_key(&secp_key(12));
    let mut entry = plugin_key(&key);
    entry.address = other.address;
    let err = PluginBackend::new(LocalPlugin::new(key.clone()), Environment::Test, &[entry])
        .err()
        .unwrap();
    assert!(
        matches!(&err, KeyError::Refused(m) if m.contains("REQ-KY-3")),
        "{err}"
    );
    let good = plugin_key(&key);
    let short_public = format!("0x{}", hex::encode(&secp_public(&key)[1..]));
    for (address, public) in [
        ("0x1234", good.public_key.as_str()),
        ("not hex", good.public_key.as_str()),
        ("", good.public_key.as_str()),
        (good.address.as_str(), short_public.as_str()),
        (
            good.address.as_str(),
            "0x050505050505050505050505050505050505050505050505050505050505050505",
        ),
    ] {
        let entry = PluginKey {
            address: address.into(),
            public_key: public.into(),
            ..good.clone()
        };
        assert!(matches!(
            PluginBackend::new(LocalPlugin::new(key.clone()), Environment::Test, &[entry]),
            Err(KeyError::Refused(_))
        ));
    }
    let entry = plugin_key(&key);
    for keys in [vec![entry.clone(), entry.clone()], {
        let mut e = entry.clone();
        e.key = "a/b".into();
        vec![e]
    }] {
        assert!(matches!(
            PluginBackend::new(LocalPlugin::new(key.clone()), Environment::Test, &keys),
            Err(KeyError::Source(_))
        ));
    }
}

#[test]
fn req_ky_6_a_published_test_fixtures_key_is_refused_in_production() {
    use sha2::{Digest, Sha256};
    let seed: [u8; 32] = Sha256::digest(b"bezent published test fixture polygon-payout").into();
    let fixture = k256::ecdsa::SigningKey::from_slice(&seed).unwrap();
    let entry = plugin_key(&fixture);
    let err = PluginBackend::new(
        LocalPlugin::new(fixture.clone()),
        Environment::Production,
        std::slice::from_ref(&entry),
    )
    .err()
    .unwrap();
    assert!(
        matches!(&err, KeyError::Refused(m) if m.contains("test fixture")),
        "{err}"
    );
    assert!(PluginBackend::new(LocalPlugin::new(fixture), Environment::Test, &[entry]).is_ok());
}
