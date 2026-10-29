// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The Vault Transit key backend (the Fargate design's ruling `d-delivery-keys`; `REQ-CU-1`,
//! `REQ-KY-1`, `REQ-KY-3`, `REQ-KY-4`, `REQ-KY-6`, `INV-9`).
//!
//! A delivery key of the Kreivo network is a native ed25519 account whose key lives in a Vault
//! Transit engine and never leaves it: Vault signs on request. The signer asks Transit to sign the
//! message itself (pure Ed25519, `prehashed` false, RFC 8032), which is what a Kreivo ed25519
//! account signs: the SCALE signing payload, or its BLAKE2b-256 past 256 bytes, as the core
//! recorded it. The account is the public key.
//!
//! At start [`TransitBackend::connect`] reads every configured key (`keys/<key>`, newest version),
//! requires an ed25519 key Vault will not export ([`bezent_vault::Session::transit_key`]),
//! compares its public key with the account the configuration declares (`REQ-KY-3`) and, in
//! production, refuses a published test fixture's key (`REQ-KY-6`). The version read is the one
//! every signature asks for, so a rotation in Vault changes nothing until the configuration is
//! changed. Each signature is verified against the public key read at start before it is returned
//! (`INV-9`): a wrong answer, or one for another version, is an error and nothing is recorded.
//!
//! The signer logs in under its task role for each signature ([`bezent_vault::Resolver::session`]
//! reuses a login for a few seconds at most, so a token is never used past its lease). Any failure
//! is an error and the intent is retried later; nothing falls back to another key. No message,
//! signature or token is logged or put in an error.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::sync::Arc;

use bezent_vault::Resolver;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;

use crate::keys::{Environment, KeyBackend, KeyError, Scheme};

/// The Transit calls the backend needs. [`VaultTransit`] is the real one; tests implement it with
/// a local key.
pub trait TransitApi: Send + Sync + 'static {
    /// The newest version of key `key` and its public key.
    fn public_key(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<(u32, [u8; 32]), KeyError>> + Send;

    /// Version `version` of `key` signs `message`: 64 bytes.
    fn sign(
        &self,
        key: &str,
        version: u32,
        message: &[u8],
    ) -> impl Future<Output = Result<[u8; 64], KeyError>> + Send;
}

/// One Transit key the configuration names.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransitKey {
    /// The signer's id for the key (what bindings name).
    pub id: String,
    /// The key's name in the Transit mount.
    pub key: String,
    /// The Kreivo account, which for an ed25519 account is the 32-byte public key, `0x` hex.
    pub account: String,
}

/// The Transit engine of a Vault server, reached as the signer's role.
pub struct VaultTransit {
    resolver: Arc<Resolver>,
    mount: String,
}

impl VaultTransit {
    /// Transit at `mount`, through `resolver` (which holds the `[vault]` table's client).
    pub fn new(resolver: Arc<Resolver>, mount: &str) -> Self {
        Self {
            resolver,
            mount: mount.to_owned(),
        }
    }
}

fn failed(e: bezent_vault::Error) -> KeyError {
    KeyError::Backend(e.to_string())
}

impl TransitApi for VaultTransit {
    async fn public_key(&self, key: &str) -> Result<(u32, [u8; 32]), KeyError> {
        let session = self.resolver.session().await.map_err(failed)?;
        let key = session
            .transit_key(&self.mount, key)
            .await
            .map_err(failed)?;
        Ok((key.version, key.public_key))
    }

    async fn sign(&self, key: &str, version: u32, message: &[u8]) -> Result<[u8; 64], KeyError> {
        let session = self.resolver.session().await.map_err(failed)?;
        session
            .transit_sign(&self.mount, key, version, message)
            .await
            .map_err(failed)
    }
}

struct Held {
    key: String,
    version: u32,
    public_key: [u8; 32],
    verifying: VerifyingKey,
}

/// Delivery keys held in Vault Transit.
pub struct TransitBackend<A: TransitApi> {
    api: A,
    environment: Environment,
    keys: BTreeMap<String, Held>,
}

fn refused(why: String) -> KeyError {
    KeyError::Refused(why)
}

impl<A: TransitApi> TransitBackend<A> {
    /// Reads and checks every key (see the module documentation). `environment` is the
    /// environment the configuration declares for this custody.
    pub async fn connect(
        api: A,
        environment: Environment,
        keys: &[TransitKey],
    ) -> Result<Self, KeyError> {
        let fixture = crate::kms::fixture_public_keys();
        let mut held = BTreeMap::new();
        for k in keys {
            let id = &k.id;
            if id.is_empty() || held.contains_key(id) {
                return Err(KeyError::Source(format!(
                    "key id {id:?} is empty or repeated"
                )));
            }
            let declared = hex::decode(k.account.strip_prefix("0x").unwrap_or(&k.account))
                .ok()
                .and_then(|a| <[u8; 32]>::try_from(a).ok())
                .ok_or_else(|| {
                    refused(format!(
                        "key {id}: an ed25519 key declares its 32-byte account, 0x hex"
                    ))
                })?;
            let (version, public_key) = api.public_key(&k.key).await?;
            if declared != public_key {
                return Err(refused(format!(
                    "key {id}: the Transit public key is not the configured account (REQ-KY-3)"
                )));
            }
            let verifying = VerifyingKey::from_bytes(&public_key).map_err(|_| {
                KeyError::Source(format!(
                    "key {id}: Transit returned an unreadable public key"
                ))
            })?;
            if environment == Environment::Production && fixture.contains(&public_key.to_vec()) {
                return Err(refused(format!(
                    "key {id}: its public key is a published test fixture's; refused in production \
                     (REQ-KY-6, AC-I4.2)"
                )));
            }
            held.insert(
                id.clone(),
                Held {
                    key: k.key.clone(),
                    version,
                    public_key,
                    verifying,
                },
            );
        }
        Ok(Self {
            api,
            environment,
            keys: held,
        })
    }

    /// The ids of the keys held.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.keys.keys().map(String::as_str)
    }

    fn held(&self, id: &str) -> Result<&Held, KeyError> {
        self.keys
            .get(id)
            .ok_or_else(|| KeyError::UnknownKey(id.to_owned()))
    }
}

impl<A: TransitApi> KeyBackend for TransitBackend<A> {
    fn environment(&self) -> Environment {
        self.environment
    }

    fn scheme(&self, id: &str) -> Result<Scheme, KeyError> {
        self.held(id).map(|_| Scheme::Ed25519)
    }

    fn public_key(&self, id: &str) -> Result<Vec<u8>, KeyError> {
        self.held(id).map(|k| k.public_key.to_vec())
    }

    fn sign(
        &self,
        id: &str,
        message: &[u8],
    ) -> impl Future<Output = Result<Vec<u8>, KeyError>> + Send {
        let held = self.held(id);
        async move {
            let held = held?;
            let signature = self.api.sign(&held.key, held.version, message).await?;
            // INV-9: what leaves is what the key's public half verifies, strictly (RFC 8032).
            let parsed = Signature::from_bytes(&signature);
            if held.verifying.verify_strict(message, &parsed).is_err() {
                return Err(KeyError::Backend(format!(
                    "key {id}: Transit's signature does not verify against the key (INV-9)"
                )));
            }
            Ok(signature.to_vec())
        }
    }
}

/// Key ids, schemes and public keys only (`REQ-KY-1`).
impl<A: TransitApi> fmt::Debug for TransitBackend<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut map = f.debug_map();
        for (id, key) in &self.keys {
            map.entry(
                id,
                &format_args!(
                    "transit ed25519 v{} 0x{}",
                    key.version,
                    hex::encode(key.public_key)
                ),
            );
        }
        map.finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use ed25519_dalek::Signer as _;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::custody::Mixed;
    use crate::keys::FileBackend;

    fn signing_key(label: u8) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[label; 32])
    }

    fn account(key: &ed25519_dalek::SigningKey) -> String {
        format!("0x{}", hex::encode(key.verifying_key().to_bytes()))
    }

    fn named(id: &str, key: &ed25519_dalek::SigningKey) -> TransitKey {
        TransitKey {
            id: id.into(),
            key: "kreivo-delivery".into(),
            account: account(key),
        }
    }

    /// A Transit that signs with a local key: what Vault would do. `rogue` signs with another key
    /// than it publishes; `down` fails.
    struct Local {
        key: ed25519_dalek::SigningKey,
        rogue: Option<ed25519_dalek::SigningKey>,
        down: Mutex<bool>,
        asked: Mutex<Vec<(String, u32)>>,
    }

    impl Local {
        fn new(key: ed25519_dalek::SigningKey) -> Self {
            Self {
                key,
                rogue: None,
                down: Mutex::new(false),
                asked: Mutex::default(),
            }
        }
    }

    impl TransitApi for Local {
        async fn public_key(&self, _key: &str) -> Result<(u32, [u8; 32]), KeyError> {
            Ok((3, self.key.verifying_key().to_bytes()))
        }

        async fn sign(
            &self,
            key: &str,
            version: u32,
            message: &[u8],
        ) -> Result<[u8; 64], KeyError> {
            self.asked.lock().unwrap().push((key.to_owned(), version));
            if *self.down.lock().unwrap() {
                return Err(KeyError::Backend("Vault is unreachable".into()));
            }
            Ok(self
                .rogue
                .as_ref()
                .unwrap_or(&self.key)
                .sign(message)
                .to_bytes())
        }
    }

    #[tokio::test]
    async fn req_cu_1_transit_signs_the_message_and_the_signature_verifies() {
        let key = signing_key(1);
        let backend = TransitBackend::connect(
            Local::new(key.clone()),
            Environment::Test,
            &[named("kreivo-delivery", &key)],
        )
        .await
        .unwrap();
        assert_eq!(backend.scheme("kreivo-delivery").unwrap(), Scheme::Ed25519);
        assert_eq!(
            backend.public_key("kreivo-delivery").unwrap(),
            key.verifying_key().to_bytes()
        );
        let signature = backend.sign("kreivo-delivery", b"payload").await.unwrap();
        assert_eq!(signature.len(), 64);
        key.verifying_key()
            .verify_strict(b"payload", &Signature::from_slice(&signature).unwrap())
            .expect("RFC 8032 over the message itself");
        // The version read at start is the one every signature asks for.
        assert_eq!(
            backend.api.asked.lock().unwrap().as_slice(),
            [("kreivo-delivery".to_owned(), 3)]
        );
        assert!(matches!(
            backend.sign("other", b"x").await,
            Err(KeyError::UnknownKey(_))
        ));
    }

    #[tokio::test]
    async fn req_ky_3_a_key_that_is_not_the_configured_account_is_refused() {
        let (key, other) = (signing_key(1), signing_key(2));
        let err = TransitBackend::connect(
            Local::new(key),
            Environment::Test,
            &[named("kreivo-delivery", &other)],
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, KeyError::Refused(m) if m.contains("REQ-KY-3")),
            "{err}"
        );
        for account in ["not hex", "0x1234", ""] {
            let key = signing_key(1);
            let mut entry = named("kreivo-delivery", &key);
            entry.account = account.into();
            assert!(matches!(
                TransitBackend::connect(Local::new(key), Environment::Test, &[entry]).await,
                Err(KeyError::Refused(_))
            ));
        }
    }

    #[tokio::test]
    async fn inv_9_a_signature_that_does_not_verify_is_never_returned() {
        let key = signing_key(1);
        let mut api = Local::new(key.clone());
        api.rogue = Some(signing_key(9));
        let backend =
            TransitBackend::connect(api, Environment::Test, &[named("kreivo-delivery", &key)])
                .await
                .unwrap();
        let err = backend
            .sign("kreivo-delivery", b"payload")
            .await
            .unwrap_err();
        assert!(
            matches!(&err, KeyError::Backend(m) if m.contains("INV-9")),
            "{err}"
        );
        // Nor a signature over other bytes.
        let good = Local::new(key.clone());
        let backend =
            TransitBackend::connect(good, Environment::Test, &[named("kreivo-delivery", &key)])
                .await
                .unwrap();
        assert!(backend.sign("kreivo-delivery", b"payload").await.is_ok());
    }

    #[tokio::test]
    async fn req_ky_1_a_failing_vault_fails_closed_and_the_next_signature_works() {
        let key = signing_key(1);
        let backend = TransitBackend::connect(
            Local::new(key.clone()),
            Environment::Test,
            &[named("kreivo-delivery", &key)],
        )
        .await
        .unwrap();
        *backend.api.down.lock().unwrap() = true;
        assert!(matches!(
            backend.sign("kreivo-delivery", b"payload").await,
            Err(KeyError::Backend(_))
        ));
        *backend.api.down.lock().unwrap() = false;
        assert!(backend.sign("kreivo-delivery", b"payload").await.is_ok());
    }

    #[tokio::test]
    async fn req_ky_6_a_published_test_fixtures_key_is_refused_in_production() {
        use sha2::{Digest, Sha256};
        let seed: [u8; 32] = Sha256::digest(b"bezent published test fixture statement").into();
        let fixture = ed25519_dalek::SigningKey::from_bytes(&seed);
        let entry = named("kreivo-delivery", &fixture);
        let err = TransitBackend::connect(
            Local::new(fixture.clone()),
            Environment::Production,
            std::slice::from_ref(&entry),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, KeyError::Refused(m) if m.contains("test fixture")),
            "{err}"
        );
        assert!(
            TransitBackend::connect(Local::new(fixture), Environment::Test, &[entry])
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn req_ky_1_ids_are_not_empty_or_repeated_and_the_debug_output_has_no_secret() {
        let key = signing_key(1);
        let mut entry = named("", &key);
        assert!(
            TransitBackend::connect(Local::new(key.clone()), Environment::Test, &[entry.clone()])
                .await
                .is_err()
        );
        entry.id = "k".into();
        assert!(
            TransitBackend::connect(
                Local::new(key.clone()),
                Environment::Test,
                &[entry.clone(), entry.clone()]
            )
            .await
            .is_err()
        );
        let backend = TransitBackend::connect(Local::new(key.clone()), Environment::Test, &[entry])
            .await
            .unwrap();
        let shown = format!("{backend:?}");
        assert!(
            shown.contains("transit ed25519 v3")
                && shown.contains(&hex::encode(key.verifying_key().to_bytes()))
        );
        assert!(!shown.contains(&hex::encode(key.to_bytes())));
    }

    #[tokio::test]
    async fn req_ky_4_a_transit_key_and_a_vault_key_may_not_share_an_id() {
        let key = signing_key(1);
        let transit = || async {
            TransitBackend::connect(
                Local::new(key.clone()),
                Environment::Test,
                &[named("kreivo-delivery", &key)],
            )
            .await
        };
        let seed = Zeroizing::new(format!("0x{}", hex::encode([7u8; 32])));
        let kv = |id: &str, environment| {
            FileBackend::from_seeds(
                environment,
                vec![(id.into(), Scheme::Ed25519, seed.clone())],
            )
            .unwrap()
        };
        let err = Mixed::new(
            transit().await.unwrap(),
            Some(kv("kreivo-delivery", Environment::Test)),
        )
        .expect_err("refused");
        assert!(
            err.to_string().contains("both Vault Transit and Vault"),
            "{err}"
        );
        let err = Mixed::new(
            transit().await.unwrap(),
            Some(kv("statement", Environment::Production)),
        )
        .expect_err("refused");
        assert!(matches!(err, KeyError::Refused(_)), "{err}");
        let mixed = Mixed::new(
            transit().await.unwrap(),
            Some(kv("statement", Environment::Test)),
        )
        .unwrap();
        // The Transit key is non-exportable; the one read from Vault KV is in the signer's memory.
        assert!(mixed.non_exportable("kreivo-delivery"));
        assert!(!mixed.non_exportable("statement"));
    }

    use zeroize::Zeroizing;

    // ------------------------------------------------------------ against a mock Vault

    /// A mock Vault answering one canned response per connection; returns its address and the raw
    /// requests it saw.
    async fn mock(answers: Vec<String>) -> (String, std::sync::Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            for answer in answers {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut request = Vec::new();
                let mut buf = vec![0u8; 16384];
                loop {
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    request.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&request).into_owned();
                    let Some((head, body)) = text.split_once("\r\n\r\n") else {
                        if n == 0 {
                            break;
                        }
                        continue;
                    };
                    let wanted = head
                        .to_lowercase()
                        .lines()
                        .find_map(|l| {
                            l.strip_prefix("content-length: ")?
                                .trim()
                                .parse::<usize>()
                                .ok()
                        })
                        .unwrap_or(0);
                    if body.len() >= wanted || n == 0 {
                        break;
                    }
                }
                log.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&request).into_owned());
                let _ = stream.write_all(answer.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        (address, seen)
    }

    fn ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn failed(code: u16) -> String {
        format!("HTTP/1.1 {code} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
    }

    /// A login whose lease is 0, so no session is ever reused: every call logs in again.
    fn login(token: &str, lease: u64) -> String {
        ok(&format!(
            r#"{{"auth":{{"client_token":"{token}","lease_duration":{lease}}}}}"#
        ))
    }

    fn key_answer(key: &ed25519_dalek::SigningKey, exportable: bool) -> String {
        use base64::Engine as _;
        let public =
            base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_bytes());
        ok(&format!(
            r#"{{"data":{{"type":"ed25519","exportable":{exportable},"derived":false,"allow_plaintext_backup":false,"supports_signing":true,"latest_version":1,"keys":{{"1":{{"public_key":"{public}"}}}}}}}}"#
        ))
    }

    fn signature_answer(signature: &[u8], version: u32) -> String {
        use base64::Engine as _;
        ok(&format!(
            r#"{{"data":{{"signature":"vault:v{version}:{}","key_version":{version}}}}}"#,
            base64::engine::general_purpose::STANDARD.encode(signature)
        ))
    }

    fn transit_at(address: &str) -> VaultTransit {
        let config = bezent_vault::VaultConfig {
            address: address.to_owned(),
            mount: "bezent".into(),
            role: "bezent-signer-iam".into(),
            sts_region: "us-east-2".into(),
            server_id: "vault.example".into(),
        };
        let credentials =
            aws_credential_types::Credentials::new("AKIDEXAMPLE", "SECRET", None, None, "test");
        let client = bezent_vault::Client::with_credentials(&config, credentials).unwrap();
        VaultTransit::new(
            Arc::new(Resolver::with_client(Some(client))),
            "transit-bezent-signer",
        )
    }

    #[tokio::test]
    async fn req_cu_1_transit_through_vault_logs_in_reads_the_key_and_signs_the_message() {
        let key = signing_key(1);
        let signature = key.sign(b"payload").to_bytes();
        // One login serves the start and the first signature: a session is reused for a few
        // seconds at most.
        let (address, seen) = mock(vec![
            login("hvs.one", 60),
            key_answer(&key, false),
            signature_answer(&signature, 1),
        ])
        .await;
        let backend = TransitBackend::connect(
            transit_at(&address),
            Environment::Test,
            &[named("kreivo-delivery", &key)],
        )
        .await
        .unwrap();
        let signed = backend.sign("kreivo-delivery", b"payload").await.unwrap();
        assert_eq!(signed, signature);
        let seen = seen.lock().unwrap();
        assert!(
            seen[0].starts_with("POST /v1/auth/bezent/login "),
            "{}",
            seen[0]
        );
        assert!(seen[1].starts_with("GET /v1/transit-bezent-signer/keys/kreivo-delivery "));
        assert!(
            seen[1]
                .to_lowercase()
                .contains("x-vault-token: hvs.one\r\n")
        );
        let sign = &seen[2];
        assert!(
            sign.starts_with("POST /v1/transit-bezent-signer/sign/kreivo-delivery "),
            "{sign}"
        );
        assert!(sign.to_lowercase().contains("x-vault-token: hvs.one\r\n"));
        let body: serde_json::Value =
            serde_json::from_str(sign.split_once("\r\n\r\n").unwrap().1).unwrap();
        use base64::Engine as _;
        assert_eq!(
            body["input"],
            base64::engine::general_purpose::STANDARD.encode(b"payload")
        );
        assert_eq!(body["prehashed"], false);
        assert_eq!(body["key_version"], 1);
    }

    #[tokio::test]
    async fn req_ky_1_an_expired_session_is_replaced_by_a_new_login() {
        let key = signing_key(1);
        let signature = key.sign(b"payload").to_bytes();
        // A lease of 0 is never fresh: the connect and each signature log in on their own.
        let (address, seen) = mock(vec![
            login("hvs.one", 0),
            key_answer(&key, false),
            login("hvs.two", 0),
            signature_answer(&signature, 1),
            login("hvs.three", 0),
            signature_answer(&signature, 1),
        ])
        .await;
        let backend = TransitBackend::connect(
            transit_at(&address),
            Environment::Test,
            &[named("kreivo-delivery", &key)],
        )
        .await
        .unwrap();
        backend.sign("kreivo-delivery", b"payload").await.unwrap();
        backend.sign("kreivo-delivery", b"payload").await.unwrap();
        let seen = seen.lock().unwrap();
        let logins = seen
            .iter()
            .filter(|r| r.contains("/auth/bezent/login"))
            .count();
        assert_eq!(logins, 3, "{seen:#?}");
        let tokens: Vec<_> = seen
            .iter()
            .filter(|r| !r.contains("/auth/"))
            .map(|r| {
                r.to_lowercase()
                    .lines()
                    .find(|l| l.starts_with("x-vault-token"))
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            tokens,
            [
                "x-vault-token: hvs.one",
                "x-vault-token: hvs.two",
                "x-vault-token: hvs.three"
            ]
        );
    }

    #[tokio::test]
    async fn req_ky_1_vault_errors_and_wrong_answers_fail_closed_without_a_value() {
        let key = signing_key(1);
        let other = signing_key(2);
        let wrong = other.sign(b"payload").to_bytes();
        let good = key.sign(b"payload").to_bytes();
        let cases = [
            ("a 403", failed(403)),
            ("a 500", failed(500)),
            ("another key's signature", signature_answer(&wrong, 1)),
            ("another version", signature_answer(&good, 2)),
            ("no signature", ok(r#"{"data":{}}"#)),
            ("a short signature", signature_answer(&good[..60], 1)),
        ];
        for (name, answer) in cases {
            // A 500 is retried for a read but a signature is never retried: one answer is enough.
            let (address, seen) =
                mock(vec![login("hvs.t", 60), key_answer(&key, false), answer]).await;
            let backend = TransitBackend::connect(
                transit_at(&address),
                Environment::Test,
                &[named("kreivo-delivery", &key)],
            )
            .await
            .unwrap();
            let err = backend
                .sign("kreivo-delivery", b"payload")
                .await
                .unwrap_err();
            assert!(matches!(err, KeyError::Backend(_)), "{name}: {err}");
            let shown = format!("{err} {backend:?}");
            assert!(!shown.contains("hvs.t"), "{name}");
            assert_eq!(seen.lock().unwrap().len(), 3, "{name}: one signing request");
        }
    }

    #[tokio::test]
    async fn req_cu_1_a_key_vault_would_export_is_refused_at_start() {
        let key = signing_key(1);
        let (address, _) = mock(vec![login("hvs.t", 60), key_answer(&key, true)]).await;
        let err = TransitBackend::connect(
            transit_at(&address),
            Environment::Test,
            &[named("kreivo-delivery", &key)],
        )
        .await
        .unwrap_err();
        assert!(matches!(err, KeyError::Backend(m) if m.contains("exported")));
    }
}
