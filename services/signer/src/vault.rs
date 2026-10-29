// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The Vault loader for the keys that are not delivery keys (`T-005-03`, issue #5; PLAN `DEC-11`):
//! the statement key (`REQ-PR-1`) and the pause key. They are read once at start, by the signer
//! process only, and held by a [`FileBackend`], so they sit behind the same [`KeyBackend`] trait.
//!
//! The signer logs in to Vault itself under its task role (the AWS auth method, `bezent-vault`;
//! issue #64): no token is read from a file or the environment (`REQ-KY-1`), and the one the login
//! returns lives for a minute. The secret at `path` is a JSON object in the shape of the file
//! backend's key file, with the environment it belongs to:
//!
//! ```json
//! { "environment": "production",
//!   "keys": [ { "id": "statement", "scheme": "ed25519", "seed": "0x…" } ] }
//! ```
//!
//! Both KV versions are read (`bezent_vault` unwraps `.data` and `.data.data`). The secret's
//! `environment` must equal the environment the configuration declares for the custody
//! (`REQ-KY-6`). The server must be reached over HTTPS, except on a loopback address (the client
//! refuses anything else).

use bezent_vault::Resolver;
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::keys::{Environment, FileBackend, KeyError, Scheme};

/// Where the Vault keys are.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultSource {
    /// The secret's path under `/v1/`, for example `secret/bezent/signer`. Where Vault is, and how
    /// to log in to it, is the configuration's `vault` table.
    pub path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Secret {
    environment: String,
    keys: Vec<SecretKey>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretKey {
    id: String,
    scheme: Scheme,
    seed: Zeroizing<String>,
}

/// Builds the backend of `environment` from the secret.
fn build_secret(secret: Secret, environment: Environment) -> Result<FileBackend, KeyError> {
    let bad = || KeyError::Source("the Vault secret is not a valid key secret".to_owned());
    let declared = match secret.environment.as_str() {
        "test" => Environment::Test,
        "production" => Environment::Production,
        _ => return Err(bad()),
    };
    if declared != environment {
        return Err(KeyError::Refused(format!(
            "the Vault secret is {declared} and the configuration is {environment} (REQ-KY-6, AC-I4.2)"
        )));
    }
    let entries = secret
        .keys
        .into_iter()
        .map(|k| (k.id, k.scheme, k.seed))
        .collect();
    let backend = FileBackend::from_seeds(environment, entries)?;
    if environment == Environment::Production {
        let fixture = crate::kms::fixture_public_keys();
        for id in backend.ids() {
            use crate::keys::KeyBackend as _;
            if backend.public_key(id).is_ok_and(|k| fixture.contains(&k)) {
                return Err(KeyError::Refused(format!(
                    "key {id}: a published test fixture's key, refused in production (REQ-KY-6)"
                )));
            }
        }
    }
    Ok(backend)
}

/// Reads the secret from Vault, logged in as the `resolver`'s role.
pub async fn load(
    source: &VaultSource,
    environment: Environment,
    resolver: &Resolver,
) -> Result<FileBackend, KeyError> {
    let session = resolver
        .session()
        .await
        .map_err(|e| KeyError::Source(e.to_string()))?;
    let secret: Secret = session
        .read(&source.path)
        .await
        .map_err(|e| KeyError::Source(e.to_string()))?;
    build_secret(secret, environment)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::KeyBackend;

    const SEED: &str = "0x0202020202020202020202020202020202020202020202020202020202020202";

    fn secret(environment: &str) -> String {
        format!(
            r#"{{"environment":"{environment}","keys":[{{"id":"statement","scheme":"ed25519","seed":"{SEED}"}}]}}"#
        )
    }

    fn parse(body: &str, environment: Environment) -> Result<FileBackend, KeyError> {
        let secret: Secret = serde_json::from_str(body)
            .map_err(|_| KeyError::Source("the Vault secret is not a valid key secret".into()))?;
        build_secret(secret, environment)
    }

    #[test]
    fn req_ky_1_a_key_secret_is_read() {
        let backend = parse(&secret("production"), Environment::Production).unwrap();
        assert_eq!(backend.scheme("statement").unwrap(), Scheme::Ed25519);
        assert_eq!(backend.environment(), Environment::Production);
    }

    #[test]
    fn req_ky_6_a_secret_of_another_environment_is_refused() {
        assert!(matches!(
            parse(&secret("test"), Environment::Production),
            Err(KeyError::Refused(_))
        ));
    }

    /// The seed of the published fixture's statement key: SHA-256 of its documented preimage.
    fn fixture_statement_seed() -> String {
        use sha2::{Digest, Sha256};
        format!(
            "0x{}",
            hex::encode(Sha256::digest(b"bezent published test fixture statement"))
        )
    }

    #[test]
    fn req_ky_6_a_fixture_key_is_refused_in_production_by_vault_too() {
        let body = format!(
            r#"{{"environment":"production","keys":[{{"id":"statement","scheme":"ed25519","seed":"{}"}}]}}"#,
            fixture_statement_seed()
        );
        assert!(matches!(
            parse(&body, Environment::Production),
            Err(KeyError::Refused(m)) if m.contains("test fixture")
        ));
        assert!(parse(&secret("production"), Environment::Production).is_ok());
    }

    #[test]
    fn req_ky_1_a_malformed_or_extended_secret_is_refused() {
        for body in ["{}", r#"{"environment":"test"}"#, "not json"] {
            assert!(matches!(
                parse(body, Environment::Test),
                Err(KeyError::Source(_))
            ));
        }
        let extra = format!(
            r#"{{"environment":"test","extra":1,"keys":[{{"id":"s","scheme":"ed25519","seed":"{SEED}"}}]}}"#
        );
        assert!(parse(&extra, Environment::Test).is_err());
        let unknown = secret("staging");
        assert!(parse(&unknown, Environment::Test).is_err());
    }

    /// A mock Vault answering the IAM login and then the secret, as the transport tests of
    /// `bezent-vault` do: the signer reads its keys through the client.
    #[tokio::test]
    async fn req_ky_1_load_logs_in_and_reads_kv_v1_and_v2() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for body in [
            format!(r#"{{"data":{}}}"#, secret("test")),
            format!(
                r#"{{"data":{{"data":{},"metadata":{{"version":3}}}}}}"#,
                secret("test")
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = format!("http://{}", listener.local_addr().unwrap());
            let login = r#"{"auth":{"client_token":"hvs.t","lease_duration":60}}"#;
            let answers = [login.to_owned(), body];
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = seen.clone();
            tokio::spawn(async move {
                for answer in answers {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut buf = vec![0u8; 16384];
                    let n = stream.read(&mut buf).await.unwrap();
                    log.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&buf[..n]).into_owned());
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                        answer.len()
                    );
                    stream.write_all(response.as_bytes()).await.unwrap();
                    let _ = stream.shutdown().await;
                }
            });
            let config = bezent_vault::VaultConfig {
                address,
                mount: "bezent".into(),
                role: "bezent-signer-iam".into(),
                sts_region: "us-east-2".into(),
                server_id: "vault.example".into(),
            };
            let credentials =
                aws_credential_types::Credentials::new("AKIDEXAMPLE", "SECRET", None, None, "test");
            let client = bezent_vault::Client::with_credentials(&config, credentials).unwrap();
            let resolver = Resolver::with_client(Some(client));
            let source = VaultSource {
                path: "secret/bezent/signer".into(),
            };
            let backend = load(&source, Environment::Test, &resolver).await.unwrap();
            assert_eq!(backend.scheme("statement").unwrap(), Scheme::Ed25519);
            let seen = seen.lock().unwrap();
            assert!(seen[0].starts_with("POST /v1/auth/bezent/login "));
            assert!(seen[1].starts_with("GET /v1/secret/bezent/signer "));
            assert!(seen[1].to_lowercase().contains("x-vault-token: hvs.t\r\n"));
        }
    }
}
