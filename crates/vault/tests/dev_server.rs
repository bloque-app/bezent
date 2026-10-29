// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! The client against a real Vault, `vault server -dev`, with a fake STS (issue #64).
//!
//! Vault's AWS auth method does not check the signature itself: it forwards the signed
//! `GetCallerIdentity` request to its configured `sts_endpoint` and trusts the answer. Here that
//! endpoint is a local server that answers with a made-up caller, so the whole path runs without
//! AWS: signature construction, the login body Vault accepts, the server-ID check, KV v1 and v2,
//! and PKI issue. Everything is a throwaway test value on a dev server.
//!
//! Ignored by default; the `vault-dev` job of CI runs it with `--include-ignored`, with
//! `BEZENT_VAULT_DEV_ADDR` (for example `http://127.0.0.1:8200`) and `BEZENT_VAULT_DEV_TOKEN` (the
//! dev server's root token) set.

use std::time::Duration;

use aws_credential_types::Credentials;
use bezent_vault::{
    Client, Error, IssueRequest, Resolver, SecretRef, Usage, VaultConfig, VaultField,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CALLER: &str = "arn:aws:iam::000000000000:user/bezent-test";

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

/// An admin call with the root token; panics on a failure, with Vault's reason (a dev server).
async fn admin(address: &str, token: &str, path: &str, body: serde_json::Value) {
    let response = reqwest::Client::new()
        .post(format!("{address}/v1/{path}"))
        .header("X-Vault-Token", token)
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = response.status();
    assert!(
        status.is_success(),
        "{path}: {status} {}",
        response.text().await.unwrap_or_default()
    );
}

fn config(address: &str, server_id: &str) -> VaultConfig {
    VaultConfig {
        address: address.to_owned(),
        mount: "bezent".into(),
        role: "bezent-test-iam".into(),
        sts_region: "us-east-2".into(),
        server_id: server_id.into(),
    }
}

fn client(address: &str, server_id: &str) -> Client {
    let credentials = Credentials::new(
        "AKIDEXAMPLE",
        "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
        Some("SESSION".into()),
        None,
        "test",
    );
    Client::with_credentials(&config(address, server_id), credentials).unwrap()
}

#[tokio::test]
#[ignore = "needs vault server -dev (BEZENT_VAULT_DEV_ADDR, BEZENT_VAULT_DEV_TOKEN); run by CI"]
async fn req_ky_1_login_kv_and_pki_against_a_dev_server() {
    let address = std::env::var("BEZENT_VAULT_DEV_ADDR").expect("BEZENT_VAULT_DEV_ADDR");
    let root = std::env::var("BEZENT_VAULT_DEV_TOKEN").expect("BEZENT_VAULT_DEV_TOKEN");
    let sts = fake_sts().await;
    let a = |path: &'static str, body: serde_json::Value| admin(&address, &root, path, body);

    // The AWS auth method, as the deployment mounts it: server-ID header, a role bound to one
    // principal, a token that lives a minute.
    a("sys/auth/bezent", serde_json::json!({"type": "aws"})).await;
    a(
        "auth/bezent/config/client",
        serde_json::json!({
            "sts_endpoint": sts,
            "sts_region": "us-east-2",
            "iam_server_id_header_value": "vault.example",
        }),
    )
    .await;
    a(
        "auth/bezent/role/bezent-test-iam",
        serde_json::json!({
            "auth_type": "iam",
            "bound_iam_principal_arn": CALLER,
            "resolve_aws_unique_ids": false,
            "token_policies": "bezent-test",
            "token_ttl": "60s",
            "token_max_ttl": "120s",
        }),
    )
    .await;
    a(
        "sys/policies/acl/bezent-test",
        serde_json::json!({"policy": "path \"bezentkv/*\" { capabilities = [\"read\"] }\npath \"secret/data/bezent/*\" { capabilities = [\"read\"] }\npath \"pki-link/issue/core\" { capabilities = [\"update\"] }\n"}),
    )
    .await;

    // A KV v1 mount, the deployment's layout; and the dev server's own KV v2 at `secret/`.
    a(
        "sys/mounts/bezentkv",
        serde_json::json!({"type": "kv", "options": {"version": "1"}}),
    )
    .await;
    a(
        "bezentkv/core",
        serde_json::json!({"db_login": "bezent_core", "db_password": "p@ss/w:rd", "n": 3}),
    )
    .await;
    a(
        "secret/data/bezent/core",
        serde_json::json!({"data": {"db_login": "bezent_core", "db_password": "p@ss/w:rd"}}),
    )
    .await;

    // A PKI engine with a role that issues exactly one name.
    a("sys/mounts/pki-link", serde_json::json!({"type": "pki"})).await;
    a(
        "pki-link/root/generate/internal",
        serde_json::json!({"common_name": "bezent test root", "ttl": "8760h", "key_type": "ec", "key_bits": 256}),
    )
    .await;
    a(
        "pki-link/roles/core",
        serde_json::json!({
            "allowed_domains": "core.link.example", "allow_bare_domains": true,
            "allow_subdomains": false, "enforce_hostnames": true,
            "key_type": "ec", "key_bits": 256, "client_flag": true, "server_flag": false,
            "ttl": "720h", "max_ttl": "720h",
        }),
    )
    .await;

    // Login, and reads of both KV versions.
    let session = client(&address, "vault.example").login().await.unwrap();
    let v1 = session
        .read_field("bezentkv/core", "db_password")
        .await
        .unwrap();
    let v2 = session
        .read_field("secret/data/bezent/core", "db_password")
        .await
        .unwrap();
    assert_eq!((v1.as_str(), v2.as_str()), ("p@ss/w:rd", "p@ss/w:rd"));
    assert!(matches!(
        session.read_field("bezentkv/core", "absent").await,
        Err(Error::NoField { .. })
    ));
    assert!(matches!(
        session.read_field("bezentkv/other", "x").await,
        Err(Error::Status { status: 404, .. })
    ));
    assert!(matches!(
        session
            .read_field("sys/policies/acl/bezent-test", "policy")
            .await,
        Err(Error::Status { status: 403, .. })
    ));

    // PKI issue: the engine's answer passes the client's checks.
    let issued = session
        .issue(
            "pki-link",
            "core",
            &IssueRequest {
                common_name: "core.link.example".into(),
                ttl: Duration::from_secs(720 * 3600),
                usage: Usage::Client,
            },
        )
        .await
        .unwrap();
    assert!(!issued.private_key_pkcs8.is_empty() && !issued.certificate_der.is_empty());
    assert!(!issued.ca_chain_pem.is_empty());
    let wrong_name = session
        .issue(
            "pki-link",
            "core",
            &IssueRequest {
                common_name: "other.link.example".into(),
                ttl: Duration::from_secs(720 * 3600),
                usage: Usage::Client,
            },
        )
        .await;
    assert!(matches!(wrong_name, Err(Error::Status { status: 400, .. })));
    // A longer life than the role allows is shortened by Vault, which the client must notice.
    let too_long = session
        .issue(
            "pki-link",
            "core",
            &IssueRequest {
                common_name: "core.link.example".into(),
                ttl: Duration::from_secs(1440 * 3600),
                usage: Usage::Client,
            },
        )
        .await;
    assert!(
        matches!(&too_long, Err(Error::Pki(_)) | Err(Error::Status { .. })),
        "{too_long:?}"
    );

    // A resolver: one login for the burst, then a store URL from two fields.
    let resolver = Resolver::with_client(Some(client(&address, "vault.example")));
    let field = |f: &str| {
        SecretRef::Vault(VaultField {
            path: "bezentkv/core".into(),
            field: f.into(),
        })
    };
    assert_eq!(
        resolver.resolve(&field("db_login")).await.unwrap().as_str(),
        "bezent_core"
    );

    // A login that names another Vault in the server-ID header is refused.
    let err = client(&address, "another-vault.example")
        .login()
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::Status {
                operation: "login",
                ..
            }
        ),
        "{err:?}"
    );
}
