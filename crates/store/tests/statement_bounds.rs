// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #84: every service's pool bounds the server side of a hung statement or a hung holder of a
//! lock, through the options it is made with ([`bezent_store::login::pool_options`]), on a real
//! PostgreSQL. A held lock and `pg_sleep` stand in for the stuck query; the bounds are short here
//! and set through the same path production uses ([`pool_options_bounded`]).
//!
//! No test depends on being quicker than a bound: each asserts an outcome (an error code, a lock
//! that is free again, a row that is not there) and waits for it at most `STUCK`.

use std::time::Duration;

use bezent_store::login::{
    Bounds, IDLE_IN_TRANSACTION_TIMEOUT, STATEMENT_TIMEOUT, pool_options, pool_options_bounded,
};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection, PgPool};

mod common;

/// The bound the tests give a connection.
const SHORT: Duration = Duration::from_millis(400);

/// How long a test waits for an outcome before it calls the server stuck.
const STUCK: Duration = Duration::from_secs(30);

/// SQLSTATE `query_canceled`: what `statement_timeout` raises.
const QUERY_CANCELED: &str = "57014";

/// An unbounded connection on the test's database, to hold a lock or to look.
async fn plain(pool: &PgPool) -> PgConnection {
    PgConnection::connect_with(&pool.connect_options())
        .await
        .unwrap()
}

/// One connection of a pool made with `bounds`, through the path production uses.
async fn bounded(pool: &PgPool, bounds: Bounds) -> PgConnection {
    let options: PgConnectOptions = (*pool.connect_options()).clone();
    pool_options_bounded(bounds)
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap()
        .acquire()
        .await
        .unwrap()
        .detach()
}

fn code(e: &sqlx::Error) -> String {
    e.as_database_error()
        .and_then(|d| d.code())
        .map(|c| c.to_string())
        .unwrap_or_else(|| format!("not a database error: {e}"))
}

fn statement_only() -> Bounds {
    Bounds {
        statement: SHORT,
        idle_in_transaction: Duration::ZERO,
    }
}

fn idle_only() -> Bounds {
    Bounds {
        statement: Duration::ZERO,
        idle_in_transaction: SHORT,
    }
}

async fn setting(conn: &mut PgConnection, name: &str) -> String {
    sqlx::query_scalar("SELECT current_setting($1)")
        .bind(name)
        .fetch_one(conn)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_84_every_services_pool_carries_both_bounds(pool: PgPool) {
    assert_eq!(STATEMENT_TIMEOUT, Duration::from_secs(60));
    assert_eq!(IDLE_IN_TRANSACTION_TIMEOUT, Duration::from_secs(90));
    assert_eq!(Bounds::DEFAULT.statement, STATEMENT_TIMEOUT);
    assert_eq!(
        Bounds::DEFAULT.idle_in_transaction,
        IDLE_IN_TRANSACTION_TIMEOUT
    );

    let options: PgConnectOptions = (*pool.connect_options()).clone();
    let mut conn = pool_options()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap()
        .acquire()
        .await
        .unwrap()
        .detach();
    assert_eq!(setting(&mut conn, "statement_timeout").await, "1min");
    assert_eq!(
        setting(&mut conn, "idle_in_transaction_session_timeout").await,
        "90s"
    );
    // A connection made any other way has neither (the server's default).
    let mut other = plain(&pool).await;
    assert_eq!(setting(&mut other, "statement_timeout").await, "0");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_84_a_statement_that_hangs_is_cancelled_and_the_connection_is_usable(pool: PgPool) {
    let mut conn = bounded(&pool, statement_only()).await;
    let e = tokio::time::timeout(
        STUCK,
        sqlx::query("SELECT pg_sleep(600)").execute(&mut conn),
    )
    .await
    .expect("the statement was not cancelled by the server")
    .unwrap_err();
    assert_eq!(code(&e), QUERY_CANCELED, "{e}");
    // Cancelled, not killed: the same connection answers.
    let one: i32 = sqlx::query_scalar("SELECT 1")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(one, 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_84_a_wait_for_a_held_lock_ends_and_the_lock_can_be_taken_after(pool: PgPool) {
    // The signing lock's shape: a transaction holds an advisory lock; another wants it.
    let mut holder = plain(&pool).await;
    let mut held = holder.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(84)")
        .execute(&mut *held)
        .await
        .unwrap();

    let mut waiter = bounded(&pool, statement_only()).await;
    let mut tx = waiter.begin().await.unwrap();
    let e = tokio::time::timeout(
        STUCK,
        sqlx::query("SELECT pg_advisory_xact_lock(84)").execute(&mut *tx),
    )
    .await
    .expect("the wait for the lock was not ended by the server")
    .unwrap_err();
    assert_eq!(code(&e), QUERY_CANCELED, "{e}");
    drop(tx);

    held.commit().await.unwrap();
    let mut tx = waiter.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(84)")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_84_a_holder_that_stops_answering_loses_its_lock_and_its_write(pool: PgPool) {
    // A holder of the signing lock that is stuck between statements (an unbounded await, a process
    // that hangs): no statement is running for `statement_timeout` to cut.
    sqlx::query("CREATE TABLE holder_probe (n int)")
        .execute(&pool)
        .await
        .unwrap();
    let mut holder = bounded(&pool, idle_only()).await;
    let mut tx = holder.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(85)")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO holder_probe VALUES (1)")
        .execute(&mut *tx)
        .await
        .unwrap();

    // Another session gets the lock once the server has ended the holder's session.
    let mut other = plain(&pool).await;
    tokio::time::timeout(STUCK, async {
        loop {
            let got: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(85)")
                .fetch_one(&mut other)
                .await
                .unwrap();
            if got {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the idle holder kept its lock");

    // The holder's transaction was rolled back whole: its commit is an error, its row is not there.
    assert!(tx.commit().await.is_err(), "the commit of a closed session");
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM holder_probe")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_84_a_commit_is_not_cut_by_the_statement_bound(pool: PgPool) {
    // `COMMIT` is a statement, but the server stops the timer before it does the commit proper.
    // What the commit can take long over is the check of a deferred constraint, here a sleep three
    // times the bound: if the bound covered it, the commit would fail after the work it asks for
    // was begun, and a caller would take a failure for a rollback it cannot be sure of.
    for sql in [
        "CREATE TABLE commit_probe (n int)",
        "CREATE FUNCTION slow_check() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
           PERFORM pg_sleep(1.2); RETURN NULL; END $$",
        "CREATE CONSTRAINT TRIGGER slow AFTER INSERT ON commit_probe \
           DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION slow_check()",
    ] {
        sqlx::query(sql).execute(&pool).await.unwrap();
    }
    let mut conn = bounded(&pool, statement_only()).await;
    let mut tx = conn.begin().await.unwrap();
    sqlx::query("INSERT INTO commit_probe VALUES (1)")
        .execute(&mut *tx)
        .await
        .unwrap();
    // The statement bound is real on this connection: the same wait, as a statement, is cut.
    let cut = sqlx::query("SELECT pg_sleep(1.2)")
        .execute(&mut *tx)
        .await
        .unwrap_err();
    assert_eq!(code(&cut), QUERY_CANCELED, "{cut}");
    drop(tx);

    let mut tx = conn.begin().await.unwrap();
    sqlx::query("INSERT INTO commit_probe VALUES (2)")
        .execute(&mut *tx)
        .await
        .unwrap();
    tokio::time::timeout(STUCK, tx.commit())
        .await
        .expect("the commit did not finish")
        .expect("the statement bound cut a commit");
    let rows: Vec<i32> = sqlx::query_scalar("SELECT n FROM commit_probe ORDER BY n")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(rows, vec![2]);
}
