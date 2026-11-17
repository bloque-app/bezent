// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-018, T-018-11 (amendment 0029, "New exposures", Open 5 and 6: alerts only): the core's alerts on
//! directed deposits, at the service layer (SPEC §9: the core over durable state), each fired here
//! through the real emission (`REQ-IC-2`, `REQ-ST-6`, `AC-A6.6`).
//!
//! - Directed mints, returns and the share of the window each page above their configured threshold,
//!   once on crossing and then daily, with a log line a filter matches and an entry that remembers
//!   it, and clear once back at or below it, so a second crossing pages at once. Nothing is capped:
//!   the deposits above the threshold are made and minted as any other.
//! - An `UnpairedDirection` pages, once per observation.
//! - An origin token's `Upgraded` pages, once per observation, and pauses and holds nothing (the open
//!   ruling `d-f018-usdc-upgrade`).

mod common;

use bezent_core_service::directed_alerts::{self, Outcome, Thresholds};
use common::pagelog::{PageLog, assert_page, page_lines};
use common::*;
use serde_json::json;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

const DIRECTING: &str = "0xD1Ec7ed00000000000000000000000000000d1ec";
const REFUND: &str = "0x52908400098527886E0F7030069857D2E4169EE7";
const FUNDER: &str = "0x00000000000000000000000000000000000000f1";

const QUIET: Thresholds = Thresholds {
    mints_per_day: 1_000,
    returns_per_day: 1_000,
    window_share_percent: 100,
};

/// Declares [`DIRECTING`] for the reserve, for [`TOKEN`], from block 0 (`REQ-AS-11`; once): only
/// declared directing code directs (review M3 of PR #139).
async fn declare_directing(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO directing_code (asset, network, address, token, minimum, maximum, \
             from_block, reference) \
         VALUES ($1, $2, $3, $4, 1, 1000000000000000, 0, 'test declaration') \
         ON CONFLICT DO NOTHING",
    )
    .bind(ASSET)
    .bind(ORIGIN)
    .bind(DIRECTING)
    .bind(TOKEN)
    .execute(pool)
    .await
    .expect("declare the directing code");
}

/// A final directed arrival of `amount` for `recipient` at (`ORIGIN`, `block`, log 0), through
/// declared directing code.
async fn final_directed(pool: &PgPool, block: i64, amount: u128, recipient: &str) -> i64 {
    declare_directing(pool).await;
    let id = observe(
        pool,
        ORIGIN,
        block,
        0,
        TOKEN,
        amount,
        Some(FUNDER),
        Some(RESERVE),
        None,
        true,
        json!({"event": "DirectedArrival", "directing": DIRECTING, "direction_log": 1,
               "recipient": recipient, "refund": REFUND}),
    )
    .await;
    decide(pool, id, "final", block + 2).await;
    id
}

/// The transfer bound to the directed arrival at `block`: (id, kind).
async fn bound(w: &World, block: i64) -> (i64, String) {
    sqlx::query_as(
        "SELECT id, kind FROM transfers WHERE refund_of IS NULL AND source_network = $1 \
           AND source_block_number = $2",
    )
    .bind(ORIGIN)
    .bind(block)
    .fetch_one(&w.admin)
    .await
    .unwrap()
}

/// Mints the directed deposit `id` on the issuing network at `block`: included, so `Delivered`.
async fn minted(w: &World, id: i64, block: i64) {
    deliver(w, id, 1, t0()).await;
    let reference = reference_of(&w.admin, id).await;
    observe(
        &w.admin,
        ISSUING,
        block,
        0,
        ISSUING_ASSET,
        5_000_000,
        None,
        Some(DESTINATION_ACCOUNT),
        Some(&reference),
        true,
        json!({"event": "Minted"}),
    )
    .await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await.0, "Delivered");
}

async fn entries(admin: &PgPool, kind: &str) -> i64 {
    scalar(
        admin,
        &format!("SELECT count(*) FROM event_log WHERE kind = '{kind}'"),
    )
    .await
}

/// `REQ-IC-2`, `REQ-ST-6` (amendment 0029, Open 6: no count cap, an alert): more directed deposits
/// minted on a route in the trailing day than the threshold pages, once, then not again while it
/// stands; both deposits are minted all the same. Raised above the count, it clears, and logs that
/// it did; a page is a log line a filter matches.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ic_2_directed_mints_above_the_threshold_page_once_and_clear_and_nothing_is_capped(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let pool = w.core.pool();
    let log = PageLog::start();
    let low = Thresholds {
        mints_per_day: 1,
        ..QUIET
    };
    final_directed(&w.admin, 10, 5_000_000, DESTINATION_ACCOUNT).await;
    final_directed(&w.admin, 11, 5_000_000, DESTINATION_ACCOUNT).await;
    w.settle(t0()).await;
    let (a, _) = bound(&w, 10).await;
    let (b, _) = bound(&w, 11).await;
    minted(&w, a, 40).await;
    assert_eq!(
        directed_alerts::check(pool, &low).await.unwrap(),
        vec![],
        "one mint is not above one"
    );
    minted(&w, b, 41).await;

    let paged = directed_alerts::check(pool, &low).await.unwrap();
    assert_eq!(
        paged,
        vec![Outcome::Paged {
            kind: directed_alerts::MINTS,
            asset: ASSET.into(),
            network: ORIGIN.into(),
            value: 2,
            repeat: false
        }]
    );
    assert_page(&log, "directed-mints-alert", 1);
    assert_eq!(entries(&w.admin, directed_alerts::MINTS).await, 1);
    // Standing: not paged again within the day.
    assert_eq!(
        directed_alerts::check(pool, &low).await.unwrap(),
        vec![Outcome::Standing {
            kind: directed_alerts::MINTS,
            asset: ASSET.into(),
            network: ORIGIN.into()
        }]
    );
    assert_eq!(page_lines(&log, "directed-mints-alert").len(), 1);
    // Alerts only: both were minted.
    for id in [a, b] {
        assert_eq!(w.state(id).await.0, "Delivered");
    }
    // At or below the threshold: cleared, once.
    let raised = Thresholds {
        mints_per_day: 2,
        ..QUIET
    };
    assert_eq!(
        directed_alerts::check(pool, &raised).await.unwrap(),
        vec![Outcome::Cleared {
            kind: directed_alerts::MINTS,
            asset: ASSET.into(),
            network: ORIGIN.into()
        }]
    );
    assert_eq!(entries(&w.admin, directed_alerts::CLEARED).await, 1);
    assert_eq!(directed_alerts::check(pool, &raised).await.unwrap(), vec![]);
    // A second crossing pages at once.
    let paged = directed_alerts::check(pool, &low).await.unwrap();
    assert!(
        matches!(paged.as_slice(), [Outcome::Paged { repeat: false, .. }]),
        "{paged:?}"
    );
    assert_page(&log, "directed-mints-alert", 2);
}

/// `REQ-IC-2` (amendment 0029, A5, Open 6): a return of a directed arrival (here, a recipient refused
/// by rule) counts toward the returns of the day; above the threshold it pages; nothing stops the
/// return.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ic_2_directed_returns_above_the_threshold_page(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let pool = w.core.pool();
    let log = PageLog::start();
    exec(
        &w.admin,
        &format!(
            "INSERT INTO ledger_accounts (network, address, reason) \
             VALUES ('{ISSUING}', '{DESTINATION}', 'a ledger''s contract account')"
        ),
    )
    .await;
    final_directed(&w.admin, 10, 5_000_000, DESTINATION_ACCOUNT).await;
    w.cycle(t0()).await;
    let (_, kind) = bound(&w, 10).await;
    assert_eq!(kind, "refund", "returned");
    let zero = Thresholds {
        returns_per_day: 0,
        ..QUIET
    };
    assert_eq!(
        directed_alerts::check(pool, &QUIET).await.unwrap(),
        vec![],
        "one return is not above the quiet threshold"
    );
    assert_eq!(
        directed_alerts::check(pool, &zero).await.unwrap(),
        vec![Outcome::Paged {
            kind: directed_alerts::RETURNS,
            asset: ASSET.into(),
            network: ORIGIN.into(),
            value: 1,
            repeat: false
        }]
    );
    assert_page(&log, "directed-returns-alert", 1);
}

/// `REQ-IC-2`, `REQ-LM-1` (amendment 0029, Open 5: no separate share, an alert): directed deposits'
/// hold on the route's window (those it counts and those waiting in `CND-DepositWindow`, which claim
/// it ahead) above the threshold share pages, in basis points of the window; the waiting deposit is
/// not returned or refused.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ic_2_directed_deposits_holding_the_window_above_the_threshold_page(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let pool = w.core.pool();
    let log = PageLog::start();
    exec(
        &w.admin,
        &format!(
            "UPDATE routes SET maximum = 6000000, window_limit = 10000000, window_seconds = 86400 \
             WHERE asset = '{ASSET}' AND network = '{ORIGIN}' AND direction = 'deposit'"
        ),
    )
    .await;
    final_directed(&w.admin, 10, 6_000_000, DESTINATION_ACCOUNT).await;
    final_directed(&w.admin, 11, 6_000_000, DESTINATION_ACCOUNT).await;
    w.cycle(t0()).await;
    let (b, _) = bound(&w, 11).await;
    assert_eq!(
        w.state(b).await,
        ("Source-Final".into(), Some("CND-DepositWindow".into()))
    );
    // 12 of 10: a's share and b's claim, 120 %.
    assert_eq!(
        directed_alerts::check(pool, &QUIET).await.unwrap(),
        vec![Outcome::Paged {
            kind: directed_alerts::WINDOW,
            asset: ASSET.into(),
            network: ORIGIN.into(),
            value: 12_000,
            repeat: false
        }],
        "above 100 % pages even the quietest threshold"
    );
    assert_page(&log, "directed-window-alert", 1);
    // Half the window is the default threshold; 60 % of it held by a alone would page too.
    let half = Thresholds {
        window_share_percent: 50,
        ..QUIET
    };
    assert!(matches!(
        directed_alerts::check(pool, &half)
            .await
            .unwrap()
            .as_slice(),
        [Outcome::Standing { .. }]
    ));
    assert_eq!(
        w.state(b).await,
        ("Source-Final".into(), Some("CND-DepositWindow".into())),
        "alerts only: it waits"
    );
}

/// `AC-A6.6`, `REQ-IC-2`: an `UnpairedDirection` pages, once per observation, naming its coordinate
/// and the directing contract; a second check pages nothing new.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_a6_6_an_unpaired_direction_pages_once(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let pool = w.core.pool();
    let log = PageLog::start();
    let id = observe(
        &w.admin,
        ORIGIN,
        20,
        3,
        TOKEN,
        5_000_000,
        Some(FUNDER),
        None,
        None,
        true,
        json!({"event": "UnpairedDirection", "directing": DIRECTING,
               "reason": "no admitted-token transfer immediately before it"}),
    )
    .await;
    decide(&w.admin, id, "final", 22).await;
    assert_eq!(
        directed_alerts::check(pool, &QUIET).await.unwrap(),
        vec![Outcome::Observation {
            kind: directed_alerts::UNPAIRED,
            id
        }]
    );
    assert_page(&log, "unpaired-direction", 1);
    let line = &page_lines(&log, "unpaired-direction")[0];
    assert!(line.contains(DIRECTING), "{line}");
    assert_eq!(directed_alerts::check(pool, &QUIET).await.unwrap(), vec![]);
    assert_page(&log, "unpaired-direction", 1);
    assert_eq!(
        w.count("SELECT count(*) FROM transfers").await,
        0,
        "binds nothing"
    );
}

/// `REQ-AS-11`, `REQ-IC-2` (open ruling `d-f018-usdc-upgrade`): an admitted origin token's `Upgraded`
/// pages, once, and pauses and holds nothing: a directed deposit made after it is minted as before.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_as_11_an_origin_tokens_upgrade_pages_and_pauses_nothing(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let pool = w.core.pool();
    let log = PageLog::start();
    let pauses = w.count("SELECT count(*) FROM pauses").await;
    let id = observe(
        &w.admin,
        ORIGIN,
        30,
        0,
        TOKEN,
        0,
        None,
        None,
        None,
        true,
        json!({"event": "TokenUpgraded", "token": TOKEN,
               "implementation": "0x0000000000000000000000000000000000000abc"}),
    )
    .await;
    decide(&w.admin, id, "final", 32).await;
    assert_eq!(
        directed_alerts::check(pool, &QUIET).await.unwrap(),
        vec![Outcome::Observation {
            kind: directed_alerts::UPGRADED,
            id
        }]
    );
    assert_page(&log, "origin-token-upgraded", 1);
    assert_eq!(directed_alerts::check(pool, &QUIET).await.unwrap(), vec![]);
    assert_eq!(
        w.count("SELECT count(*) FROM pauses").await,
        pauses,
        "no pause"
    );
    assert_eq!(
        directed_alerts::token_upgrades_since(pool, ORIGIN, TOKEN, 0)
            .await
            .unwrap(),
        vec![id],
        "what a hold would read, were one ruled"
    );
    final_directed(&w.admin, 40, 5_000_000, DESTINATION_ACCOUNT).await;
    w.settle(t0()).await;
    let (d, kind) = bound(&w, 40).await;
    assert_eq!(kind, "deposit");
    assert_eq!(w.state(d).await, ("Source-Final".into(), None), "not held");
    assert!(order(&w.admin, d, 1).await.is_some(), "its mint is ordered");
}
