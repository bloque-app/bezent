// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! The client against a mock Vault: login, KV, PKI, and the ways each fails. Every test names the
//! requirement it checks (`REQ-KY-1`: no secret in an error, a log, a Debug output or the
//! environment; a token never kept).

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aws_credential_types::Credentials;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{Client, Error, IssueRequest, Resolver, SecretRef, Usage, VaultConfig, VaultField};

type Seen = Arc<Mutex<Vec<String>>>;

/// A mock HTTP server answering one canned response per connection. Returns its address and the
/// raw requests it saw (head and body).
async fn mock(responses: Vec<String>) -> (String, Seen) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let seen = Seen::default();
    let log = seen.clone();
    tokio::spawn(async move {
        for response in responses {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut request = Vec::new();
            let mut buf = vec![0u8; 8192];
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
                        l.strip_prefix("content-length: ")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if body.len() >= wanted || n == 0 {
                    break;
                }
            }
            log.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&request).into_owned());
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    });
    (address, seen)
}

fn ok(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn status(code: u16, body: &str) -> String {
    format!(
        "HTTP/1.1 {code} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

const TOKEN: &str = "hvs.TOKEN-THAT-MUST-NOT-LEAK";

fn login_ok() -> String {
    ok(&format!(
        r#"{{"auth":{{"client_token":"{TOKEN}","lease_duration":60,"policies":["p"]}}}}"#
    ))
}

fn config(address: &str) -> VaultConfig {
    VaultConfig {
        address: address.to_owned(),
        mount: "bezent".into(),
        role: "bezent-core-iam".into(),
        sts_region: "us-east-2".into(),
        server_id: "vault.example".into(),
    }
}

fn client(address: &str) -> Client {
    let credentials = Credentials::new(
        "AKIDEXAMPLE",
        "SECRETKEY",
        Some("SESSION".into()),
        None,
        "test",
    );
    Client::with_credentials(&config(address), credentials).unwrap()
}

#[tokio::test]
async fn req_ky_1_login_signs_the_sts_call_and_reads_a_field_with_the_token() {
    let secret = r#"{"data":{"login":"bezent_core","password":"pw"}}"#;
    let (address, seen) = mock(vec![login_ok(), ok(secret)]).await;
    let session = client(&address).login().await.unwrap();
    let password = session
        .read_field("secret/bezent/core", "password")
        .await
        .unwrap();
    assert_eq!(password.as_str(), "pw");
    let seen = seen.lock().unwrap();
    assert!(
        seen[0].starts_with("POST /v1/auth/bezent/login "),
        "{}",
        seen[0]
    );
    let body = seen[0].split_once("\r\n\r\n").unwrap().1;
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["role"], "bezent-core-iam");
    let headers = String::from_utf8(
        STANDARD
            .decode(v["iam_request_headers"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    assert!(headers.contains("X-Vault-AWS-IAM-Server-ID") && headers.contains("vault.example"));
    assert!(
        !seen[0].contains("SECRETKEY"),
        "the secret key is never sent"
    );
    assert!(
        seen[1].starts_with("GET /v1/secret/bezent/core "),
        "{}",
        seen[1]
    );
    assert!(
        seen[1]
            .to_lowercase()
            .contains(&format!("x-vault-token: {}\r\n", TOKEN.to_lowercase()))
    );
}

#[tokio::test]
async fn req_ky_1_no_error_or_debug_output_carries_a_token_or_a_body() {
    let leaky = r#"{"errors":["SECRETBODY hvs.TOKEN-THAT-MUST-NOT-LEAK"]}"#;
    let (address, _) = mock(vec![login_ok(), status(403, leaky), status(404, leaky)]).await;
    let session = client(&address).login().await.unwrap();
    let shown = format!("{session:?}");
    assert!(!shown.contains(TOKEN) && shown.contains("redacted"));
    let err = session.read_field("secret/x", "f").await.unwrap_err();
    assert_eq!(
        err,
        Error::Status {
            operation: "read",
            status: 403
        }
    );
    let all = format!("{err} {err:?}");
    assert!(!all.contains("SECRETBODY") && !all.contains(TOKEN));
    let err = session.read_field("secret/x", "f").await.unwrap_err();
    assert!(matches!(err, Error::Status { status: 404, .. }));
}

#[tokio::test]
async fn req_ky_1_a_missing_field_and_a_malformed_login_are_errors() {
    let (address, _) = mock(vec![login_ok(), ok(r#"{"data":{"a":"b"}}"#)]).await;
    let session = client(&address).login().await.unwrap();
    let err = session.read_field("secret/x", "nope").await.unwrap_err();
    assert_eq!(
        err,
        Error::NoField {
            path: "secret/x".into(),
            field: "nope".into()
        }
    );
    let (address, _) = mock(vec![ok(r#"{"auth":null}"#)]).await;
    assert!(matches!(
        client(&address).login().await,
        Err(Error::Malformed(_))
    ));
    let (address, _) = mock(vec![ok(r#"{"auth":{"client_token":""}}"#)]).await;
    assert!(matches!(
        client(&address).login().await,
        Err(Error::Malformed(_))
    ));
}

#[tokio::test]
async fn req_ky_1_a_redirect_is_never_followed() {
    let (target, target_seen) = mock(vec![login_ok()]).await;
    let redirect = status(307, "").replace(
        "X\r\n",
        &format!("Temporary Redirect\r\nlocation: {target}/v1/auth/bezent/login\r\n"),
    );
    let (address, _) = mock(vec![redirect]).await;
    let err = client(&address).login().await.unwrap_err();
    assert!(matches!(err, Error::Status { status: 307, .. }));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        target_seen.lock().unwrap().is_empty(),
        "the redirect was followed"
    );
}

#[tokio::test]
async fn req_ky_1_a_5xx_is_retried_a_4xx_is_not() {
    let (address, seen) = mock(vec![status(503, ""), login_ok()]).await;
    client(&address).login().await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 2);
    let (address, seen) = mock(vec![status(400, ""), login_ok()]).await;
    assert!(matches!(
        client(&address).login().await,
        Err(Error::Status { status: 400, .. })
    ));
    assert_eq!(seen.lock().unwrap().len(), 1);
    let (address, seen) = mock(vec![
        status(500, ""),
        status(500, ""),
        status(502, ""),
        login_ok(),
    ])
    .await;
    assert!(matches!(
        client(&address).login().await,
        Err(Error::Status { status: 502, .. })
    ));
    assert_eq!(seen.lock().unwrap().len(), 3, "bounded retries");
}

#[tokio::test]
async fn req_ky_1_an_unreachable_vault_is_an_error_without_the_address() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let err = client(&address).login().await.unwrap_err();
    assert!(matches!(err, Error::Unreachable(_)));
    assert!(!err.to_string().contains("127.0.0.1"));
}

#[test]
fn req_ky_1_the_client_refuses_plain_http_off_loopback_and_odd_configuration() {
    let credentials = Credentials::new("A", "S", None, None, "test");
    assert!(
        Client::with_credentials(&config("http://vault.example:8200"), credentials.clone())
            .is_err()
    );
    assert!(
        Client::with_credentials(&config("https://vault.example:8200"), credentials.clone())
            .is_ok()
    );
    let mut c = config("https://vault.example");
    c.mount = "../sys".into();
    assert!(Client::with_credentials(&c, credentials.clone()).is_err());
    let mut c = config("https://vault.example");
    c.sts_region = "us-east-2.evil.example".into();
    assert!(Client::with_credentials(&c, credentials).is_err());
}

#[tokio::test]
async fn req_ky_1_a_secret_path_that_is_not_plain_is_refused_before_any_request() {
    let (address, seen) = mock(vec![login_ok()]).await;
    let session = client(&address).login().await.unwrap();
    for path in [
        "../sys/seal",
        "secret//x",
        "secret/x?list=true",
        "secret/a b",
        "",
    ] {
        assert!(
            matches!(session.read_field(path, "f").await, Err(Error::Config(_))),
            "{path}"
        );
    }
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn req_ky_1_a_resolver_logs_in_once_per_burst_and_reads_files() {
    let secret = r#"{"data":{"a":"one","b":"two"}}"#;
    let (address, seen) = mock(vec![login_ok(), ok(secret), ok(secret)]).await;
    let resolver = Resolver::with_client(Some(client(&address)));
    let field = |f: &str| {
        SecretRef::Vault(VaultField {
            path: "secret/x".into(),
            field: f.into(),
        })
    };
    assert_eq!(resolver.resolve(&field("a")).await.unwrap().as_str(), "one");
    assert_eq!(resolver.resolve(&field("b")).await.unwrap().as_str(), "two");
    let logins = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.contains("/login"))
        .count();
    assert_eq!(logins, 1);

    let dir = std::env::temp_dir().join(format!("bezent-vault-resolve-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("token");
    std::fs::write(&file, "from-file\n").unwrap();
    assert_eq!(
        resolver
            .resolve(&SecretRef::File(file.clone()))
            .await
            .unwrap()
            .as_str(),
        "from-file"
    );
    let missing = Resolver::with_client(None)
        .resolve(&SecretRef::File(dir.join("absent")))
        .await;
    assert!(matches!(missing, Err(Error::File { .. })));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn req_ky_1_a_vault_ref_without_a_vault_table_is_refused() {
    let resolver = Resolver::new(None).unwrap();
    let r = SecretRef::Vault(VaultField {
        path: "secret/x".into(),
        field: "f".into(),
    });
    assert!(matches!(resolver.resolve(&r).await, Err(Error::Config(_))));
}

#[tokio::test]
async fn req_ky_1_an_empty_secret_is_refused_with_its_own_error() {
    let (address, _) = mock(vec![login_ok(), ok(r#"{"data":{"token":"","ok":"x"}}"#)]).await;
    let session = client(&address).login().await.unwrap();
    let err = session.read_field("secret/x", "token").await.unwrap_err();
    assert_eq!(
        err,
        Error::Empty {
            what: "secret/x token".into()
        }
    );
    let dir = std::env::temp_dir().join(format!("bezent-vault-empty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("blank"), " \n").unwrap();
    let r = Resolver::with_client(None)
        .resolve(&SecretRef::File(dir.join("blank")))
        .await;
    assert!(matches!(r, Err(Error::Empty { .. })));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn req_ky_1_a_429_on_login_is_retried_within_the_bounds() {
    let (address, seen) = mock(vec![status(429, ""), login_ok()]).await;
    client(&address).login().await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 2);
}

// ---- PKI ------------------------------------------------------------------------------------

const NOW: u64 = 1_793_275_200; // 2026-10-29T12:00:00Z
const NAME: &str = "core.link.example";
const CLIENT_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x02];
const SERVER_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];
const TTL: u64 = 720 * 3600;

fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(NOW)
}

fn der(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    match content.len() {
        n if n < 0x80 => out.push(n as u8),
        n if n < 0x100 => out.extend([0x81, n as u8]),
        n => out.extend([0x82, (n >> 8) as u8, n as u8]),
    }
    out.extend_from_slice(content);
    out
}

fn utc(secs: u64) -> Vec<u8> {
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil date from days since the epoch (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let text = format!(
        "{:02}{month:02}{day:02}{:02}{:02}{:02}Z",
        year % 100,
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    );
    der(0x17, text.as_bytes())
}

/// What a test varies in a certificate.
struct Cert<'a> {
    public: &'a [u8],
    not_before: u64,
    not_after: u64,
    common_name: &'a str,
    /// `(tag, bytes)` of each subject alternative name.
    sans: Vec<(u8, Vec<u8>)>,
    /// The extended key usage OIDs, if the extension is there.
    eku: Option<Vec<&'a [u8]>>,
    /// The key usage first byte, if the extension is there.
    key_usage: Option<u8>,
    ca: bool,
}

impl<'a> Cert<'a> {
    fn leaf(public: &'a [u8], life: u64) -> Self {
        Self {
            public,
            not_before: NOW - 30,
            not_after: NOW + life,
            common_name: NAME,
            sans: vec![(0x82, NAME.as_bytes().to_vec())],
            eku: Some(vec![CLIENT_AUTH]),
            key_usage: Some(0x80),
            ca: false,
        }
    }

    /// DER, signed by `signer` (the issuing CA's key; a CA signs itself).
    fn der(&self, signer: &EcdsaKeyPair) -> Vec<u8> {
        let sig_alg = der(
            0x30,
            &der(0x06, &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02]),
        );
        let spki = der(
            0x30,
            &[
                der(
                    0x30,
                    &[
                        der(0x06, &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01]),
                        der(0x06, &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07]),
                    ]
                    .concat(),
                ),
                der(0x03, &[&[0u8][..], self.public].concat()),
            ]
            .concat(),
        );
        let subject = der(
            0x30,
            &der(
                0x31,
                &der(
                    0x30,
                    &[
                        der(0x06, &[0x55, 0x04, 0x03]),
                        der(0x0c, self.common_name.as_bytes()),
                    ]
                    .concat(),
                ),
            ),
        );
        let ext =
            |oid: &[u8], value: Vec<u8>| der(0x30, &[der(0x06, oid), der(0x04, &value)].concat());
        let mut extensions = Vec::new();
        if !self.sans.is_empty() {
            let names: Vec<u8> = self.sans.iter().flat_map(|(t, n)| der(*t, n)).collect();
            extensions.push(ext(&[0x55, 0x1d, 0x11], der(0x30, &names)));
        }
        if let Some(eku) = &self.eku {
            let oids: Vec<u8> = eku.iter().flat_map(|o| der(0x06, o)).collect();
            extensions.push(ext(&[0x55, 0x1d, 0x25], der(0x30, &oids)));
        }
        if let Some(bits) = self.key_usage {
            extensions.push(ext(&[0x55, 0x1d, 0x0f], der(0x03, &[7, bits])));
        }
        if self.ca {
            extensions.push(ext(&[0x55, 0x1d, 0x13], der(0x30, &der(0x01, &[0xff]))));
        }
        let tbs = der(
            0x30,
            &[
                der(0xa0, &der(0x02, &[2])),
                der(0x02, &[1]),
                sig_alg.clone(),
                der(0x30, &[]),
                der(0x30, &[utc(self.not_before), utc(self.not_after)].concat()),
                subject,
                spki,
                der(0xa3, &der(0x30, &extensions.concat())),
            ]
            .concat(),
        );
        let signature = signer.sign(&SystemRandom::new(), &tbs).unwrap();
        let bits = [&[0u8][..], signature.as_ref()].concat();
        der(0x30, &[tbs, sig_alg, der(0x03, &bits)].concat())
    }
}

fn pem(label: &str, bytes: &[u8]) -> String {
    let b64 = STANDARD.encode(bytes);
    let lines: Vec<_> = b64
        .as_bytes()
        .chunks(64)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect();
    format!(
        "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
        lines.join("\n")
    )
}

struct Pair {
    pkcs8: Vec<u8>,
    public: Vec<u8>,
    key: EcdsaKeyPair,
}

fn pair() -> Pair {
    let rng = SystemRandom::new();
    let doc = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
    let key =
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, doc.as_ref(), &rng).unwrap();
    Pair {
        pkcs8: doc.as_ref().to_vec(),
        public: key.public_key().as_ref().to_vec(),
        key,
    }
}

/// The answer of the PKI engine for `cert`, whose issuing CA is `ca`.
fn issued(ca: &Pair, cert: &[u8], key: Option<&[u8]>, expiration: u64) -> String {
    let ca_cert = Cert {
        ca: true,
        sans: vec![],
        eku: None,
        key_usage: Some(0x06),
        not_after: NOW + 10 * TTL,
        common_name: "test ca",
        ..Cert::leaf(&ca.public, 0)
    }
    .der(&ca.key);
    let key = key.map(|k| pem("PRIVATE KEY", k)).unwrap_or_default();
    serde_json::json!({"data": {
        "certificate": pem("CERTIFICATE", cert),
        "issuing_ca": pem("CERTIFICATE", &ca_cert),
        "ca_chain": [pem("CERTIFICATE", &ca_cert)],
        "private_key": key,
        "private_key_type": "ec",
        "serial_number": "aa:bb",
        "expiration": expiration,
    }})
    .to_string()
}

fn request(ttl: u64) -> IssueRequest {
    IssueRequest {
        common_name: NAME.into(),
        ttl: Duration::from_secs(ttl),
        usage: Usage::Client,
    }
}

async fn issue(body: String) -> (Result<crate::Issued, Error>, Seen) {
    let (address, seen) = mock(vec![login_ok(), ok(&body)]).await;
    let session = client(&address).login().await.unwrap();
    (
        session
            .issue_at("pki-link", "core", &request(TTL), now())
            .await,
        seen,
    )
}

/// The result of issuing `cert` for `p`, signed by `ca`, with the matching key.
async fn issue_cert(ca: &Pair, p: &Pair, cert: Cert<'_>) -> Result<crate::Issued, Error> {
    let der = cert.der(&ca.key);
    issue(issued(ca, &der, Some(&p.pkcs8), cert.not_after))
        .await
        .0
}

fn why(result: Result<crate::Issued, Error>) -> String {
    result.unwrap_err().to_string()
}

#[tokio::test]
async fn req_ky_1_pki_issue_returns_a_checked_certificate_and_key() {
    let (ca, p) = (pair(), pair());
    let cert = Cert::leaf(&p.public, TTL).der(&ca.key);
    let (issued, seen) = issue(issued(&ca, &cert, Some(&p.pkcs8), NOW + TTL)).await;
    let issued = issued.unwrap();
    assert_eq!(issued.certificate_der, cert);
    assert_eq!(issued.private_key_pkcs8.as_slice(), p.pkcs8.as_slice());
    assert_eq!(
        issued.not_after,
        UNIX_EPOCH + Duration::from_secs(NOW + TTL)
    );
    assert_eq!(issued.ca_chain_pem.len(), 1);
    assert!(!format!("{issued:?}").contains(&STANDARD.encode(&p.pkcs8)));
    let seen = seen.lock().unwrap();
    assert!(
        seen[1].starts_with("POST /v1/pki-link/issue/core "),
        "{}",
        seen[1]
    );
    let body: serde_json::Value =
        serde_json::from_str(seen[1].split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["common_name"], NAME);
    assert_eq!(body["ttl"], "2592000s");
    assert_eq!(body["private_key_format"], "pkcs8");
}

#[tokio::test]
async fn req_ky_1_pki_issue_refuses_a_missing_or_foreign_key() {
    let (ca, p, other) = (pair(), pair(), pair());
    let cert = Cert::leaf(&p.public, TTL).der(&ca.key);
    for (key, why) in [
        (None, "no private key"),
        (Some(other.pkcs8.as_slice()), "not the certificate's key"),
        (Some([0u8, 1, 2].as_slice()), "can be used"),
    ] {
        let (result, _) = issue(issued(&ca, &cert, key, NOW + TTL)).await;
        let err = result.unwrap_err();
        assert!(matches!(&err, Error::Pki(m) if m.contains(why)), "{err}");
    }
}

#[tokio::test]
async fn req_ky_1_pki_issue_refuses_a_ttl_that_is_not_the_one_asked_for() {
    let (ca, p) = (pair(), pair());
    // A role whose max_ttl shortened the request without an error, and one that lengthened it.
    for life in [24 * 3600, TTL + 24 * 3600] {
        let err = why(issue_cert(&ca, &p, Cert::leaf(&p.public, life)).await);
        assert!(err.contains("as long as"), "{life}: {err}");
    }
    // Vault's stated expiration must be the certificate's.
    let cert = Cert::leaf(&p.public, TTL).der(&ca.key);
    let (result, _) = issue(issued(&ca, &cert, Some(&p.pkcs8), NOW + TTL + 5)).await;
    assert!(matches!(result, Err(Error::Pki(m)) if m.contains("expiration")));
    // A certificate that is not valid yet.
    let mut late = Cert::leaf(&p.public, TTL);
    late.not_before = NOW + 3600;
    assert!(why(issue_cert(&ca, &p, late).await).contains("not valid yet"));
}

#[tokio::test]
async fn req_ky_1_pki_issue_refuses_the_wrong_name_extra_names_and_ip_names() {
    let (ca, p) = (pair(), pair());
    let mut wrong_cn = Cert::leaf(&p.public, TTL);
    wrong_cn.common_name = "other.link.example";
    assert!(why(issue_cert(&ca, &p, wrong_cn).await).contains("common name"));
    let mut extra = Cert::leaf(&p.public, TTL);
    extra.sans.push((0x82, b"signer.link.example".to_vec()));
    assert!(why(issue_cert(&ca, &p, extra).await).contains("alternative names"));
    let mut ip = Cert::leaf(&p.public, TTL);
    ip.sans.push((0x87, vec![10, 0, 0, 1]));
    assert!(why(issue_cert(&ca, &p, ip).await).contains("alternative names"));
    let mut other_san = Cert::leaf(&p.public, TTL);
    other_san.sans = vec![(0x82, b"other.link.example".to_vec())];
    assert!(why(issue_cert(&ca, &p, other_san).await).contains("alternative names"));
    let mut none = Cert::leaf(&p.public, TTL);
    none.sans.clear();
    assert!(why(issue_cert(&ca, &p, none).await).contains("alternative names"));
    // The matching case, in another letter case, is the same name.
    let mut upper = Cert::leaf(&p.public, TTL);
    upper.common_name = "CORE.link.example";
    assert!(issue_cert(&ca, &p, upper).await.is_ok());
}

#[tokio::test]
async fn req_ky_1_pki_issue_refuses_a_certificate_for_another_use() {
    let (ca, p) = (pair(), pair());
    let mut server = Cert::leaf(&p.public, TTL);
    server.eku = Some(vec![SERVER_AUTH]);
    assert!(why(issue_cert(&ca, &p, server).await).contains("extended key usage"));
    let mut both = Cert::leaf(&p.public, TTL);
    both.eku = Some(vec![CLIENT_AUTH, SERVER_AUTH]);
    assert!(why(issue_cert(&ca, &p, both).await).contains("extended key usage"));
    let mut signs_certs = Cert::leaf(&p.public, TTL);
    signs_certs.key_usage = Some(0x84);
    assert!(why(issue_cert(&ca, &p, signs_certs).await).contains("key usage"));
    let mut no_signature = Cert::leaf(&p.public, TTL);
    no_signature.key_usage = Some(0x20);
    assert!(why(issue_cert(&ca, &p, no_signature).await).contains("key usage"));
    let mut is_ca = Cert::leaf(&p.public, TTL);
    is_ca.ca = true;
    assert!(why(issue_cert(&ca, &p, is_ca).await).contains("CA"));
    // Only an extension that is there is held to the role.
    let mut bare = Cert::leaf(&p.public, TTL);
    bare.eku = None;
    bare.key_usage = None;
    assert!(issue_cert(&ca, &p, bare).await.is_ok());
    // A server role takes a server certificate.
    let mut leaf = Cert::leaf(&p.public, TTL);
    leaf.eku = Some(vec![SERVER_AUTH]);
    let cert = leaf.der(&ca.key);
    let body = issued(&ca, &cert, Some(&p.pkcs8), NOW + TTL);
    let (address, _) = mock(vec![login_ok(), ok(&body)]).await;
    let session = client(&address).login().await.unwrap();
    let server = IssueRequest {
        usage: Usage::Server,
        ..request(TTL)
    };
    assert!(
        session
            .issue_at("pki-link", "signer", &server, now())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn req_ky_1_pki_issue_checks_the_leafs_signature_against_the_issuing_ca() {
    let (ca, forger, p) = (pair(), pair(), pair());
    // Signed by a key that is not the issuing CA's.
    let cert = Cert::leaf(&p.public, TTL).der(&forger.key);
    let (result, _) = issue(issued(&ca, &cert, Some(&p.pkcs8), NOW + TTL)).await;
    assert!(matches!(result, Err(Error::Pki(m)) if m.contains("not signed by the issuing CA")));
    // No issuing CA in the answer.
    let cert = Cert::leaf(&p.public, TTL).der(&ca.key);
    let mut body: serde_json::Value =
        serde_json::from_str(&issued(&ca, &cert, Some(&p.pkcs8), NOW + TTL)).unwrap();
    body["data"].as_object_mut().unwrap().remove("issuing_ca");
    let (result, _) = issue(body.to_string()).await;
    assert!(matches!(result, Err(Error::Pki(m)) if m.contains("no issuing CA")));
}

#[tokio::test]
async fn req_ky_1_pki_issue_is_not_retried_and_refuses_odd_paths() {
    let (address, seen) = mock(vec![login_ok(), status(503, ""), login_ok()]).await;
    let session = client(&address).login().await.unwrap();
    let err = session
        .issue_at("pki-link", "core", &request(TTL), now())
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        Error::Status {
            operation: "issue",
            status: 503
        }
    ));
    assert_eq!(
        seen.lock().unwrap().len(),
        2,
        "an issue is not retried: it would issue twice"
    );
    for (mount, role) in [("../x", "core"), ("pki", "a/b"), ("pki", "")] {
        assert!(matches!(
            session.issue_at(mount, role, &request(TTL), now()).await,
            Err(Error::Config(_))
        ));
    }
}

#[test]
fn a_certificate_that_is_not_x509_is_refused() {
    for junk in [vec![], vec![0x30, 0x03, 1, 2, 3], vec![0x30, 0xff, 0xff]] {
        assert!(crate::pki::Certificate::parse(&junk).is_none());
    }
}
