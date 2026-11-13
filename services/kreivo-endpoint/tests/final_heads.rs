// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #131: every outcome the endpoint writes rests on a recorded final head (`REQ-OB-2`,
//! `REQ-OB-5`), recorded in the same transaction with its block time read from the chain; another
//! hash at its height records nothing. Heads that outcomes written before this rule name are
//! recorded by the endpoint itself, once the finalized chain shows the hash they name, so a
//! withdrawal that waited on one reaches `Source-Final` with no hand edit (`INV-7`, `REQ-RV-1`).
//! The service layer (SPEC §9).

mod common;

use bezent_core_service::{Core, Settings};
use bezent_kreivo_endpoint::chain::BlockRef;
use bezent_kreivo_endpoint::chain::fake::{FakeChain, time_of};
use bezent_kreivo_endpoint::kreivo::{AssetId, Backend, ContractEvent, Payout};
use bezent_kreivo_endpoint::observe::{Backfill, Follower, Step};
use bezent_kreivo_endpoint::supply::{Recorded, Supplies};
use chrono::{DateTime, Duration, Utc};
use common::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

const ORIGIN: &str = "polygon-test";
const ASSET: &str = "bUSDC";
const TOKEN_ID: &str = "0x41e94eb019c0762f9bfcf9fb1e58725bfb0e7582";
const BENEFICIARY: [u8; 20] = [0xab; 20];

async fn exec(pool: &sqlx::PgPool, sql: &str) {
    sqlx::raw_sql(sql)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

fn redeemed(reference: [u8; 32]) -> ContractEvent {
    ContractEvent::Redeemed {
        seq: 0,
        reference,
        redeemer: [7; 32],
        asset: AssetId::Here(101),
        amount: 1_000_000,
        backend: Backend::Evm { chain_id: 80002 },
        payout: Payout::Address(BENEFICIARY),
    }
}

async fn follow(follower: &Follower<'_, FakeChain>) -> Step {
    loop {
        match follower.step().await.unwrap() {
            Step::Read { .. } => continue,
            other => return other,
        }
    }
}

fn supplies(f: &Fixture) -> Supplies<'_, FakeChain> {
    Supplies {
        chain: &f.chain,
        pool: &f.endpoint,
        network: NETWORK,
        contract: CONTRACT,
        delivery: f.lane.account,
    }
}

/// The final point the one outcome names.
async fn final_point(f: &Fixture) -> (i64, Vec<u8>) {
    sqlx::query_as("SELECT final_block_number, final_block_hash FROM observation_outcomes")
        .fetch_one(&f.admin)
        .await
        .unwrap()
}

/// The final head recorded at `number`: its hash and block time, in milliseconds.
async fn head_at(f: &Fixture, number: u64) -> Option<(Vec<u8>, i64)> {
    sqlx::query_as(
        "SELECT block_hash, (extract(epoch FROM block_time) * 1000)::bigint FROM final_heads \
         WHERE network = $1 AND block_number = $2",
    )
    .bind(NETWORK)
    .bind(number as i64)
    .fetch_optional(&f.admin)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ob_2_an_outcome_is_recorded_with_the_final_head_it_rests_on(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    // Block 4: a redemption. The head is far past the first rung (blocks 1 to 5), so the outcome
    // rests on a head that is not among the blocks read.
    f.chain.emit(redeemed([1; 32]));
    f.chain.advance(12);
    let finalized = f.chain.finalized_ref();
    assert!(matches!(follow(&f.follower()).await, Step::CaughtUp { .. }));
    let (number, hash) = final_point(&f).await;
    assert_eq!(number as u64, finalized.number);
    assert_eq!(hash, finalized.hash.to_vec());
    assert_eq!(
        head_at(&f, finalized.number).await,
        Some((finalized.hash.to_vec(), time_of(finalized.number) as i64))
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ob_2_another_hash_recorded_at_the_heads_height_records_nothing(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    f.chain.emit(redeemed([2; 32]));
    f.chain.advance(1);
    let finalized = f.chain.finalized_ref();
    exec(
        &f.admin,
        &format!(
            "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
             VALUES ('{NETWORK}', {}, '\\x{}', now())",
            finalized.number,
            "ee".repeat(32)
        ),
    )
    .await;
    let step = follow(&f.follower()).await;
    let Step::Unavailable(why) = &step else {
        panic!("{step:?}");
    };
    assert!(why.contains("REQ-OB-2"), "{why}");
    // Nothing is recorded under it: no observation, no outcome, no range.
    assert_eq!(f.count("SELECT count(*) FROM observations").await, 0);
    assert_eq!(
        f.count("SELECT count(*) FROM observation_outcomes").await,
        0
    );
    assert_eq!(
        f.count("SELECT count(*) FROM observation_ranges WHERE outcome = 'read'")
            .await,
        0
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_8_the_head_an_outcome_rests_on_has_its_supply_readings(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    exec(
        &f.admin,
        &format!(
            "INSERT INTO assets (asset, precision, assurance_class, issuing_asset) \
             VALUES ('{ASSET}', 6, 'public', 'Here(101)')"
        ),
    )
    .await;
    f.chain.set_supply(101, 5_000, 5_000);
    let s = supplies(&f);
    let supply_at = |n: u64| {
        let admin = f.admin.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM reserve_readings WHERE kind = 'supply' AND block_number = $1",
            )
            .bind(n as i64)
            .fetch_one(&admin)
            .await
            .unwrap()
        }
    };

    // With the supply reader, as the endpoint runs: the head is recorded with its supply, so the
    // reserve check, which takes the latest final head read to, finds supply there (REQ-BK-8).
    f.chain.emit(redeemed([3; 32]));
    f.chain.advance(1);
    let first = f.chain.finalized_ref();
    let follower = Follower {
        supplies: Some(&s),
        ..f.follower()
    };
    assert!(matches!(follow(&follower).await, Step::CaughtUp { .. }));
    assert!(head_at(&f, first.number).await.is_some());
    assert_eq!(supply_at(first.number).await, 1);

    // Without it, the head is recorded alone; the supply reader then reads it there, and does not
    // pass it over as already recorded.
    f.chain.emit(redeemed([4; 32]));
    f.chain.advance(1);
    let second = f.chain.finalized_ref();
    assert!(matches!(follow(&f.follower()).await, Step::CaughtUp { .. }));
    assert!(head_at(&f, second.number).await.is_some());
    assert_eq!(supply_at(second.number).await, 0);
    assert!(matches!(
        s.record_at(&second).await.unwrap(),
        Recorded::At { .. }
    ));
    assert_eq!(supply_at(second.number).await, 1);
    assert_eq!(
        s.record_at(&second).await.unwrap(),
        Recorded::Already(second.number)
    );
}

/// The counts of a backfill, without its findings.
fn counts(b: &Backfill) -> (usize, usize, usize, usize) {
    (b.recorded, b.mismatched, b.pending, b.too_far)
}

/// What the endpoint does each pass before the backfill: follows to the finalized head (the
/// watermark) and records the supply there.
async fn pass(f: &Fixture) {
    assert!(matches!(follow(&f.follower()).await, Step::CaughtUp { .. }));
    supplies(f).record().await.unwrap();
}

/// Writes the observation and the outcome of `event` at block `at`, final under `under`, as the
/// endpoint did before issue #131: the head is not recorded. Returns the observation's id.
async fn recorded_before_131(f: &Fixture, at: BlockRef, under: (u64, [u8; 32])) -> i64 {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, token, amount, sender, on_chain_reference, executed, detail, kind) \
         VALUES ($1, $2, $3, to_timestamp(0) + $4 * interval '1 millisecond', 'log', 9, \
             'Here(101)', 1000000, $5, $6, true, $7, 'redemption') RETURNING id",
    )
    .bind(NETWORK)
    .bind(at.number as i64)
    .bind(at.hash.as_slice())
    .bind(time_of(at.number) as i64)
    .bind(format!("0x{}", hex::encode([7u8; 32])))
    .bind([0x0b_u8; 32].as_slice())
    .bind(serde_json::json!({
        "contract": format!("0x{}", hex::encode(CONTRACT)), "event": "Redeemed", "seq": 0,
        "backend": {"kind": "evm", "chain_id": 80002},
        "payout": {"address": format!("0x{}", hex::encode(BENEFICIARY))},
    }))
    .fetch_one(&f.admin)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
             final_block_hash) VALUES ($1, 'final', $2, $3)",
    )
    .bind(id)
    .bind(under.0 as i64)
    .bind(under.1.as_slice())
    .execute(&f.admin)
    .await
    .unwrap();
    id
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ob_2_the_endpoint_records_a_head_an_earlier_outcome_names_once_the_chain_shows_it(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    f.chain.advance(20);
    let under = f.chain.block(14);
    recorded_before_131(&f, f.chain.block(13), (under.number, under.hash)).await;
    assert!(head_at(&f, 14).await.is_none());

    pass(&f).await;
    let done = f.follower().record_missing_heads().await.unwrap();
    assert_eq!(counts(&done), (1, 0, 0, 0), "{done:?}");
    // The chain's own hash and block time at that height.
    assert_eq!(
        head_at(&f, 14).await,
        Some((under.hash.to_vec(), time_of(14) as i64))
    );
    // Nothing is left to record, and nothing is recorded twice.
    assert_eq!(
        f.follower().record_missing_heads().await.unwrap(),
        Backfill::default()
    );
    // The head the supply reader recorded, and the one backfilled.
    assert_eq!(f.count("SELECT count(*) FROM final_heads").await, 2);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ob_2_a_head_whose_hash_the_finalized_chain_does_not_have_is_never_recorded(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    f.chain.advance(20);
    recorded_before_131(&f, f.chain.block(13), (14, [0xee; 32])).await;
    // Above the finalized head: not reachable yet, so not decided either way.
    recorded_before_131(
        &f,
        BlockRef {
            number: 30,
            hash: [0x30; 32],
        },
        (31, [0x31; 32]),
    )
    .await;
    pass(&f).await;
    for _ in 0..2 {
        let done = f.follower().record_missing_heads().await.unwrap();
        assert_eq!(counts(&done), (0, 1, 1, 0), "{done:?}");
        assert_eq!(done.findings.len(), 2, "{done:?}");
    }
    // Only the supply reader's head.
    assert_eq!(f.count("SELECT count(*) FROM final_heads").await, 1);
    assert!(head_at(&f, 14).await.is_none());
}

/// A backfilled head carries no supply readings, and the reserve check takes the latest final head
/// at or below the watermark (`REQ-BK-8`, `INV-1`). So it is recorded only below a head that has
/// its readings, at or below the watermark; until there is one, it waits, and the check never
/// takes a head without supply.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_8_a_backfilled_head_waits_for_a_head_with_supply_above_it(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    f.chain.advance(20);
    let under = f.chain.block(14);
    recorded_before_131(&f, f.chain.block(13), (under.number, under.hash)).await;
    // No head with supply readings: it waits.
    let done = f.follower().record_missing_heads().await.unwrap();
    assert_eq!(counts(&done), (0, 0, 1, 0), "{done:?}");
    assert!(done.findings[0].contains("supply"), "{done:?}");
    // One above it, but past the watermark (nothing read yet): it still waits.
    supplies(&f).record().await.unwrap();
    let done = f.follower().record_missing_heads().await.unwrap();
    assert_eq!(counts(&done), (0, 0, 1, 0), "{done:?}");
    assert!(head_at(&f, 14).await.is_none());
    // Read to it: now the check takes that head, not this one, and this one is recorded.
    assert!(matches!(follow(&f.follower()).await, Step::CaughtUp { .. }));
    let done = f.follower().record_missing_heads().await.unwrap();
    assert_eq!(counts(&done), (1, 0, 0, 0), "{done:?}");
    assert!(head_at(&f, 14).await.is_some());
}

/// The backfill runs in the endpoint's loop: a final point further below where its walk would
/// start than the bound is not walked to at all, and is named, not retried as a walk every pass
/// (`REQ-OB-3`'s walk, bounded).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ob_2_a_final_point_beyond_the_walk_bound_is_not_walked_to(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    f.chain.advance(20);
    let under = f.chain.block(14);
    recorded_before_131(&f, f.chain.block(13), (under.number, under.hash)).await;
    // Read to the head in one rung, so no anchor is near the point: its walk would start at the
    // finalized head, 9 blocks above it.
    let wide = Follower {
        rung: 1_000,
        ..f.follower()
    };
    assert!(matches!(follow(&wide).await, Step::CaughtUp { .. }));
    supplies(&f).record().await.unwrap();
    let near = Follower {
        max_walk: 5,
        ..wide
    };
    let before = f.chain.header_reads().len();
    for _ in 0..2 {
        let done = near.record_missing_heads().await.unwrap();
        assert_eq!(counts(&done), (0, 0, 0, 1), "{done:?}");
        assert!(done.findings[0].contains("not walked to"), "{done:?}");
    }
    assert_eq!(f.chain.header_reads().len(), before);
    assert!(head_at(&f, 14).await.is_none());
}

/// Issue #131's withdrawal, reproduced: a redemption recorded final under a head that was never
/// recorded waits in `Accepted`, nothing paid out (`INV-7`, `REQ-OB-2`). The endpoint records the
/// head once the finalized chain shows the hash the outcome names, and the next cycle of the core
/// takes the withdrawal to `Source-Final`, resting on it, with no hand edit.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_7_req_ob_2_issue_131_a_withdrawal_waiting_on_an_unrecorded_head_reaches_source_final(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect.clone()).await;
    exec(
        &f.admin,
        &format!(
            "INSERT INTO networks (network, role, address_format, backend) VALUES \
               ('{ORIGIN}', 'origin', 'evm', 'evm:80002'); \
             INSERT INTO assets (asset, precision, assurance_class, issuing_asset) VALUES \
               ('{ASSET}', 6, 'public', 'Here(101)'); \
             INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
               ('{ORIGIN}', '{TOKEN_ID}', '{ASSET}', 6); \
             INSERT INTO routes (asset, network, direction, minimum, maximum, window_limit, \
                 window_seconds, capacity, cutoffs, completion_budget_seconds) VALUES \
               ('{ASSET}', '{ORIGIN}', 'withdrawal', 1, 1000000000, 10000000000, 86400, 100, '[]', 86400);"
        ),
    )
    .await;
    let core = Core::new(
        role_pool(&f.admin, bezent_store::CORE_ROLE).await,
        Settings {
            default_deadline: Duration::days(1),
            ..Settings::default()
        },
    );
    let now: DateTime<Utc> = "2026-11-13T15:00:00Z".parse().unwrap();
    let cycle = || async {
        let report = core.cycle(now).await.expect("cycle");
        assert_eq!(report.failed, 0, "a step failed: {report:?}");
    };
    let state = || async {
        sqlx::query_scalar::<_, String>(
            "SELECT state::text FROM transfers WHERE kind = 'withdrawal'",
        )
        .fetch_one(&f.admin)
        .await
        .unwrap()
    };

    // Block 13: the redemption, final under block 14, whose head was never recorded. Another
    // head, later, is (the supply reader's), as on the network the issue was seen on.
    f.chain.advance(20);
    let under = f.chain.block(14);
    recorded_before_131(&f, f.chain.block(13), (under.number, under.hash)).await;
    pass(&f).await;
    cycle().await;
    cycle().await;
    assert_eq!(state().await, "Accepted");
    assert_eq!(f.count("SELECT count(*) FROM intents").await, 0);

    // The endpoint's cycle records the head; the core's next takes the withdrawal to
    // Source-Final, at that head.
    f.follower().record_missing_heads().await.unwrap();
    cycle().await;
    assert_eq!(state().await, "Source-Final");
    let (n, h): (i64, Vec<u8>) = sqlx::query_as(
        "SELECT source_final_block_number, source_final_block_hash FROM transfers \
         WHERE kind = 'withdrawal'",
    )
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!((n as u64, h), (under.number, under.hash.to_vec()));
}
