// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-004 T-004-05: the statement key is ed25519 and used for nothing else (`REQ-PR-1`). The signer
//! signs only intents bound to a network by its configuration; the statement key bound to any
//! network refuses the configuration, so it can never sign a transaction. (Another ed25519 key
//! may be a Kreivo delivery account: `tests/ed25519_delivery.rs`.)
//! Issue #42 (PLAN DEC-16, 2026-10-14): the statement key is named apart and signs statement
//! intents only, through the same recorded-intent path as every other key.

mod common;

use bezent_signer::config::Config;
use bezent_signer::keys::{FileBackend, KeyBackend, Scheme};
use bezent_signer::signer::{Outcome, Refusal};
use common::*;
use sha2::Digest;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

#[tokio::test]
async fn req_pr_1_the_statement_key_is_refused_for_any_network_binding() {
    let dir = temp_dir();
    let mut keys: Vec<_> = KEYS
        .iter()
        .map(|(id, scheme)| {
            serde_json::json!({"id": id, "scheme": scheme, "seed": format!("0x{}", hex::encode(seed(id)))})
        })
        .collect();
    keys.push(serde_json::json!({"id": "statement", "scheme": "ed25519",
                                 "seed": format!("0x{}", hex::encode(seed("statement")))}));
    let path = dir.join("test-keys.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({"environment": "test", "keys": keys})).unwrap(),
    )
    .unwrap();
    let backend = FileBackend::load(&path).unwrap();
    assert_eq!(backend.scheme("statement").unwrap(), Scheme::Ed25519);

    // Held beside the delivery keys and bound to nothing: the configuration stands.
    let json = config_json(&dir, my_uid(), TOKEN);
    let config = Config::parse(json.to_string().as_bytes()).unwrap();
    assert!(config.policy(&backend).is_ok());

    // Bound to a network, for any purpose: refused.
    let mut json = config_json(&dir, my_uid(), TOKEN);
    json["statement_key"] = "statement".into();
    json["bindings"][2]["key"] = "statement".into();
    let config = Config::parse(json.to_string().as_bytes()).unwrap();
    let err = config.policy(&backend).unwrap_err();
    assert!(err.0.contains("REQ-PR-1"), "{err}");
    std::fs::remove_dir_all(dir).ok();
}

/// The fixture's keys and the statement key, written into `dir`.
fn keys_with_statement(dir: &std::path::Path) -> FileBackend {
    let mut keys: Vec<_> = KEYS
        .iter()
        .map(|(id, scheme)| {
            serde_json::json!({"id": id, "scheme": scheme, "seed": format!("0x{}", hex::encode(seed(id)))})
        })
        .collect();
    keys.push(serde_json::json!({"id": "statement", "scheme": "ed25519",
                                 "seed": format!("0x{}", hex::encode(seed("statement")))}));
    let path = dir.join("test-keys.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({"environment": "test", "keys": keys})).unwrap(),
    )
    .unwrap();
    FileBackend::load(&path).unwrap()
}

/// Issue #42 (PLAN DEC-16, 2026-10-14): the statement key is named apart, by `statement_key`; it is
/// ed25519, bound to no network, and no binding takes the purpose `statement`.
#[tokio::test]
async fn req_pr_1_the_statement_key_is_named_apart_and_no_binding_takes_its_purpose() {
    let dir = temp_dir();
    let backend = keys_with_statement(&dir);
    let policy = |json: serde_json::Value| {
        Config::parse(json.to_string().as_bytes())
            .unwrap()
            .policy(&backend)
    };

    let mut json = config_json(&dir, my_uid(), TOKEN);
    json["statement_key"] = "statement".into();
    assert_eq!(policy(json).unwrap().statement_key(), Some("statement"));

    // Another scheme is not the statement key.
    let mut json = config_json(&dir, my_uid(), TOKEN);
    json["statement_key"] = "kreivo-mint".into();
    let err = policy(json).unwrap_err();
    assert!(
        err.0.contains("REQ-PR-1") && err.0.contains("ed25519"),
        "{err}"
    );

    // No key signs `statement` on a network.
    let mut json = config_json(&dir, my_uid(), TOKEN);
    json["bindings"][0]["purposes"] = serde_json::json!(["mint", "statement"]);
    let err = policy(json).unwrap_err();
    assert!(err.0.contains("REQ-PR-1"), "{err}");
    std::fs::remove_dir_all(dir).ok();
}

/// A statement document as the core composes it, as far as the signer reads it.
fn statement(sequence: i64, public_key: &[u8], note: &str) -> Vec<u8> {
    serde_json::to_vec_pretty(&serde_json::json!({
        "statement": "bezent reserve statement",
        "asset": "bUSDC",
        "sequence": sequence,
        "signing_key": {"scheme": "ed25519", "public_key": format!("0x{}", hex::encode(public_key))},
        "note": note,
    }))
    .unwrap()
}

/// Records a statement intent for `doc`, as the core does (0039 holds its shape).
async fn statement_intent(pool: &sqlx::PgPool, sequence: i64, doc: &[u8]) -> i64 {
    let payload = serde_json::json!({
        "message": format!("0x{}", hex::encode(doc)),
        "amount": "0",
        "statement": {"asset": "bUSDC", "sequence": sequence,
                      "sha256": format!("0x{}", hex::encode(sha2::Sha256::digest(doc)))},
    });
    sqlx::query_scalar(
        "INSERT INTO intents (network, purpose, payload) VALUES ('kreivo-test', 'statement', \
           $1::text::jsonb) RETURNING id",
    )
    .bind(payload.to_string())
    .fetch_one(pool)
    .await
    .unwrap()
}

fn refusal(outcome: Outcome) -> Refusal {
    match outcome {
        Outcome::Refused(r) => r,
        other => panic!("not refused: {other:?}"),
    }
}

/// Issue #42 (PLAN DEC-16, 2026-10-14): the signer signs a statement intent with the statement
/// key, and only a statement: of the asset and sequence the intent names, naming that key; never a
/// second, different statement for the same place (`REQ-PR-1`, `REQ-PR-2`, `INV-9`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_pr_1_req_pr_2_dec_16_a_statement_intent_is_signed_by_the_statement_key_only(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    sqlx::raw_sql(
        "INSERT INTO networks (network, role, address_format) VALUES \
           ('kreivo-test', 'issuing', 'ss58'), ('polygon-test', 'origin', 'evm') \
           ON CONFLICT DO NOTHING; \
         INSERT INTO assets (asset, precision, assurance_class, issuing_asset) \
           VALUES ('bUSDC', 6, 'public', 'Here(201)');",
    )
    .execute(&f.admin)
    .await
    .unwrap();
    let backend = keys_with_statement(&f.dir);
    let public = backend.public_key("statement").unwrap();
    let mut json = config_json(&f.dir, my_uid(), TOKEN);
    json["statement_key"] = "statement".into();
    let config = Config::parse(json.to_string().as_bytes()).unwrap();
    let (signer, _log, _) = Fixture::start(&config, &f.connect).await;

    let doc = statement(1, &public, "first");
    let id = statement_intent(&f.admin, 1, &doc).await;
    // A signer with no statement key signs none.
    assert!(matches!(
        refusal(f.signer.sign(id).await),
        Refusal::NoKey { .. }
    ));
    let signed = match signer.sign(id).await {
        Outcome::Signed(s) => s,
        other => panic!("not signed: {other:?}"),
    };
    assert_eq!(signed.scheme, "ed25519");
    assert_eq!(signed.key_id, "statement");
    assert_eq!(signed.public_key, public);
    assert!(bezent_signer::verify::verifies_exactly(
        "ed25519",
        &public,
        &doc,
        &signed.signature
    ));
    // Asked again: the recorded signature, nothing new.
    let Outcome::Signed(again) = signer.sign(id).await else {
        panic!("not replayed");
    };
    assert!(again.replayed);
    assert_eq!(again.signature, signed.signature);

    // Another statement for the same place: refused, whatever it says.
    let other = statement(1, &public, "a revision");
    let id2 = statement_intent(&f.admin, 1, &other).await;
    let r = refusal(signer.sign(id2).await);
    assert!(
        matches!(r, Refusal::StatementEquivocation { sequence: 1, .. }),
        "{r:?}"
    );
    assert_eq!(r.spec(), "REQ-PR-2");

    // Bytes that do not name the statement key, or are not a statement at all: refused.
    let elsewhere = statement(2, &[7; 32], "another key");
    let r = refusal(
        signer
            .sign(statement_intent(&f.admin, 2, &elsewhere).await)
            .await,
    );
    assert!(matches!(r, Refusal::NotAStatement(_)), "{r:?}");
    let r = refusal(
        signer
            .sign(statement_intent(&f.admin, 3, br#"{"not": "a statement"}"#).await)
            .await,
    );
    assert!(matches!(r, Refusal::NotAStatement(_)), "{r:?}");
    assert_eq!(r.spec(), "REQ-PR-1");

    // The statement key signs nothing but statements: a mint intent never reaches it.
    let mint = f
        .intent("kreivo-test", "mint", Some(&[9; 32]), "1", None)
        .await;
    let Outcome::Signed(m) = signer.sign(mint).await else {
        panic!("the mint is the mint key's");
    };
    assert_eq!(m.key_id, "kreivo-mint");
}

/// A statement document for `sequence`, naming `previous` and `abandoned` places.
fn chained(sequence: i64, public_key: &[u8], previous: Option<&[u8]>, abandoned: &str) -> Vec<u8> {
    let abandoned: serde_json::Value = serde_json::from_str(abandoned).unwrap();
    serde_json::to_vec_pretty(&serde_json::json!({
        "statement": "bezent reserve statement",
        "asset": "bUSDC",
        "sequence": sequence,
        "previous_sha256": previous.map(|p| format!("0x{}", hex::encode(sha2::Sha256::digest(p)))),
        "abandoned": abandoned,
        "signing_key": {"scheme": "ed25519", "public_key": format!("0x{}", hex::encode(public_key))},
    }))
    .unwrap()
}

/// PR #50 re-check S-1, S-3: the signer signs a statement only at a place the chain allows. It
/// never skips a place that is not abandoned, never signs at an abandoned place, and signs past
/// an abandoned place only a statement that names it, with the hash of what was signed for it
/// (`REQ-PR-2`). The rule is the store's, read through its one function.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s_1_s_3_req_pr_2_the_signer_skips_only_abandoned_places_naming_each(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    sqlx::raw_sql(
        "INSERT INTO networks (network, role, address_format) VALUES \
           ('kreivo-test', 'issuing', 'ss58'), ('polygon-test', 'origin', 'evm') \
           ON CONFLICT DO NOTHING; \
         INSERT INTO assets (asset, precision, assurance_class, issuing_asset) \
           VALUES ('bUSDC', 6, 'public', 'Here(201)');",
    )
    .execute(&f.admin)
    .await
    .unwrap();
    let backend = keys_with_statement(&f.dir);
    let public = backend.public_key("statement").unwrap();
    let mut json = config_json(&f.dir, my_uid(), TOKEN);
    json["statement_key"] = "statement".into();
    let config = Config::parse(json.to_string().as_bytes()).unwrap();
    let (signer, _log, _) = Fixture::start(&config, &f.connect).await;

    // Place 1 is signed, and never published.
    let one = chained(1, &public, None, "[]");
    let i1 = statement_intent(&f.admin, 1, &one).await;
    assert!(matches!(signer.sign(i1).await, Outcome::Signed(_)));
    // Place 2 may not skip it while it is not abandoned.
    let two = chained(2, &public, None, "[]");
    let r = refusal(signer.sign(statement_intent(&f.admin, 2, &two).await).await);
    assert!(
        matches!(r, Refusal::StatementPlace { sequence: 2, .. }),
        "{r:?}"
    );
    assert_eq!(r.spec(), "REQ-PR-2");

    // The core abandons place 1 (as the store lets it: the signed hash, the key, the intent).
    let sha_one = format!("0x{}", hex::encode(sha2::Sha256::digest(&one)));
    sqlx::query(
        "INSERT INTO reserve_statement_abandoned (asset, sequence, sha256, public_key, intent_id, \
             reason, actor) VALUES ('bUSDC', 1, $1, $2, $3, 'it no longer re-derives', '')",
    )
    .bind(sha2::Sha256::digest(&one).to_vec())
    .bind(&public)
    .bind(i1)
    .execute(&f.admin)
    .await
    .unwrap();
    // Nothing more is signed for place 1.
    let again = chained(1, &public, None, "[]");
    let r = refusal(
        signer
            .sign(statement_intent(&f.admin, 1, &again).await)
            .await,
    );
    assert!(
        matches!(
            r,
            Refusal::StatementEquivocation { .. } | Refusal::StatementPlace { .. }
        ),
        "{r:?}"
    );
    // Place 2 must name the abandoned place with its hash, exactly.
    for wrong in [
        "[]".to_owned(),
        format!(
            "[{{\"sequence\": 1, \"sha256\": \"0x{}\"}}]",
            "00".repeat(32)
        ),
        format!("[{{\"sequence\": 3, \"sha256\": \"{sha_one}\"}}]"),
    ] {
        let doc = chained(2, &public, None, &wrong);
        let r = refusal(signer.sign(statement_intent(&f.admin, 2, &doc).await).await);
        assert!(
            matches!(r, Refusal::StatementPlace { .. }),
            "{wrong}: {r:?}"
        );
    }
    let named = format!(
        "[{{\"sequence\": 1, \"sha256\": \"{sha_one}\", \"reason\": \"it no longer re-derives\"}}]"
    );
    let two = chained(2, &public, None, &named);
    let Outcome::Signed(s) = signer.sign(statement_intent(&f.admin, 2, &two).await).await else {
        panic!("a statement naming the abandoned place it skips is signed");
    };
    assert_eq!(s.key_id, "statement");
}
