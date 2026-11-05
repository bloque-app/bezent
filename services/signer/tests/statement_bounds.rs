// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #84: the signer's one signing lock cannot be held for good by a holder that has stopped
//! making progress, and a holder that is ended writes nothing.
//!
//! The signer signs and inserts the signature in one transaction under an advisory lock. Here a
//! backend that does not answer (a hung key service) is inside that transaction. The server ends
//! the idle session (`idle_in_transaction_session_timeout`, a short one set through
//! [`pool_options_bounded`], as production sets its own), the lock is free, another signer signs
//! the same intent, and the first, when its backend finally answers, delivers nothing: a signature
//! that was made is not returned and not recorded, and the caller sees "unavailable", never
//! "refused" or "signed".

mod common;

use std::sync::Arc;
use std::time::Duration;

use bezent_signer::Outcome;
use bezent_signer::engine::{EngineKind, Health, Signable, SigningEngine};
use bezent_signer::keys::{Environment, FileBackend, KeyError, Scheme};
use bezent_signer::log::Log;
use bezent_signer::signer::{IDLE_IN_TRANSACTION_TIMEOUT, Signer, pool_options};
use bezent_store::login::{Bounds, pool_options_bounded};
use common::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::sync::Notify;

/// How long the server lets the signer's session sit idle inside its transaction, in the test.
const IDLE: Duration = Duration::from_millis(400);

/// How long the test waits for an outcome before it calls the lock held for good.
const STUCK: Duration = Duration::from_secs(30);

/// A key backend that says it has been asked, then does not answer until it is let go.
struct Hung {
    inner: FileBackend,
    asked: Arc<Notify>,
    release: Arc<Notify>,
}

impl SigningEngine for Hung {
    fn kind(&self) -> EngineKind {
        self.inner.kind()
    }
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn environment(&self) -> Environment {
        self.inner.environment()
    }
    fn key_ids(&self) -> Vec<String> {
        self.inner.key_ids()
    }
    fn scheme(&self, id: &str) -> Result<Scheme, KeyError> {
        self.inner.scheme(id)
    }
    fn public_key(&self, id: &str) -> Result<Vec<u8>, KeyError> {
        self.inner.public_key(id)
    }
    async fn sign_prepared(
        &self,
        id: &str,
        scheme: Scheme,
        input: Signable<'_>,
    ) -> Result<Vec<u8>, KeyError> {
        self.asked.notify_one();
        self.release.notified().await;
        self.inner.sign_prepared(id, scheme, input).await
    }
    async fn health(&self) -> Health {
        Health::ready()
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_84_a_hung_holder_of_the_signing_lock_is_ended_and_writes_nothing(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect.clone()).await;
    let id = f
        .intent("kreivo-test", "mint", Some(b"\x01"), "10", None)
        .await;

    // A signer whose backend hangs, over a pool made as production makes it, with a short bound.
    let (asked, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let pool = login_pool_with(
        &connect,
        SIGNER_LOGIN,
        bezent_store::SIGNER_ROLE,
        pool_options_bounded(Bounds {
            statement: Duration::ZERO,
            idle_in_transaction: IDLE,
        })
        .max_connections(4),
    )
    .await;
    let hung = Arc::new(Signer::new(
        pool,
        f.policy.clone(),
        Hung {
            inner: f.backend(),
            asked: asked.clone(),
            release: release.clone(),
        },
        Log::capture().0,
    ));
    let stuck = tokio::spawn({
        let hung = hung.clone();
        async move { hung.sign(id).await }
    });
    // The hung signer holds the lock inside its transaction, waiting for its backend.
    tokio::time::timeout(STUCK, asked.notified())
        .await
        .expect("the hung signer never reached its backend");

    // Another signer (the fixture's, a process of its own in production) is not kept out for good.
    let Outcome::Signed(signed) = tokio::time::timeout(STUCK, f.signer.sign(id))
        .await
        .expect("the signing lock was held by a holder that stopped answering")
    else {
        panic!("the second signer did not sign");
    };
    assert!(!signed.replayed);

    // The hung backend now answers. Its signature was made, but its transaction is gone: nothing is
    // delivered and nothing recorded, and it is "unavailable", which the caller retries.
    release.notify_one();
    let outcome = tokio::time::timeout(STUCK, stuck)
        .await
        .expect("the hung signer never returned")
        .unwrap();
    assert!(
        matches!(outcome, Outcome::Unavailable(_)),
        "the ended holder said {outcome:?}"
    );
    assert_eq!(
        f.signatures().await,
        1,
        "one signature, the second signer's"
    );
    let recorded: Vec<u8> =
        sqlx::query_scalar("SELECT signature FROM signatures WHERE intent_id = $1")
            .bind(id)
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert_eq!(recorded, signed.signature);

    // And a retry of the intent finds the recorded signature: no second signature is ever made.
    let Outcome::Signed(again) = f.signer.sign(id).await else {
        panic!("the retry did not return the signature");
    };
    assert!(again.replayed);
    assert_eq!(again.signature, signed.signature);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_84_the_signers_pool_has_its_own_idle_bound_and_the_stores_statement_bound(
    base: sqlx::PgPool,
) {
    assert_eq!(IDLE_IN_TRANSACTION_TIMEOUT, Duration::from_secs(150));
    let pool = pool_options()
        .connect_with((*base.connect_options()).clone())
        .await
        .unwrap();
    let (statement, idle): (String, String) = sqlx::query_as(
        "SELECT current_setting('statement_timeout'), \
                current_setting('idle_in_transaction_session_timeout')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((statement.as_str(), idle.as_str()), ("1min", "150s"));
}
