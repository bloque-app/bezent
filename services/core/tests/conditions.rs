// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-003 T-003-05: the named conditions of SPEC §10, one test each. A condition is held in the
//! transfer's state and is never a failure; it clears by itself or by evidence; past its deadline
//! it sends a transfer to review only where the §5.4 table allows (`Source-Final` with
//! `CND-AwaitingConversion` or `CND-DestinationRefused`; `Delivering` and `Delivered`); the one
//! addition, an attempt whose preparation never passes and names no condition, is
//! `unsigned_attempts.rs` (issue #25).
//! `CND-Unassociated` is `ac_a4_1_…` and `CND-AmountMismatch` is `req_tr_5_…` in `lifecycle.rs`.

mod common;

use bezent_signer::{Outcome, Refusal};
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

/// `AC-I2.1`, `REQ-GV-1`: with a pause in place new work waits (`CND-Paused`, no intent), and resumes when it lifts.
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

/// `AC-I2.1`, `INV-15`, `REQ-GV-1` (amendment 0022, A1b, ruled `d-pause-0022`, 2026-11-06): an attempt
/// is in flight from its first recorded signature, and everything before it is new work. A signing
/// intent the core recorded a moment before a pause, with no signature yet, is held by the pause:
/// the signer refuses it (custody, `INV-9`'s layer), nothing is signed, the core holds the transfer
/// in `Source-Final` as `CND-Paused` and records nothing new. Once the pause is lifted the same
/// intent is signed and the transfer moves to `Delivering`.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_inv_15_a1b_an_intent_recorded_before_the_pause_is_not_signed_until_it_is_lifted(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let id = source_final_deposit(&w).await;
    w.cycle(t0()).await;
    let (o, _) = order(&w.admin, id, 1).await.expect("the mint order");
    prepare(&w.admin, o, None).await;
    w.cycle(t0()).await;
    let (s, _) = signing_intent(&w.admin, id, 1)
        .await
        .expect("the signing intent, recorded before the pause");
    let intents = w.count("SELECT count(*) FROM intents").await;

    exec(&w.admin, "INSERT INTO pauses (asset) VALUES (NULL)").await;
    let refused = w.signer.sign(s).await;
    assert!(
        matches!(refused, Outcome::Refused(Refusal::Paused)),
        "signed new work under a pause: {refused:?}"
    );
    assert_eq!(w.count("SELECT count(*) FROM signatures").await, 0);
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-Paused".into()))
    );
    assert_eq!(
        w.count("SELECT count(*) FROM intents").await,
        intents,
        "the core recorded new work under the pause"
    );
    // Asked again under the pause, by an endpoint that keeps the intent: still held.
    assert!(matches!(
        w.signer.sign(s).await,
        Outcome::Refused(Refusal::Paused)
    ));

    exec(&w.admin, "DELETE FROM pauses").await;
    sign(&w, s).await;
    w.settle(t0()).await;
    assert_eq!(w.state(id).await, ("Delivering".into(), None));
}

/// A key backend that commits a pause while it signs: the moment between the signer's first read
/// of the pause and the record of the signature.
struct PausesWhileSigning {
    inner: bezent_signer::keys::FileBackend,
    admin: sqlx::PgPool,
}

impl bezent_signer::engine::SigningEngine for PausesWhileSigning {
    fn kind(&self) -> bezent_signer::engine::EngineKind {
        self.inner.kind()
    }
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn environment(&self) -> bezent_signer::keys::Environment {
        self.inner.environment()
    }
    fn key_ids(&self) -> Vec<String> {
        self.inner.key_ids()
    }
    fn scheme(
        &self,
        id: &str,
    ) -> Result<bezent_signer::keys::Scheme, bezent_signer::keys::KeyError> {
        self.inner.scheme(id)
    }
    fn public_key(&self, id: &str) -> Result<Vec<u8>, bezent_signer::keys::KeyError> {
        self.inner.public_key(id)
    }
    async fn sign_prepared(
        &self,
        id: &str,
        scheme: bezent_signer::keys::Scheme,
        input: bezent_signer::engine::Signable<'_>,
    ) -> Result<Vec<u8>, bezent_signer::keys::KeyError> {
        sqlx::query("INSERT INTO pauses (asset) VALUES (NULL)")
            .execute(&self.admin)
            .await
            .expect("the pause commits while the key signs");
        self.inner.sign_prepared(id, scheme, input).await
    }
}

/// `AC-I2.1`, `INV-15`, `REQ-GV-1`, `NFR-8` (amendment 0022, A1b): a pause committed after the signer
/// first read the pause and before it records the signature holds that signature. The key backend
/// signs, the signer reads the pause again in its transaction before the INSERT, refuses, and nothing
/// is recorded; once the pause is lifted the same intent is signed.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_inv_15_a1b_a_pause_committed_while_the_key_signs_holds_the_signature(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    let id = source_final_deposit(&w).await;
    w.cycle(t0()).await;
    let (o, _) = order(&w.admin, id, 1).await.expect("the mint order");
    prepare(&w.admin, o, None).await;
    w.cycle(t0()).await;
    let (s, _) = signing_intent(&w.admin, id, 1)
        .await
        .expect("the signing intent");

    let admin = w.admin.clone();
    let racing = w
        .signer_over(|inner| PausesWhileSigning { inner, admin })
        .await;
    let refused = racing.sign(s).await;
    assert!(
        matches!(refused, Outcome::Refused(Refusal::Paused)),
        "recorded a signature under a pause that committed while the key signed: {refused:?}"
    );
    assert_eq!(w.count("SELECT count(*) FROM signatures").await, 0);

    exec(&w.admin, "DELETE FROM pauses").await;
    sign(&w, s).await;
    assert_eq!(w.count("SELECT count(*) FROM signatures").await, 1);
}

/// `AC-I2.1`, `REQ-GV-1`: a global pause stops new work only; a transfer in flight is still observed and completed.
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
    // REQ-CV-1, REQ-CV-6. The conversion itself is the requester's (F-010); the hold is here. The
    // deposit names a convertible arrival (amendment 0018, A1b), and the core puts the condition
    // there itself, on the edge into Source-Final: no transition is written by hand.
    let w = World::new(options, connect).await;
    let a = observe(
        &w.admin,
        ORIGIN,
        10,
        0,
        "0xc2132D05D31c914a87C6611C10748AEb04B58e8F",
        5_000,
        Some("0xfunder"),
        Some(RESERVE),
        None,
        true,
        json!({"event": "ConvertibleArrival"}),
    )
    .await;
    decide(&w.admin, a, "final", 12).await;
    let id = deposit(&w.admin, 10, 5_000).await;
    w.cycle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Source-Final".into(), Some("CND-AwaitingConversion".into()))
    );
    head(&w.admin, ISSUING, 100).await;
    w.settle(t0()).await;
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
    head(&w.admin, ISSUING, 12 + BUDGET + 1).await;
    w.settle(t0()).await;
    assert_eq!(
        w.state(id).await,
        ("Needs-Review".into(), Some("CND-AwaitingConversion".into()))
    );
    // REQ-RF-1: review, never a refund on a timer.
    assert_eq!(
        w.count("SELECT count(*) FROM transfers WHERE kind = 'refund'")
            .await,
        0
    );
    assert_eq!(w.count("SELECT count(*) FROM intents").await, 0);
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

// ---------------------------------------------------------------------------------------------
// AC-I2.1, INV-15 (service): a pause per route, per asset, per direction or global stops the new work it
// covers and nothing else. Each case has one deposit and one withdrawal, both final and ready to be ordered,
// and a pause in place before the core first looks at them.
// ---------------------------------------------------------------------------------------------

/// Whether the deposit and the withdrawal are held (`CND-Paused`, no order) or let through (an order), under
/// the `pauses` row `pause` inserts.
async fn pause_case(
    options: PgPoolOptions,
    connect: PgConnectOptions,
    pause: &str,
    deposit_held: bool,
    withdrawal_held: bool,
) {
    pause_case_with(options, connect, "", pause, deposit_held, withdrawal_held).await;
}

/// [`pause_case`] with `extra` run first: another asset and another network the pause can name.
async fn pause_case_with(
    options: PgPoolOptions,
    connect: PgConnectOptions,
    extra: &str,
    pause: &str,
    deposit_held: bool,
    withdrawal_held: bool,
) {
    let w = World::new(options, connect).await;
    if !extra.is_empty() {
        exec(&w.admin, extra).await;
    }
    exec(&w.admin, &format!("INSERT INTO pauses {pause}")).await;
    reserve(&w.admin, 1_000_000).await;
    let deposit = source_final_deposit(&w).await;
    let r = redemption(&w.admin, 20, &[7; 32], 700).await;
    decide(&w.admin, r, "final", 22).await;
    w.settle(t0()).await;
    let withdrawal: i64 = scalar(
        &w.admin,
        "SELECT id FROM transfers WHERE kind = 'withdrawal'",
    )
    .await;
    for (name, id, held) in [
        ("deposit", deposit, deposit_held),
        ("withdrawal", withdrawal, withdrawal_held),
    ] {
        if held {
            assert_eq!(
                w.state(id).await,
                ("Source-Final".into(), Some("CND-Paused".into())),
                "{name} under {pause}"
            );
            assert!(
                order(&w.admin, id, 1).await.is_none(),
                "{name} ordered under {pause}"
            );
        } else {
            assert!(
                order(&w.admin, id, 1).await.is_some(),
                "{name} held under {pause}"
            );
        }
    }
}

/// `AC-I2.1`, `INV-15`: a pause of the deposit route (asset, network, direction) holds deposits only.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_a_pause_of_the_deposit_route_holds_deposits_only(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let sql = format!("(asset, network, direction) VALUES ('{ASSET}', '{ORIGIN}', 'deposit')");
    pause_case(options, connect, &sql, true, false).await;
}

/// `AC-I2.1`, `INV-15`: a pause of the withdrawal route holds withdrawals only.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_a_pause_of_the_withdrawal_route_holds_withdrawals_only(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let sql = format!("(asset, network, direction) VALUES ('{ASSET}', '{ORIGIN}', 'withdrawal')");
    pause_case(options, connect, &sql, false, true).await;
}

/// `AC-I2.1`, `INV-15`: a pause of the deposit direction, on every asset and network, holds deposits only.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_a_pause_of_the_deposit_direction_holds_deposits_only(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    pause_case(
        options,
        connect,
        "(direction) VALUES ('deposit')",
        true,
        false,
    )
    .await;
}

/// `AC-I2.1`, `INV-15`: a pause of an asset holds its deposits and its withdrawals.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_a_pause_per_asset_holds_both_directions(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    pause_case(
        options,
        connect,
        &format!("(asset) VALUES ('{ASSET}')"),
        true,
        true,
    )
    .await;
}

/// `AC-I2.1`, `INV-15`: a pause of a network holds the routes through it, both directions.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_a_pause_per_network_holds_both_directions(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    pause_case(
        options,
        connect,
        &format!("(network) VALUES ('{ORIGIN}')"),
        true,
        true,
    )
    .await;
}

/// `AC-I2.1`, `INV-15`: a global pause holds every new transfer.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_a_global_pause_holds_both_directions(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    pause_case(options, connect, "(asset) VALUES (NULL)", true, true).await;
}

/// `AC-I2.1`, `INV-15`: a pause of the withdrawal direction leaves deposits alone.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_a_pause_of_the_withdrawal_direction_holds_withdrawals_only(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    pause_case(
        options,
        connect,
        "(direction) VALUES ('withdrawal')",
        false,
        true,
    )
    .await;
}

/// Another asset and another network, declared and not used by the transfers under test.
const ANOTHER: &str = "INSERT INTO networks (network, role, address_format, backend) VALUES \
       ('base-test', 'origin', 'evm', 'evm:84532'); \
     INSERT INTO assets (asset, precision, assurance_class, issuing_asset, surplus_floor, surplus_alert) \
       VALUES ('bOTHER', 6, 'public', 'Here(102)', 0, 1);";

/// `AC-I2.1`, `INV-15`, "and nothing else": a pause of another asset leaves this asset's deposit and
/// withdrawal alone.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_a_pause_of_another_asset_holds_nothing_of_this_one(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    pause_case_with(
        options,
        connect,
        ANOTHER,
        "(asset) VALUES ('bOTHER')",
        false,
        false,
    )
    .await;
}

/// `AC-I2.1`, `INV-15`, "and nothing else": a pause of another network, of that network's route of
/// this asset, and of another asset's route through this network, all in place together, leave these
/// transfers alone.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_a_pause_of_another_network_or_route_holds_nothing_of_this_one(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    pause_case_with(
        options,
        connect,
        ANOTHER,
        "(asset, network, direction) VALUES \
           (NULL, 'base-test', NULL), ('bUSDC', 'base-test', 'deposit'), \
           ('bOTHER', 'polygon-test', 'withdrawal')",
        false,
        false,
    )
    .await;
}

/// A deposit in flight, then `pause` (which covers its route) in place: it is still observed and
/// completed (`AC-I2.1`, `REQ-GV-1`, `INV-15`: a pause stops new work only).
async fn deposit_in_flight_under(options: PgPoolOptions, connect: PgConnectOptions, pause: &str) {
    let w = World::new(options, connect).await;
    let id = source_final_deposit(&w).await;
    deliver(&w, id, 1, t0()).await;
    assert_eq!(w.state(id).await.0, "Delivering");
    exec(&w.admin, &format!("INSERT INTO pauses {pause}")).await;
    // A deposit that is new meanwhile waits.
    let late = arrival(&w.admin, 11, 5_000).await;
    decide(&w.admin, late, "final", 13).await;
    deposit(&w.admin, 11, 5_000).await;
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
    assert_eq!(w.state(id).await, ("Completed".into(), None), "{pause}");
    let waiting: i64 = w
        .count("SELECT count(*) FROM transfers WHERE condition = 'CND-Paused'")
        .await;
    assert_eq!(waiting, 1, "the new deposit waits under {pause}");
}

/// `AC-I2.1`, `REQ-GV-1`, `INV-15`: a pause of the asset, ...
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_a_deposit_in_flight_completes_under_a_pause_of_its_asset(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    deposit_in_flight_under(options, connect, "(asset) VALUES ('bUSDC')").await;
}

/// ... of its network, ...
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_a_deposit_in_flight_completes_under_a_pause_of_its_network(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    deposit_in_flight_under(options, connect, "(network) VALUES ('polygon-test')").await;
}

/// ... of the deposit direction, ...
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_a_deposit_in_flight_completes_under_a_pause_of_the_deposit_direction(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    deposit_in_flight_under(options, connect, "(direction) VALUES ('deposit')").await;
}

/// ... and of its route.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_a_deposit_in_flight_completes_under_a_pause_of_its_route(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    deposit_in_flight_under(
        options,
        connect,
        "(asset, network, direction) VALUES ('bUSDC', 'polygon-test', 'deposit')",
    )
    .await;
}

/// `AC-I2.1`, `REQ-GV-1`, `INV-15`: under a global pause a withdrawal in flight is still observed and completed,
/// and a withdrawal whose payout is proven not executed is still refunded, the refund delivered and observed,
/// while the pause stays in place and a withdrawal that is new waits.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn ac_i2_1_a_global_pause_lets_a_withdrawal_complete_and_a_refund_finish(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let w = World::new(options, connect).await;
    reserve(&w.admin, 1_000_000).await;
    let mut ids = Vec::new();
    for (block, reference) in [(20, [1u8; 32]), (30, [2u8; 32])] {
        let r = redemption(&w.admin, block, &reference, 700).await;
        decide(&w.admin, r, "final", block + 2).await;
        w.cycle(t0()).await;
        let id: i64 = scalar(
            &w.admin,
            &format!(
                "SELECT id FROM transfers WHERE kind = 'withdrawal' AND on_chain_reference = '\\x{}'",
                hex::encode(reference)
            ),
        )
        .await;
        deliver(&w, id, 1, t0()).await;
        assert_eq!(w.state(id).await.0, "Delivering");
        ids.push((id, reference));
    }
    exec(&w.admin, "INSERT INTO pauses (asset) VALUES (NULL)").await;

    // The first is paid out and the payout is final: completed, under the pause.
    let (paid_id, _) = ids[0];
    let paid = observe(
        &w.admin,
        ORIGIN,
        50,
        3,
        TOKEN,
        700,
        None,
        Some(BENEFICIARY),
        Some(&payout_reference_of(&w.admin, paid_id).await),
        true,
        json!({"event": "Payout"}),
    )
    .await;
    decide(&w.admin, paid, "final", 60).await;

    // The second is proven not executed past its deadline: one refund, delivered and observed, under the pause.
    let (lost_id, reference) = ids[1];
    prove(&w.admin, lost_id, 1, ORIGIN, 32 + BUDGET + 100).await;
    w.settle(t0()).await;
    assert_eq!(w.state(paid_id).await, ("Completed".into(), None));
    assert_eq!(w.state(lost_id).await.0, "Refunding");
    let refund: i64 = scalar(
        &w.admin,
        &format!("SELECT id FROM transfers WHERE refund_of = {lost_id}"),
    )
    .await;
    deliver(&w, refund, 1, t0()).await;
    assert_eq!(w.state(refund).await.0, "Delivering");
    let refunded = observe(
        &w.admin,
        ISSUING,
        500,
        0,
        ISSUING_ASSET,
        700,
        None,
        Some(REDEEMER),
        Some(&reference),
        true,
        json!({"event": "RefundMinted"}),
    )
    .await;
    decide(&w.admin, refunded, "final", 505).await;
    w.settle(t0()).await;
    assert_eq!(w.state(refund).await, ("Completed".into(), None));
    assert_eq!(w.state(lost_id).await.0, "Refunded");

    // The pause is still there and a new withdrawal still waits behind it.
    assert_eq!(w.count("SELECT count(*) FROM pauses").await, 1);
    let r = redemption(&w.admin, 70, &[9; 32], 700).await;
    decide(&w.admin, r, "final", 72).await;
    w.settle(t0()).await;
    let fresh: i64 = scalar(
        &w.admin,
        &format!(
            "SELECT id FROM transfers WHERE kind = 'withdrawal' AND on_chain_reference = '\\x{}'",
            hex::encode([9u8; 32])
        ),
    )
    .await;
    assert_eq!(
        w.state(fresh).await,
        ("Source-Final".into(), Some("CND-Paused".into()))
    );
}
