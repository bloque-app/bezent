// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! T-005-02: the core↔signer link is authenticated both ways (`REQ-AR-3`, `REQ-CU-1`). An
//! unauthenticated caller is refused before any request is read, and the core refuses a socket
//! the signer's user does not serve.

mod common;

use std::sync::Arc;

use bezent_signer::config::Caller;
use bezent_signer::keys::KeyBackend;
use bezent_signer::link::{self, Callers, Client, LinkError, Request, Response};
use common::*;
use sha2::{Digest, Sha256};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::net::UnixStream;

fn callers(uid: u32) -> Arc<Callers> {
    Arc::new(Callers::sharing_the_signer_uid_in_tests(&[Caller {
        name: "core".into(),
        uid,
        token_sha256: vec![hex::encode(Sha256::digest(TOKEN.as_bytes()))],
    }]))
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ar_3_unauthenticated_caller_is_refused(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let uid = my_uid();
    let id = f.intent("kreivo-test", "mint", None, "1", None).await;

    let socket = f.dir.join("s.sock");
    let listener = link::bind(&socket).unwrap();
    tokio::spawn(link::serve(listener, callers(uid), f.signer.clone()));

    // A wrong token.
    let err = Client::connect(&socket, "not-the-token", uid)
        .await
        .unwrap_err();
    assert!(matches!(err, LinkError::Unauthenticated), "{err}");

    // No token at all: a request as the first frame.
    let mut raw = UnixStream::connect(&socket).await.unwrap();
    assert_eq!(
        Client::raw(&mut raw, &Request::Sign { intent: id })
            .await
            .unwrap(),
        Response::Unauthenticated
    );

    // The right token from a user id the configuration does not list.
    let other = f.dir.join("other.sock");
    let listener = link::bind(&other).unwrap();
    tokio::spawn(link::serve(
        listener,
        callers(uid.wrapping_add(1)),
        f.signer.clone(),
    ));
    let err = Client::connect(&other, TOKEN, uid).await.unwrap_err();
    assert!(matches!(err, LinkError::Unauthenticated), "{err}");

    // Nothing was signed, and every refusal is logged.
    assert_eq!(f.signatures().await, 0);
    assert_eq!(
        f.log.text().matches("REQ-AR-3: unauthenticated").count(),
        3,
        "{}",
        f.log.text()
    );

    // The authenticated core is served, and the signature verifies over the recorded bytes.
    let pool = f.core_pool().await;
    let mut core = Client::connect(&socket, TOKEN, uid).await.unwrap();
    let Response::Signed { intent, key, .. } = core.sign(&pool, id).await.unwrap() else {
        panic!("not signed");
    };
    assert_eq!((intent, key.as_str()), (id, "kreivo-mint"));
    assert!(matches!(
        core.sign(&pool, 4242).await.unwrap(),
        Response::Refused { spec, .. } if spec == "INV-9"
    ));
}

#[test]
fn req_cu_1_signer_refuses_a_caller_with_its_own_uid() {
    let caller = |uid| Caller {
        name: "core".into(),
        uid,
        token_sha256: vec![hex::encode(Sha256::digest(TOKEN.as_bytes()))],
    };
    let err = Callers::new(&[caller(1000)], 1000).unwrap_err();
    assert_eq!((err.caller.as_str(), err.uid), ("core", 1000));
    assert!(err.to_string().contains("REQ-CU-1"), "{err}");
    assert!(Callers::new(&[caller(1001)], 1000).is_ok());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_cu_1_signer_does_not_start_beside_a_caller_with_its_uid(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    // The fixture's caller runs as this process's user, as the signer binary would.
    let mut json = config_json(&f.dir, my_uid(), TOKEN);
    json["database_url_file"] = f.dir.join("no-such-url").to_string_lossy().into();
    let path = f.dir.join("shared-uid.json");
    std::fs::write(&path, json.to_string()).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bezent-signer"))
        .arg(&path)
        .env_clear()
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("the signer's own") && stderr.contains("REQ-CU-1"),
        "{stderr}"
    );
    assert!(
        !f.dir.join("s.sock").exists(),
        "the signer bound its socket"
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_core_refuses_a_returned_signature_over_other_bytes(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let uid = my_uid();
    let id = f
        .intent("polygon-test", "payout", Some(b"\x0c"), "10", None)
        .await;
    // Review M1: a compromised signer records a signature over other bytes against the intent;
    // the signer then returns it as the intent's (a replay).
    let backend = f.backend();
    // SET LOCAL ROLE, in a transaction: the pooled connection never keeps the role (#27).
    let mut conn = f.admin.begin().await.unwrap();
    sqlx::raw_sql("SET LOCAL ROLE bezent_signer")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO signatures (intent_id, key_id, scheme, public_key, signature) \
         VALUES ($1, 'polygon-payout', 'secp256k1-evm', $2, $3)",
    )
    .bind(id)
    .bind(backend.public_key("polygon-payout").unwrap())
    .bind(
        backend
            .sign("polygon-payout", b"rogue drain")
            .await
            .unwrap(),
    )
    .execute(&mut *conn)
    .await
    .unwrap();
    conn.commit().await.unwrap();

    let socket = f.dir.join("s.sock");
    let listener = link::bind(&socket).unwrap();
    tokio::spawn(link::serve(listener, callers(uid), f.signer.clone()));
    let pool = f.core_pool().await;
    let mut core = Client::connect(&socket, TOKEN, uid).await.unwrap();
    // Unverified, the signer's answer looks like a signature.
    assert!(matches!(
        core.request(id).await.unwrap(),
        Response::Signed { replayed: true, .. }
    ));
    // The core verifies it, refuses it, and raises the alarm.
    let err = core.sign(&pool, id).await.unwrap_err();
    let LinkError::Forged {
        intent,
        alert_seq,
        pause_seq,
    } = err
    else {
        panic!("not refused as forged: {err}");
    };
    assert_eq!(intent, id);
    let rows: Vec<(i64, String, String)> =
        sqlx::query_as("SELECT seq, kind, payload::text FROM event_log ORDER BY seq")
            .fetch_all(&f.admin)
            .await
            .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (rows[0].0, rows[0].1.as_str()),
        (alert_seq, "key-compromise")
    );
    assert!(rows[0].2.contains("signer-link"), "{}", rows[0].2);
    assert_eq!(
        (rows[1].0, rows[1].1.as_str()),
        (pause_seq, "pause-requested")
    );
    assert!(rows[1].2.contains("\"scope\": \"global\""));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ar_3_core_refuses_a_socket_the_signer_does_not_serve(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let uid = my_uid();
    let socket = f.dir.join("s.sock");
    let listener = link::bind(&socket).unwrap();
    tokio::spawn(link::serve(listener, callers(uid), f.signer.clone()));
    // The core expects the signer to run as another user: it sends no token to this socket.
    let err = Client::connect(&socket, TOKEN, uid.wrapping_add(1))
        .await
        .unwrap_err();
    assert!(matches!(err, LinkError::WrongPeer { .. }), "{err}");
    assert!(!f.log.text().contains("unauthenticated"));
}
