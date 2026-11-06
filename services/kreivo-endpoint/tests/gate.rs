// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Issue #104: submissions are pinned to a transaction version and to the runtime's own metadata,
//! not to a spec version, and to a lane that holds the contract's delivery role. A closed gate
//! builds, records and signs nothing; an open one builds with the live runtime's versions.

mod common;

use bezent_kreivo_endpoint::chain::{ChainError, RuntimeVersion};
use bezent_kreivo_endpoint::kreivo::{self, Roles};
use bezent_kreivo_endpoint::submit::{GateState, Prepared, Submitted};
use common::*;
use frame_metadata::v15::RuntimeMetadataV15;
use frame_metadata::{RuntimeMetadata, RuntimeMetadataPrefixed};
use parity_scale_codec::{Decode, Encode};
use scale_info::interner::UntrackedSymbol;
use scale_info::{TypeDef, form::PortableForm};
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

/// The gate's state at the finalized head, after a block, so that nothing cached answers.
async fn judged(
    f: &Fixture,
    s: &bezent_kreivo_endpoint::submit::Submitter<'_, impl bezent_kreivo_endpoint::chain::Chain>,
) -> GateState {
    f.chain.advance(1);
    s.gate(&f.chain.finalized_ref()).await
}

fn closed_why(state: GateState) -> String {
    match state {
        GateState::Closed(why) => why,
        other => panic!("not closed: {other:?}"),
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_104_a_chain_that_does_not_answer_leaves_the_gate_unavailable_and_asks_again(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let s = f.submitter();
    f.chain.set_unavailable(true);
    let state = judged(&f, &s).await;
    assert!(matches!(state, GateState::Unavailable(_)), "{state:?}");
    // Not remembered: the next ask reads again, and the gate opens when the chain answers.
    f.chain.set_unavailable(false);
    let head = f.chain.finalized_ref();
    assert_eq!(s.gate(&head).await, GateState::Open(version(134, 13)));

    // A metadata read that does not answer, with everything else well, is the same.
    f.chain
        .set_metadata_error(Some(ChainError::Unavailable("no peer".into())));
    let state = judged(&f, &s).await;
    assert!(
        matches!(&state, GateState::Unavailable(why) if why.contains("the runtime's metadata") && why.contains("no peer")),
        "{state:?}"
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_104_a_chain_that_answers_what_cannot_be_used_closes_the_gate_with_the_reason(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let s = f.submitter();

    // A runtime with no V15 metadata.
    f.chain.set_metadata_error(Some(ChainError::Invalid(
        "the runtime has no metadata V15".into(),
    )));
    let why = closed_why(judged(&f, &s).await);
    assert!(
        why.contains("the runtime's metadata cannot be used") && why.contains("no metadata V15"),
        "{why}"
    );
    f.chain.set_metadata_error(None);

    // A contract whose roles() does not dispatch, or whose answer is not the roles.
    f.chain.clear_roles();
    let why = closed_why(judged(&f, &s).await);
    assert!(why.contains("roles() cannot be read"), "{why}");
    f.chain.set_roles(Roles {
        delivery: f.lane.account,
        pause: [0xee; 32],
        governance: [0xef; 32],
    });
    assert!(matches!(judged(&f, &s).await, GateState::Open(_)));

    // The orders wait, as for any closed gate, and nothing is recorded.
    f.chain.clear_roles();
    let order = f.order_mint([7; 32], TO, 5).await;
    f.chain.advance(1);
    let prepared = s.prepare(order).await.unwrap();
    assert!(
        matches!(&prepared, Prepared::Refused(r) if r.contains("roles() cannot be read")),
        "{prepared:?}"
    );
    assert_eq!(prepared_rows(&f).await, 0);
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_104_the_format_is_checked_live_while_the_role_is_wrong_and_both_reasons_are_given(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let s = f.submitter();
    f.chain.set_roles(Roles {
        delivery: [0xc3; 32],
        pause: [0xee; 32],
        governance: [0xef; 32],
    });
    // The role alone.
    let why = closed_why(judged(&f, &s).await);
    assert!(
        why.contains("delivery role") && !why.contains("transaction format"),
        "{why}"
    );
    // The role is still wrong; the format breaks as well, and is reported with it.
    f.chain.set_version(version(137, 14));
    f.chain.set_metadata(moved_contracts());
    let why = closed_why(judged(&f, &s).await);
    assert!(
        why.contains("delivery role") && why.contains("call.Contracts.call"),
        "{why}"
    );
    // And an unknown transaction version, a third reason.
    f.chain.set_version(version(138, 15));
    let why = closed_why(judged(&f, &s).await);
    assert!(
        why.contains("delivery role")
            && why.contains("call.Contracts.call")
            && why.contains("transaction_version 15"),
        "{why}"
    );
    // A closed condition outweighs one the chain did not answer.
    f.chain
        .set_metadata_error(Some(ChainError::Unavailable("no peer".into())));
    let why = closed_why(judged(&f, &s).await);
    assert!(why.contains("delivery role"), "{why}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn issue_104_each_change_to_what_the_endpoint_reads_or_writes_closes_the_gate(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let s = f.submitter();
    f.chain.set_version(version(136, 14));
    for (what, metadata) in changed_metadata() {
        f.chain.set_metadata(metadata);
        let why = closed_why(judged(&f, &s).await);
        assert!(why.contains(what), "{what}: {why}");
        let order = f.order_mint([8; 32], TO, 5).await;
        let prepared = s.prepare(order).await.unwrap();
        assert!(
            matches!(prepared, Prepared::Refused(_)),
            "{what}: {prepared:?}"
        );
        assert_eq!(prepared_rows(&f).await, 0, "{what}");
    }
    // The unchanged metadata opens it again.
    f.chain.set_metadata(metadata(136));
    assert!(matches!(judged(&f, &s).await, GateState::Open(_)));
}

/// Spec 136's metadata with one thing the endpoint reads or writes changed, and the item that
/// names it. The unit tests of `format` change each item; these are the ones that reach the gate
/// through the runtime's own answer.
fn changed_metadata() -> Vec<(&'static str, Vec<u8>)> {
    let decoded = || {
        let m = RuntimeMetadataPrefixed::decode(&mut &metadata(136)[..]).unwrap();
        let RuntimeMetadata::V15(v15) = m.1 else {
            panic!("not V15")
        };
        v15
    };
    let encode = |v15| {
        RuntimeMetadataPrefixed(frame_metadata::META_RESERVED, RuntimeMetadata::V15(v15)).encode()
    };
    let mut out = Vec::new();

    // The fee extension is no longer the fee extension: another one's data in its place.
    let mut m = decoded();
    let nonce = m.extrinsic.signed_extensions[6].ty;
    let fee = m
        .extrinsic
        .signed_extensions
        .iter_mut()
        .find(|e| e.identifier == "ChargeAssetTxPayment")
        .unwrap();
    fee.ty = nonce;
    out.push(("extension.ChargeAssetTxPayment (explicit)", encode(m)));

    // PassAuthenticate carries what CheckNonce does.
    let mut m = decoded();
    m.extrinsic.signed_extensions[0].ty = m.extrinsic.signed_extensions[6].ty;
    out.push(("extension.PassAuthenticate (explicit)", encode(m)));

    // The era is the nonce's type.
    let mut m = decoded();
    m.extrinsic.signed_extensions[5].ty = m.extrinsic.signed_extensions[6].ty;
    out.push(("extension.CheckMortality (explicit)", encode(m)));

    // The dry run's result is another type: the fields the endpoint reads by position are gone.
    let mut m = decoded();
    let other = m.extrinsic.signed_extensions[6].ty;
    m.apis
        .iter_mut()
        .find(|a| a.name == "ContractsApi")
        .unwrap()
        .methods
        .iter_mut()
        .find(|c| c.name == "call")
        .unwrap()
        .output = other;
    out.push(("api.ContractsApi.call", encode(m)));

    // The extrinsic format is another version.
    let mut m = decoded();
    m.extrinsic.version = 5;
    out.push(("extrinsic.version", encode(m)));

    // Inside the dry run's result, read by position: a Weight whose `ref_time` is another type,
    // `StorageDeposit`'s arms exchanged, and the first two fields exchanged.
    let mut m = decoded();
    let weight = result_field(&m, "gas_consumed");
    fields_of(&mut m, weight)[0].ty = UntrackedSymbol::from(m.extrinsic.address_ty.id);
    out.push(("api.ContractsApi.call", encode(m)));

    let mut m = decoded();
    let deposit = result_field(&m, "storage_deposit");
    for v in variants_of(&mut m, deposit) {
        v.index = 1 - v.index;
    }
    out.push(("api.ContractsApi.call", encode(m)));

    let mut m = decoded();
    let result = dry_run_result(&m);
    fields_of(&mut m, result).swap(0, 1);
    out.push(("api.ContractsApi.call", encode(m)));

    // An Option whose arms move: the dry run's `storage_deposit_limit`.
    let mut m = decoded();
    let option = {
        let api = m.apis.iter().find(|a| a.name == "ContractsApi").unwrap();
        let call = api.methods.iter().find(|c| c.name == "call").unwrap();
        call.inputs
            .iter()
            .find(|p| p.name == "storage_deposit_limit")
            .unwrap()
            .ty
            .id
    };
    for v in variants_of(&mut m, option) {
        v.index = 1 - v.index;
    }
    out.push(("api.ContractsApi.call", encode(m)));
    out
}

fn type_def_of(m: &mut RuntimeMetadataV15, id: u32) -> &mut TypeDef<PortableForm> {
    &mut m
        .types
        .types
        .iter_mut()
        .find(|t| t.id == id)
        .unwrap()
        .ty
        .type_def
}

fn variants_of(m: &mut RuntimeMetadataV15, id: u32) -> &mut Vec<scale_info::Variant<PortableForm>> {
    let TypeDef::Variant(v) = type_def_of(m, id) else {
        panic!("not an enum")
    };
    &mut v.variants
}

fn fields_of(m: &mut RuntimeMetadataV15, id: u32) -> &mut Vec<scale_info::Field<PortableForm>> {
    let TypeDef::Composite(c) = type_def_of(m, id) else {
        panic!("not a struct")
    };
    &mut c.fields
}

/// The type of `ContractsApi_call`'s result.
fn dry_run_result(m: &RuntimeMetadataV15) -> u32 {
    m.apis
        .iter()
        .find(|a| a.name == "ContractsApi")
        .unwrap()
        .methods
        .iter()
        .find(|c| c.name == "call")
        .unwrap()
        .output
        .id
}

/// The type of one field of the dry run's result.
fn result_field(m: &RuntimeMetadataV15, name: &str) -> u32 {
    let result = dry_run_result(m);
    let TypeDef::Composite(c) = &m.types.resolve(result).unwrap().type_def else {
        panic!("not a struct")
    };
    c.fields
        .iter()
        .find(|f| f.name.as_deref() == Some(name))
        .unwrap()
        .ty
        .id
}
