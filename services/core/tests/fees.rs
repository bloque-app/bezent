// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-014 · the fee position floor and its page (`REQ-ST-6`, `AC-H3.1`; PLAN `DEC-18`; amendment 0025,
//! `d-fee-position-scope`). The floor is in the core's configuration; the core pages `fee-position-low` when a
//! network's fee position is below it, once on crossing and then daily, and a balance back above it re-arms the page.

mod common;

use std::collections::BTreeMap;

use bezent_core_service::fees::{self, Outcome};
use chrono::{DateTime, Duration, Utc};
use common::pagelog::{PageLog, assert_page, page_lines};
use common::*;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

fn floors(floor: u128) -> BTreeMap<String, u128> {
    BTreeMap::from([(ISSUING.to_owned(), floor)])
}

/// Records a final head of `network` at `block` and its fee position there: `amount`, or unknown.
async fn reading(admin: &PgPool, network: &str, block: i64, amount: Option<&str>) {
    let hash = format!("\\x{block:04x}");
    let (column, value) = match amount {
        Some(a) => ("amount", a.to_owned()),
        None => (
            "unknown_reason",
            "'the balance could not be read'".to_owned(),
        ),
    };
    exec(
        admin,
        &format!(
            "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
               VALUES ('{network}', {block}, '{hash}', now()) ON CONFLICT DO NOTHING; \
             INSERT INTO fee_position_readings (network, block_number, block_hash, holder, {column}) \
               VALUES ('{network}', {block}, '{hash}', '0xlane', {value})"
        ),
    )
    .await;
}

async fn entries(admin: &PgPool, kind: &str) -> i64 {
    scalar(
        admin,
        &format!("SELECT count(*) FROM event_log WHERE kind = '{kind}'"),
    )
    .await
}

fn at(hours: i64) -> DateTime<Utc> {
    "2026-11-06T12:00:00Z".parse::<DateTime<Utc>>().unwrap() + Duration::hours(hours)
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_h3_1_req_st_6_a_fee_position_below_its_floor_pages_once_then_daily_and_recovery_rearms(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let pool = w.core.pool();
    let log = PageLog::start();
    let floors = floors(1_000);

    // Never read, and then read above the floor: nothing to page, and nothing to recover from.
    assert_eq!(fees::check(pool, &floors, at(0)).await.unwrap(), vec![]);
    reading(&w.admin, ISSUING, 100, Some("5000")).await;
    assert_eq!(fees::check(pool, &floors, at(0)).await.unwrap(), vec![]);
    assert!(page_lines(&log, "fee-position-low").is_empty());
    assert_eq!(entries(&w.admin, fees::RECOVERED).await, 0);

    // Below the floor: a page, a log line a filter matches, and an entry that remembers it.
    reading(&w.admin, ISSUING, 101, Some("999")).await;
    assert_eq!(
        fees::check(pool, &floors, at(1)).await.unwrap(),
        vec![Outcome::Paged {
            network: ISSUING.into(),
            repeat: false
        }]
    );
    assert_page(&log, "fee-position-low", 1);
    let line = &page_lines(&log, "fee-position-low")[0];
    assert!(
        line.contains(ISSUING) && line.contains("999") && line.contains("1000"),
        "{line}"
    );
    assert!(
        !line.contains("0xlane"),
        "a page carries no address: {line}"
    );
    let (spec, network, amount, floor): (String, String, String, String) = sqlx::query_as(
        "SELECT payload ->> 'spec', payload ->> 'network', payload ->> 'amount', payload ->> 'floor' \
         FROM event_log WHERE kind = 'fee-position-low'",
    )
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(
        (network.as_str(), amount.as_str(), floor.as_str()),
        (ISSUING, "999", "1000")
    );
    assert!(spec.contains("REQ-ST-6"));

    // The next minutes, and the next hours of the day: the page stands, nothing more is said.
    for hours in [1, 2, 23] {
        assert_eq!(
            fees::check(pool, &floors, at(hours)).await.unwrap(),
            vec![Outcome::Standing(ISSUING.into())],
            "{hours} h"
        );
    }
    assert_page(&log, "fee-position-low", 1);

    // A reading that cannot be made is not low and not recovered: the page's state is left as it was, and the
    // missing-data alarm is what says it (alerts.md).
    reading(&w.admin, ISSUING, 102, None).await;
    assert_eq!(fees::check(pool, &floors, at(30)).await.unwrap(), vec![]);
    assert_page(&log, "fee-position-low", 1);
    assert_eq!(entries(&w.admin, fees::RECOVERED).await, 0);

    // Still low a day on: the page repeats, once, as a repeat.
    reading(&w.admin, ISSUING, 103, Some("500")).await;
    assert_eq!(
        fees::check(pool, &floors, at(26)).await.unwrap(),
        vec![Outcome::Paged {
            network: ISSUING.into(),
            repeat: true
        }]
    );
    assert_page(&log, "fee-position-low", 2);
    assert_eq!(
        fees::check(pool, &floors, at(27)).await.unwrap(),
        vec![Outcome::Standing(ISSUING.into())]
    );

    // A top-up: back at the floor exactly is recovered (below is strictly less), and says so once.
    reading(&w.admin, ISSUING, 104, Some("1000")).await;
    assert_eq!(
        fees::check(pool, &floors, at(28)).await.unwrap(),
        vec![Outcome::Recovered(ISSUING.into())]
    );
    assert_eq!(fees::check(pool, &floors, at(29)).await.unwrap(), vec![]);
    assert_eq!(entries(&w.admin, fees::RECOVERED).await, 1);
    assert_page(&log, "fee-position-low", 2);

    // A drop after the recovery is a new crossing: it pages at once, not a day after the last page.
    reading(&w.admin, ISSUING, 105, Some("10")).await;
    assert_eq!(
        fees::check(pool, &floors, at(30)).await.unwrap(),
        vec![Outcome::Paged {
            network: ISSUING.into(),
            repeat: false
        }]
    );
    assert_page(&log, "fee-position-low", 3);
    assert_eq!(entries(&w.admin, fees::KIND).await, 3);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_6_a_network_without_a_floor_is_never_paged_and_each_network_has_its_own_state(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let pool = w.core.pool();
    let log = PageLog::start();
    // Both networks are nearly empty; only the issuing network has a floor.
    reading(&w.admin, ISSUING, 100, Some("1")).await;
    reading(&w.admin, ORIGIN, 100, Some("1")).await;
    assert_eq!(
        fees::check(pool, &floors(1_000), at(0)).await.unwrap(),
        vec![Outcome::Paged {
            network: ISSUING.into(),
            repeat: false
        }]
    );
    assert_page(&log, "fee-position-low", 1);
    // A floor for the other network pages it on its own, and the first one's standing page is not repeated for it.
    let both = BTreeMap::from([(ISSUING.to_owned(), 1_000), (ORIGIN.to_owned(), 1_000)]);
    assert_eq!(
        fees::check(pool, &both, at(1)).await.unwrap(),
        // In the order of the network ids the store lists them in.
        vec![
            Outcome::Standing(ISSUING.into()),
            Outcome::Paged {
                network: ORIGIN.into(),
                repeat: false
            },
        ]
    );
    assert_page(&log, "fee-position-low", 2);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_6_the_floor_is_compared_exactly_not_as_a_float(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let log = PageLog::start();
    // 2^53 + 1 and 2^53 + 2: the same f64, different amounts. An 18-decimal token's balance is in this range.
    reading(&w.admin, ISSUING, 100, Some("9007199254740993")).await;
    assert_eq!(
        fees::check(w.core.pool(), &floors(9_007_199_254_740_994), at(0))
            .await
            .unwrap(),
        vec![Outcome::Paged {
            network: ISSUING.into(),
            repeat: false
        }]
    );
    assert_page(&log, "fee-position-low", 1);
    // And the same amount against an equal floor is not low.
    reading(&w.admin, ISSUING, 101, Some("9007199254740994")).await;
    assert_eq!(
        fees::check(w.core.pool(), &floors(9_007_199_254_740_994), at(1))
            .await
            .unwrap(),
        vec![Outcome::Recovered(ISSUING.into())]
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_st_6_a_fresh_watch_remembers_the_page_from_the_log_after_a_restart(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let log = PageLog::start();
    reading(&w.admin, ISSUING, 100, Some("1")).await;
    let t = std::time::Instant::now();
    let mut first = fees::Watch::new(floors(1_000));
    first.tick(w.core.pool(), t, at(0)).await;
    assert_page(&log, "fee-position-low", 1);
    // Inside its minute, a watch does not look again.
    first.tick(w.core.pool(), t, at(24)).await;
    assert_page(&log, "fee-position-low", 1);
    // A process that starts afresh has no memory but the log's, and says nothing more inside the day.
    let mut restarted = fees::Watch::new(floors(1_000));
    restarted.tick(w.core.pool(), t, at(2)).await;
    assert_page(&log, "fee-position-low", 1);
    assert_eq!(entries(&w.admin, fees::KIND).await, 1);
    // Past the day it repeats, from the log's time.
    let mut later = fees::Watch::new(floors(1_000));
    later.tick(w.core.pool(), t, at(25)).await;
    assert_page(&log, "fee-position-low", 2);
}
