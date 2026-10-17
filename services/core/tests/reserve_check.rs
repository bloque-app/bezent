// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! F-004 T-004-02 to T-004-04: the reserve check, every minute per asset at zero tolerance, on a
//! clock the test moves (`INV-1`, `NFR-5`, `REQ-BK-1`, `REQ-BK-2`, `REQ-BK-3`, `REQ-BK-8`,
//! `REQ-BK-10`, `REQ-BE-6`, `REQ-IC-2`, `AC-E2.1`). The networks are fakes of the reading
//! interface; the observations table is the chain they read.

mod common;

use bezent_core_service::refs::{self, Coordinate};
use bezent_core_service::reserve::book::{self, Cost, Report};
use bezent_core_service::reserve::check::{Clock, Outcome};
use bezent_core_service::reserve::coordinate_key;
use bezent_core_service::reserve::readings::AttestedPosition;
use chrono::{Duration, Utc};
use common::reserve::*;
use common::*;
use serde_json::json;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

fn at(network: &str, block: i64, index: i64) -> Coordinate {
    Coordinate {
        network: network.into(),
        block_number: block,
        block_hash: hash(network, block),
        position_kind: "log".into(),
        position_index: index,
    }
}

/// A second public asset on the same networks, so "per asset" means two.
async fn second_asset(w: &World) {
    exec(
        &w.admin,
        &format!(
            "INSERT INTO assets (asset, precision, assurance_class, issuing_asset) \
               VALUES ('bEURC', 6, 'public', 'Here(102)'); \
             INSERT INTO admitted_tokens (network, token, asset, precision) \
               VALUES ('{ORIGIN}', 'eurc', 'bEURC', 6); \
             INSERT INTO reserves (asset, network, address) VALUES ('bEURC', '{ORIGIN}', '0xee');"
        ),
    )
    .await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_1_nfr_5_the_equation_is_evaluated_once_a_minute_per_asset(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    second_asset(&w).await;
    fund(&w, 5, 1_000).await;
    // Final heads through the ten minutes the test runs for (review MF2: a head older than the
    // bound by the clock is a stall).
    read_all(&w.admin, 700).await;
    let (check, readings, _) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    let clock = ManualClock::at(t0());
    // Ten minutes, a tick every fifteen seconds: each asset is evaluated once a minute.
    let mut done = Vec::new();
    for _ in 0..40 {
        done.extend(check.tick(&clock).await.unwrap());
        clock.advance(Duration::seconds(15));
    }
    for asset in [ASSET, "bEURC"] {
        let times: Vec<_> = done
            .iter()
            .filter(|e| e.asset == asset)
            .map(|e| e.evaluated_at)
            .collect();
        assert_eq!(times.len(), 10, "{asset}: {times:?}");
        for pair in times.windows(2) {
            assert_eq!(pair[1] - pair[0], Duration::minutes(1));
        }
    }
    // At zero tolerance, every one held (INV-1).
    assert!(done.iter().all(|e| e.outcome == Outcome::Holds), "{done:?}");
    assert_eq!(
        w.count("SELECT count(*) FROM reserve_evaluations WHERE outcome = 'holds'")
            .await,
        20
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_1_req_bk_2_req_bk_8_a_book_that_matches_the_networks_holds_with_every_term_itemised(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000_000).await;
    minted_deposit(&w, 10, 5_000).await;
    // An arrival no deposit binds yet, and a redemption not yet paid out.
    final_arrival(&w.admin, 60, 0, 300).await;
    let id = redeemed(&w, 70, [5; 32], 700).await;
    deliver(&w, id, 1, t0()).await;
    read_all(&w.admin, 200).await;
    let (check, readings, pager) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    let e = check.evaluate(ASSET, t0()).await.unwrap();
    assert_eq!(e.outcome, Outcome::Holds, "{:?}", e.differences);
    // REQ-BK-2: each term a list, never a residual.
    let unbound = coordinate_key(&at(ORIGIN, 60, 0));
    assert_eq!(
        e.chain.owed_mint.iter().collect::<Vec<_>>(),
        vec![(&unbound, &300)]
    );
    let withdrawal = refs::withdrawal(REDEEMER, &[5; 32]);
    assert_eq!(
        e.chain.owed_payout.iter().collect::<Vec<_>>(),
        vec![(&withdrawal, &700)]
    );
    assert!(e.chain.in_transit.is_empty());
    assert_eq!(e.chain.supply, 5_000 - 700);
    assert_eq!(e.chain.reserve[ORIGIN], 1_000_000 + 5_000 + 300);
    assert_eq!(e.book.surplus, 1_000_000);
    assert_eq!(e.chain, e.book);
    // T-004-02: every reading was made at the final point the evaluation names, the same point
    // the book was read as of.
    let asked = readings.chain().asked.clone();
    assert!(!asked.is_empty());
    for p in &asked {
        assert_eq!(&e.points[&p.network], p);
    }
    assert_eq!(e.points[ORIGIN].block_number, 200);
    assert!(pager.pages().is_empty());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_8_the_networks_are_read_below_every_movement_still_awaiting_its_outcome(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000).await;
    head(&w.admin, ORIGIN, 30).await;
    // Observed at 40, its outcome not yet recorded: the evaluation rests on a point before it,
    // within the network's lag bound of the final head (a minute by default).
    arrival(&w.admin, 40, 2_000).await;
    read_all(&w.admin, 85).await;
    let (check, readings, _) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    let e = check.evaluate(ASSET, t0()).await.unwrap();
    assert_eq!(e.outcome, Outcome::Holds, "{:?}", e.differences);
    assert_eq!(e.points[ORIGIN].block_number, 30);
    assert_eq!(e.lags[ORIGIN].seconds, 55);
    assert_eq!(e.points[ISSUING].block_number, 85);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_e2_1_a_divergence_pauses_the_assets_routes_keeps_observing_and_pages_a_human(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000_000).await;
    minted_deposit(&w, 10, 5_000).await;
    // A withdrawal in flight.
    let id = redeemed(&w, 40, [6; 32], 700).await;
    deliver(&w, id, 1, t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    read_all(&w.admin, 100).await;
    let (check, readings, pager) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    // One minor unit missing from the reserve address (REQ-BK-1: to the smallest unit).
    *readings
        .chain()
        .reserve
        .get_mut(&(TOKEN.to_owned(), RESERVE.to_owned()))
        .unwrap() -= 1;
    let clock = ManualClock::at(t0());
    let e = check.tick(&clock).await.unwrap().remove(0);
    assert_eq!(e.outcome, Outcome::Diverges);
    assert!(
        e.differences
            .iter()
            .any(|d| d.term == "reserve" && d.chain + 1 == d.book),
        "{:?}",
        e.differences
    );
    // The asset's routes pause, and a human is paged once (REQ-IC-2).
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM pauses WHERE asset = '{ASSET}' AND network IS NULL \
               AND direction IS NULL AND cause = 'REQ-BK-1' AND evaluation_id = {}",
            e.id
        ))
        .await,
        1
    );
    assert_eq!(pager.kinds(), ["reserve-divergence"]);
    assert_eq!(
        w.count("SELECT count(*) FROM event_log WHERE kind = 'reserve-divergence' AND transfer_id IS NULL")
            .await,
        1
    );
    // New work stops: a new deposit holds CND-Paused, nothing is ordered for it.
    final_arrival(&w.admin, 45, 0, 5_000).await;
    let new = deposit(&w.admin, 45, 5_000).await;
    w.settle(t0()).await;
    assert_eq!(
        w.state(new).await,
        ("Source-Final".into(), Some("CND-Paused".into()))
    );
    assert!(order(&w.admin, new, 1).await.is_none());
    // Work in flight continues to be observed: the payout leg completes the withdrawal (INV-15).
    paid_out(&w, id, 60, 700).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Completed");
    // Still apart a minute later: evaluated, recorded, not paged again while the pause stands.
    clock.advance(Duration::minutes(1));
    let again = check.tick(&clock).await.unwrap().remove(0);
    assert_eq!(again.outcome, Outcome::Diverges);
    assert_eq!(pager.kinds(), ["reserve-divergence"]);
    assert_eq!(
        w.count("SELECT count(*) FROM pauses WHERE cause = 'REQ-BK-1'")
            .await,
        1
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_1_a_mint_no_deposit_explains_diverges_on_supply(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000).await;
    // Supply grew on the issuing network by a mint bezent never ordered.
    let rogue = observe(
        &w.admin,
        ISSUING,
        40,
        0,
        ISSUING_ASSET,
        50,
        None,
        Some(DESTINATION_ACCOUNT),
        Some(&[0xee; 32]),
        true,
        json!({"event": "Minted"}),
    )
    .await;
    decide(&w.admin, rogue, "final", 42).await;
    read_all(&w.admin, 100).await;
    let (check, readings, pager) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    let e = check.evaluate(ASSET, t0()).await.unwrap();
    assert_eq!(e.outcome, Outcome::Diverges);
    assert!(
        e.differences
            .iter()
            .any(|d| d.term == "supply" && d.chain == 50 && d.book == 0),
        "{:?}",
        e.differences
    );
    assert_eq!(pager.kinds(), ["reserve-divergence"]);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_3_a_negative_surplus_is_a_divergence(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 10).await;
    minted_deposit(&w, 10, 5_000).await;
    // A network cost of 25 paid from the reserve, more than the surplus holds.
    let mut conn = w.core.pool().acquire().await.unwrap();
    book::cost(&mut conn, ASSET, Cost::Network, ORIGIN, 25, "gas:1", None)
        .await
        .unwrap();
    read_all(&w.admin, 100).await;
    let (check, readings, _) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    *readings
        .chain()
        .reserve
        .get_mut(&(TOKEN.to_owned(), RESERVE.to_owned()))
        .unwrap() -= 25;
    let e = check.evaluate(ASSET, t0()).await.unwrap();
    assert_eq!(e.outcome, Outcome::Diverges);
    assert_eq!(e.book.surplus, -15);
    // Every term matches the network; only the shortfall of backing is the divergence.
    assert_eq!(e.differences.len(), 1, "{:?}", e.differences);
    assert_eq!(e.differences[0].term, "surplus");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn nfr_5_an_evaluation_that_cannot_read_is_incomplete_never_holds_and_pages_once(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000).await;
    let (check, readings, pager) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    let clock = ManualClock::at(t0());
    // No range read without a hole on either network: no final point to rest on.
    let e = check.tick(&clock).await.unwrap().remove(0);
    assert_eq!(e.outcome, Outcome::Incomplete);
    assert!(e.reason.as_deref().unwrap().contains("REQ-OB-3"), "{e:?}");
    clock.advance(Duration::minutes(1));
    read_all(&w.admin, 100).await;
    readings.chain().unreachable = true;
    let e = check.tick(&clock).await.unwrap().remove(0);
    assert_eq!(e.outcome, Outcome::Incomplete);
    assert!(
        e.reason.as_deref().unwrap().contains("unreachable"),
        "{e:?}"
    );
    assert_eq!(pager.kinds(), ["reserve-check-incomplete"]);
    // Never a pause: nothing diverged.
    assert_eq!(w.count("SELECT count(*) FROM pauses").await, 0);
    clock.advance(Duration::minutes(1));
    readings.chain().unreachable = false;
    let e = check.tick(&clock).await.unwrap().remove(0);
    assert_eq!(e.outcome, Outcome::Holds);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_10_surplus_below_its_alert_level_alerts_before_the_floor_is_reached(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    exec(
        &w.admin,
        &format!(
            "UPDATE assets SET surplus_floor = 100, surplus_alert = 200 WHERE asset = '{ASSET}'"
        ),
    )
    .await;
    fund(&w, 5, 150).await;
    read_all(&w.admin, 100).await;
    let (check, readings, pager) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    let clock = ManualClock::at(t0());
    let e = check.tick(&clock).await.unwrap().remove(0);
    assert_eq!(e.outcome, Outcome::Holds);
    assert_eq!(e.alerts, ["surplus-below-alert-level"]);
    assert_eq!(pager.kinds(), ["reserve-alert"]);
    // Paged once while it lasts.
    clock.advance(Duration::minutes(1));
    check.tick(&clock).await.unwrap();
    assert_eq!(pager.kinds(), ["reserve-alert"]);
    // The operator tops up from its own funds (REQ-BK-10); the alert clears.
    fund(&w, 50, 100).await;
    read_all(&w.admin, 200).await;
    sync_chain(&w.admin, &readings).await;
    clock.advance(Duration::minutes(1));
    let e = check.tick(&clock).await.unwrap().remove(0);
    assert_eq!(e.outcome, Outcome::Holds, "{:?}", e.differences);
    assert!(e.alerts.is_empty(), "{:?}", e.alerts);
}

/// An asset on an attested backend (`REQ-BE-2`): its reserve is a party's report.
async fn attested_asset(w: &World, floor: u128, surplus: u128) {
    let alert = floor + 1;
    exec(
        &w.admin,
        &format!(
            "INSERT INTO networks (network, role, address_format, backend) \
               VALUES ('attested-test', 'origin', 'opaque', 'attested:1'); \
             INSERT INTO assets (asset, precision, assurance_class, issuing_asset, surplus_floor, \
                 surplus_alert) VALUES ('COPM', 6, 'attested', 'Here(104)', {floor}, {alert}); \
             INSERT INTO admitted_tokens (network, token, asset, precision) \
               VALUES ('attested-test', 'cop', 'COPM', 6); \
             INSERT INTO reserves (asset, network, address) \
               VALUES ('COPM', 'attested-test', 'acct-1'); \
             WITH e AS (INSERT INTO book_entries (reference) VALUES ('seed') RETURNING id) \
             INSERT INTO book_postings (entry_id, asset, account, network, side, amount, item) \
             SELECT id, 'COPM', 'reserve', 'attested-test', 'debit', {surplus}, 'acct-1' FROM e \
             UNION ALL SELECT id, 'COPM', 'surplus', NULL, 'credit', {surplus}, NULL FROM e;"
        ),
    )
    .await;
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_be_6_a_reversal_below_the_floor_pauses_new_mints_until_the_reserve_is_restored(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    attested_asset(&w, 500, 1_000).await;
    let mut conn = w.core.pool().acquire().await.unwrap();
    let reported = Report {
        party: "swaps",
        report: "reversal-1",
    };
    book::reversal(&mut conn, "COPM", "attested-test", "acct-1", 600, &reported)
        .await
        .unwrap();
    // The shortfall is charged to surplus; nothing minted is clawed back.
    assert_eq!(balance(&w.admin, "COPM", "surplus", None).await, -400);
    assert_eq!(balance(&w.admin, "COPM", "reserve", None).await, 400);
    assert!(book::mints_paused(&mut conn, "COPM").await.unwrap());
    // A deposit of the asset, final at its source, is not minted while paused.
    let a = observe(
        &w.admin,
        "attested-test",
        10,
        0,
        "cop",
        5_000,
        Some("payer"),
        Some("acct-1"),
        None,
        true,
        json!({}),
    )
    .await;
    decide(&w.admin, a, "final", 12).await;
    let id: i64 = scalar(
        &w.admin,
        &format!(
            "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                 destination_network, destination, refund_destination_network, refund_destination, \
                 source_network, source_block_number, source_block_hash, source_position_kind, \
                 source_position_index, state) \
             VALUES ('bz-d-cop', 'req-cop', 'deposit', 'COPM', 5000, '{ISSUING}', '{DESTINATION}', \
                 'attested-test', 'payer', 'attested-test', 10, '\\x{}', 'log', 0, 'Received') \
             RETURNING id",
            hex::encode(hash("attested-test", 10))
        ),
    )
    .await;
    exec(
        &w.admin,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state) VALUES ({id}, 'Received', 'Accepted')"
        ),
    )
    .await;
    let now = t0() + Duration::minutes(5);
    w.settle(now).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-Paused".into()))
    );
    assert!(order(&w.admin, id, 1).await.is_none());
    // Only the party that reported the reversal restores it (REQ-BE-6; review S8).
    let other = book::restoration(
        &mut conn,
        "COPM",
        &reported,
        &Report {
            party: "someone-else",
            report: "restored-0",
        },
    )
    .await;
    assert!(
        matches!(other, Err(book::BookError::Refused(_))),
        "{other:?}"
    );
    assert!(book::mints_paused(&mut conn, "COPM").await.unwrap());
    // A reversal that leaves surplus at or above the floor pauses nothing; this one did not.
    // The party restores the reserve in full: mints resume with no one acting.
    book::restoration(
        &mut conn,
        "COPM",
        &reported,
        &Report {
            party: "swaps",
            report: "restored-1",
        },
    )
    .await
    .unwrap();
    assert!(!book::mints_paused(&mut conn, "COPM").await.unwrap());
    assert_eq!(balance(&w.admin, "COPM", "surplus", None).await, -1_000);
    w.settle(now).await;
    assert_eq!(w.state(id).await.1, None);
    assert!(order(&w.admin, id, 1).await.is_some());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_be_6_a_reversal_not_restored_within_one_business_day_alerts(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    attested_asset(&w, 500, 1_000).await;
    let mut conn = w.core.pool().acquire().await.unwrap();
    book::reversal(
        &mut conn,
        "COPM",
        "attested-test",
        "acct-1",
        100,
        &Report {
            party: "swaps",
            report: "reversal-2",
        },
    )
    .await
    .unwrap();
    // The reversal is recorded at the store's time; a week later it is past its business day. The
    // issuing network's final head is that late too (review MF2: an old head is a stall).
    let later = Utc::now() + Duration::days(7);
    read_through(&w.admin, ISSUING, (later - t0()).num_seconds()).await;
    let (check, readings, pager) = checker(&w);
    readings.chain().attested.insert(
        ("COPM".into(), "attested-test".into()),
        vec![AttestedPosition {
            position: "acct-1".into(),
            amount: "900".into(),
            party: "swaps".into(),
            report: "balance-7".into(),
        }],
    );
    let e = check.evaluate("COPM", later).await.unwrap();
    assert_eq!(e.outcome, Outcome::Holds, "{:?}", e.differences);
    assert_eq!(e.book.surplus, 900);
    assert!(
        e.alerts
            .contains(&"reversal-not-restored:swaps:reversal-2".to_owned()),
        "{:?}",
        e.alerts
    );
    // Above the floor: no mint pause, no floor alert.
    assert!(
        !e.alerts.iter().any(|a| a.contains("floor")),
        "{:?}",
        e.alerts
    );
    assert_eq!(pager.kinds(), ["reserve-alert"]);
}

/// Review M1, probe `probe_zero_amount_arrival_wedges_check_and_cycle`: anyone can send zero of an
/// admitted token to the reserve address. The arrival is kept as observed, posts nothing and is
/// owed nothing, and neither the core's cycle nor the reserve check stops on it.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_1_req_ic_2_m1_a_zero_value_arrival_stops_neither_the_core_nor_the_check(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000).await;
    let zero = observe(
        &w.admin,
        ORIGIN,
        20,
        0,
        TOKEN,
        0,
        Some("0xanyone"),
        Some(RESERVE),
        None,
        true,
        json!({}),
    )
    .await;
    decide(&w.admin, zero, "final", 22).await;
    read_all(&w.admin, 100).await;
    let (check, readings, pager) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    let clock = ManualClock::at(t0());
    for _ in 0..3 {
        let e = check.tick(&clock).await.unwrap().remove(0);
        assert_eq!(e.outcome, Outcome::Holds, "{:?}", e.differences);
        assert!(e.chain.owed_mint.is_empty(), "{:?}", e.chain.owed_mint);
        w.cycle(clock.now()).await;
        clock.advance(Duration::minutes(1));
    }
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM book_entries WHERE observation_id = {zero}"
        ))
        .await,
        0
    );
    assert_eq!(
        w.count("SELECT count(*) FROM reserve_evaluations WHERE outcome = 'holds'")
            .await,
        3
    );
    assert_eq!(w.count("SELECT count(*) FROM book_quarantine").await, 0);
    assert_eq!(w.count("SELECT count(*) FROM pauses").await, 0);
    assert!(pager.pages().is_empty(), "{:?}", pager.pages());
}

/// Review M1: an observation the store refuses to book is quarantined once, pages a human and
/// pauses its asset (`REQ-IC-2`); everything else is still posted, the check still evaluates the
/// asset, and the core's cycle goes on.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_1_req_ic_2_m1_an_observation_the_book_cannot_post_is_quarantined_paged_and_paused(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000_000).await;
    // The store refuses any posting of 4242: an observation the book cannot post, whatever why.
    exec(
        &w.admin,
        "CREATE FUNCTION refuse_4242() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN IF NEW.amount = 4242 THEN \
           RAISE EXCEPTION 'refused for the test' USING ERRCODE = 'BZ001'; END IF; \
           RETURN NEW; END $$; \
         REVOKE EXECUTE ON FUNCTION refuse_4242() FROM PUBLIC; \
         CREATE TRIGGER refuse_4242 BEFORE INSERT ON book_postings \
           FOR EACH ROW EXECUTE FUNCTION refuse_4242();",
    )
    .await;
    let bad = final_arrival(&w.admin, 20, 0, 4_242).await;
    let good = final_arrival(&w.admin, 30, 1, 300).await;
    read_all(&w.admin, 100).await;
    let (check, readings, pager) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    let clock = ManualClock::at(t0());
    let e = check.tick(&clock).await.unwrap().remove(0);
    // The rest is posted; the refused one is set aside once, with its asset.
    for (id, n) in [(good, 1), (bad, 0)] {
        assert_eq!(
            w.count(&format!(
                "SELECT count(*) FROM book_entries WHERE observation_id = {id}"
            ))
            .await,
            n
        );
    }
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM book_quarantine WHERE observation_id = {bad} AND asset = '{ASSET}'"
        ))
        .await,
        1
    );
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM pauses WHERE asset = '{ASSET}' AND cause = 'REQ-IC-2' \
               AND quarantine_id IS NOT NULL"
        ))
        .await,
        1
    );
    assert_eq!(pager.kinds()[0], "book-quarantine");
    // The check still evaluates the asset: the network holds 4,242 the book does not.
    assert_eq!(e.outcome, Outcome::Diverges);
    assert!(
        e.differences
            .iter()
            .any(|d| d.term == "owed_mint" && d.chain == 4_242 && d.book == 0),
        "{:?}",
        e.differences
    );
    // The core's cycle goes on, and the quarantined observation is never tried again.
    w.cycle(t0()).await;
    clock.advance(Duration::minutes(1));
    check.tick(&clock).await.unwrap();
    assert_eq!(w.count("SELECT count(*) FROM book_quarantine").await, 1);
    assert_eq!(
        pager
            .kinds()
            .iter()
            .filter(|k| *k == "book-quarantine")
            .count(),
        1
    );
}

/// Review M2, probe `probe_stuck_observation_freezes_the_final_point`: one issuing-network
/// observation whose outcome is never recorded cannot freeze the point the check reads at. A point
/// that lags the final head beyond the network's bound is a divergence: the asset pauses and a
/// human is paged (`NFR-5`, `REQ-IC-2`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_1_nfr_5_req_ic_2_m2_an_observation_without_an_outcome_cannot_freeze_the_point(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000).await;
    head(&w.admin, ISSUING, 20).await;
    observe(
        &w.admin,
        ISSUING,
        30,
        0,
        ISSUING_ASSET,
        50,
        None,
        Some(DESTINATION_ACCOUNT),
        Some(&[0xab; 32]),
        true,
        json!({"event": "Minted"}),
    )
    .await;
    read_all(&w.admin, 1_000).await;
    let (check, readings, pager) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    let clock = ManualClock::at(t0());
    for _ in 0..3 {
        let e = check.tick(&clock).await.unwrap().remove(0);
        assert_eq!(e.outcome, Outcome::Diverges, "{e:?}");
        assert_eq!(e.points[ISSUING].block_number, 20);
        assert_eq!(e.lags[ISSUING].seconds, 980);
        assert!(
            e.differences
                .iter()
                .any(|d| d.term == "final_point" && d.item.as_deref().unwrap().contains(ISSUING)),
            "{:?}",
            e.differences
        );
        clock.advance(Duration::hours(6));
    }
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM pauses WHERE asset = '{ASSET}' AND cause = 'REQ-BK-1'"
        ))
        .await,
        1
    );
    assert_eq!(pager.kinds(), ["reserve-divergence"]);
    // The bound is the network's own: one minute plus its finality time, as declared (within the
    // ceiling of review SF2), measured at a clock the head is fresh at.
    exec(
        &w.admin,
        &format!(
            "UPDATE networks SET reserve_point_lag_seconds = 1000 WHERE network = '{ISSUING}'"
        ),
    )
    .await;
    let e = check.evaluate(ASSET, t0()).await.unwrap();
    assert_eq!(e.outcome, Outcome::Holds, "{:?}", e.differences);
}

/// Review M2: an evaluation that cannot be made for longer than the bound pauses the asset's
/// routes and pages, once (`REQ-IC-2`: a finality stall, an observation gap).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ic_2_nfr_5_m2_an_evaluation_that_cannot_be_made_beyond_the_bound_pauses(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000).await;
    read_all(&w.admin, 100).await;
    let (check, readings, pager) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    readings.chain().unreachable = true;
    let clock = ManualClock::at(t0());
    for _ in 0..2 {
        let e = check.tick(&clock).await.unwrap().remove(0);
        assert_eq!(e.outcome, Outcome::Incomplete);
        clock.advance(Duration::minutes(1));
    }
    // A minute of incomplete evaluations is within the bound.
    assert_eq!(w.count("SELECT count(*) FROM pauses").await, 0);
    let e = check.tick(&clock).await.unwrap().remove(0);
    assert_eq!(e.outcome, Outcome::Incomplete);
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM pauses WHERE asset = '{ASSET}' AND cause = 'REQ-IC-2' \
               AND evaluation_id = {}",
            e.id
        ))
        .await,
        1
    );
    assert_eq!(
        pager.kinds(),
        ["reserve-check-incomplete", "reserve-check-stalled"]
    );
    clock.advance(Duration::minutes(1));
    check.tick(&clock).await.unwrap();
    assert_eq!(w.count("SELECT count(*) FROM pauses").await, 1);
    assert_eq!(pager.kinds().len(), 2);
}

/// Review S6: a floor or alert level the operator did not declare alerts (`REQ-BK-10`: "a floor
/// it declares", and an alert before it).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_10_an_undeclared_floor_or_alert_level_alerts(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    exec(
        &w.admin,
        &format!("UPDATE assets SET surplus_alert = NULL WHERE asset = '{ASSET}'"),
    )
    .await;
    fund(&w, 5, 1_000).await;
    read_all(&w.admin, 100).await;
    let (check, readings, pager) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    let e = check.tick(&ManualClock::at(t0())).await.unwrap().remove(0);
    assert_eq!(e.outcome, Outcome::Holds);
    assert_eq!(e.alerts, ["surplus-levels-undeclared"]);
    assert_eq!(pager.kinds(), ["reserve-alert"]);
}

/// Inserts the deposit request a requester would make for the arrival at (`ORIGIN`, `block`, log
/// `index`), straight into the store as its owner, and returns what the store answers.
async fn bind(w: &World, block: i64, index: i64, amount: u128) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, refund_destination_network, refund_destination, \
             source_network, source_block_number, source_block_hash, source_position_kind, \
             source_position_index, state) \
         VALUES ('bz-d-' || $1 || '-' || $9, 'req-' || $1 || '-' || $9, 'deposit', $2, \
             $3::numeric, $4, $5, $6, $7, $6, $1, $8, 'log', $9, 'Received') RETURNING id",
    )
    .bind(block)
    .bind(ASSET)
    .bind(amount.to_string())
    .bind(ISSUING)
    .bind(DESTINATION)
    .bind(ORIGIN)
    .bind(BENEFICIARY)
    .bind(hash(ORIGIN, block))
    .bind(index)
    .fetch_one(&w.admin)
    .await
}

/// The operator's top-up of `amount` from [`FUNDING`] at (`ORIGIN`, `block`, log `index`), final
/// two blocks later.
async fn top_up(w: &World, block: i64, index: i64, amount: u128) -> i64 {
    let id = observe(
        &w.admin,
        ORIGIN,
        block,
        index,
        TOKEN,
        amount,
        Some(FUNDING),
        Some(RESERVE),
        None,
        true,
        json!({}),
    )
    .await;
    decide(&w.admin, id, "final", block + 2).await;
    id
}

/// Review MF1, probe `probe_a_funding_arrival_can_be_bound_and_minted`: an arrival from a declared
/// operator funding address is a surplus contribution, and no deposit can ever bind it. The store
/// refuses the binding (and Source-Final on one bound before the rule), the core never moves it,
/// and the check keeps holding with the top-up as surplus: nobody gets a mint for it, and the
/// asset does not pause (`INV-1`, `INV-2`, `REQ-IN-3`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_1_inv_2_req_in_3_mf1_an_operator_top_up_is_never_bound_nor_minted(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000).await;
    top_up(&w, 20, 0, 5_000).await;
    // 1. A deposit naming the top-up's coordinate is refused by the store, whoever writes it.
    let refused = bind(&w, 20, 0, 5_000)
        .await
        .expect_err("bound an operator top-up");
    assert!(refused.to_string().contains("REQ-IN-3"), "{refused}");
    // 2. A binding made before the rule existed never reaches Source-Final: the core holds it,
    //    and the store refuses the transition if anything asks for it.
    top_up(&w, 30, 1, 2_000).await;
    exec(
        &w.admin,
        "ALTER TABLE transfers DISABLE TRIGGER transfers_source_not_funding",
    )
    .await;
    let legacy = bind(&w, 30, 1, 2_000).await.unwrap();
    exec(
        &w.admin,
        "ALTER TABLE transfers ENABLE TRIGGER transfers_source_not_funding",
    )
    .await;
    exec(
        &w.admin,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state) \
             VALUES ({legacy}, 'Received', 'Accepted')"
        ),
    )
    .await;
    w.cycle(t0() + Duration::minutes(5)).await;
    assert_eq!(w.state(legacy).await.0, "Accepted");
    let forced = sqlx::query(&format!(
        "INSERT INTO transitions (transfer_id, from_state, to_state, network, block_number, \
             block_hash, block_time) \
         VALUES ({legacy}, 'Accepted', 'Source-Final', '{ORIGIN}', 32, '\\x{}', '{}')",
        hex::encode(hash(ORIGIN, 32)),
        (t0() + Duration::seconds(32)).to_rfc3339()
    ))
    .execute(&w.admin)
    .await
    .expect_err("Source-Final on an operator top-up");
    assert!(forced.to_string().contains("REQ-IN-3"), "{forced}");
    // 3. The check holds: both top-ups are surplus, nothing is owed for them, nothing pauses.
    read_all(&w.admin, 100).await;
    let (check, readings, pager) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    let e = check.tick(&ManualClock::at(t0())).await.unwrap().remove(0);
    assert_eq!(e.outcome, Outcome::Holds, "{:?}", e.differences);
    assert!(e.chain.owed_mint.is_empty(), "{:?}", e.chain.owed_mint);
    assert_eq!(e.book.surplus, 8_000);
    assert_eq!(w.count("SELECT count(*) FROM pauses").await, 0);
    assert!(pager.pages().is_empty(), "{:?}", pager.pages());
}

/// Review MF2, probe `probe_b_stale_head_holds_for_hours`: final heads stop being recorded at
/// block 100. The point does not lag the latest *recorded* head, but that head lags the clock:
/// past the bound the evaluation diverges, pauses and pages, never holds (`INV-1`, `NFR-5`,
/// `REQ-IC-2`: a finality stall).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_1_nfr_5_req_ic_2_mf2_a_final_head_older_than_the_bound_never_holds(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000).await;
    read_all(&w.admin, 100).await;
    let (check, readings, pager) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    // While the head is fresh by the clock, the equation holds.
    let e = check.evaluate(ASSET, t0()).await.unwrap();
    assert_eq!(e.outcome, Outcome::Holds, "{:?}", e.differences);
    for hours in [6, 24] {
        let e = check
            .evaluate(ASSET, t0() + Duration::hours(hours))
            .await
            .unwrap();
        assert_eq!(e.outcome, Outcome::Diverges, "{e:?}");
        assert_eq!(e.lags[ISSUING].seconds, 0);
        assert_eq!(
            e.lags[ISSUING].head_age_seconds,
            hours * 3_600 - 100,
            "{:?}",
            e.lags
        );
        for network in [ISSUING, ORIGIN] {
            assert!(
                e.differences.iter().any(|d| d.term == "final_head"
                    && d.item.as_deref().unwrap().contains(network)),
                "{:?}",
                e.differences
            );
        }
    }
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM pauses WHERE asset = '{ASSET}' AND cause = 'REQ-BK-1'"
        ))
        .await,
        1
    );
    assert_eq!(pager.kinds(), ["reserve-divergence"]);
}

/// Review SF1: an evaluation that errors (here, the store refuses to record any complete one) is
/// recorded as incomplete, paged once when it turns so, and pauses the asset's routes once it has
/// been failing for longer than the bound; never a page a minute (`REQ-IC-2`, `NFR-5`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ic_2_nfr_5_sf1_an_evaluation_that_errors_pauses_past_the_bound_and_pages_once(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000).await;
    read_all(&w.admin, 1_000).await;
    exec(
        &w.admin,
        "CREATE FUNCTION refuse_complete() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN RAISE EXCEPTION 'refused for the test' USING ERRCODE = 'BZ001'; END $$; \
         REVOKE EXECUTE ON FUNCTION refuse_complete() FROM PUBLIC; \
         CREATE TRIGGER refuse_complete BEFORE INSERT ON reserve_evaluations \
           FOR EACH ROW WHEN (NEW.outcome <> 'incomplete') EXECUTE FUNCTION refuse_complete();",
    )
    .await;
    let (check, readings, pager) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    let clock = ManualClock::at(t0());
    for _ in 0..2 {
        assert!(check.tick(&clock).await.unwrap().is_empty());
        clock.advance(Duration::minutes(1));
    }
    // A minute of failures is within the bound: paged once, not paused.
    assert_eq!(pager.kinds(), ["reserve-check-failed"]);
    assert_eq!(w.count("SELECT count(*) FROM pauses").await, 0);
    for _ in 0..3 {
        assert!(check.tick(&clock).await.unwrap().is_empty());
        clock.advance(Duration::minutes(1));
    }
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM pauses WHERE asset = '{ASSET}' AND cause = 'REQ-IC-2' \
               AND evaluation_id IS NOT NULL"
        ))
        .await,
        1
    );
    assert_eq!(
        w.count("SELECT count(*) FROM reserve_evaluations WHERE outcome = 'incomplete'")
            .await,
        5
    );
    assert_eq!(
        pager.kinds(),
        ["reserve-check-failed", "reserve-check-stalled"]
    );
}

/// Review SF2: the lag bound has a ceiling per network: at most an hour, and no more than the
/// completion budget of any route the network serves. Above it the configuration is refused, so
/// no single value can switch off the stale-point divergence or the stall pause (`NFR-5`, `NFR-6`,
/// `REQ-IC-2`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn nfr_5_req_ic_2_sf2_the_lag_bound_is_refused_above_its_ceiling(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let set = |network: &'static str, seconds: i32| {
        let pool = w.admin.clone();
        async move {
            sqlx::query("UPDATE networks SET reserve_point_lag_seconds = $1 WHERE network = $2")
                .bind(seconds)
                .bind(network)
                .execute(&pool)
                .await
        }
    };
    // At most an hour, on any network, whatever the routes' budgets.
    exec(
        &w.admin,
        "UPDATE routes SET completion_budget_seconds = 7200",
    )
    .await;
    for network in [ISSUING, ORIGIN] {
        let e = set(network, 3_601).await.expect_err("an hour and a second");
        assert!(e.to_string().contains("at_most_an_hour"), "{e}");
        set(network, 3_600).await.unwrap();
    }
    // A budget below a bound the networks are set to is refused (SF-C); lowered with the bounds.
    for network in [ISSUING, ORIGIN] {
        set(network, 60).await.unwrap();
    }
    // And no more than the budget of a route the network serves (the issuing network serves all).
    exec(
        &w.admin,
        &format!("UPDATE routes SET completion_budget_seconds = 600 WHERE network = '{ORIGIN}'"),
    )
    .await;
    for network in [ISSUING, ORIGIN] {
        let e = set(network, 601)
            .await
            .expect_err("beyond the route's budget");
        assert!(e.to_string().contains("NFR-6"), "{e}");
        set(network, 600).await.unwrap();
    }
}

/// Review SF-A, probe `probe_a_refund_binds_an_operator_top_up`: the store refuses a refund of an
/// unassociated arrival from a declared funding address, as it refuses a deposit, whoever writes
/// it; a refund of an ordinary unassociated arrival is untouched (`INV-1`, `INV-2`, `REQ-IN-3`,
/// `REQ-RQ-6`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn inv_2_req_rq_6_sf_a_the_store_refuses_a_refund_of_an_operator_top_up(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000).await;
    top_up(&w, 20, 0, 5_000).await;
    let refund = |block: i64, index: i64| {
        let pool = w.admin.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
                     destination_network, destination, source_network, source_block_number, \
                     source_block_hash, source_position_kind, source_position_index, state) \
                 VALUES ('bz-u-' || $1 || '-' || $5, 'req-u-' || $1 || '-' || $5, 'refund', $2, \
                     1000, $3, $4, $3, $1, $6, 'log', $5, 'Received') RETURNING id",
            )
            .bind(block)
            .bind(ASSET)
            .bind(ORIGIN)
            .bind(BENEFICIARY)
            .bind(index)
            .bind(hash(ORIGIN, block))
            .fetch_one(&pool)
            .await
        }
    };
    let refused = refund(20, 0)
        .await
        .expect_err("refunded an operator top-up");
    assert!(refused.to_string().contains("REQ-IN-3"), "{refused}");
    assert!(refused.to_string().contains("refund refused"), "{refused}");
    // An ordinary arrival (from another sender) is refundable as before.
    refund(5, 0)
        .await
        .expect("an ordinary unassociated arrival");
}

/// Review SF-B: the lag bound defaults to the network's finality time plus a minute, so a network
/// that finalizes in 13 minutes is not paused while its heads arrive on time, and is when they
/// stop (`NFR-5`, `REQ-IC-2`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn nfr_5_req_ic_2_sf_b_a_slow_finality_network_pauses_only_when_heads_stop(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    exec(
        &w.admin,
        &format!("UPDATE networks SET finality_seconds = 780 WHERE network = '{ORIGIN}'"),
    )
    .await;
    // One minute plus the finality time; the issuing network keeps the minute.
    assert_eq!(
        w.count(&format!(
            "SELECT reserve_point_lag_seconds::bigint FROM networks WHERE network = '{ORIGIN}'"
        ))
        .await,
        840
    );
    assert_eq!(
        w.count(&format!(
            "SELECT reserve_point_lag_seconds::bigint FROM networks WHERE network = '{ISSUING}'"
        ))
        .await,
        60
    );
    fund(&w, 5, 1_000).await;
    // The issuing network's head is the clock's; the origin's final head is 13 minutes old by its
    // block time, as a finalized head is when it has just arrived.
    read_through(&w.admin, ISSUING, 880).await;
    read_through(&w.admin, ORIGIN, 100).await;
    let (check, readings, pager) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    let e = check
        .evaluate(ASSET, t0() + Duration::seconds(880))
        .await
        .unwrap();
    assert_eq!(e.outcome, Outcome::Holds, "{:?}", e.differences);
    assert_eq!(e.lags[ORIGIN].head_age_seconds, 780);
    assert_eq!(e.lags[ORIGIN].bound_seconds, 840);
    assert_eq!(w.count("SELECT count(*) FROM pauses").await, 0);
    assert!(pager.kinds().is_empty(), "{:?}", pager.kinds());
    // The heads stop: the origin's is past its finality time plus the minute.
    let e = check
        .evaluate(ASSET, t0() + Duration::seconds(100 + 841))
        .await
        .unwrap();
    assert_eq!(e.outcome, Outcome::Diverges, "{e:?}");
    assert!(
        e.differences
            .iter()
            .any(|d| d.term == "final_head" && d.item.as_deref().unwrap().contains(ORIGIN)),
        "{:?}",
        e.differences
    );
}

/// Review SF3: a funding declaration applies only from the block it is declared at. An arrival
/// already observed from the address stays owed mint (no permanent divergence, no customer
/// deposit turned into surplus); a declaration naming an earlier block is refused; arrivals after
/// it are contributions (`REQ-BK-5`, `INV-1`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_bk_5_inv_1_sf3_a_funding_declaration_never_applies_retroactively(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    fund(&w, 5, 1_000).await;
    // An arrival from 0xfunder, posted as owed mint.
    let early = final_arrival(&w.admin, 20, 0, 300).await;
    book::post_pending(w.core.pool()).await.unwrap();
    // A declaration naming a block already recorded is refused.
    let refused = sqlx::query(&format!(
        "INSERT INTO surplus_funding_addresses (asset, network, address, reference, from_block) \
         VALUES ('{ASSET}', '{ORIGIN}', '0xfunder', 'funding-declaration-2', 10)"
    ))
    .execute(&w.admin)
    .await
    .expect_err("a retroactive declaration");
    assert!(
        refused.to_string().contains("never retroactive"),
        "{refused}"
    );
    // Declared now, it applies from the block after the latest recorded on the network.
    exec(
        &w.admin,
        &format!(
            "INSERT INTO surplus_funding_addresses (asset, network, address, reference) \
             VALUES ('{ASSET}', '{ORIGIN}', '0xfunder', 'funding-declaration-2')"
        ),
    )
    .await;
    let from: i64 = scalar(
        &w.admin,
        "SELECT from_block FROM surplus_funding_addresses WHERE address = '0xfunder'",
    )
    .await;
    assert_eq!(from, 23);
    // A later arrival from it is surplus.
    let later = final_arrival(&w.admin, 40, 1, 200).await;
    read_all(&w.admin, 100).await;
    let (check, readings, pager) = checker(&w);
    sync_chain(&w.admin, &readings).await;
    let e = check.tick(&ManualClock::at(t0())).await.unwrap().remove(0);
    assert_eq!(e.outcome, Outcome::Holds, "{:?}", e.differences);
    assert_eq!(
        e.chain.owed_mint.keys().cloned().collect::<Vec<_>>(),
        [coordinate_key(&at(ORIGIN, 20, 0))]
    );
    assert_eq!(e.book.surplus, 1_200);
    for (id, kind) in [(early, "arrival"), (later, "surplus-contribution")] {
        assert_eq!(
            scalar::<String>(
                &w.admin,
                &format!("SELECT kind FROM book_entries WHERE observation_id = {id}"),
            )
            .await,
            kind
        );
    }
    assert!(pager.pages().is_empty(), "{:?}", pager.pages());
}
