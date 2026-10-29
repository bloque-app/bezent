// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! A native Kreivo ed25519 account as a delivery key (the Fargate design's ruling
//! `d-delivery-keys`): the policy lets an ed25519 key that is not the statement key take a Kreivo
//! network's intents, only when its custody never releases it (`REQ-CU-1`), and every check that
//! binds a key to a network still holds (`REQ-KY-4`, `REQ-KY-6`, `REQ-PR-1`, `INV-9`).

mod common;

use std::sync::Arc;

use bezent_signer::config::Config;
use bezent_signer::custody::Mixed;
use bezent_signer::keys::{Environment, FileBackend, KeyBackend, KeyError, Scheme};
use bezent_signer::link::{self, Callers, Client, Response};
use bezent_signer::transit::{TransitApi, TransitBackend, TransitKey};
use common::*;
use ed25519_dalek::Signer as _;
use sha2::Digest;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// The fixture's keys, a delivery key `kreivo-ed` and the statement key, all ed25519 or as KEYS.
fn backend(dir: &std::path::Path, extra: &[(&str, &str)]) -> FileBackend {
    let mut keys: Vec<_> = KEYS
        .iter()
        .chain(extra)
        .map(|(id, scheme)| {
            serde_json::json!({"id": id, "scheme": scheme, "seed": format!("0x{}", hex::encode(seed(id)))})
        })
        .collect();
    // The same secret as the statement key under another id.
    keys.push(serde_json::json!({"id": "alias", "scheme": "ed25519",
                                 "seed": format!("0x{}", hex::encode(seed("statement")))}));
    let path = dir.join("test-keys.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({"environment": "test", "keys": keys})).unwrap(),
    )
    .unwrap();
    FileBackend::load(&path).unwrap()
}

fn config(edit: impl FnOnce(&mut serde_json::Value)) -> Config {
    let dir = temp_dir();
    let mut json = config_json(&dir, my_uid(), TOKEN);
    edit(&mut json);
    Config::parse(json.to_string().as_bytes()).unwrap()
}

const EXTRA: &[(&str, &str)] = &[("kreivo-ed", "ed25519"), ("statement", "ed25519")];

#[tokio::test]
async fn req_ky_4_an_ed25519_delivery_key_is_bound_to_a_kreivo_network() {
    let dir = temp_dir();
    let backend = backend(&dir, EXTRA);
    let bind = |config: &mut serde_json::Value| {
        config["bindings"][0]["key"] = "kreivo-ed".into();
        config["statement_key"] = "statement".into();
    };
    let policy = config(bind).policy(&backend).expect("bound");
    assert_eq!(
        policy.binding("kreivo-test", "mint").unwrap().key,
        "kreivo-ed"
    );
    assert_eq!(policy.statement_key(), Some("statement"));

    // The message is signed as sr25519 and ECDSA ones are: the bytes themselves, RFC 8032.
    let signature = backend.sign("kreivo-ed", b"payload").await.unwrap();
    assert_eq!(signature.len(), 64);
    assert!(bezent_signer::verify::verifies(
        "ed25519",
        &backend.public_key("kreivo-ed").unwrap(),
        b"payload",
        &signature
    ));
    assert!(!bezent_signer::verify::verifies(
        "ed25519",
        &backend.public_key("kreivo-ed").unwrap(),
        b"other",
        &signature
    ));
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn req_ky_4_an_ed25519_key_is_refused_for_an_evm_network() {
    let dir = temp_dir();
    let backend = backend(&dir, EXTRA);
    let err = config(|c| c["bindings"][2]["key"] = "kreivo-ed".into())
        .policy(&backend)
        .unwrap_err();
    assert!(
        err.0.contains("ed25519") && err.0.contains("EVM network") && err.0.contains("INV-9"),
        "{err}"
    );
    // An EVM key still is not a Kreivo one.
    let err = config(|c| c["bindings"][0]["key"] = "polygon-payout".into())
        .policy(&backend)
        .unwrap_err();
    assert!(err.0.contains("INV-9"), "{err}");
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn req_pr_1_the_statement_key_is_still_refused_by_id_and_by_public_key() {
    let dir = temp_dir();
    let backend = backend(&dir, EXTRA);
    // By id, on a Kreivo network.
    let err = config(|c| {
        c["statement_key"] = "statement".into();
        c["bindings"][0]["key"] = "statement".into();
    })
    .policy(&backend)
    .unwrap_err();
    assert!(err.0.contains("REQ-PR-1"), "{err}");
    // By public key: the statement key's secret under another id.
    let err = config(|c| {
        c["statement_key"] = "statement".into();
        c["bindings"][0]["key"] = "alias".into();
    })
    .policy(&backend)
    .unwrap_err();
    assert!(
        err.0.contains("REQ-PR-1") && err.0.contains("alias"),
        "{err}"
    );
    // Neither the purpose.
    let err = config(|c| {
        c["bindings"][0]["key"] = "kreivo-ed".into();
        c["bindings"][0]["purposes"] = serde_json::json!(["mint", "statement"]);
    })
    .policy(&backend)
    .unwrap_err();
    assert!(err.0.contains("REQ-PR-1"), "{err}");
    std::fs::remove_dir_all(dir).ok();
}

/// A Transit that signs with a local key.
struct Local(ed25519_dalek::SigningKey);

impl TransitApi for Local {
    async fn public_key(&self, _: &str) -> Result<(u32, [u8; 32]), KeyError> {
        Ok((1, self.0.verifying_key().to_bytes()))
    }

    async fn sign(&self, _: &str, _: u32, message: &[u8]) -> Result<[u8; 64], KeyError> {
        Ok(self.0.sign(message).to_bytes())
    }
}

#[tokio::test]
async fn req_cu_1_in_production_only_a_key_the_custody_never_releases_is_a_delivery_key() {
    let dir = temp_dir();
    let key = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
    let entry = TransitKey {
        id: "kreivo-ed".into(),
        key: "kreivo-delivery".into(),
        account: format!("0x{}", hex::encode(key.verifying_key().to_bytes())),
    };
    let production = |config: &mut serde_json::Value| {
        config["bindings"] = serde_json::json!([{
            "key": "kreivo-ed", "network": "kreivo", "purposes": ["destroy"],
            "window_cap": "1000", "window_seconds": 3600
        }]);
    };
    // Held in Vault Transit: bound.
    let transit = TransitBackend::connect(Local(key.clone()), Environment::Production, &[entry])
        .await
        .unwrap();
    let mixed = Mixed::new(transit, None).unwrap();
    assert_eq!(mixed.scheme("kreivo-ed").unwrap(), Scheme::Ed25519);
    let policy = config(production).policy(&mixed).expect("bound");
    assert_eq!(
        policy.binding("kreivo", "destroy").unwrap().key,
        "kreivo-ed"
    );

    // The same key read into the signer from Vault KV: refused (REQ-CU-1).
    let kv = FileBackend::from_seeds(
        Environment::Production,
        vec![(
            "kreivo-ed".into(),
            Scheme::Ed25519,
            zeroize::Zeroizing::new(format!("0x{}", hex::encode([5u8; 32]))),
        )],
    )
    .unwrap();
    let err = config(production).policy(&kv).unwrap_err();
    assert!(
        err.0.contains("REQ-CU-1") && err.0.contains("key-management"),
        "{err}"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn req_ky_1_a_transit_configuration_needs_the_vault_table_and_takes_nothing_else() {
    let base = |keys: serde_json::Value| {
        let dir = temp_dir();
        let mut json = config_json(&dir, my_uid(), TOKEN);
        json["keys"] = keys;
        json
    };
    let transit = serde_json::json!({
        "backend": "transit", "environment": "production",
        "mount": "transit-bezent-signer",
        "keys": [{"id": "kreivo-ed", "key": "kreivo-delivery", "account": format!("0x{}", "ab".repeat(32))}],
        "vault": {"path": "secret/bezent/signer"}
    });
    let parse = |json: &serde_json::Value| Config::parse(json.to_string().as_bytes());
    // No `vault` table to log in with.
    let err = parse(&base(transit.clone())).unwrap_err();
    assert!(err.0.contains("no vault table"), "{err}");
    let mut json = base(transit.clone());
    json["vault"] = serde_json::json!({"address": "https://vault.example", "mount": "aws",
        "role": "bezent-signer-iam", "sts_region": "us-east-2", "server_id": "vault.example"});
    assert!(parse(&json).is_ok());
    // A field the configuration does not know, a token file for example, is refused.
    let mut odd = transit;
    odd["token_file"] = "/run/secrets/vault".into();
    let mut json = base(odd);
    json["vault"] = serde_json::json!({"address": "https://vault.example", "mount": "aws",
        "role": "bezent-signer-iam", "sts_region": "us-east-2", "server_id": "vault.example"});
    assert!(parse(&json).is_err());
}

/// Records a signature as the signer would, with the public key of test key `id`.
async fn record(f: &Fixture, intent: i64, scheme: &str, id: &str) -> Result<(), sqlx::Error> {
    let public = ed25519_dalek::SigningKey::from_bytes(&seed(id)).verifying_key();
    sqlx::query(&format!(
        "INSERT INTO signatures (intent_id, key_id, scheme, public_key, signature) \
         VALUES ({intent}, '{id}', '{scheme}', '\\x{}', '\\x{}')",
        hex::encode(public.to_bytes()),
        hex::encode([9u8; 64])
    ))
    .execute(&f.admin)
    .await
    .map(|_| ())
}

fn bzs_refused(result: Result<(), sqlx::Error>) -> bool {
    matches!(result, Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("BZ001"))
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ky_4_an_ed25519_delivery_key_signs_a_mint_through_the_store_and_the_link(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    // The fixture's keys, a Kreivo ed25519 delivery key and the statement key; the mint is the
    // delivery key's.
    backend(&f.dir, EXTRA);
    let mut json = config_json(&f.dir, my_uid(), TOKEN);
    json["bindings"][0]["key"] = "kreivo-ed".into();
    json["statement_key"] = "statement".into();
    let path = f.dir.join("ed.json");
    std::fs::write(&path, json.to_string()).unwrap();
    let config = Config::load(&path).unwrap();
    let (signer, _log, _) = Fixture::start(&config, &f.connect).await;

    let listener = link::bind(&config.socket).unwrap();
    let callers = Arc::new(Callers::sharing_the_signer_uid_in_tests(&config.callers));
    tokio::spawn(link::serve(listener, callers, signer));
    let uid = my_uid();
    let core = f.core_pool().await;
    let mut client = Client::connect(&config.socket, TOKEN, uid).await.unwrap();

    // The core verifies what comes back over the recorded bytes (INV-9) and the store records it.
    let mint = f.intent("kreivo-test", "mint", None, "100", None).await;
    let response = client.sign(&core, mint).await.unwrap();
    let Response::Signed {
        key,
        scheme,
        public_key,
        signature,
        ..
    } = response
    else {
        panic!("not signed: {response:?}");
    };
    assert_eq!((key.as_str(), scheme.as_str()), ("kreivo-ed", "ed25519"));
    assert_eq!(signature.len(), 2 + 128);
    assert_eq!(
        public_key,
        format!(
            "0x{}",
            hex::encode(
                ed25519_dalek::SigningKey::from_bytes(&seed("kreivo-ed"))
                    .verifying_key()
                    .to_bytes()
            )
        )
    );
    assert_eq!(f.signatures().await, 1);

    // The key is the delivery key's: it signs no statement (REQ-PR-1, in the store).
    let doc = b"a statement";
    let sha = format!("0x{}", hex::encode(sha2::Sha256::digest(doc)));
    let payload = serde_json::json!({
        "message": format!("0x{}", hex::encode(doc)), "amount": "0",
        "statement": {"asset": ASSET, "sequence": 1, "sha256": sha},
    });
    let statement: i64 = sqlx::query_scalar(
        "INSERT INTO intents (network, purpose, payload) VALUES ('kreivo-test', 'statement', \
         $1::text::jsonb) RETURNING id",
    )
    .bind(payload.to_string())
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert!(bzs_refused(
        record(&f, statement, "ed25519", "kreivo-ed").await
    ));
    // The statement key signs it, and then no other intent; a statement is signed by ed25519 only.
    assert!(bzs_refused(
        record(&f, statement, "ecdsa", "kreivo-destroy").await
    ));
    record(&f, statement, "ed25519", "statement").await.unwrap();
    let another = f.intent("kreivo-test", "mint", None, "100", None).await;
    assert!(bzs_refused(
        record(&f, another, "ed25519", "statement").await
    ));
    // A second delivery signature by the delivery key stands.
    record(&f, another, "ed25519", "kreivo-ed").await.unwrap();
}
