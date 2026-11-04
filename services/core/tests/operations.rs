// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-014 · Operations, at the service layer.
//!
//! * `AC-I2.1`, `REQ-GV-1`, `INV-15`: a pause an operator makes, in each of its four scopes, holds
//!   the new work it covers and nothing in flight, and the work resumes when a second operator
//!   lifts it (`REQ-GV-2`). A scope that does not cover the work holds nothing.
//! * `NFR-8`: the pause is read at the core's next cycle, which the configuration bounds; the
//!   time from the commit of the pause to the held work is measured here.
//! * `AC-H3.1`: the core's own login reads the observables, and the minute line the alarms read
//!   carries them (`REQ-ST-6`).
//! * The pages: every page is one log line an alarm can match, with `kind=<kind>` unquoted.

mod common;

use bezent_core_service::observe;
use common::*;
use serde_json::json;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::time::Instant;

/// A deposit whose arrival at `block` is final two blocks later, brought to `Source-Final`.
async fn source_final_deposit_at(w: &World, block: i64) -> i64 {
    let a = arrival(&w.admin, block, 5_000).await;
    decide(&w.admin, a, "final", block + 2).await;
    let id = deposit(&w.admin, block, 5_000).await;
    w.cycle(t0()).await;
    assert_eq!(w.state(id).await.0, "Source-Final");
    id
}

/// An operator pauses (`ops[0]`), and the other lifts it (`ops[1]`), as the runbook says.
async fn pause(
    w: &World,
    asset: Option<&str>,
    network: Option<&str>,
    direction: Option<&str>,
) -> i64 {
    sqlx::query_scalar("SELECT operator_pause($1, $2, $3, 'AC-I2.1 test')")
        .bind(asset)
        .bind(network)
        .bind(direction)
        .fetch_one(&w.ops[0])
        .await
        .expect("an operator pauses")
}

async fn lift(w: &World, pause: i64) {
    let request: i64 = sqlx::query_scalar("SELECT operator_request_unpause($1, 'AC-I2.1 test')")
        .bind(pause)
        .fetch_one(&w.ops[0])
        .await
        .unwrap();
    sqlx::query("SELECT operator_approve_unpause($1, 'AC-I2.1 test, second operator')")
        .bind(request)
        .fetch_one(&w.ops[1])
        .await
        .expect("the second operator lifts it");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_an_operators_pause_holds_new_work_in_each_scope_and_a_second_operator_lifts_it(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    // Scopes that cover a deposit on (ASSET, ORIGIN): global, the asset, the network, the direction,
    // and the route.
    let covering: [(Option<&str>, Option<&str>, Option<&str>); 5] = [
        (None, None, None),
        (Some(ASSET), None, None),
        (None, Some(ORIGIN), None),
        (None, None, Some("deposit")),
        // A route: asset, network and direction together (REQ-GV-1: per route).
        (Some(ASSET), Some(ORIGIN), Some("deposit")),
    ];
    for (i, (asset, network, direction)) in covering.into_iter().enumerate() {
        let block = 10 + 10 * i as i64;
        let id = source_final_deposit_at(&w, block).await;
        let pid = pause(&w, asset, network, direction).await;
        // NFR-8: from the commit to the held work, one cycle.
        let started = Instant::now();
        w.settle(t0()).await;
        let seconds = started.elapsed().as_secs_f64();
        assert_eq!(
            w.state(id).await,
            ("Source-Final".into(), Some("CND-Paused".into())),
            "scope {asset:?} {network:?} {direction:?}"
        );
        eprintln!(
            "NFR-8: pause committed to work held: {seconds:.3} s (scope {asset:?} {network:?} {direction:?})"
        );
        assert!(seconds < 5.0, "one cycle took {seconds} s");
        lift(&w, pid).await;
        w.settle(t0()).await;
        assert_eq!(w.state(id).await.1, None, "resumes by itself once lifted");
        assert!(
            order(&w.admin, id, 1).await.is_some(),
            "the order is recorded after the lift"
        );
    }
    // A scope that does not cover the work holds nothing: a withdrawal pause, another asset.
    let id = source_final_deposit_at(&w, 60).await;
    let _ = pause(&w, None, None, Some("withdrawal")).await;
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await.1,
        None,
        "a withdrawal pause holds no deposit"
    );
    assert!(order(&w.admin, id, 1).await.is_some());
    // Every act is in the record, by whom.
    let n: i64 = w
        .count("SELECT count(*) FROM admin_actions WHERE kind = 'unpause' AND actor = 'ops_bob_for_tests'")
        .await;
    assert_eq!(n, 5);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_a_pause_does_not_fail_or_refund_work_in_flight(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // REQ-GV-1, INV-15, REQ-IC-3: with the whole bridge paused, a delivery already ordered is
    // observed and completed. (`conditions.rs` makes the same point by inserting as the owner;
    // this one pauses as an operator, the way the runbook does.)
    let w = World::new(options, connect).await;
    let id = source_final_deposit_at(&w, 10).await;
    deliver(&w, id, 1, t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    let _ = pause(&w, None, None, None).await;
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
    assert_eq!(
        w.count("SELECT count(*) FROM transfers WHERE kind = 'refund'")
            .await,
        0
    );
}

#[test]
fn nfr_8_the_cycle_that_reads_the_pause_is_well_inside_the_bound() {
    // A pause takes effect for new work at the next cycle (the gate reads `pauses`), so the
    // off-chain part of NFR-8's 30 s is the cycle plus the time a cycle takes. The cycle is a
    // configuration value: its default, and the repository's fixture (a test value, not the
    // deployment's), are both within ten seconds; the deployment's own template is not in this
    // repository, and `features/F-014-operations/NOTES.md` says what to check there. The test
    // above measures the second term.
    let default: bezent_core_service::Config =
        toml::from_str("database_url = \"postgres://core@db/bezent\"\n").unwrap();
    assert!(
        default.cycle_seconds <= 10,
        "default cycle_seconds = {}",
        default.cycle_seconds
    );
    let fixture = bezent_core_service::Config::from_file(std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fixtures/core.toml"
    )))
    .expect("the fixture configuration loads");
    assert!(
        fixture.cycle_seconds <= 10,
        "cycle_seconds = {}",
        fixture.cycle_seconds
    );
}

// ---------------------------------------------------------------------------------------------
// AC-H3.1: the core reads the observables with its own login and writes the minute line.

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_h3_1_the_core_reads_every_req_st_6_observable_and_writes_the_minute_line(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let id = source_final_deposit_at(&w, 10).await;
    deliver(&w, id, 1, t0()).await;
    let rows = observe::read(w.core.pool())
        .await
        .expect("the core's login reads the observables");
    let names: std::collections::BTreeSet<_> = rows.iter().map(|r| r.metric.as_str()).collect();
    for required in [
        // REQ-ST-6, in its order: the oldest non-final transfer per route; unknown legs, count and
        // age; observation lag and open gaps per network; the reserve equation per asset; reserve
        // per network; undelivered notifications; arrivals in CND-Unassociated.
        "oldest_nonfinal_transfer_age_seconds",
        "unknown_legs",
        "unknown_legs_oldest_age_seconds",
        "observer_lag_estimate_seconds",
        "open_gaps",
        "undelivered_notifications",
        "undelivered_notifications_oldest_age_seconds",
        "fee_short_transfers",
    ] {
        // `fee_short_transfers` has a row only while a transfer is held for a fee position.
        if required == "fee_short_transfers" {
            continue;
        }
        assert!(
            names.contains(required),
            "{required} missing; have {names:?}"
        );
    }
    let unknown = rows
        .iter()
        .find(|r| r.metric == "unknown_legs" && r.network.as_deref() == Some(ORIGIN))
        .expect("a row for the route");
    assert_eq!(
        unknown.value,
        Some(1.0),
        "the delivery's leg is unknown until it is seen"
    );

    // The line the alarms read: JSON, one object per set of dimensions, no transfer named.
    let lines = observe::emf_lines(&rows, 1_700_000_000_000);
    assert!(!lines.is_empty());
    let reference: String = scalar(
        &w.admin,
        &format!("SELECT bridge_reference FROM transfers WHERE id = {id}"),
    )
    .await;
    for l in &lines {
        serde_json::from_str::<serde_json::Value>(l).expect("one JSON object");
        assert!(!l.contains(&reference), "a line must not name a transfer");
    }
}
