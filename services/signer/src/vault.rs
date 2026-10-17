// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Vault loader for the keys that are not delivery keys (`T-005-03`, issue #5; PLAN `DEC-11`):
//! the statement key (`REQ-PR-1`) and the pause key. They are read once at start, by the signer
//! process only, and held by a [`FileBackend`], so they sit behind the same [`KeyBackend`] trait.
//!
//! The token is read from a file the configuration names, never from the environment
//! (`REQ-KY-1`). The secret at `path` is a JSON object in the shape of the file backend's key
//! file, with the environment it belongs to:
//!
//! ```json
//! { "environment": "production",
//!   "keys": [ { "id": "statement", "scheme": "ed25519", "seed": "0x…" } ] }
//! ```
//!
//! Both KV versions are read (v1 under `.data`, v2 under `.data.data`). The secret's
//! `environment` must equal the environment the configuration declares for the custody
//! (`REQ-KY-6`). The server must be reached over HTTPS, except on a loopback address.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use zeroize::Zeroizing;

use crate::keys::{Environment, FileBackend, KeyError, Scheme};

/// Where the Vault keys are.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultSource {
    /// The server's base URL, for example `https://vault.internal:8200`.
    pub address: String,
    /// A file holding the Vault token.
    pub token_file: PathBuf,
    /// The secret's path under `/v1/`, for example `secret/bezent/signer`.
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

#[derive(Deserialize)]
struct EnvelopeV1 {
    data: Secret,
}

#[derive(Deserialize)]
struct EnvelopeV2 {
    data: DataV2,
}

#[derive(Deserialize)]
struct DataV2 {
    data: Secret,
    #[allow(dead_code)]
    metadata: serde::de::IgnoredAny,
}

/// The client: no redirects (a standby node's redirect must not carry the token anywhere) and
/// bounded connect and request times, so start-up cannot hang on Vault.
fn client() -> Result<reqwest::Client, KeyError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|_| KeyError::Source("the Vault client could not be built".to_owned()))
}

/// Whether `address` is plain HTTP to a loopback host. The host is the parsed one, so userinfo
/// tricks such as `http://localhost:80@evil.example` name `evil.example` and are not loopback.
fn loopback(address: &str) -> bool {
    let Ok(url) = url::Url::parse(address) else {
        return false;
    };
    if url.scheme() != "http" {
        return false;
    }
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// The URL of the secret, refusing plain HTTP off loopback.
pub fn secret_url(source: &VaultSource) -> Result<String, KeyError> {
    if !(source.address.starts_with("https://") || loopback(&source.address)) {
        return Err(KeyError::Refused(
            "the Vault address must be https (REQ-KY-1)".to_owned(),
        ));
    }
    Ok(format!(
        "{}/v1/{}",
        source.address.trim_end_matches('/'),
        source.path.trim_matches('/')
    ))
}

/// Parses the body Vault answered with (KV v1 or v2) into a backend of `environment`.
pub fn parse_secret(body: &[u8], environment: Environment) -> Result<FileBackend, KeyError> {
    let bad = || KeyError::Source("the Vault secret is not a valid key secret".to_owned());
    // Straight into zeroizing types: no intermediate `serde_json::Value` ever holds a seed. KV v2
    // nests the secret under data.data (next to data.metadata); KV v1 has it under data.
    let secret = match serde_json::from_slice::<EnvelopeV2>(body) {
        Ok(v2) => v2.data.data,
        Err(_) => {
            serde_json::from_slice::<EnvelopeV1>(body)
                .map_err(|_| bad())?
                .data
        }
    };
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

/// Reads the secret from Vault with the token in `source.token_file`.
pub async fn load(source: &VaultSource, environment: Environment) -> Result<FileBackend, KeyError> {
    let url = secret_url(source)?;
    let token = read_token(&source.token_file)?;
    let response = client()?
        .get(&url)
        .header("X-Vault-Token", token.trim())
        .send()
        .await
        .map_err(|e| KeyError::Source(format!("Vault is unreachable ({})", e.without_url())))?;
    if !response.status().is_success() {
        return Err(KeyError::Source(format!(
            "Vault answered {} for the secret",
            response.status().as_u16()
        )));
    }
    let body = Zeroizing::new(
        response
            .bytes()
            .await
            .map_err(|_| KeyError::Source("Vault's answer could not be read".to_owned()))?
            .to_vec(),
    );
    parse_secret(&body, environment)
}

fn read_token(path: &Path) -> Result<Zeroizing<String>, KeyError> {
    std::fs::read_to_string(path)
        .map(Zeroizing::new)
        .map_err(|e| {
            KeyError::Source(format!(
                "cannot read the Vault token file {}: {}",
                path.display(),
                e.kind()
            ))
        })
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

    #[test]
    fn req_ky_1_reads_kv_v1_and_v2() {
        let v1 = format!(r#"{{"data":{}}}"#, secret("production"));
        let v2 = format!(
            r#"{{"data":{{"data":{},"metadata":{{"version":3}}}}}}"#,
            secret("production")
        );
        for body in [v1, v2] {
            let backend = parse_secret(body.as_bytes(), Environment::Production).unwrap();
            assert_eq!(backend.scheme("statement").unwrap(), Scheme::Ed25519);
            assert_eq!(backend.environment(), Environment::Production);
        }
    }

    #[test]
    fn req_ky_6_a_secret_of_another_environment_is_refused() {
        let body = format!(r#"{{"data":{}}}"#, secret("test"));
        assert!(matches!(
            parse_secret(body.as_bytes(), Environment::Production),
            Err(KeyError::Refused(_))
        ));
    }

    #[test]
    fn req_ky_1_vault_must_be_https_off_loopback() {
        let source = |address: &str| VaultSource {
            address: address.to_owned(),
            token_file: "t".into(),
            path: "secret/x".into(),
        };
        assert!(secret_url(&source("https://vault.internal:8200")).is_ok());
        assert!(secret_url(&source("http://127.0.0.1:8200")).is_ok());
        assert!(secret_url(&source("http://vault.internal:8200")).is_err());
        assert!(secret_url(&source("http://localhost.evil.example")).is_err());
    }

    #[test]
    fn req_ky_1_loopback_is_judged_on_the_parsed_host() {
        for ok in [
            "http://localhost:8200",
            "http://LOCALHOST",
            "http://127.0.0.1:8200",
            "http://127.1.2.3:8200",
            "http://[::1]:8200",
            "http://[::1]",
        ] {
            assert!(loopback(ok), "{ok}");
        }
        for bad in [
            "http://localhost:80@evil.example",
            "http://localhost@evil.example:8200/",
            "http://127.0.0.1:80@evil.example",
            "http://[::1]:80@evil.example",
            "http://evil.example/@localhost",
            "http://evil.example#@localhost",
            "http://evil.example?@127.0.0.1",
            "http://vault.internal:8200",
            "http://10.0.0.1:8200",
            "http://[::2]:8200",
            "http://0.0.0.0:8200",
            "ftp://localhost",
            "localhost:8200",
            "",
        ] {
            assert!(!loopback(bad), "{bad}");
        }
    }

    fn src(address: &str, token_file: &Path) -> VaultSource {
        VaultSource {
            address: address.to_owned(),
            token_file: token_file.to_path_buf(),
            path: "secret/x".into(),
        }
    }

    #[test]
    fn req_ky_1_a_userinfo_trick_is_not_accepted_as_plain_http() {
        assert!(secret_url(&src("http://localhost:80@evil.example", Path::new("t"))).is_err());
        assert!(secret_url(&src("http://[::1]:8200", Path::new("t"))).is_ok());
        assert!(secret_url(&src("http://vault.example", Path::new("t"))).is_err());
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
            r#"{{"data":{{"environment":"production","keys":[{{"id":"statement","scheme":"ed25519","seed":"{}"}}]}}}}"#,
            fixture_statement_seed()
        );
        assert!(matches!(
            parse_secret(body.as_bytes(), Environment::Production),
            Err(KeyError::Refused(m)) if m.contains("test fixture")
        ));
        let ok = format!(r#"{{"data":{}}}"#, secret("production"));
        assert!(parse_secret(ok.as_bytes(), Environment::Production).is_ok());
    }

    #[test]
    fn req_ky_1_a_malformed_or_extended_secret_is_refused() {
        for body in [
            "{}",
            r#"{"data":{}}"#,
            r#"{"data":{"data":{},"metadata":{}}}"#,
            "not json",
        ] {
            assert!(matches!(
                parse_secret(body.as_bytes(), Environment::Test),
                Err(KeyError::Source(_))
            ));
        }
        let extra = format!(
            r#"{{"data":{{"environment":"test","extra":1,"keys":[{{"id":"s","scheme":"ed25519","seed":"{SEED}"}}]}}}}"#
        );
        assert!(parse_secret(extra.as_bytes(), Environment::Test).is_err());
    }

    /// A scratch directory, removed on drop.
    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("bezent-vault-{tag}-{}", std::process::id()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn token(&self, token: &str) -> PathBuf {
            let path = self.0.join("token");
            std::fs::write(&path, format!("{token}\n")).unwrap();
            path
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    type Seen = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    /// A mock HTTP server answering one canned response per connection. Returns its address and
    /// the raw requests it saw.
    async fn mock(responses: Vec<String>) -> (String, Seen) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let seen = Seen::default();
        let log = seen.clone();
        tokio::spawn(async move {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                log.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf[..n]).into_owned());
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        (address, seen)
    }

    fn ok_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[tokio::test]
    async fn req_ky_1_load_reads_the_secret_with_the_token_from_the_file() {
        let dir = Dir::new("load");
        let token = dir.token("s.test-token");
        let body = format!(r#"{{"data":{}}}"#, secret("test"));
        let (address, seen) = mock(vec![ok_response(&body)]).await;
        let backend = load(&src(&address, &token), Environment::Test)
            .await
            .unwrap();
        assert_eq!(backend.scheme("statement").unwrap(), Scheme::Ed25519);
        let request = seen.lock().unwrap()[0].to_lowercase();
        assert!(request.starts_with("get /v1/secret/x "), "{request}");
        assert!(request.contains("x-vault-token: s.test-token\r\n"));
    }

    #[tokio::test]
    async fn req_ky_1_load_reports_a_refusal_status_and_a_missing_token_file() {
        let dir = Dir::new("status");
        let token = dir.token("t");
        let forbidden =
            "HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_owned();
        let (address, _) = mock(vec![forbidden]).await;
        let err = load(&src(&address, &token), Environment::Test)
            .await
            .unwrap_err();
        assert!(matches!(err, KeyError::Source(m) if m.contains("403")));
        let missing = dir.0.join("absent");
        assert!(matches!(
            load(
                &src("https://vault.internal:8200", &missing),
                Environment::Test
            )
            .await,
            Err(KeyError::Source(_))
        ));
    }

    #[tokio::test]
    async fn req_ky_1_load_refuses_a_redirect_and_never_follows_it() {
        let dir = Dir::new("redirect");
        let token = dir.token("s.secret-token");
        let body = format!(r#"{{"data":{}}}"#, secret("test"));
        let (target, target_seen) = mock(vec![ok_response(&body)]).await;
        let redirect = format!(
            "HTTP/1.1 307 Temporary Redirect\r\nlocation: {target}/v1/secret/x\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        );
        let (address, _) = mock(vec![redirect]).await;
        let err = load(&src(&address, &token), Environment::Test)
            .await
            .unwrap_err();
        assert!(matches!(err, KeyError::Source(m) if m.contains("307")));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            target_seen.lock().unwrap().is_empty(),
            "the redirect was followed"
        );
    }
}
