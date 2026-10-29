// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The Vault signing-plugin engine against a real Vault, `vault server -dev`, with
//! `blockchain-signer-vault-plugin` built from its pinned commit, registered by its SHA-256 and
//! mounted by the `vault-dev` job of CI (`.github/workflows/ci.yml`). The login is the AWS auth
//! method against a fake STS the test starts, as in `crates/vault/tests/dev_server.rs`.
//!
//! It proves, on the plugin itself:
//!
//! - the engine passes the conformance suite (`tests/conformance`; `REQ-CU-1`, `INV-9`, `REQ-KY-1`);
//! - every signature recovers the key's address with an implementation apart from the engine's
//!   (libsecp256k1 through the `secp256k1` crate, and `tiny-keccak`), and verifies with it
//!   (`REQ-KY-3`, `INV-9`);
//! - a key that is not the declared one is refused at the first signature, and nothing is returned
//!   (`INV-9`);
//! - the policy the deployment gives the engine's role, `update` on its own key's `sign/hash` and
//!   nothing else, signs, and refuses everything else: a read of the key, another key, `sign/message`,
//!   a listing, a new key (`REQ-KY-1`, `REQ-CU-1`);
//! - the plugin the job mounted is the one registered with the SHA-256 the job computed.
//!
//! Ignored by default; CI runs it with `--include-ignored` and `BEZENT_VAULT_DEV_ADDR`,
//! `BEZENT_VAULT_DEV_TOKEN` (the dev server's throwaway root token), `BEZENT_VAULT_PLUGIN_MOUNT`,
//! `BEZENT_VAULT_PLUGIN_NAME` and `BEZENT_VAULT_PLUGIN_SHA256` set.

mod conformance;

use std::sync::Arc;

use aws_credential_types::Credentials;
use bezent_signer::engine::SigningEngine;
use bezent_signer::keys::{Environment, KeyError, Scheme};
use bezent_signer::vault_plugin::{PluginBackend, PluginKey, VaultPlugin};
use bezent_vault::{Client, Error, Resolver, VaultConfig};
use conformance::{Case, PAYLOAD, Setup, run};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CALLER: &str = "arn:aws:iam::000000000000:user/bezent-plugin-test";
const AUTH: &str = "bezent-plugin-auth";
const ROLE: &str = "bezent-evm-signer-plugin";
const POLICY: &str = "bezent-evm-signer-plugin";
const KEY: &str = "bezent-polygon-delivery";
const OTHER: &str = "bezent-other";

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

/// A fake STS: answers every request with `GetCallerIdentity` for [`CALLER`].
async fn fake_sts() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16384];
                let _ = stream.read(&mut buf).await;
                let body = format!(
                    "<GetCallerIdentityResponse xmlns=\"https://sts.amazonaws.com/doc/2011-06-15/\"><GetCallerIdentityResult><Arn>{CALLER}</Arn><UserId>AIDAEXAMPLE</UserId><Account>000000000000</Account></GetCallerIdentityResult><ResponseMetadata><RequestId>1</RequestId></ResponseMetadata></GetCallerIdentityResponse>"
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/xml\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    address
}

/// A call to the dev server with `token`: the status and the JSON body (null if none).
async fn call(
    address: &str,
    token: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let mut request = reqwest::Client::new()
        .request(method, format!("{address}/v1/{path}"))
        .header("X-Vault-Token", token);
    if let Some(body) = body {
        request = request.body(body.to_string());
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    (
        status,
        serde_json::from_str(&text).unwrap_or(serde_json::Value::Null),
    )
}

/// An admin call with the root token; panics on a failure (a dev server).
async fn admin(
    address: &str,
    root: &str,
    path: &str,
    body: serde_json::Value,
) -> serde_json::Value {
    let (status, answer) = call(address, root, reqwest::Method::POST, path, Some(body)).await;
    assert!((200..300).contains(&status), "{path}: {status} {answer}");
    answer
}

/// The 20-byte EVM address of a compressed secp256k1 key, by libsecp256k1 and tiny-keccak: not
/// the engine's code.
fn independent_address(compressed: &[u8]) -> [u8; 20] {
    use tiny_keccak::Hasher as _;
    let public = secp256k1::PublicKey::from_slice(compressed).unwrap();
    let mut hash = [0u8; 32];
    let mut keccak = tiny_keccak::Keccak::v256();
    keccak.update(&public.serialize_uncompressed()[1..]);
    keccak.finalize(&mut hash);
    hash[12..].try_into().unwrap()
}

fn independent_keccak(message: &[u8]) -> [u8; 32] {
    use tiny_keccak::Hasher as _;
    let mut hash = [0u8; 32];
    let mut keccak = tiny_keccak::Keccak::v256();
    keccak.update(message);
    keccak.finalize(&mut hash);
    hash
}

/// Recovers the address from `r ‖ s ‖ v` over the Keccak-256 of `message` and verifies it, with
/// libsecp256k1.
fn independent_recover(message: &[u8], signature: &[u8]) -> [u8; 20] {
    use secp256k1::ecdsa::{RecoverableSignature, RecoveryId};
    assert_eq!(signature.len(), 65);
    let secp = secp256k1::Secp256k1::verification_only();
    let digest = secp256k1::Message::from_digest(independent_keccak(message));
    let id = RecoveryId::try_from(i32::from(signature[64])).unwrap();
    let recoverable = RecoverableSignature::from_compact(&signature[..64], id).unwrap();
    let public = secp.recover_ecdsa(&digest, &recoverable).unwrap();
    let mut plain = recoverable.to_standard();
    plain.normalize_s();
    assert_eq!(plain.serialize_compact()[..], signature[..64], "low s");
    secp.verify_ecdsa(&digest, &plain, &public)
        .expect("verifies with libsecp256k1");
    independent_address(&public.serialize())
}

fn vault_config(address: &str) -> VaultConfig {
    VaultConfig {
        address: address.to_owned(),
        mount: AUTH.into(),
        role: ROLE.into(),
        sts_region: "us-east-2".into(),
        server_id: "vault.example".into(),
    }
}

fn client(address: &str) -> Client {
    let credentials = Credentials::new(
        "AKIDEXAMPLE",
        "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
        Some("SESSION".into()),
        None,
        "test",
    );
    Client::with_credentials(&vault_config(address), credentials).unwrap()
}

fn plugin(address: &str, mount: &str) -> VaultPlugin {
    VaultPlugin::new(
        Arc::new(Resolver::with_client(Some(client(address)))),
        mount,
    )
}

#[tokio::test]
#[ignore = "needs vault server -dev with the signing plugin mounted (BEZENT_VAULT_DEV_ADDR, BEZENT_VAULT_PLUGIN_*); run by CI"]
async fn req_cu_1_inv_9_the_plugin_engine_against_the_plugin_in_vault() {
    let address = env("BEZENT_VAULT_DEV_ADDR");
    let root = env("BEZENT_VAULT_DEV_TOKEN");
    let mount = env("BEZENT_VAULT_PLUGIN_MOUNT");
    let name = env("BEZENT_VAULT_PLUGIN_NAME");
    let sha256 = env("BEZENT_VAULT_PLUGIN_SHA256");

    // The plugin mounted is the one registered with the job's SHA-256.
    let (status, entry) = call(
        &address,
        &root,
        reqwest::Method::GET,
        &format!("sys/plugins/catalog/secret/{name}"),
        None,
    )
    .await;
    assert_eq!(status, 200, "{entry}");
    assert_eq!(entry["data"]["sha256"], sha256);
    let (_, mounts) = call(&address, &root, reqwest::Method::GET, "sys/mounts", None).await;
    let mounted = &mounts["data"][format!("{mount}/")];
    assert!(
        mounted["type"] == name.as_str() || mounted["config"]["plugin_name"] == name.as_str(),
        "{mounted}"
    );

    // The engine's role: its own auth mount, bound to one principal, a token of a minute, and a
    // policy of update on its own key's sign/hash and nothing else.
    let sts = fake_sts().await;
    admin(
        &address,
        &root,
        &format!("sys/auth/{AUTH}"),
        serde_json::json!({"type": "aws"}),
    )
    .await;
    admin(
        &address,
        &root,
        &format!("auth/{AUTH}/config/client"),
        serde_json::json!({
            "sts_endpoint": sts, "sts_region": "us-east-2",
            "iam_server_id_header_value": "vault.example",
        }),
    )
    .await;
    admin(
        &address,
        &root,
        &format!("auth/{AUTH}/role/{ROLE}"),
        serde_json::json!({
            "auth_type": "iam", "bound_iam_principal_arn": CALLER,
            "resolve_aws_unique_ids": false, "token_policies": POLICY,
            "token_ttl": "60s", "token_max_ttl": "120s", "token_no_default_policy": true,
        }),
    )
    .await;
    let policy =
        format!("path \"{mount}/keys/{KEY}/sign/hash\" {{ capabilities = [\"update\"] }}\n");
    admin(
        &address,
        &root,
        &format!("sys/policies/acl/{POLICY}"),
        serde_json::json!({ "policy": policy }),
    )
    .await;

    // Two keys, created inside Vault; the administrator reads the public halves.
    let mut public = Vec::new();
    for id in [KEY, OTHER] {
        admin(
            &address,
            &root,
            &format!("{mount}/keys"),
            serde_json::json!({"id": id, "curve": "secp256k1"}),
        )
        .await;
        let (status, key) = call(
            &address,
            &root,
            reqwest::Method::GET,
            &format!("{mount}/keys/{id}"),
            None,
        )
        .await;
        assert_eq!(status, 200, "{key}");
        let text = key["data"]["public_key"].as_str().unwrap();
        let bytes = hex::decode(text.strip_prefix("0x").unwrap_or(text)).unwrap();
        assert_eq!(bytes.len(), 33, "compressed");
        // No private material in the description.
        let fields: Vec<_> = key["data"].as_object().unwrap().keys().cloned().collect();
        assert!(fields.iter().all(|f| !f.contains("private")), "{fields:?}");
        public.push(bytes);
    }
    let address_of_key = independent_address(&public[0]);
    let declared = PluginKey {
        id: "polygon-delivery".into(),
        key: KEY.into(),
        address: format!("0x{}", hex::encode(address_of_key)),
        public_key: format!("0x{}", hex::encode(&public[0])),
    };

    // The conformance suite against the plugin itself.
    let engine = PluginBackend::new(
        plugin(&address, &mount),
        Environment::Test,
        std::slice::from_ref(&declared),
    )
    .unwrap();
    assert_eq!(engine.address("polygon-delivery").unwrap(), address_of_key);
    run(Setup {
        engine: &engine,
        cases: vec![Case {
            id: "polygon-delivery",
            scheme: Scheme::Secp256k1Evm,
            public_key: public[0].clone(),
            non_exportable: true,
        }],
        secrets: vec![],
    })
    .await;

    // Every signature recovers the address and verifies, by libsecp256k1; both recovery ids occur.
    let mut seen = [false; 2];
    for i in 0..24u8 {
        let message = [PAYLOAD, &[i]].concat();
        let signature = engine.sign("polygon-delivery", &message).await.unwrap();
        assert_eq!(independent_recover(&message, &signature), address_of_key);
        seen[usize::from(signature[64])] = true;
        // Deterministic, as the trial found.
        assert_eq!(
            engine.sign("polygon-delivery", &message).await.unwrap(),
            signature
        );
    }
    assert_eq!(seen, [true, true], "v = 0 and v = 1 both occur");

    // A key that is not the declared one: the plugin signs with KEY, the configuration declares
    // OTHER's key. Refused at the first signature, nothing returned (INV-9).
    let wrong = PluginKey {
        address: format!("0x{}", hex::encode(independent_address(&public[1]))),
        public_key: format!("0x{}", hex::encode(&public[1])),
        ..declared.clone()
    };
    let rogue = PluginBackend::new(plugin(&address, &mount), Environment::Test, &[wrong]).unwrap();
    let err = rogue.sign("polygon-delivery", PAYLOAD).await.unwrap_err();
    assert!(
        matches!(&err, KeyError::Backend(m) if m.contains("INV-9")),
        "{err}"
    );

    // The role's tokens carry its policy alone: not Vault's `default` policy either.
    let (status, role) = call(
        &address,
        &root,
        reqwest::Method::GET,
        &format!("auth/{AUTH}/role/{ROLE}"),
        None,
    )
    .await;
    assert_eq!(status, 200, "{role}");
    assert_eq!(role["data"]["token_no_default_policy"], true, "{role}");
    assert_eq!(
        role["data"]["token_policies"],
        serde_json::json!([POLICY]),
        "{role}"
    );

    // The role signs its own key and nothing else.
    let session = client(&address).login().await.unwrap();
    let hash = [0x5a; 32];
    assert!(session.plugin_sign_hash(&mount, KEY, &hash).await.is_ok());
    assert!(matches!(
        session.plugin_sign_hash(&mount, OTHER, &hash).await,
        Err(Error::Status { status: 403, .. })
    ));
    assert!(matches!(
        session
            .read::<serde_json::Value>(&format!("{mount}/keys/{KEY}"))
            .await,
        Err(Error::Status { status: 403, .. })
    ));
    assert!(matches!(
        session.plugin_sign_hash(&mount, KEY, &hash[..31]).await,
        Err(Error::Plugin(_))
    ));
    let other_engine = PluginBackend::new(
        plugin(&address, &mount),
        Environment::Test,
        &[PluginKey {
            key: OTHER.into(),
            address: format!("0x{}", hex::encode(independent_address(&public[1]))),
            public_key: format!("0x{}", hex::encode(&public[1])),
            ..declared.clone()
        }],
    )
    .unwrap();
    let denied = other_engine
        .sign("polygon-delivery", PAYLOAD)
        .await
        .unwrap_err();
    assert!(
        matches!(&denied, KeyError::Backend(_)) && !denied.is_transient(),
        "a denial is not worth waiting for: {denied}"
    );

    // The policy itself, with a token that carries it and nothing else: every other route of the
    // plugin is refused.
    let token = admin(
        &address,
        &root,
        "auth/token/create",
        serde_json::json!({"policies": [POLICY], "no_default_policy": true, "ttl": "60s"}),
    )
    .await["auth"]["client_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let hex_hash = hex::encode(hash);
    let ok = call(
        &address,
        &token,
        reqwest::Method::POST,
        &format!("{mount}/keys/{KEY}/sign/hash"),
        Some(serde_json::json!({"hash": hex_hash})),
    )
    .await;
    assert_eq!(ok.0, 200, "update alone signs: {}", ok.1);
    for (method, path, body) in [
        (reqwest::Method::GET, format!("{mount}/keys/{KEY}"), None),
        (
            reqwest::Method::from_bytes(b"LIST").unwrap(),
            format!("{mount}/keys"),
            None,
        ),
        (
            reqwest::Method::POST,
            format!("{mount}/keys/{KEY}/sign/message"),
            Some(serde_json::json!({"message": "68656c6c6f", "hash_function": "keccak256"})),
        ),
        (
            reqwest::Method::POST,
            format!("{mount}/keys/{OTHER}/sign/hash"),
            Some(serde_json::json!({"hash": hex_hash})),
        ),
        (
            reqwest::Method::POST,
            format!("{mount}/keys"),
            Some(serde_json::json!({"id": "bezent-new", "curve": "secp256k1"})),
        ),
        (reqwest::Method::DELETE, format!("{mount}/keys/{KEY}"), None),
    ] {
        let (status, answer) = call(&address, &token, method.clone(), &path, body).await;
        assert_eq!(status, 403, "{method} {path}: {answer}");
    }
}

/// The independent check itself, against a local key, so the job's run is not its first.
#[tokio::test]
async fn req_ky_3_the_independent_recovery_agrees_with_the_engine_on_a_local_key() {
    use conformance::doubles::{LocalPlugin, secp_key};
    let key = secp_key(21);
    let public = key
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .to_vec();
    let address = independent_address(&public);
    let engine = PluginBackend::new(
        LocalPlugin::new(key),
        Environment::Test,
        &[PluginKey {
            id: "polygon-delivery".into(),
            key: KEY.into(),
            address: format!("0x{}", hex::encode(address)),
            public_key: format!("0x{}", hex::encode(&public)),
        }],
    )
    .unwrap();
    assert_eq!(engine.address("polygon-delivery").unwrap(), address);
    for i in 0..16u8 {
        let message = [PAYLOAD, &[i]].concat();
        let signature = engine.sign("polygon-delivery", &message).await.unwrap();
        assert_eq!(independent_recover(&message, &signature), address);
    }
}
