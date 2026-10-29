// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! A signer beside its one caller (the Fargate design's sidecar): `prepare` writes the caller's
//! token and the signer's configuration, and the caller signs through the socket with its user id
//! and that token. Only the keys the configuration lists are the caller's (`REQ-AR-3`,
//! `REQ-CU-1`, `REQ-KY-1`).

mod common;

use std::os::unix::fs::MetadataExt;
use std::sync::Arc;

use bezent_signer::config::Config;
use bezent_signer::link::{self, Callers, Client, LinkError, Response};
use bezent_signer::prepare::{self, CallerSpec, Owner, Spec};
use common::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// The sidecar's template: the fixture's configuration without callers, and one binding, the
/// payout key's, with the socket in the socket directory `prepare` makes.
fn template(f: &Fixture) -> std::path::PathBuf {
    let mut json = config_json(&f.dir, my_uid(), TOKEN);
    json["callers"] = serde_json::json!([]);
    json["socket"] = f.dir.join("sock/signer.sock").to_string_lossy().into();
    json["bindings"] = serde_json::json!([json["bindings"][2].clone()]);
    let path = f.dir.join("template.json");
    std::fs::write(&path, json.to_string()).unwrap();
    path
}

fn spec(f: &Fixture) -> Spec {
    let me = std::fs::metadata(&f.dir).unwrap();
    Spec {
        template: template(f),
        config: f.dir.join("signer-config/signer.json"),
        signer: Owner {
            uid: me.uid(),
            gid: me.gid(),
        },
        caller: CallerSpec {
            name: "evm-endpoint".into(),
            uid: me.uid(),
            gid: me.gid(),
        },
        token_file: f.dir.join("caller-token/token"),
        socket_dir: f.dir.join("sock"),
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ar_3_the_caller_signs_through_the_sidecar_with_its_uid_and_token(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let spec = spec(&f);
    // The tests run as one user; the binary refuses a caller of the signer's own user id.
    prepare::run_as_one_user_in_tests(&spec).unwrap();
    assert_eq!(
        std::fs::metadata(&spec.token_file).unwrap().mode() & 0o7777,
        0o600
    );

    // The signer starts from the rendered configuration alone.
    let config = Config::load(&spec.config).unwrap();
    assert_eq!(config.callers.len(), 1);
    assert_eq!(config.callers[0].name, "evm-endpoint");
    assert_eq!(config.bindings.len(), 1);
    let (signer, log, policy) = Fixture::start(&config, &f.connect).await;
    assert!(policy.binding("polygon-test", "payout").is_some());
    assert!(policy.binding("kreivo-test", "mint").is_none());

    let listener = link::bind(&config.socket).unwrap();
    let socket_mode = std::fs::metadata(&config.socket).unwrap().mode() & 0o7777;
    assert_eq!(socket_mode, 0o660);
    assert_eq!(
        std::fs::metadata(&spec.socket_dir).unwrap().mode() & 0o7777,
        0o2750
    );
    let callers = Arc::new(Callers::sharing_the_signer_uid_in_tests(&config.callers));
    tokio::spawn(link::serve(listener, callers, signer));

    // The endpoint reads its token file and its signer's socket and user id, as its `[signer]` table
    // names them.
    let token = std::fs::read_to_string(&spec.token_file).unwrap();
    let uid = my_uid();
    let core = f.core_pool().await;
    let mut client = Client::connect(&config.socket, &token, uid).await.unwrap();

    // A payout, the key the configuration lists, is signed and the signature verifies.
    let payout = f.intent("polygon-test", "payout", None, "100", None).await;
    let Response::Signed { key, scheme, .. } = client.sign(&core, payout).await.unwrap() else {
        panic!("not signed");
    };
    assert_eq!(
        (key.as_str(), scheme.as_str()),
        ("polygon-payout", "secp256k1-evm")
    );

    // A key the configuration does not list is not the caller's: the mint key is on the file
    // backend too, and signs nothing.
    let mint = f.intent("kreivo-test", "mint", None, "100", None).await;
    assert!(matches!(
        client.sign(&core, mint).await.unwrap(),
        Response::Refused { .. }
    ));
    assert_eq!(f.signatures().await, 1);

    // A wrong token, an empty one and the hash itself are not the token.
    for wrong in ["not-the-token", "", &hex::encode(sha2_of(&token))] {
        let err = Client::connect(&config.socket, wrong, uid)
            .await
            .unwrap_err();
        assert!(matches!(err, LinkError::Unauthenticated), "{err}");
    }
    // Nor is the token of a caller with another user id the endpoint's.
    let other = f.dir.join("sock/other.sock");
    let listener = link::bind(&other).unwrap();
    let mut elsewhere = config.callers.clone();
    elsewhere[0].uid = uid.wrapping_add(1);
    let (signer, _, _) = Fixture::start(&config, &f.connect).await;
    tokio::spawn(link::serve(
        listener,
        Arc::new(Callers::sharing_the_signer_uid_in_tests(&elsewhere)),
        signer,
    ));
    let err = Client::connect(&other, &token, uid).await.unwrap_err();
    assert!(matches!(err, LinkError::Unauthenticated), "{err}");
    assert!(log.text().contains("signed"), "{}", log.text());
}

fn sha2_of(text: &str) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes()).to_vec()
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_cu_1_the_binary_refuses_to_prepare_a_caller_of_the_signers_own_user(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let spec = spec(&f);
    let path = f.dir.join("spec.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "template": spec.template, "config": spec.config,
            "signer": {"uid": spec.signer.uid, "gid": spec.signer.gid},
            "caller": {"name": "evm-endpoint", "uid": spec.signer.uid, "gid": spec.signer.gid},
            "token_file": spec.token_file, "socket_dir": spec.socket_dir,
        })
        .to_string(),
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bezent-signer"))
        .args(["prepare"])
        .arg(&path)
        .env_clear()
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("REQ-CU-1"), "{stderr}");
    assert!(!spec.token_file.exists() && !spec.config.exists());
}
