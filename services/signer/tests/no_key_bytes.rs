// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! T-005-06: no key material in any log, error or configuration dump (`REQ-KY-1`).
//!
//! The signer is driven through every path that writes a line or returns an error: a signature,
//! a replay, each guard's refusal, an unauthenticated connection, the compromise path, broken key
//! files and configurations, and the binary's own `--check`. Everything it printed is scanned for
//! each test key's seed (hex in either case, and raw) and for the sr25519 key's expanded secret.

mod common;

use bezent_signer::compromise::{self, ObservedSignature};
use bezent_signer::config::Config;
use bezent_signer::keys::{FileBackend, KeyBackend};
use bezent_signer::link::{self, Callers, Client};
use common::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// Every form of the test keys' secrets that must not appear.
fn needles() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for (id, _) in KEYS {
        let seed = seed(id);
        out.push((format!("{id} seed, raw"), seed.to_vec()));
        out.push((format!("{id} seed, hex"), hex::encode(seed).into_bytes()));
        out.push((
            format!("{id} seed, HEX"),
            hex::encode_upper(seed).into_bytes(),
        ));
        // A long prefix of the hex, in case something truncates.
        out.push((
            format!("{id} seed, hex prefix"),
            hex::encode(seed).as_bytes()[..24].to_vec(),
        ));
    }
    let mini = schnorrkel::MiniSecretKey::from_bytes(&seed("kreivo-mint")).unwrap();
    let expanded = mini
        .expand_to_keypair(schnorrkel::ExpansionMode::Ed25519)
        .secret
        .to_bytes();
    out.push(("kreivo-mint expanded secret, raw".into(), expanded.to_vec()));
    out.push((
        "kreivo-mint expanded secret, hex".into(),
        hex::encode(expanded).into_bytes(),
    ));
    out
}

fn assert_clean(what: &str, haystack: &[u8]) {
    for (name, needle) in needles() {
        assert!(
            !haystack
                .windows(needle.len())
                .any(|w| w == needle.as_slice()),
            "{what} carries {name}"
        );
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ky_1_no_key_bytes_in_logs_errors_or_config_dumps(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let mut printed: Vec<(String, String)> = Vec::new();

    // Signatures, a replay, and every guard's refusal.
    let ids = [
        f.intent("kreivo-test", "mint", Some(b"\x01"), "10", None)
            .await,
        f.intent("kreivo-test", "mint", Some(b"\x01"), "10", None)
            .await,
        f.intent("kreivo-test", "mint", None, "5000", None).await,
        f.intent("kreivo-test", "destroy", None, "1", Some("0xdead"))
            .await,
        f.intent("kreivo-test", "destroy", None, "1", Some(CONTRACT))
            .await,
        f.intent("polygon-test", "payout", None, "1", None).await,
        f.intent("kreivo", "mint", None, "1", None).await,
        f.intent("kreivo-test", "mint", None, "x", None).await,
        f.intent("base-test", "mint", None, "1", None).await,
    ];
    for id in ids.into_iter().chain([ids[0], 4242]) {
        printed.push((
            format!("outcome of {id}"),
            format!("{:?}", f.signer.sign(id).await),
        ));
    }

    // An unauthenticated connection, and an authenticated one.
    let socket = f.dir.join("s.sock");
    let listener = link::bind(&socket).unwrap();
    let callers = std::sync::Arc::new(Callers::sharing_the_signer_uid_in_tests(&f.config.callers));
    tokio::spawn(link::serve(listener, callers, f.signer.clone()));
    let refused = Client::connect(&socket, "wrong", my_uid())
        .await
        .unwrap_err();
    printed.push(("link error".into(), format!("{refused} {refused:?}")));
    let mut core = Client::connect(&socket, TOKEN, my_uid()).await.unwrap();
    printed.push((
        "link answer".into(),
        format!("{:?}", core.sign(&f.core_pool().await, ids[0]).await),
    ));

    // The compromise path.
    let core_pool = f.core_pool().await;
    let backend = f.backend();
    let rogue = backend.sign("kreivo-destroy", b"rogue").await.unwrap();
    let verdict = compromise::check(
        &core_pool,
        &ObservedSignature {
            network: "kreivo-test".into(),
            public_key: backend.public_key("kreivo-destroy").unwrap(),
            signature: rogue,
            block_number: 1,
            block_hash: vec![1],
            position_index: 0,
        },
    )
    .await
    .unwrap();
    printed.push(("verdict".into(), format!("{verdict:?}")));
    let events: Vec<String> = sqlx::query_scalar("SELECT payload::text FROM event_log")
        .fetch_all(&f.admin)
        .await
        .unwrap();
    printed.push(("event log".into(), events.join("\n")));

    // Configuration and key dumps.
    printed.push(("config".into(), format!("{:?}", f.config)));
    printed.push(("policy".into(), format!("{:?}", f.policy)));
    printed.push(("backend".into(), format!("{backend:?}")));
    printed.push(("signer".into(), format!("{:?}", f.signer)));

    // Errors from broken key files: each embeds a secret where a parser might quote it.
    let seed_hex = hex::encode(seed("kreivo-mint"));
    for broken in [
        // 31 bytes: the hex is the key's but one byte short.
        format!(
            r#"{{"environment": "test", "keys": [{{"id": "k", "scheme": "sr25519", "seed": "0x{}"}}]}}"#,
            &seed_hex[..62]
        ),
        // Not hex.
        format!(
            r#"{{"environment": "test", "keys": [{{"id": "k", "scheme": "sr25519", "seed": "{seed_hex}zz"}}]}}"#
        ),
        // The wrong type and an unknown field around the secret.
        format!(
            r#"{{"environment": "test", "keys": [{{"id": "k", "scheme": "x", "seed": "{seed_hex}"}}]}}"#
        ),
        format!(r#"{{"environment": "test", "{seed_hex}": 1, "keys": []}}"#),
        format!(
            r#"{{"environment": "test", "keys": [{{"id": "k", "scheme": "ecdsa", "seed": ["{seed_hex}"]}}]}}"#
        ),
        // Not JSON at all.
        format!("{seed_hex} {seed_hex}"),
        // A production key file.
        format!(
            r#"{{"environment": "production", "keys": [{{"id": "k", "scheme": "sr25519", "seed": "{seed_hex}"}}]}}"#
        ),
    ] {
        let err = FileBackend::parse(broken.as_bytes()).unwrap_err();
        printed.push(("key file error".into(), format!("{err} {err:?}")));
    }
    // And a configuration that quotes a secret where a parser might echo it.
    for broken in [
        format!(r#"{{"socket": "{seed_hex}", "callers": 1}}"#),
        format!(r#"{{"{seed_hex}": true}}"#),
    ] {
        let err = Config::parse(broken.as_bytes()).unwrap_err();
        printed.push(("config error".into(), format!("{err} {err:?}")));
    }

    // The binary's own output: --check, and a run that fails at the store.
    let config_path = f.dir.join("signer.json");
    for args in [vec!["--check"], vec![]] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_bezent-signer"))
            .args(&args)
            .arg(&config_path)
            .env_clear()
            .output()
            .unwrap();
        printed.push((
            format!("bezent-signer {args:?}"),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        ));
    }
    assert!(printed.last().unwrap().1.contains("database_url_file"));

    // The signer's log saw every path.
    let log = f.log.text();
    for expected in [
        "signed intent=",
        "replayed",
        "INV-2",
        "REQ-KY-3",
        "INV-4",
        "REQ-KY-6",
        "INV-9",
        "unauthenticated",
    ] {
        assert!(log.contains(expected), "the log lacks {expected}: {log}");
    }

    assert_clean("the signer's log", &f.log.bytes());
    for (what, text) in &printed {
        assert_clean(what, text.as_bytes());
    }
    // Nor does the core's token appear.
    assert!(!log.contains(TOKEN));
}
