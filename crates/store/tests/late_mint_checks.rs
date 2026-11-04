// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Migration 0075 (issue #26; `AC-I1.2`, `REQ-IC-4`, `INV-8`): the queue of final mint observations
//! the core's late-mint check works from. The store queues an observation in the transaction that
//! records its `final` outcome, whatever its id and whenever it is finalised; the migration queues
//! the ones already recorded; nobody but the core's verdict columns can change it.

mod common;

use common::{REVERSIBLE_MIGRATOR, exec};
use sqlx::PgPool;

const NETWORKS: &str = "INSERT INTO networks (network, role, address_format) VALUES \
     ('kreivo', 'issuing', 'ss58'), ('polygon', 'origin', 'evm')";

/// An observation of `event` on `network` at `block`, with a reference, executed; `outcome` is
/// recorded at once when given.
async fn observe(
    pool: &PgPool,
    network: &str,
    block: i64,
    event: &str,
    outcome: Option<&str>,
) -> i64 {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, on_chain_reference, executed, detail, kind) \
         VALUES ($1, $2, '\\x01', now(), 'log', 0, '\\x07', true, jsonb_build_object('event', $3::text), \
             observation_kind_for_event($3)) \
         RETURNING id",
    )
    .bind(network)
    .bind(block)
    .bind(event)
    .fetch_one(pool)
    .await
    .unwrap();
    if let Some(outcome) = outcome {
        sqlx::query(
            "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
                 final_block_hash) VALUES ($1, $2, $3, '\\x02')",
        )
        .bind(id)
        .bind(outcome)
        .bind(block + 2)
        .execute(pool)
        .await
        .unwrap();
    }
    id
}

async fn queued(pool: &PgPool) -> Vec<i64> {
    sqlx::query_scalar("SELECT observation_id FROM late_mint_checks ORDER BY 1")
        .fetch_all(pool)
        .await
        .unwrap()
}

/// What is queued: a final `Minted` or `RefundMinted` of the issuing network, and nothing else.
async fn seed(pool: &PgPool) -> Vec<i64> {
    exec(pool, NETWORKS).await;
    let minted = observe(pool, "kreivo", 10, "Minted", Some("final")).await;
    let refund = observe(pool, "kreivo", 11, "RefundMinted", Some("final")).await;
    // Not queued: withdrawn, undecided, another event, another network's (the store refuses a `Minted` on an origin network).
    observe(pool, "kreivo", 12, "Minted", Some("withdrawn")).await;
    observe(pool, "kreivo", 13, "Minted", None).await;
    observe(pool, "kreivo", 14, "Redeemed", Some("final")).await;
    observe(pool, "polygon", 15, "Arrival", Some("final")).await;
    vec![minted, refund]
}

#[sqlx::test(migrations = false)]
async fn ac_i1_2_the_migration_queues_the_final_mints_already_recorded(pool: PgPool) {
    REVERSIBLE_MIGRATOR.run(&pool).await.expect("up");
    REVERSIBLE_MIGRATOR
        .undo(&pool, 74)
        .await
        .expect("down to just before 0075");
    let expected = seed(&pool).await;
    REVERSIBLE_MIGRATOR.run(&pool).await.expect("up again");
    assert_eq!(queued(&pool).await, expected);
    let open: i64 =
        sqlx::query_scalar("SELECT count(*) FROM late_mint_checks WHERE settled_at IS NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(open, 2, "each is still to be looked at");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_an_outcome_queues_its_observation_when_it_is_recorded_and_only_a_final_mint(
    pool: PgPool,
) {
    exec(&pool, NETWORKS).await;
    // Observed first, finalised later: queued when finalised.
    let early = observe(&pool, "kreivo", 10, "Minted", None).await;
    assert!(queued(&pool).await.is_empty());
    let later = observe(&pool, "kreivo", 11, "Minted", Some("final")).await;
    assert_eq!(queued(&pool).await, vec![later]);
    sqlx::query(
        "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
             final_block_hash) VALUES ($1, 'final', 12, '\\x02')",
    )
    .bind(early)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(queued(&pool).await, vec![early, later]);
    observe(&pool, "kreivo", 13, "Minted", Some("withdrawn")).await;
    observe(&pool, "polygon", 14, "Arrival", Some("final")).await;
    assert_eq!(queued(&pool).await, vec![early, later]);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_a_queued_row_is_never_deleted_and_a_verdict_names_its_settling(pool: PgPool) {
    exec(&pool, NETWORKS).await;
    let id = observe(&pool, "kreivo", 10, "Minted", Some("final")).await;
    let deleted = sqlx::query("DELETE FROM late_mint_checks")
        .execute(&pool)
        .await;
    assert!(deleted.is_err(), "a check is never unqueued");
    let half =
        sqlx::query("UPDATE late_mint_checks SET settled_at = now() WHERE observation_id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    assert!(half.is_err(), "settled with no verdict");
    sqlx::query(
        "UPDATE late_mint_checks SET settled_at = now(), verdict = 'not-late' WHERE observation_id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
}

/// The look at one signed transaction and the open set are served by indexes, not scans.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i1_2_the_check_has_the_indexes_it_reads_by(pool: PgPool) {
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT indexname::text FROM pg_indexes WHERE schemaname = 'public' \
           AND indexname IN ('late_mint_checks_open', 'observations_by_signed_transaction', \
                             'event_log_anomaly_by_signed_transaction') ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        names,
        [
            "event_log_anomaly_by_signed_transaction",
            "late_mint_checks_open",
            "observations_by_signed_transaction"
        ]
    );
}
