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

use crate::{Client, Error, IssueRequest, Resolver, SecretRef, VaultConfig, VaultField};

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

// ---- PKI ------------------------------------------------------------------------------------

const NOW: u64 = 1_793_275_200; // 2026-10-29T12:00:00Z

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

/// A certificate for `public` valid `[not_before, not_after]`. The signature is dummy: the client
/// does not verify the chain (Vault is trusted over HTTPS); it reads the key and the validity.
fn certificate(public: &[u8], not_before: u64, not_after: u64) -> Vec<u8> {
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
            der(0x03, &[&[0u8][..], public].concat()),
        ]
        .concat(),
    );
    let tbs = der(
        0x30,
        &[
            der(0xa0, &der(0x02, &[2])),
            der(0x02, &[1]),
            sig_alg.clone(),
            der(0x30, &[]),
            der(0x30, &[utc(not_before), utc(not_after)].concat()),
            der(0x30, &[]),
            spki,
        ]
        .concat(),
    );
    der(0x30, &[tbs, sig_alg, der(0x03, &[0, 0])].concat())
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
}

fn pair() -> Pair {
    let rng = SystemRandom::new();
    let doc = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
    let key =
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, doc.as_ref(), &rng).unwrap();
    Pair {
        pkcs8: doc.as_ref().to_vec(),
        public: key.public_key().as_ref().to_vec(),
    }
}

fn issued(cert: &[u8], key: Option<&[u8]>, expiration: u64) -> String {
    let key = key.map(|k| pem("PRIVATE KEY", k)).unwrap_or_default();
    serde_json::json!({"data": {
        "certificate": pem("CERTIFICATE", cert),
        "issuing_ca": pem("CERTIFICATE", &[1]),
        "ca_chain": [pem("CERTIFICATE", &[1])],
        "private_key": key,
        "private_key_type": "ec",
        "serial_number": "aa:bb",
        "expiration": expiration,
    }})
    .to_string()
}

fn request(ttl: u64) -> IssueRequest {
    IssueRequest {
        common_name: "core.link.example".into(),
        ttl: Duration::from_secs(ttl),
    }
}

const TTL: u64 = 720 * 3600;

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

#[tokio::test]
async fn req_ky_1_pki_issue_returns_a_checked_certificate_and_key() {
    let p = pair();
    let cert = certificate(&p.public, NOW - 30, NOW + TTL);
    let (issued, seen) = issue(issued(&cert, Some(&p.pkcs8), NOW + TTL)).await;
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
    assert_eq!(body["common_name"], "core.link.example");
    assert_eq!(body["ttl"], "2592000s");
    assert_eq!(body["private_key_format"], "pkcs8");
}

#[tokio::test]
async fn req_ky_1_pki_issue_refuses_a_missing_or_foreign_key() {
    let (p, other) = (pair(), pair());
    let cert = certificate(&p.public, NOW - 30, NOW + TTL);
    for (key, why) in [
        (None, "no private key"),
        (Some(other.pkcs8.as_slice()), "not the certificate's key"),
        (Some([0u8, 1, 2].as_slice()), "can be used"),
    ] {
        let (result, _) = issue(issued(&cert, key, NOW + TTL)).await;
        let err = result.unwrap_err();
        assert!(matches!(&err, Error::Pki(m) if m.contains(why)), "{err}");
    }
}

#[tokio::test]
async fn req_ky_1_pki_issue_refuses_a_ttl_that_is_not_the_one_asked_for() {
    let p = pair();
    // A role whose max_ttl shortened the request without an error, and one that lengthened it.
    for life in [24 * 3600, TTL + 24 * 3600] {
        let cert = certificate(&p.public, NOW - 30, NOW + life);
        let (result, _) = issue(issued(&cert, Some(&p.pkcs8), NOW + life)).await;
        assert!(
            matches!(result, Err(Error::Pki(m)) if m.contains("as long as")),
            "{life}"
        );
    }
    // Vault's stated expiration must be the certificate's.
    let cert = certificate(&p.public, NOW - 30, NOW + TTL);
    let (result, _) = issue(issued(&cert, Some(&p.pkcs8), NOW + TTL + 5)).await;
    assert!(matches!(result, Err(Error::Pki(m)) if m.contains("expiration")));
    // A certificate that is not valid yet.
    let cert = certificate(&p.public, NOW + 3600, NOW + TTL);
    let (result, _) = issue(issued(&cert, Some(&p.pkcs8), NOW + TTL)).await;
    assert!(matches!(result, Err(Error::Pki(m)) if m.contains("not valid yet")));
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
        assert!(crate::pki::Certificate::parse_for_tests(&junk).is_none());
    }
}
