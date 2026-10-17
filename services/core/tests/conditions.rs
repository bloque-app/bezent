// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-003 T-003-05: the named conditions of SPEC §10, one test each. A condition is held in the
//! transfer's state and is never a failure; it clears by itself or by evidence; past its deadline
//! it sends a transfer to review only where the §5.4 table allows (`Source-Final` with
//! `CND-AwaitingConversion` or `CND-DestinationRefused`; `Delivering` and `Delivered`).
//! `CND-Unassociated` is `ac_a4_1_…` and `CND-AmountMismatch` is `req_tr_5_…` in `lifecycle.rs`.

mod common;

use chrono::Duration;
use common::*;
use serde_json::json;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// A deposit whose arrival at block 10 is final at block 12, brought to `Source-Final`.
async fn source_final_deposit(w: &World) -> i64 {
    let a = arrival(&w.admin, 10, 5_000).await;
    decide(&w.admin, a, "final", 12).await;
    let id = deposit(&w.admin, 10, 5_000).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Source-Final");
    id
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_paused_new_work_waits_and_resumes_when_the_route_reopens(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    exec(
        &w.admin,
        &format!("INSERT INTO pauses (asset) VALUES ('{ASSET}')"),
    )
    .await;
    let id = source_final_deposit(&w).await;
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-Paused".into()))
    );
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
    exec(&w.admin, "DELETE FROM pauses").await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await, ("Source-Final".into(), None));
    assert!(order(&w.admin, id, 1).await.is_some());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_paused_work_in_flight_continues(options: PgPoolOptions, connect: PgConnectOptions) {
    // REQ-GV-1, AC-I2.1: a pause stops new work only.
    let w = World::new(options, connect).await;
    let id = source_final_deposit(&w).await;
    deliver(&w, id, 1, t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    exec(&w.admin, "INSERT INTO pauses (asset) VALUES (NULL)").await;
    let reference = reference_of(&w.admin, id).await;
    let minted = observe(
        &w.admin,
        ISSUING,
        40,
        0,
        ISSUING_ASSET,
        5_000,
        None,
        Some(DESTINATION_ACCOUNT),
        Some(&reference),
        true,
        json!({"event": "Minted"}),
    )
    .await;
    decide(&w.admin, minted, "final", 45).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await, ("Completed".into(), None));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_cutoff_a_step_past_the_route_cut_off_holds_until_it_reopens(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // Amendment 0010 A2: no request is refused for a cut-off; the transfer holds and resumes.
    let w = World::new(options, connect).await;
    exec(
        &w.admin,
        &format!(
            "UPDATE routes SET cutoffs = '[{{\"weekdays\": [2], \"from\": \"00:00\", \"to\": \"06:00\"}}]' \
             WHERE asset = '{ASSET}' AND direction = 'deposit'"
        ),
    )
    .await;
    let id = source_final_deposit(&w).await;
    // Tuesday 01:00: closed.
    w.settle(t0() + Duration::hours(1)).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-CutOff".into()))
    );
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
    // Tuesday 06:00: reopened, resumes with no one acting.
    w.settle(t0() + Duration::hours(6)).await;
    assert_eq!(w.state(id).await, ("Source-Final".into(), None));
    assert!(order(&w.admin, id, 1).await.is_some());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_reserve_short_ac_b3_1_holds_and_raises_one_condition_per_asset_and_network(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    for (block, reference) in [(20, [1u8; 32]), (30, [2u8; 32])] {
        let r = redemption(&w.admin, block, &reference, 700).await;
        decide(&w.admin, r, "final", block + 2).await;
    }
    w.settle(t0()).await;
    let held = w
        .count("SELECT count(*) FROM transfers WHERE kind = 'withdrawal' AND condition = 'CND-ReserveShort'")
        .await;
    assert_eq!(held, 2);
    // Never a failure, a refund or another network (REQ-OT-5).
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
    assert_eq!(
        w.count("SELECT count(*) FROM transfers WHERE kind = 'refund'")
            .await,
        0
    );
    let raised = w
        .count("SELECT count(*) FROM event_log WHERE kind = 'condition-raised' AND payload ->> 'condition' = 'CND-ReserveShort'")
        .await;
    assert_eq!(raised, 1);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_reserve_short_ac_b3_2_resumes_with_no_one_acting_when_reserve_recovers(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let r = redemption(&w.admin, 20, &[1; 32], 700).await;
    decide(&w.admin, r, "final", 22).await;
    w.settle(t0()).await;
    let id: i64 = scalar(
        &w.admin,
        "SELECT id FROM transfers WHERE kind = 'withdrawal'",
    )
    .await;
    assert_eq!(w.state(id).await.1.as_deref(), Some("CND-ReserveShort"));
    reserve(&w.admin, 700).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await, ("Source-Final".into(), None));
    assert!(order(&w.admin, id, 1).await.is_some());
    let cleared = w
        .count("SELECT count(*) FROM event_log WHERE kind = 'condition-cleared' AND payload ->> 'condition' = 'CND-ReserveShort'")
        .await;
    assert_eq!(cleared, 1);
}

/// A failed dry run holds `condition`; the core orders again; a passing one delivers and clears.
async fn dry_run_condition(w: &World, reason: &str, condition: &str) {
    let id = source_final_deposit(w).await;
    w.cycle(t0()).await;
    let (first, _) = order(&w.admin, id, 1).await.unwrap();
    prepare(&w.admin, first, Some(reason)).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some(condition.into()))
    );
    assert_eq!(w.count("SELECT count(*) FROM signatures").await, 0);
    // A new order (F-007: a failed dry run is not prepared again), prepared and passing.
    let (second, _) = order(&w.admin, id, 1).await.unwrap();
    assert_ne!(first, second);
    prepare(&w.admin, second, None).await;
    w.cycle(t0()).await;
    let (s, _) = signing_intent(&w.admin, id, 1).await.unwrap();
    sign(w, s).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await, ("Delivering".into(), None));
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_mint_window_holds_until_the_window_rolls_over(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    dry_run_condition(&w, "AboveWindow", "CND-MintWindow").await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_fee_short_holds_until_the_fee_position_can_pay(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    dry_run_condition(&w, "Payment: Inability to pay some fees", "CND-FeeShort").await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_destination_refused_mints_nothing_and_resumes_when_the_mint_applies(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // REQ-IN-8, REQ-IN-9: bezent never checks the destination; the network's refusal holds it.
    let w = World::new(options, connect).await;
    dry_run_condition(&w, "Kreivo(CannotCreate)", "CND-DestinationRefused").await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_destination_refused_past_its_deadline_goes_to_review(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let id = source_final_deposit(&w).await;
    w.cycle(t0()).await;
    let (o, _) = order(&w.admin, id, 1).await.unwrap();
    prepare(&w.admin, o, Some("Kreivo(CannotCreate)")).await;
    w.cycle(t0()).await;
    head(&w.admin, ISSUING, 100).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Source-Final");
    // Past the route's deadline from the source leg's finality (block 12), at a final block.
    head(&w.admin, ISSUING, 12 + BUDGET + 1).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Needs-Review".into(), Some("CND-DestinationRefused".into()))
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_awaiting_conversion_mints_nothing_and_goes_to_review_past_its_deadline(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // REQ-CV-1, REQ-CV-6. The conversion itself is the requester's (F-010); the hold is here.
    let w = World::new(options, connect).await;
    let id = source_final_deposit(&w).await;
    exec(
        &w.admin,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state, condition) \
             VALUES ({id}, 'Source-Final', 'Source-Final', 'CND-AwaitingConversion')"
        ),
    )
    .await;
    head(&w.admin, ISSUING, 100).await;
    w.settle(t0()).await;
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
    head(&w.admin, ISSUING, 12 + BUDGET + 1).await;
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Needs-Review".into(), Some("CND-AwaitingConversion".into()))
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn cnd_withdrawn_an_inclusion_that_stops_matching_the_chain_goes_to_review(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // REQ-IN-7 for a destination leg: the inclusion is unknown until evidence (F-001 NOTES S5).
    let w = World::new(options, connect).await;
    let id = source_final_deposit(&w).await;
    deliver(&w, id, 1, t0()).await;
    let reference = reference_of(&w.admin, id).await;
    let minted = observe(
        &w.admin,
        ISSUING,
        40,
        0,
        ISSUING_ASSET,
        5_000,
        None,
        Some(DESTINATION_ACCOUNT),
        Some(&reference),
        true,
        json!({"event": "Minted"}),
    )
    .await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Delivered");
    decide(&w.admin, minted, "withdrawn", 45).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Needs-Review".into(), Some("CND-Withdrawn".into()))
    );
}
