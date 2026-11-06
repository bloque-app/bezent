// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #104: submissions are pinned to a transaction version and to the runtime's own metadata,
//! not to a spec version, and to a lane that holds the contract's delivery role. A closed gate
//! builds, records and signs nothing; an open one builds with the live runtime's versions.

mod common;

use bezent_kreivo_endpoint::chain::RuntimeVersion;
use bezent_kreivo_endpoint::kreivo::{self, Roles};
use bezent_kreivo_endpoint::submit::{GateState, Prepared, Submitted};
use common::*;
use frame_metadata::{RuntimeMetadata, RuntimeMetadataPrefixed};
use parity_scale_codec::{Decode, Encode};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

const TO: [u8; 32] = [0x77; 32];

fn version(spec_version: u32, transaction_version: u32) -> RuntimeVersion {
    RuntimeVersion {
        spec_version,
        transaction_version,
    }
}

async fn prepared_rows(f: &Fixture) -> i64 {
    f.count("SELECT count(*) FROM prepared_transactions").await
}

/// Spec 136's metadata with `Contracts` moved to pallet index 81.
fn moved_contracts() -> Vec<u8> {
    let mut m = RuntimeMetadataPrefixed::decode(&mut &metadata(136)[..]).unwrap();
    let RuntimeMetadata::V15(v15) = &mut m.1 else {
        panic!("not V15")
    };
    v15.pallets
        .iter_mut()
        .find(|p| p.name == "Contracts")
        .unwrap()
        .index = 81;
    m.encode()
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_104_spec_136_is_built_and_signed_with_its_own_versions(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    f.chain.set_version(version(136, 14));
    f.chain.set_metadata(metadata(136));
    let s = f.submitter();
    assert_eq!(
        s.gate(&f.chain.finalized_ref()).await,
        GateState::Open(version(136, 14))
    );
    let order = f.order_mint([1; 32], TO, 1_000_000).await;
    let prepared = s.prepare(order).await.unwrap();
    assert!(matches!(prepared, Prepared::Ready { .. }), "{prepared:?}");
    let payload: Vec<u8> = sqlx::query_scalar("SELECT signing_payload FROM prepared_transactions")
        .fetch_one(&f.admin)
        .await
        .unwrap();
    let d = kreivo::decode_signing_payload(&payload).unwrap();
    assert_eq!((d.spec_version, d.transaction_version), (136, 14));
    let signing = f
        .signing_intent("mint", [1; 32], &prepared, 1_000_000)
        .await;
    let out = s.sign_and_submit(signing).await.unwrap();
    assert!(matches!(out, Submitted::Submitted { .. }), "{out:?}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_104_a_later_spec_with_a_known_transaction_version_and_format_is_built(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // The endpoint does not stop at every upgrade: a spec it has never seen builds when its
    // transaction version is known and its metadata matches.
    let f = Fixture::new(pool, connect).await;
    f.chain.set_version(version(140, 14));
    f.chain.set_metadata(metadata(136));
    let order = f.order_mint([2; 32], TO, 5).await;
    let prepared = f.submitter().prepare(order).await.unwrap();
    assert!(matches!(prepared, Prepared::Ready { .. }), "{prepared:?}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_104_an_unknown_transaction_version_builds_and_records_nothing(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    // Its metadata matches: the transaction version alone closes the gate.
    f.chain.set_version(version(137, 15));
    f.chain.set_metadata(metadata(136));
    let s = f.submitter();
    let order = f.order_mint([3; 32], TO, 5).await;
    let prepared = s.prepare(order).await.unwrap();
    assert!(
        matches!(&prepared, Prepared::Refused(r) if r.contains("transaction_version 15")),
        "{prepared:?}"
    );
    assert_eq!(prepared_rows(&f).await, 0);
    // The order is still there to prepare, and is prepared once the runtime is one it knows.
    assert_eq!(
        s.orders_to_prepare(&f.chain.finalized_ref()).await.unwrap(),
        vec![order]
    );
    f.chain.set_version(version(137, 14));
    f.chain.advance(1);
    let prepared = s.prepare(order).await.unwrap();
    assert!(matches!(prepared, Prepared::Ready { .. }), "{prepared:?}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_104_metadata_that_does_not_match_builds_and_records_nothing(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // Transaction version 14, as known, but Contracts moved: the convention is not trusted.
    let f = Fixture::new(pool, connect).await;
    f.chain.set_version(version(137, 14));
    f.chain.set_metadata(moved_contracts());
    let s = f.submitter();
    let order = f.order_mint([4; 32], TO, 5).await;
    let prepared = s.prepare(order).await.unwrap();
    assert!(
        matches!(&prepared, Prepared::Refused(r) if r.contains("call.Contracts.call") && r.contains("pallet 81")),
        "{prepared:?}"
    );
    assert_eq!(prepared_rows(&f).await, 0);

    // Metadata the chain cannot give is not a match either: nothing is built, and it is asked again.
    f.chain.set_metadata(b"not metadata".to_vec());
    f.chain.advance(1);
    let prepared = s.prepare(order).await.unwrap();
    assert!(
        matches!(&prepared, Prepared::Refused(r) if r.contains("does not decode")),
        "{prepared:?}"
    );
    assert_eq!(prepared_rows(&f).await, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_104_a_lane_without_the_delivery_role_spends_no_dry_run(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // The contract's delivery role is another account: a dry run would be refused as
    // Unauthorised and the order never prepared again, so none is made.
    let f = Fixture::new(pool, connect).await;
    let other = Roles {
        delivery: [0xc3; 32],
        pause: [0xee; 32],
        governance: [0xef; 32],
    };
    f.chain.set_roles(other);
    let s = f.submitter();
    let order = f.order_mint([5; 32], TO, 5).await;
    let prepared = s.prepare(order).await.unwrap();
    assert!(
        matches!(&prepared, Prepared::Refused(r) if r.contains("delivery role")),
        "{prepared:?}"
    );
    assert_eq!(prepared_rows(&f).await, 0);

    // Governance hands the role to the lane: the order is prepared.
    f.chain.set_roles(Roles {
        delivery: f.lane.account,
        ..other
    });
    f.chain.advance(1);
    let prepared = s.prepare(order).await.unwrap();
    assert!(matches!(prepared, Prepared::Ready { .. }), "{prepared:?}");

    // The role moves away again before the signature: nothing is signed.
    let signing = f.signing_intent("mint", [5; 32], &prepared, 5).await;
    f.chain.set_roles(other);
    f.chain.advance(1);
    let out = s.sign_and_submit(signing).await.unwrap();
    assert!(
        matches!(&out, Submitted::Refused(r) if r.contains("delivery role")),
        "{out:?}"
    );
    assert_eq!(f.signatures().await, 0);
    assert!(f.chain.submitted().is_empty());
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_104_an_upgrade_between_preparation_and_signature_signs_nothing(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    // Prepared at spec 134 (transaction version 13); the chain moves to spec 136 before the
    // signature: the payload is stale, not an anomaly, and nothing is signed.
    let f = Fixture::new(pool, connect).await;
    let s = f.submitter();
    let order = f.order_mint([6; 32], TO, 5).await;
    let prepared = s.prepare(order).await.unwrap();
    assert!(matches!(prepared, Prepared::Ready { .. }), "{prepared:?}");
    let signing = f.signing_intent("mint", [6; 32], &prepared, 5).await;
    f.chain.set_version(version(136, 14));
    f.chain.set_metadata(metadata(136));
    f.chain.advance(1);
    let out = s.sign_and_submit(signing).await.unwrap();
    assert!(
        matches!(&out, Submitted::Refused(r) if r.contains("another runtime")),
        "{out:?}"
    );
    assert_eq!(f.signatures().await, 0);
    assert_eq!(
        f.count("SELECT count(*) FROM event_log WHERE kind = 'delivery-anomaly'")
            .await,
        0
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_104_the_gate_is_judged_again_when_the_runtime_changes(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let s = f.submitter();
    let head = f.chain.finalized_ref();
    s.watch(&head).await;
    assert_eq!(s.gate(&head).await, GateState::Open(version(134, 13)));
    // An upgrade to a format the endpoint does not write: closed at the next block.
    f.chain.set_version(version(137, 14));
    f.chain.set_metadata(moved_contracts());
    f.chain.advance(1);
    let head = f.chain.finalized_ref();
    s.watch(&head).await;
    assert!(matches!(s.gate(&head).await, GateState::Closed(_)));
}
