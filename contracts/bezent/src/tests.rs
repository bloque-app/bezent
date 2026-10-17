// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Unit tests: every refusal of every message, and the acceptance criteria and invariants F-006
//! serves. They run in ink!'s off-chain environment with `crate::mock` standing in for Kreivo's
//! assets API. Each test is named after the `AC-*`, `INV-*` or `REQ-*` id it verifies.
//!
//! ink!'s off-chain engine does not revert a message that returns an error, so every refusal test
//! also checks that nothing moved: the contract checks everything before it calls Kreivo or writes.

use crate::bezent::{
    BackendsSet, Bezent, BoundsSet, DelayScheduled, DelaySet, GovernanceScheduled, Minted, Paused,
    RaiseCancelled, RaiseScheduled, Redeemed, RefundMinted, RolesSet, SupplyCounted, Unpaused,
};
use crate::kreivo::{
    AssetsCall, KreivoApiStatus, KreivoEnvironment as Env, MultiAddress, RuntimeCall, FN_BALANCE,
    FN_BURN, FN_DEPOSIT,
};
use crate::mock;
use crate::mock::CALL_RUNTIME;
use crate::types::*;
use ink::env::test;
use scale::{Decode, Encode};

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

const BUSDC: AssetId = AssetId::Here(101);
const BBTC: AssetId = AssetId::Here(103);
const UNKNOWN: AssetId = AssetId::Here(999);
const MIN_BALANCE: Balance = 1;
const DELAY: BlockNumber = 100;
const START: BlockNumber = 1;

const BOUNDS: Bounds = Bounds {
    supply_ceiling: 1_000_000,
    window_amount: 100_000,
    window_blocks: 10,
    per_transfer_max: 50_000,
    redeem_minimum: 100,
};

const EVM: Backend = Backend::Evm { chain_id: 137 };
const ATTESTED: Backend = Backend::Attested { id: 1 };
const ADDRESS: [u8; 20] = [0xAD; 20];
const COMMITMENT: [u8; 32] = [0xC0; 32];

/// An asset's configuration in the tests: `bounds`, and both backends.
fn config(bounds: Bounds) -> AssetConfig {
    AssetConfig {
        bounds,
        backends: vec![EVM, ATTESTED],
        initial_supply: 0,
    }
}

fn acc(byte: u8) -> AccountId {
    AccountId::from([byte; 32])
}
thread_local! {
    /// Which instance the test is on. ink!'s off-chain engine keeps storage per contract account
    /// and cannot be cleared, so each `setup` instantiates at a fresh account.
    static INSTANCE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// The contract's account.
fn me() -> AccountId {
    let mut bytes = [0xEE; 32];
    bytes[0] = INSTANCE.with(|i| i.get());
    AccountId::from(bytes)
}
fn delivery() -> AccountId {
    acc(1)
}
fn pauser() -> AccountId {
    acc(2)
}
fn governance() -> AccountId {
    acc(3)
}
fn holder() -> AccountId {
    acc(4)
}
fn stranger() -> AccountId {
    acc(5)
}
fn roles() -> Roles {
    Roles {
        delivery: delivery(),
        pause: pauser(),
        governance: governance(),
    }
}
fn r(byte: u8) -> Reference {
    [byte; 32]
}

/// Make `who` the caller of the contract.
fn caller(who: AccountId) {
    test::set_caller::<Env>(who);
    mock::enter_frame(who, me());
}

fn at_block(block: BlockNumber) {
    test::set_block_number::<Env>(block);
}

/// A fresh chain with bUSDC and bBTC administered by the contract's community, and the contract
/// instantiated with both.
fn setup() -> Bezent {
    setup_with(BOUNDS)
}

/// As [`setup`], with bUSDC under `bounds`.
fn setup_with(bounds: Bounds) -> Bezent {
    INSTANCE.with(|i| i.set(i.get() + 1));
    mock::reset();
    mock::create_asset(BUSDC, MIN_BALANCE);
    mock::create_asset(BBTC, MIN_BALANCE);
    test::set_callee::<Env>(me());
    at_block(START);
    caller(governance());
    Bezent::new(
        roles(),
        DELAY,
        vec![(BUSDC, config(bounds)), (BBTC, config(BOUNDS))],
    )
    .expect("instantiates")
}

fn mint(
    contract: &mut Bezent,
    to: AccountId,
    amount: Balance,
    reference: Reference,
) -> Result<(), Error> {
    caller(delivery());
    contract.mint(BUSDC, to, amount, reference)
}

/// `holder()` holds `amount` more than it redeems, so its asset account stays alive (`Preserve`).
fn funded(amount: Balance) -> Bezent {
    let mut contract = setup();
    mint(&mut contract, holder(), amount + 10, r(0x01)).expect("mint");
    contract
}

fn redeem_evm(contract: &mut Bezent, amount: Balance, reference: Reference) -> Result<(), Error> {
    caller(holder());
    contract.redeem(BUSDC, amount, EVM, ADDRESS.to_vec(), reference)
}

fn events() -> Vec<test::EmittedEvent> {
    test::recorded_events().collect()
}

fn last<E: Decode>() -> E {
    let event = events().pop().expect("an event");
    E::decode(&mut &event.data[..]).expect("event decodes")
}

/// What a refusal must leave as it was.
fn snapshot(contract: &Bezent) -> impl PartialEq + std::fmt::Debug {
    (
        events().len(),
        contract.event_seq(),
        [BUSDC, BBTC].map(|a| {
            (
                contract.supply(a),
                contract.minted_in_window(a),
                mock::issuance(a),
            )
        }),
        [holder(), stranger(), me()].map(|w| mock::balance(BUSDC, w)),
        mock::burns().len(),
    )
}

/// Asserts `f` is refused with `err` and changes nothing.
fn assert_refused(
    contract: &mut Bezent,
    f: impl FnOnce(&mut Bezent) -> Result<(), Error>,
    err: Error,
) {
    let before = snapshot(contract);
    assert_eq!(f(contract), Err(err));
    assert_eq!(snapshot(contract), before);
}

// ---------------------------------------------------------------------------------------------
// Pins against Kreivo's own types (T-006-01)
// ---------------------------------------------------------------------------------------------

#[test]
fn req_hn_1_asset_id_encodes_as_kreivos_fungible_asset_location() {
    for id in [0, 100, 101, 102, 103, u32::MAX] {
        let ours = AssetId::Here(id);
        let kreivos = virto_common::FungibleAssetLocation::Here(id);
        assert_eq!(ours.encode(), kreivos.encode());
        assert_eq!(AssetId::decode(&mut &kreivos.encode()[..]).unwrap(), ours);
    }
}

#[test]
fn req_hn_5_status_codes_decode_as_kreivo_writes_them() {
    use kreivo_apis::apis::{AssetsApiError as K, KreivoApisError, KreivoApisErrorCode};
    let pairs = [
        (K::CannotDeposit, AssetsApiError::CannotDeposit),
        (K::CannotTransfer, AssetsApiError::CannotTransfer),
        (K::CannotMint, AssetsApiError::CannotMint),
        (K::CannotBurn, AssetsApiError::CannotBurn),
        (K::NotAGroupContract, AssetsApiError::NotAGroupContract),
        (K::NotAuthorised, AssetsApiError::NotAuthorised),
        (K::NotALocalAsset, AssetsApiError::NotALocalAsset),
    ];
    for (kreivos, ours) in pairs {
        let code: u32 = KreivoApisErrorCode::from(KreivoApisError::Assets(kreivos)).into();
        assert_eq!(
            KreivoApiError::from(KreivoApiStatus(code)),
            KreivoApiError::Assets(ours)
        );
    }
    let code: u32 = KreivoApisErrorCode::from(KreivoApisError::ExtQueryError).into();
    assert_eq!(
        KreivoApiError::from(KreivoApiStatus(code)),
        KreivoApiError::ExtQueryError
    );
    assert_eq!(
        KreivoApiError::from(KreivoApiStatus(0x0001_0007)),
        KreivoApiError::UnknownError
    );
}

// ---------------------------------------------------------------------------------------------
// Storage and roles (T-006-02)
// ---------------------------------------------------------------------------------------------

#[test]
fn req_hn_9_new_refuses_roles_not_distinct() {
    mock::reset();
    test::set_callee::<Env>(me());
    for roles in [
        Roles {
            delivery: acc(1),
            pause: acc(1),
            governance: acc(3),
        },
        Roles {
            delivery: acc(1),
            pause: acc(2),
            governance: acc(1),
        },
        Roles {
            delivery: acc(1),
            pause: acc(2),
            governance: acc(2),
        },
    ] {
        assert_eq!(
            Bezent::new(roles, DELAY, vec![]).err(),
            Some(Error::RolesNotDistinct)
        );
    }
}

#[test]
fn req_hn_9_set_roles_refuses_roles_not_distinct() {
    let mut contract = setup();
    caller(governance());
    let shared = Roles {
        delivery: acc(7),
        pause: acc(7),
        governance: governance(),
    };
    assert_eq!(contract.set_roles(shared), Err(Error::RolesNotDistinct));
    assert_eq!(contract.roles(), roles());
}

#[test]
fn req_hn_9_set_roles_is_governance_only_and_takes_effect() {
    let mut contract = setup();
    let new = Roles {
        delivery: acc(7),
        pause: acc(8),
        governance: governance(),
    };
    for who in [delivery(), pauser(), stranger()] {
        caller(who);
        assert_eq!(contract.set_roles(new), Err(Error::Unauthorised));
    }
    caller(governance());
    contract.set_roles(new).expect("governance sets roles");
    assert_eq!(last::<RolesSet>().roles, new);
    // The old delivery key has no power left.
    assert_eq!(
        mint(&mut contract, holder(), 10, r(0x10)),
        Err(Error::Unauthorised)
    );
    caller(acc(7));
    contract
        .mint(BUSDC, holder(), 10, r(0x10))
        .expect("the new delivery key mints");
}

/// The review's S3 scenario, at construction and in `set_roles`: the all-zero account or the
/// contract's own in any role would leave that role with no one to use it.
#[test]
fn req_hn_9_roles_refuse_the_zero_account_and_the_contracts_own() {
    mock::reset();
    test::set_callee::<Env>(me());
    let zero = AccountId::from([0u8; 32]);
    let unusable = |account: AccountId| {
        [
            Roles {
                delivery: account,
                ..roles()
            },
            Roles {
                pause: account,
                ..roles()
            },
            Roles {
                governance: account,
                ..roles()
            },
        ]
    };
    for roles in unusable(zero).into_iter().chain(unusable(me())) {
        assert_eq!(
            Bezent::new(roles, DELAY, vec![]).err(),
            Some(Error::InvalidRoleAccount)
        );
    }
    let mut contract = setup();
    caller(governance());
    for account in [zero, me()] {
        for roles in &unusable(account)[..2] {
            assert_eq!(contract.set_roles(*roles), Err(Error::InvalidRoleAccount));
        }
        assert_eq!(
            contract.schedule_governance(account),
            Err(Error::InvalidRoleAccount)
        );
    }
    assert_eq!(contract.roles(), roles());
}

/// The review's S3 scenario: one mistaken `set_roles` handed governance to an account that could
/// never act. Now `set_roles` cannot change governance; a change waits the raise delay and takes
/// effect only when the incoming account accepts it, and until then the current governance still
/// acts and can withdraw it (`REQ-GV-3`, `REQ-HN-9`).
#[test]
fn req_gv_3_a_governance_change_waits_the_delay_and_the_new_accounts_acceptance() {
    let mut contract = setup();
    let new = acc(9);
    caller(governance());
    assert_eq!(
        contract.set_roles(Roles {
            governance: new,
            ..roles()
        }),
        Err(Error::GovernanceChangeIsDelayed)
    );
    assert_eq!(
        contract.schedule_governance(delivery()),
        Err(Error::RolesNotDistinct)
    );
    for who in [delivery(), pauser(), new] {
        caller(who);
        assert_eq!(contract.schedule_governance(new), Err(Error::Unauthorised));
    }
    caller(governance());
    contract.schedule_governance(new).unwrap();
    assert_eq!(
        last::<GovernanceScheduled>(),
        GovernanceScheduled {
            seq: contract.event_seq() - 1,
            governance: new,
            due: START + DELAY
        }
    );
    // Before the delay, nobody can accept, and the current governance still governs.
    at_block(START + DELAY - 1);
    caller(new);
    assert_eq!(contract.accept_governance(), Err(Error::GovernanceNotDue));
    caller(governance());
    contract.pause(PauseScope::All).unwrap();
    contract.unpause(PauseScope::All).unwrap();
    // After it, only the incoming account can accept.
    at_block(START + DELAY);
    for who in [governance(), stranger()] {
        caller(who);
        assert_eq!(contract.accept_governance(), Err(Error::Unauthorised));
    }
    caller(new);
    contract
        .accept_governance()
        .expect("the new account accepts");
    assert_eq!(contract.roles().governance, new);
    assert_eq!(last::<RolesSet>().roles.governance, new);
    assert_eq!(contract.scheduled_governance(), None);
    caller(governance());
    assert_eq!(contract.unpause(PauseScope::All), Err(Error::Unauthorised));
    caller(new);
    contract
        .unpause(PauseScope::All)
        .expect("the new governance governs");
    assert_eq!(
        contract.accept_governance(),
        Err(Error::NoGovernanceScheduled)
    );
}

#[test]
fn req_gv_3_a_scheduled_governance_change_can_be_withdrawn() {
    let mut contract = setup();
    caller(governance());
    contract.schedule_governance(acc(9)).unwrap();
    contract.schedule_governance(governance()).unwrap();
    at_block(START + DELAY);
    caller(acc(9));
    assert_eq!(contract.accept_governance(), Err(Error::Unauthorised));
    caller(governance());
    contract.accept_governance().unwrap();
    assert_eq!(contract.roles(), roles());
}

#[test]
fn req_hn_5_new_refuses_invalid_bounds() {
    mock::reset();
    test::set_callee::<Env>(me());
    let zero_window = Bounds {
        window_blocks: 0,
        ..BOUNDS
    };
    let zero_minimum = Bounds {
        redeem_minimum: 0,
        ..BOUNDS
    };
    for bounds in [zero_window, zero_minimum] {
        assert_eq!(
            Bezent::new(roles(), DELAY, vec![(BUSDC, config(bounds))]).err(),
            Some(Error::InvalidBounds)
        );
    }
}

#[test]
fn req_hn_5_new_records_roles_delay_and_assets() {
    let contract = setup();
    assert_eq!(contract.roles(), roles());
    assert_eq!(contract.raise_delay(), (DELAY, None));
    assert_eq!(contract.bounds(BUSDC), Some(BOUNDS));
    assert_eq!(contract.bounds(BBTC), Some(BOUNDS));
    assert_eq!(contract.bounds(UNKNOWN), None);
    assert_eq!(contract.backends(BUSDC), vec![EVM, ATTESTED]);
    assert_eq!(contract.backends(UNKNOWN), vec![]);
    assert_eq!(contract.supply(BUSDC), 0);
    // RolesSet, then a BoundsSet, a BackendsSet and a SupplyCounted per asset.
    assert_eq!(contract.event_seq(), 7);
}

// ---------------------------------------------------------------------------------------------
// Mint (T-006-03)
// ---------------------------------------------------------------------------------------------

#[test]
fn inv_2_mint_mints_to_the_order_destination_with_its_reference() {
    let mut contract = setup();
    mint(&mut contract, holder(), 5_000, r(0x10)).expect("mint");
    assert_eq!(mock::balance(BUSDC, holder()), 5_000);
    assert_eq!(contract.supply(BUSDC), 5_000);
    assert_eq!(contract.minted_in_window(BUSDC), 5_000);
    assert!(contract.mint_reference_used(r(0x10)));
    let event = events().pop().unwrap();
    assert_eq!(event.topics[1], r(0x10).to_vec());
    assert_eq!(
        Minted::decode(&mut &event.data[..]).unwrap(),
        Minted {
            seq: contract.event_seq() - 1,
            reference: r(0x10),
            asset: BUSDC,
            to: holder(),
            amount: 5_000
        }
    );
}

#[test]
fn ac_d2_2_mint_refuses_a_reference_used_before() {
    let mut contract = setup();
    mint(&mut contract, holder(), 5_000, r(0x10)).expect("mint");
    assert_refused(
        &mut contract,
        |c| mint(c, stranger(), 5_000, r(0x10)),
        Error::ReferenceUsed,
    );
    // Also for another asset and another destination: a reference is used once, forever.
    caller(delivery());
    assert_eq!(
        contract.mint(BBTC, stranger(), 1, r(0x10)),
        Err(Error::ReferenceUsed)
    );
}

/// The review's M1 scenario: a stranger who computes a deposit's on-chain reference while the
/// arrival waits for finality redeems the minimum under it. The deposit's mint must still go
/// through: a redemption cannot consume or block a mint reference (`REQ-ID-4`, `INV-2`).
#[test]
fn req_id_4_a_strangers_redemption_cannot_block_a_deposits_mint() {
    let mut contract = setup();
    mock::fund(BUSDC, stranger(), 1_000);
    let deposit_reference = r(0x42);
    caller(stranger());
    contract
        .redeem(
            BUSDC,
            BOUNDS.redeem_minimum,
            EVM,
            ADDRESS.to_vec(),
            deposit_reference,
        )
        .expect("the stranger's own redemption");
    assert!(!contract.mint_reference_used(deposit_reference));
    mint(&mut contract, holder(), 5_000, deposit_reference).expect("the deposit's mint");
    assert_eq!(mock::balance(BUSDC, holder()), 5_000);
    assert!(contract.mint_reference_used(deposit_reference));
    // And the mint's reference stays used by mints, forever.
    assert_refused(
        &mut contract,
        |c| mint(c, holder(), 5_000, deposit_reference),
        Error::ReferenceUsed,
    );
}

#[test]
fn inv_3_mint_refuses_above_the_per_transfer_maximum() {
    let mut contract = setup();
    assert_refused(
        &mut contract,
        |c| mint(c, holder(), BOUNDS.per_transfer_max + 1, r(0x10)),
        Error::AboveTransferMaximum,
    );
    mint(&mut contract, holder(), BOUNDS.per_transfer_max, r(0x10)).expect("at the maximum");
}

#[test]
fn inv_3_mint_refuses_above_the_mint_window() {
    let mut contract = setup();
    mint(&mut contract, holder(), 50_000, r(0x10)).unwrap();
    mint(&mut contract, holder(), 50_000, r(0x11)).unwrap();
    assert_eq!(contract.minted_in_window(BUSDC), BOUNDS.window_amount);
    at_block(START + BOUNDS.window_blocks - 1);
    assert_refused(
        &mut contract,
        |c| mint(c, holder(), 1, r(0x12)),
        Error::AboveWindow,
    );
}

#[test]
fn inv_3_mint_window_starts_again_after_its_blocks() {
    let mut contract = setup();
    mint(&mut contract, holder(), 50_000, r(0x10)).unwrap();
    mint(&mut contract, holder(), 50_000, r(0x11)).unwrap();
    at_block(START + BOUNDS.window_blocks);
    assert_eq!(contract.minted_in_window(BUSDC), 0);
    mint(&mut contract, holder(), 50_000, r(0x12)).expect("a new window");
    assert_eq!(contract.minted_in_window(BUSDC), 50_000);
}

/// The review's M2 scenario: with a fixed window, the delivery key minted 199 999 in blocks 10
/// and 11 with a window of 100 000 per 10 blocks. The window slides now: block 11 has room only
/// for what blocks 2 to 10 did not use (`INV-3`).
#[test]
fn inv_3_no_two_blocks_mint_more_than_the_window_across_a_window_edge() {
    let mut contract = setup();
    at_block(10);
    mint(&mut contract, holder(), 50_000, r(0x10)).unwrap();
    mint(&mut contract, holder(), 49_999, r(0x11)).unwrap();
    at_block(11);
    assert_refused(
        &mut contract,
        |c| mint(c, holder(), 50_000, r(0x12)),
        Error::AboveWindow,
    );
    mint(&mut contract, holder(), 1, r(0x12)).expect("what is left of the window");
    assert_refused(
        &mut contract,
        |c| mint(c, holder(), 1, r(0x13)),
        Error::AboveWindow,
    );
    // Block 19 still sees block 10; block 20 no longer does.
    at_block(19);
    assert_eq!(contract.minted_in_window(BUSDC), 100_000);
    assert_refused(
        &mut contract,
        |c| mint(c, holder(), 1, r(0x13)),
        Error::AboveWindow,
    );
    at_block(20);
    assert_eq!(contract.minted_in_window(BUSDC), 1);
    mint(&mut contract, holder(), 50_000, r(0x13)).expect("block 10 left the window");
    mint(&mut contract, holder(), 49_999, r(0x14)).expect("up to the bound");
    assert_refused(
        &mut contract,
        |c| mint(c, holder(), 1, r(0x15)),
        Error::AboveWindow,
    );
}

/// A small deterministic generator (xorshift64*), so the property test needs no new dependency
/// and every failure can be replayed from its seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// `INV-3` over random mint sequences: for many window lengths and seeds, mints (and refund
/// mints, which go through the same window) at random blocks and amounts. Checks every span of
/// `window_blocks` blocks against the bound, by brute force over the mints let through. Also
/// checks the window is conservative only by a bucket: a refused mint would have exceeded the bound
/// over the window stretched by one bucket's length.
#[test]
fn inv_3_mints_in_any_window_never_exceed_the_bound_over_random_sequences() {
    const AMOUNT: Balance = 100_000;
    for window_blocks in [1, 2, 3, 7, 10, 11, 25, 64, 100] {
        for seed in 1..=4u64 {
            let bounds = Bounds {
                supply_ceiling: Balance::MAX,
                window_amount: AMOUNT,
                window_blocks,
                per_transfer_max: AMOUNT,
                redeem_minimum: 1,
            };
            let mut contract = setup_with(bounds);
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ u64::from(window_blocks));
            let mut block = START;
            let mut accepted: Vec<(BlockNumber, Balance)> = Vec::new();
            // Each refusal, with how many mints had been let through before it.
            let mut refused: Vec<(BlockNumber, Balance, usize)> = Vec::new();
            for n in 0..240u32 {
                // Several mints in a block, short gaps, and now and then a long one.
                block += match rng.below(10) {
                    0..=2 => 0,
                    3..=8 => rng.below(u64::from(window_blocks) / 2 + 2) as BlockNumber,
                    _ => rng.below(u64::from(window_blocks) * 2 + 1) as BlockNumber,
                };
                at_block(block);
                // Mostly small amounts, now and then up to the whole window.
                let most = if rng.below(4) == 0 {
                    AMOUNT as u64
                } else {
                    30_000
                };
                let amount = 1 + rng.below(most) as Balance;
                let mut reference = [0u8; 32];
                reference[..4].copy_from_slice(&n.to_le_bytes());
                match mint(&mut contract, holder(), amount, reference) {
                    Ok(()) => accepted.push((block, amount)),
                    Err(Error::AboveWindow) => refused.push((block, amount, accepted.len())),
                    Err(other) => panic!("unexpected refusal {other:?}"),
                }
            }
            let context = format!("window_blocks {window_blocks}, seed {seed}");
            assert!(!accepted.is_empty() && !refused.is_empty(), "{context}");

            let sum = |before: usize, from: BlockNumber, to: BlockNumber| -> Balance {
                accepted[..before]
                    .iter()
                    .filter(|(b, _)| (from..=to).contains(b))
                    .map(|(_, a)| a)
                    .sum()
            };
            // Every span of `window_blocks` blocks (the heaviest start at a mint).
            for &(first, _) in &accepted {
                let last = first + window_blocks - 1;
                assert!(
                    sum(accepted.len(), first, last) <= AMOUNT,
                    "{context}: span {first}..={last}"
                );
            }
            // Under-admission is bounded by one bucket.
            let stretch = window_blocks + bucket_blocks(window_blocks) - 1;
            for (at, amount, before) in refused {
                let first = (at + 1).saturating_sub(stretch);
                assert!(
                    sum(before, first, at) + amount > AMOUNT,
                    "{context}: refused {amount} at {at}"
                );
            }
        }
    }
}

/// Review 2's S5 scenario, with bounds of 100 000 per 10 blocks: block 10 mints 100 000, block 20
/// mints 1 (dropping block 10, rightly under a 10-block window), then the pause role lengthens the
/// window to 100 blocks. Block 21 let 50 000 and 49 999 through, so the span 10..=109 held 200 001
/// against a bound of 100 000. Now the asset mints nothing, refund mints included, until block 10
/// has left the new window: 90 blocks, the new length less the old (`INV-3`).
#[test]
fn inv_3_lengthening_the_window_stalls_mints_until_what_it_dropped_has_left_it() {
    let bounds = Bounds {
        per_transfer_max: 100_000,
        ..BOUNDS
    };
    let mut contract = setup_with(bounds);
    at_block(10);
    mint(&mut contract, holder(), 100_000, r(0x10)).unwrap();
    at_block(20);
    mint(&mut contract, holder(), 1, r(0x11)).unwrap();
    redeem_evm(&mut contract, 500, r(0x20)).unwrap();
    caller(pauser());
    contract
        .lower_bounds(
            BUSDC,
            Bounds {
                window_blocks: 100,
                ..bounds
            },
        )
        .unwrap();
    for block in [21, 109] {
        at_block(block);
        assert_eq!(contract.minted_in_window(BUSDC), Balance::MAX);
        assert_refused(
            &mut contract,
            |c| mint(c, holder(), 50_000, r(0x12)),
            Error::AboveWindow,
        );
        assert_refused(
            &mut contract,
            |c| mint(c, holder(), 49_999, r(0x13)),
            Error::AboveWindow,
        );
        assert_refused(
            &mut contract,
            |c| {
                caller(delivery());
                c.refund_mint(holder(), r(0x20))
            },
            Error::AboveWindow,
        );
    }
    // Block 110 is the first whose 100-block window leaves block 10 out: 20 + (100 - 10).
    at_block(110);
    assert_eq!(contract.minted_in_window(BUSDC), 1);
    caller(delivery());
    contract
        .refund_mint(holder(), r(0x20))
        .expect("block 10 left the window");
    mint(&mut contract, holder(), 99_499, r(0x12)).expect("up to the bound");
    assert_refused(
        &mut contract,
        |c| mint(c, holder(), 1, r(0x13)),
        Error::AboveWindow,
    );
}

/// `INV-3` over random mint sequences whose window changes length: lengthened at once by the
/// pause role (`lower_bounds`), and lengthened or shortened by a raise (`apply_raise`). After each
/// mint let through, the window in force then holds no more than the bound, counting every mint
/// let through so far, whatever window was in force when each was made.
#[test]
fn inv_3_mints_in_any_window_never_exceed_the_bound_when_the_window_changes_length() {
    const AMOUNT: Balance = 100_000;
    for first_window in [1, 3, 10, 25, 40] {
        for seed in 1..=4u64 {
            let mut bounds = Bounds {
                supply_ceiling: Balance::MAX,
                window_amount: AMOUNT,
                window_blocks: first_window,
                per_transfer_max: AMOUNT,
                redeem_minimum: 1,
            };
            let mut contract = setup_with(bounds);
            // A raise delay of one block, so the sequence can change the window through raises.
            caller(governance());
            contract.schedule_raise_delay(1).unwrap();
            let mut block = START + DELAY;
            at_block(block);
            contract.apply_raise_delay().unwrap();
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ u64::from(first_window));
            // Each mint let through: its block, its amount and the window in force.
            let mut accepted: Vec<(BlockNumber, Balance, BlockNumber)> = Vec::new();
            let (mut refused, mut lengthenings) = (0, 0);
            for n in 0..320u32 {
                let window_blocks = bounds.window_blocks;
                block += match rng.below(10) {
                    0..=2 => 0,
                    3..=8 => rng.below(u64::from(window_blocks) / 2 + 2) as BlockNumber,
                    _ => rng.below(u64::from(window_blocks) * 2 + 1) as BlockNumber,
                };
                at_block(block);
                if rng.below(8) == 0 {
                    let longer =
                        window_blocks + 1 + rng.below(u64::from(window_blocks) * 2) as BlockNumber;
                    let shorter = 1 + rng.below(u64::from(window_blocks)) as BlockNumber;
                    match (rng.below(3), window_blocks >= 200) {
                        (0, false) => {
                            bounds.window_blocks = longer;
                            caller(pauser());
                            contract.lower_bounds(BUSDC, bounds).unwrap();
                        }
                        (kind, too_long) => {
                            bounds.window_blocks = if kind == 1 && !too_long {
                                longer
                            } else {
                                shorter
                            };
                            caller(governance());
                            contract.schedule_raise(BUSDC, config(bounds)).unwrap();
                            block += 1;
                            at_block(block);
                            contract.apply_raise(BUSDC).unwrap();
                        }
                    }
                    if bounds.window_blocks > window_blocks {
                        lengthenings += 1;
                    }
                }
                let most = if rng.below(4) == 0 {
                    AMOUNT as u64
                } else {
                    30_000
                };
                let amount = 1 + rng.below(most) as Balance;
                let mut reference = [0u8; 32];
                reference[..4].copy_from_slice(&n.to_le_bytes());
                match mint(&mut contract, holder(), amount, reference) {
                    Ok(()) => accepted.push((block, amount, bounds.window_blocks)),
                    Err(Error::AboveWindow) => refused += 1,
                    Err(other) => panic!("unexpected refusal {other:?}"),
                }
            }
            let context = format!("first window {first_window}, seed {seed}");
            assert!(
                !accepted.is_empty() && refused > 0 && lengthenings > 0,
                "{context}"
            );
            for (i, &(at, _, window_blocks)) in accepted.iter().enumerate() {
                let first = (at + 1).saturating_sub(window_blocks);
                let held: Balance = accepted[..=i]
                    .iter()
                    .filter(|(b, _, _)| (first..=at).contains(b))
                    .map(|(_, a, _)| a)
                    .sum();
                assert!(
                    held <= AMOUNT,
                    "{context}: {held} in {first}..={at} after mint {i}"
                );
            }
        }
    }
}

#[test]
fn inv_3_mint_refuses_above_the_supply_ceiling() {
    let mut contract = setup();
    caller(pauser());
    let low = Bounds {
        supply_ceiling: 60_000,
        ..BOUNDS
    };
    contract.lower_bounds(BUSDC, low).unwrap();
    mint(&mut contract, holder(), 50_000, r(0x10)).unwrap();
    assert_refused(
        &mut contract,
        |c| mint(c, holder(), 10_001, r(0x11)),
        Error::AboveCeiling,
    );
    mint(&mut contract, holder(), 10_000, r(0x11)).expect("up to the ceiling");
    assert_eq!(contract.supply(BUSDC), 60_000);
}

/// The review's S4 scenario: an asset that already has supply on chain when the community takes
/// it over. The counter starts from it, so the ceiling bounds all of the asset's supply (`INV-3`).
#[test]
fn inv_3_the_supply_counter_starts_from_the_supply_that_already_exists() {
    INSTANCE.with(|i| i.set(i.get() + 1));
    mock::reset();
    mock::create_asset(BUSDC, MIN_BALANCE);
    mock::fund(BUSDC, stranger(), 960_000);
    test::set_callee::<Env>(me());
    at_block(START);
    caller(governance());
    let existing = AssetConfig {
        initial_supply: mock::issuance(BUSDC),
        ..config(BOUNDS)
    };
    let mut contract = Bezent::new(roles(), DELAY, vec![(BUSDC, existing)]).unwrap();
    assert_eq!(contract.supply(BUSDC), 960_000);
    assert_eq!(last::<SupplyCounted>().supply, 960_000);
    mint(&mut contract, holder(), 40_000, r(0x10)).expect("up to the ceiling");
    assert_eq!(mock::issuance(BUSDC), BOUNDS.supply_ceiling);
    assert_refused(
        &mut contract,
        |c| mint(c, holder(), 1, r(0x11)),
        Error::AboveCeiling,
    );
    // Destroying what existed before frees room under the ceiling, as any redemption does.
    caller(stranger());
    contract
        .redeem(BUSDC, 1_000, EVM, ADDRESS.to_vec(), r(0x20))
        .unwrap();
    assert_eq!(contract.supply(BUSDC), mock::issuance(BUSDC));
    mint(&mut contract, holder(), 1_000, r(0x11)).expect("room again");
}

#[test]
fn inv_3_an_asset_added_by_a_delayed_raise_starts_from_its_initial_supply() {
    let mut contract = setup();
    let new = AssetId::Here(102);
    mock::create_asset(new, 1);
    mock::fund(new, stranger(), 999_990);
    caller(governance());
    let added = AssetConfig {
        initial_supply: 999_990,
        ..config(BOUNDS)
    };
    contract.schedule_raise(new, added).unwrap();
    at_block(START + DELAY);
    contract.apply_raise(new).unwrap();
    assert_eq!(contract.supply(new), 999_990);
    caller(delivery());
    assert_eq!(
        contract.mint(new, holder(), 11, r(0x10)),
        Err(Error::AboveCeiling)
    );
    contract
        .mint(new, holder(), 10, r(0x10))
        .expect("to the ceiling");
}

#[test]
fn req_gv_5_a_known_assets_supply_counter_is_not_set_by_governance() {
    let mut contract = setup();
    mint(&mut contract, holder(), 5_000, r(0x10)).unwrap();
    caller(governance());
    let reset = AssetConfig {
        initial_supply: 1,
        ..config(raised())
    };
    assert_eq!(
        contract.schedule_raise(BUSDC, reset),
        Err(Error::SupplyOfAKnownAsset)
    );
    contract.schedule_raise(BUSDC, config(raised())).unwrap();
    at_block(START + DELAY);
    contract.apply_raise(BUSDC).unwrap();
    assert_eq!(contract.supply(BUSDC), 5_000);
}

#[test]
fn ac_i2_1_mint_refuses_when_the_asset_is_paused() {
    let mut contract = setup();
    caller(pauser());
    contract.pause(PauseScope::Asset(BUSDC)).unwrap();
    assert_refused(
        &mut contract,
        |c| mint(c, holder(), 10, r(0x10)),
        Error::Paused,
    );
}

#[test]
fn ac_i2_1_mint_refuses_when_its_direction_is_paused() {
    let mut contract = setup();
    caller(pauser());
    contract
        .pause(PauseScope::AssetDirection(BUSDC, Direction::Mint))
        .unwrap();
    assert_refused(
        &mut contract,
        |c| mint(c, holder(), 10, r(0x10)),
        Error::Paused,
    );
}

#[test]
fn req_hn_5_mint_refuses_an_unknown_asset() {
    let mut contract = setup();
    assert_refused(
        &mut contract,
        |c| {
            caller(delivery());
            c.mint(UNKNOWN, holder(), 10, r(0x10))
        },
        Error::UnknownAsset,
    );
}

#[test]
fn req_hn_9_mint_refuses_any_caller_but_the_delivery_role() {
    let mut contract = setup();
    for who in [pauser(), governance(), holder(), stranger()] {
        assert_refused(
            &mut contract,
            |c| {
                caller(who);
                c.mint(BUSDC, holder(), 10, r(0x10))
            },
            Error::Unauthorised,
        );
    }
}

#[test]
fn req_hn_5_mint_refuses_zero() {
    let mut contract = setup();
    assert_refused(
        &mut contract,
        |c| mint(c, holder(), 0, r(0x10)),
        Error::ZeroAmount,
    );
}

#[test]
fn inv_4_mint_without_the_communitys_issuer_role_is_refused_by_kreivo() {
    let mut contract = setup();
    let foreign = AssetId::Here(55);
    mock::create_foreign_asset(foreign, 1);
    caller(governance());
    contract.schedule_raise(foreign, config(BOUNDS)).unwrap();
    at_block(START + DELAY);
    contract.apply_raise(foreign).unwrap();
    caller(delivery());
    assert_eq!(
        contract.mint(foreign, holder(), 10, r(0x10)),
        Err(Error::Kreivo(KreivoApiError::Assets(
            AssetsApiError::NotAuthorised
        )))
    );
    assert_eq!(mock::issuance(foreign), 0);
}

// ---------------------------------------------------------------------------------------------
// Redeem (T-006-04)
// ---------------------------------------------------------------------------------------------

#[test]
fn ac_b5_1_redeem_destroys_in_the_same_call_and_records_in_order() {
    let mut contract = funded(1_000);
    let issuance = mock::issuance(BUSDC);
    let seq = contract.event_seq();
    redeem_evm(&mut contract, 1_000, r(0x20)).expect("redeem");

    // Destroyed in the call: nothing left in the contract, issuance and supply down.
    assert_eq!(mock::balance(BUSDC, holder()), 10);
    assert_eq!(mock::balance(BUSDC, me()), 0);
    assert_eq!(mock::issuance(BUSDC), issuance - 1_000);
    assert_eq!(contract.supply(BUSDC), 10);

    // Reference and instructions recorded, in the contract's order.
    let record = Redemption {
        redeemer: holder(),
        asset: BUSDC,
        amount: 1_000,
        backend: EVM,
        payout: Payout::Address(ADDRESS),
        refunded: false,
    };
    assert_eq!(contract.redemption(holder(), r(0x20)), Some(record));
    assert_eq!(contract.redemption(stranger(), r(0x20)), None);
    assert!(!contract.mint_reference_used(r(0x20)));
    let event = events().pop().unwrap();
    assert_eq!(event.topics[1], r(0x20).to_vec());
    assert_eq!(
        Redeemed::decode(&mut &event.data[..]).unwrap(),
        Redeemed {
            seq,
            reference: r(0x20),
            redeemer: holder(),
            asset: BUSDC,
            amount: 1_000,
            backend: EVM,
            payout: Payout::Address(ADDRESS),
        }
    );
}

#[test]
fn req_hn_8_redeem_pulls_then_checks_then_burns_only_from_the_contract() {
    let mut contract = funded(1_000);
    let calls_before = mock::calls().len();
    redeem_evm(&mut contract, 1_000, r(0x20)).expect("redeem");
    // Balance, pull (no approval, so the approved pull is refused and `deposit` pulls), balance,
    // burn: in that order.
    assert_eq!(
        mock::calls()[calls_before..],
        [FN_BALANCE, CALL_RUNTIME, FN_DEPOSIT, FN_BALANCE, FN_BURN]
    );
    let burns = mock::burns();
    assert_eq!(burns.len(), 1);
    assert_eq!(
        burns[0].who,
        me(),
        "the burn names the contract's own account"
    );
    assert_ne!(burns[0].who, holder());
    assert_eq!(burns[0].burnt, 1_000);
}

#[test]
fn inv_4_redeem_never_burns_from_the_redeemer() {
    // Kreivo's burn could take from the redeemer directly; the contract never asks it to, even when
    // the pull is refused.
    let mut contract = funded(1_000);
    caller(holder());
    // All of it: `Preserve` keeps the holder's account alive, so the pull is refused.
    assert_eq!(
        contract.redeem(BUSDC, 1_010, EVM, ADDRESS.to_vec(), r(0x20)),
        Err(Error::Kreivo(KreivoApiError::Assets(
            AssetsApiError::CannotDeposit
        )))
    );
    assert!(mock::burns().is_empty());
    assert_eq!(mock::balance(BUSDC, holder()), 1_010);
}

#[test]
fn req_hn_8_transfer_approved_encodes_as_kreivos_assets_call() {
    use scale::Compact;
    type Address = sp_runtime::MultiAddress<sp_runtime::AccountId32, u16>;
    let call = RuntimeCall::Assets(AssetsCall::TransferApproved {
        id: BUSDC,
        owner: MultiAddress::Id(holder()),
        destination: MultiAddress::Id(me()),
        amount: 1_010,
    });
    let mut expected = vec![13u8, 25];
    virto_common::FungibleAssetLocation::Here(101).encode_to(&mut expected);
    Address::Id(sp_runtime::AccountId32::new([4; 32])).encode_to(&mut expected);
    Address::Id(sp_runtime::AccountId32::new(*me().as_ref())).encode_to(&mut expected);
    Compact(1_010u128).encode_to(&mut expected);
    assert_eq!(call.encode(), expected);
}

/// Item 4 of the review fixes: a holder approves the contract for their whole balance and redeems
/// it all; the approved pull may empty their account, and the order and the check hold
/// (`REQ-HN-8`, `INV-4`).
#[test]
fn req_hn_8_a_holder_redeems_their_whole_balance_through_an_approval() {
    let mut contract = funded(1_000);
    let all = mock::balance(BUSDC, holder());
    mock::approve(BUSDC, holder(), me(), all);
    let calls_before = mock::calls().len();
    redeem_evm(&mut contract, all, r(0x20)).expect("redeems everything");
    assert_eq!(
        mock::calls()[calls_before..],
        [FN_BALANCE, CALL_RUNTIME, FN_BALANCE, FN_BURN]
    );
    assert_eq!(mock::balance(BUSDC, holder()), 0);
    assert_eq!(mock::balance(BUSDC, me()), 0);
    assert_eq!(mock::approved(BUSDC, holder(), me()), 0);
    assert_eq!(mock::burns().len(), 1);
    assert_eq!(mock::burns()[0].who, me());
    assert_eq!(contract.supply(BUSDC), 0);
    assert_eq!(contract.redemption(holder(), r(0x20)).unwrap().amount, all);
}

#[test]
fn req_hn_8_an_approval_short_of_the_amount_falls_back_to_the_kept_alive_pull() {
    let mut contract = funded(1_000);
    mock::approve(BUSDC, holder(), me(), 400);
    redeem_evm(&mut contract, 500, r(0x20)).expect("pulled by deposit");
    assert_eq!(mock::approved(BUSDC, holder(), me()), 400);
    assert_eq!(mock::balance(BUSDC, holder()), 510);
}

/// An approved pull that leaves the holder a rest below the minimum balance moves that rest too:
/// the contract's balance rises by more than the amount, and the redemption is refused before
/// anything is burnt (`REQ-HN-8`).
#[test]
fn req_hn_8_an_approved_pull_that_moves_more_than_the_amount_is_refused() {
    let mut contract = setup();
    mock::create_asset(BBTC, 10);
    mock::fund(BBTC, holder(), 1_000);
    mock::approve(BBTC, holder(), me(), 995);
    caller(holder());
    assert_eq!(
        contract.redeem(BBTC, 995, EVM, ADDRESS.to_vec(), r(0x20)),
        Err(Error::DepositMismatch)
    );
    assert!(mock::burns().is_empty());
    assert_eq!(contract.redemption(holder(), r(0x20)), None);
}

#[test]
fn req_hn_8_redeem_refuses_when_the_contracts_balance_did_not_rise_by_the_amount() {
    let mut contract = funded(1_000);
    mock::short_deposit(1);
    assert_eq!(
        redeem_evm(&mut contract, 1_000, r(0x20)),
        Err(Error::DepositMismatch)
    );
    assert!(mock::burns().is_empty());
    assert_eq!(contract.redemption(holder(), r(0x20)), None);
}

#[test]
fn ac_b5_2_redeem_refuses_a_reference_used_before() {
    let mut contract = funded(1_000);
    redeem_evm(&mut contract, 500, r(0x20)).expect("redeem");
    assert_refused(
        &mut contract,
        |c| redeem_evm(c, 500, r(0x20)),
        Error::ReferenceUsed,
    );
}

/// The review's M1 front-running: another caller who sees a redemption's reference in the pool
/// and uses it first takes nothing from the original, which still goes through under its own
/// key, and a refund mint names the redeemer it refunds (`REQ-ID-4`, `INV-2`, `INV-8`).
#[test]
fn req_id_4_a_redeemer_cannot_take_another_redeemers_reference() {
    let mut contract = funded(1_000);
    mock::fund(BUSDC, stranger(), 1_000);
    caller(stranger());
    contract
        .redeem(BUSDC, 200, EVM, ADDRESS.to_vec(), r(0x20))
        .expect("the stranger's redemption");
    redeem_evm(&mut contract, 500, r(0x20)).expect("the holder's redemption, unaffected");
    assert_eq!(
        contract.redemption(stranger(), r(0x20)).unwrap().amount,
        200
    );
    assert_eq!(contract.redemption(holder(), r(0x20)).unwrap().amount, 500);
    // A mint reference is no redemption's either: r(0x01) funded the holder.
    redeem_evm(&mut contract, 100, r(0x01)).expect("a mint's reference is another namespace");
    // Each redeemer is refunded only their own.
    caller(delivery());
    contract.refund_mint(holder(), r(0x20)).expect("refund");
    assert_eq!(last::<RefundMinted>().to, holder());
    assert_eq!(last::<RefundMinted>().amount, 500);
    assert!(!contract.redemption(stranger(), r(0x20)).unwrap().refunded);
}

#[test]
fn req_rq_9_an_evm_backend_takes_a_20_byte_address_inline() {
    let mut contract = funded(1_000);
    for payout in [vec![], vec![0xAD; 19], vec![0xAD; 21], COMMITMENT.to_vec()] {
        assert_refused(
            &mut contract,
            |c| {
                caller(holder());
                c.redeem(BUSDC, 500, EVM, payout, r(0x20))
            },
            Error::PayoutDoesNotFitBackend,
        );
    }
}

#[test]
fn req_rq_9_an_attested_backend_takes_only_a_32_byte_commitment() {
    let mut contract = funded(1_000);
    for payout in [vec![], ADDRESS.to_vec(), vec![0xC0; 33]] {
        assert_refused(
            &mut contract,
            |c| {
                caller(holder());
                c.redeem(BUSDC, 500, ATTESTED, payout, r(0x20))
            },
            Error::PayoutDoesNotFitBackend,
        );
    }
    caller(holder());
    contract
        .redeem(BUSDC, 500, ATTESTED, COMMITMENT.to_vec(), r(0x20))
        .expect("a commitment fits");
    assert_eq!(
        contract.redemption(holder(), r(0x20)).unwrap().payout,
        Payout::Commitment(COMMITMENT)
    );
}

#[test]
fn req_rq_9_an_evm_backend_refuses_the_zero_address() {
    let mut contract = funded(1_000);
    assert_refused(
        &mut contract,
        |c| {
            caller(holder());
            c.redeem(BUSDC, 500, EVM, vec![0; 20], r(0x20))
        },
        Error::PayoutDoesNotFitBackend,
    );
}

/// The review's S2 scenario: a bUSDC redemption naming chain 1, or an attested backend that does
/// not exist, was destroyed at once with no way back but a refund mint. The contract now refuses
/// any backend the asset does not declare (`REQ-OT-4`, `REQ-RQ-9`).
#[test]
fn req_ot_4_redeem_refuses_a_backend_the_asset_does_not_declare() {
    let mut contract = setup();
    caller(governance());
    contract
        .schedule_raise(
            BUSDC,
            AssetConfig {
                bounds: BOUNDS,
                backends: vec![EVM],
                initial_supply: 0,
            },
        )
        .unwrap();
    at_block(START + DELAY);
    contract.apply_raise(BUSDC).unwrap();
    assert_eq!(contract.backends(BUSDC), vec![EVM]);
    assert_eq!(last::<BackendsSet>().backends, vec![EVM]);
    mint(&mut contract, holder(), 1_010, r(0x01)).unwrap();
    for (backend, payout) in [
        (Backend::Evm { chain_id: 1 }, ADDRESS.to_vec()),
        (ATTESTED, COMMITMENT.to_vec()),
        (Backend::Attested { id: 99 }, COMMITMENT.to_vec()),
    ] {
        assert_refused(
            &mut contract,
            |c| {
                caller(holder());
                c.redeem(BUSDC, 500, backend, payout, r(0x20))
            },
            Error::BackendNotDeclared,
        );
    }
    redeem_evm(&mut contract, 500, r(0x20)).expect("the declared backend");
}

#[test]
fn req_gv_3_an_assets_backends_are_checked_and_change_only_through_the_delay() {
    let mut contract = setup();
    caller(governance());
    for backends in [vec![], vec![EVM, EVM], vec![EVM; MAX_BACKENDS + 1]] {
        let bad = AssetConfig {
            bounds: BOUNDS,
            backends,
            initial_supply: 0,
        };
        assert_eq!(
            contract.schedule_raise(BUSDC, bad.clone()),
            Err(Error::InvalidBackends)
        );
        test::set_callee::<Env>(me());
        assert_eq!(
            Bezent::new(roles(), DELAY, vec![(BBTC, bad)]).err(),
            Some(Error::InvalidBackends)
        );
    }
    let evm_only = AssetConfig {
        bounds: BOUNDS,
        backends: vec![EVM],
        initial_supply: 0,
    };
    contract.schedule_raise(BUSDC, evm_only).unwrap();
    at_block(START + DELAY - 1);
    assert_eq!(contract.apply_raise(BUSDC), Err(Error::RaiseNotDue));
    assert_eq!(contract.backends(BUSDC), vec![EVM, ATTESTED]);
    at_block(START + DELAY);
    contract.apply_raise(BUSDC).unwrap();
    assert_eq!(contract.backends(BUSDC), vec![EVM]);
}

#[test]
fn req_lm_1_redeem_refuses_below_the_assets_minimum() {
    let mut contract = funded(1_000);
    assert_refused(
        &mut contract,
        |c| redeem_evm(c, BOUNDS.redeem_minimum - 1, r(0x20)),
        Error::BelowMinimum,
    );
    redeem_evm(&mut contract, BOUNDS.redeem_minimum, r(0x20)).expect("at the minimum");
}

#[test]
fn ac_i2_1_redeem_refuses_when_the_asset_or_its_direction_is_paused() {
    for scope in [
        PauseScope::Asset(BUSDC),
        PauseScope::AssetDirection(BUSDC, Direction::Redeem),
    ] {
        let mut contract = funded(1_000);
        caller(pauser());
        contract.pause(scope).unwrap();
        assert_refused(
            &mut contract,
            |c| redeem_evm(c, 500, r(0x20)),
            Error::Paused,
        );
    }
}

#[test]
fn req_hn_5_redeem_refuses_an_unknown_asset() {
    let mut contract = funded(1_000);
    assert_refused(
        &mut contract,
        |c| {
            caller(holder());
            c.redeem(UNKNOWN, 500, EVM, ADDRESS.to_vec(), r(0x20))
        },
        Error::UnknownAsset,
    );
}

#[test]
fn req_hn_5_anyone_may_redeem() {
    let mut contract = funded(1_000);
    caller(holder());
    // A redeemer that holds none of the role keys.
    contract
        .redeem(BUSDC, 500, EVM, ADDRESS.to_vec(), r(0x20))
        .expect("anyone");
    assert_eq!(
        contract.redemption(holder(), r(0x20)).unwrap().redeemer,
        holder()
    );
}

// ---------------------------------------------------------------------------------------------
// Refund mint (T-006-05)
// ---------------------------------------------------------------------------------------------

fn redeemed() -> Bezent {
    let mut contract = funded(1_000);
    redeem_evm(&mut contract, 1_000, r(0x20)).expect("redeem");
    contract
}

#[test]
fn inv_8_refund_mint_mints_once_to_the_redeemer_tied_to_the_redemption() {
    let mut contract = redeemed();
    caller(delivery());
    contract.refund_mint(holder(), r(0x20)).expect("refund");
    assert_eq!(mock::balance(BUSDC, holder()), 1_010);
    assert!(contract.redemption(holder(), r(0x20)).unwrap().refunded);
    let event = events().pop().unwrap();
    assert_eq!(event.topics[1], r(0x20).to_vec());
    let refund = RefundMinted::decode(&mut &event.data[..]).unwrap();
    assert_eq!(
        (refund.reference, refund.to, refund.asset, refund.amount),
        (r(0x20), holder(), BUSDC, 1_000)
    );
}

#[test]
fn inv_8_a_second_refund_mint_is_refused() {
    let mut contract = redeemed();
    caller(delivery());
    contract
        .refund_mint(holder(), r(0x20))
        .expect("first refund");
    assert_refused(
        &mut contract,
        |c| {
            caller(delivery());
            c.refund_mint(holder(), r(0x20))
        },
        Error::AlreadyRefunded,
    );
    // Not in a later window either.
    at_block(START + 10 * BOUNDS.window_blocks);
    caller(delivery());
    assert_eq!(
        contract.refund_mint(holder(), r(0x20)),
        Err(Error::AlreadyRefunded)
    );
}

#[test]
fn req_ot_6_refund_mint_refuses_an_unknown_redemption() {
    let mut contract = redeemed();
    // r(0x01) is a mint's reference, not a redemption's; r(0x20) is the holder's, not the
    // stranger's.
    for (redeemer, reference) in [
        (holder(), r(0x99)),
        (holder(), r(0x01)),
        (stranger(), r(0x20)),
    ] {
        assert_refused(
            &mut contract,
            |c| {
                caller(delivery());
                c.refund_mint(redeemer, reference)
            },
            Error::NoSuchRedemption,
        );
    }
}

#[test]
fn req_rf_4_refund_mint_refuses_when_the_mint_window_would_be_exceeded() {
    let mut contract = redeemed();
    // The funding mint used 1 010 of the window; fill the rest but 999.
    mint(&mut contract, stranger(), 50_000, r(0x11)).unwrap();
    mint(
        &mut contract,
        stranger(),
        100_000 - 1_010 - 50_000 - 999,
        r(0x12),
    )
    .unwrap();
    assert_refused(
        &mut contract,
        |c| {
            caller(delivery());
            c.refund_mint(holder(), r(0x20))
        },
        Error::AboveWindow,
    );
    at_block(START + BOUNDS.window_blocks);
    caller(delivery());
    contract
        .refund_mint(holder(), r(0x20))
        .expect("in the next window");
}

#[test]
fn req_rf_4_refund_mint_counts_against_the_mint_window() {
    let mut contract = redeemed();
    // The funding mint used 1 010 of the window.
    assert_eq!(contract.minted_in_window(BUSDC), 1_010);
    caller(delivery());
    contract.refund_mint(holder(), r(0x20)).unwrap();
    assert_eq!(contract.minted_in_window(BUSDC), 2_010);
    // What is left of the window is what the refund did not use.
    mint(&mut contract, stranger(), 50_000, r(0x11)).unwrap();
    mint(&mut contract, stranger(), 100_000 - 2_010 - 50_000, r(0x12)).unwrap();
    assert_refused(
        &mut contract,
        |c| mint(c, stranger(), 1, r(0x13)),
        Error::AboveWindow,
    );
}

#[test]
fn inv_3_refund_mint_refuses_above_the_supply_ceiling() {
    let mut contract = redeemed();
    // Supply is 10; a ceiling of 500 leaves no room for a refund of 1 000.
    caller(pauser());
    contract
        .lower_bounds(
            BUSDC,
            Bounds {
                supply_ceiling: 500,
                ..BOUNDS
            },
        )
        .unwrap();
    assert_refused(
        &mut contract,
        |c| {
            caller(delivery());
            c.refund_mint(holder(), r(0x20))
        },
        Error::AboveCeiling,
    );
}

#[test]
fn req_hn_9_refund_mint_refuses_any_caller_but_the_delivery_role() {
    let mut contract = redeemed();
    for who in [pauser(), governance(), holder()] {
        assert_refused(
            &mut contract,
            |c| {
                caller(who);
                c.refund_mint(holder(), r(0x20))
            },
            Error::Unauthorised,
        );
    }
}

#[test]
fn req_gv_1_a_pause_does_not_stop_a_refund_mint_in_flight() {
    let mut contract = redeemed();
    caller(pauser());
    contract.pause(PauseScope::All).unwrap();
    caller(delivery());
    contract
        .refund_mint(holder(), r(0x20))
        .expect("completes the withdrawal in flight");
}

// ---------------------------------------------------------------------------------------------
// Pauses (T-006-06)
// ---------------------------------------------------------------------------------------------

/// Whether a mint and a redemption of each asset are let through by the pauses. The holder is
/// funded first, so a redemption that is not paused succeeds.
fn running(contract: &mut Bezent) -> [(bool, bool); 2] {
    let mut out = [(false, false); 2];
    for (i, asset) in [BUSDC, BBTC].into_iter().enumerate() {
        mock::fund(asset, holder(), 1_000);
        let i8 = i as u8;
        caller(delivery());
        let minted = contract.mint(asset, stranger(), 10, r(0x70 + i8));
        caller(holder());
        let redeemed = contract.redeem(asset, 100, EVM, ADDRESS.to_vec(), r(0x80 + i8));
        for result in [minted, redeemed] {
            assert!(matches!(result, Ok(()) | Err(Error::Paused)), "{result:?}");
        }
        out[i] = (minted.is_ok(), redeemed.is_ok());
    }
    out
}

#[test]
fn ac_i2_1_each_pause_scope_stops_its_direction() {
    let cases = [
        (PauseScope::All, [(false, false), (false, false)]),
        (
            PauseScope::Direction(Direction::Mint),
            [(false, true), (false, true)],
        ),
        (
            PauseScope::Direction(Direction::Redeem),
            [(true, false), (true, false)],
        ),
        (PauseScope::Asset(BUSDC), [(false, false), (true, true)]),
        (
            PauseScope::AssetDirection(BUSDC, Direction::Mint),
            [(false, true), (true, true)],
        ),
        (
            PauseScope::AssetDirection(BBTC, Direction::Redeem),
            [(true, true), (true, false)],
        ),
    ];
    for (scope, expected) in cases {
        let mut contract = setup();
        caller(pauser());
        contract.pause(scope).unwrap();
        assert!(contract.paused(scope));
        assert_eq!(
            last::<Paused>(),
            Paused {
                seq: contract.event_seq() - 1,
                scope,
                by: pauser()
            }
        );
        assert_eq!(running(&mut contract), expected, "{scope:?}");
    }
}

#[test]
fn ac_i2_1_governance_may_pause_and_no_one_else_but_the_pause_role() {
    let mut contract = setup();
    for who in [delivery(), holder()] {
        caller(who);
        assert_eq!(contract.pause(PauseScope::All), Err(Error::Unauthorised));
    }
    caller(governance());
    contract.pause(PauseScope::All).expect("governance pauses");
}

#[test]
fn ac_i2_2_the_pause_role_cannot_unpause() {
    let mut contract = setup();
    caller(pauser());
    contract.pause(PauseScope::Asset(BUSDC)).unwrap();
    for who in [pauser(), delivery(), stranger()] {
        caller(who);
        assert_eq!(
            contract.unpause(PauseScope::Asset(BUSDC)),
            Err(Error::Unauthorised)
        );
        assert!(contract.paused(PauseScope::Asset(BUSDC)));
    }
}

#[test]
fn ac_i2_2_governance_unpauses() {
    let mut contract = setup();
    caller(pauser());
    contract.pause(PauseScope::Asset(BUSDC)).unwrap();
    caller(governance());
    contract.unpause(PauseScope::Asset(BUSDC)).unwrap();
    assert!(!contract.paused(PauseScope::Asset(BUSDC)));
    assert_eq!(last::<Unpaused>().scope, PauseScope::Asset(BUSDC));
    assert_eq!(running(&mut contract), [(true, true), (true, true)]);
}

// ---------------------------------------------------------------------------------------------
// Bounds (T-006-07)
// ---------------------------------------------------------------------------------------------

fn raised() -> Bounds {
    Bounds {
        supply_ceiling: 2_000_000,
        window_amount: 200_000,
        window_blocks: 10,
        per_transfer_max: 100_000,
        redeem_minimum: 100,
    }
}

#[test]
fn req_gv_3_a_raise_applied_before_the_delay_is_refused() {
    let mut contract = setup();
    caller(governance());
    contract.schedule_raise(BUSDC, config(raised())).unwrap();
    assert_eq!(
        last::<RaiseScheduled>(),
        RaiseScheduled {
            seq: contract.event_seq() - 1,
            asset: BUSDC,
            config: config(raised()),
            due: START + DELAY
        }
    );
    at_block(START + DELAY - 1);
    caller(stranger());
    assert_eq!(contract.apply_raise(BUSDC), Err(Error::RaiseNotDue));
    assert_eq!(contract.bounds(BUSDC), Some(BOUNDS));
}

#[test]
fn req_gv_3_a_raise_applies_after_the_delay_by_anyone() {
    let mut contract = setup();
    caller(governance());
    contract.schedule_raise(BUSDC, config(raised())).unwrap();
    at_block(START + DELAY);
    caller(stranger());
    contract.apply_raise(BUSDC).expect("due");
    assert_eq!(contract.bounds(BUSDC), Some(raised()));
    assert_eq!(contract.scheduled_raise(BUSDC), None);
    let events = events();
    let set = BoundsSet::decode(&mut &events[events.len() - 2].data[..]).unwrap();
    assert_eq!(set.bounds, raised());
    assert_eq!(contract.apply_raise(BUSDC), Err(Error::NoRaiseScheduled));
}

#[test]
fn ac_i2_2_only_governance_schedules_a_raise() {
    let mut contract = setup();
    for who in [pauser(), delivery(), stranger()] {
        caller(who);
        assert_eq!(
            contract.schedule_raise(BUSDC, config(raised())),
            Err(Error::Unauthorised)
        );
    }
}

#[test]
fn ac_i2_2_lower_bounds_refuses_any_raise() {
    let mut contract = setup();
    let loosenings = [
        Bounds {
            supply_ceiling: BOUNDS.supply_ceiling + 1,
            ..BOUNDS
        },
        Bounds {
            window_amount: BOUNDS.window_amount + 1,
            ..BOUNDS
        },
        Bounds {
            per_transfer_max: BOUNDS.per_transfer_max + 1,
            ..BOUNDS
        },
        Bounds {
            window_blocks: BOUNDS.window_blocks - 1,
            ..BOUNDS
        },
        Bounds {
            redeem_minimum: BOUNDS.redeem_minimum - 1,
            ..BOUNDS
        },
    ];
    for who in [pauser(), governance()] {
        for bounds in loosenings {
            caller(who);
            assert_eq!(
                contract.lower_bounds(BUSDC, bounds),
                Err(Error::NotALowering)
            );
        }
    }
    assert_eq!(contract.bounds(BUSDC), Some(BOUNDS));
}

#[test]
fn req_gv_2_lowering_is_immediate_by_the_pause_role() {
    let mut contract = setup();
    let lower = Bounds {
        supply_ceiling: 10,
        window_amount: 10,
        window_blocks: 20,
        per_transfer_max: 10,
        redeem_minimum: 200,
    };
    caller(pauser());
    contract.lower_bounds(BUSDC, lower).expect("lowering");
    assert_eq!(contract.bounds(BUSDC), Some(lower));
    assert_eq!(last::<BoundsSet>().bounds, lower);
    assert_eq!(
        mint(&mut contract, holder(), 11, r(0x10)),
        Err(Error::AboveTransferMaximum)
    );
}

#[test]
fn req_gv_3_lowering_cancels_a_scheduled_raise() {
    let mut contract = setup();
    caller(governance());
    contract.schedule_raise(BUSDC, config(raised())).unwrap();
    caller(pauser());
    let lower = Bounds {
        per_transfer_max: 1_000,
        ..BOUNDS
    };
    contract.lower_bounds(BUSDC, lower).unwrap();
    let events = events();
    let cancelled = RaiseCancelled::decode(&mut &events[events.len() - 2].data[..]).unwrap();
    assert_eq!(cancelled.asset, BUSDC);
    at_block(START + DELAY);
    assert_eq!(contract.apply_raise(BUSDC), Err(Error::NoRaiseScheduled));
    assert_eq!(contract.bounds(BUSDC), Some(lower));
}

#[test]
fn req_gv_3_lower_bounds_refuses_others_and_bad_bounds() {
    let mut contract = setup();
    for who in [delivery(), stranger()] {
        caller(who);
        assert_eq!(
            contract.lower_bounds(BUSDC, BOUNDS),
            Err(Error::Unauthorised)
        );
    }
    caller(pauser());
    assert_eq!(
        contract.lower_bounds(UNKNOWN, BOUNDS),
        Err(Error::UnknownAsset)
    );
    assert_eq!(
        contract.lower_bounds(
            BUSDC,
            Bounds {
                redeem_minimum: 0,
                ..BOUNDS
            }
        ),
        Err(Error::InvalidBounds)
    );
    caller(governance());
    assert_eq!(
        contract.schedule_raise(
            BUSDC,
            config(Bounds {
                window_blocks: 0,
                ..BOUNDS
            })
        ),
        Err(Error::InvalidBounds)
    );
}

#[test]
fn req_gv_3_a_new_asset_is_known_only_after_the_delay() {
    let mut contract = setup();
    let new = AssetId::Here(102);
    mock::create_asset(new, 1);
    caller(governance());
    contract.schedule_raise(new, config(BOUNDS)).unwrap();
    caller(delivery());
    assert_eq!(
        contract.mint(new, holder(), 10, r(0x10)),
        Err(Error::UnknownAsset)
    );
    at_block(START + DELAY);
    contract.apply_raise(new).unwrap();
    caller(delivery());
    contract
        .mint(new, holder(), 10, r(0x10))
        .expect("known now");
}

#[test]
fn req_gv_3_the_raise_delay_changes_only_through_the_same_delay() {
    let mut contract = setup();
    for who in [pauser(), delivery(), stranger()] {
        caller(who);
        assert_eq!(contract.schedule_raise_delay(1), Err(Error::Unauthorised));
    }
    caller(governance());
    contract.schedule_raise_delay(1).unwrap();
    assert_eq!(
        last::<DelayScheduled>(),
        DelayScheduled {
            seq: contract.event_seq() - 1,
            delay: 1,
            due: START + DELAY
        }
    );
    at_block(START + DELAY - 1);
    assert_eq!(contract.apply_raise_delay(), Err(Error::DelayNotDue));
    assert_eq!(contract.raise_delay().0, DELAY);
    at_block(START + DELAY);
    caller(stranger());
    contract.apply_raise_delay().unwrap();
    assert_eq!(last::<DelaySet>().delay, 1);
    assert_eq!(contract.raise_delay(), (1, None));
    assert_eq!(contract.apply_raise_delay(), Err(Error::NoDelayScheduled));
}

/// The review's S1 scenario: a raise delay of zero lets governance apply a raise in the block it
/// schedules it. Refused at instantiation and on change (`REQ-GV-3`).
#[test]
fn req_gv_3_a_zero_raise_delay_is_refused_at_instantiation_and_on_change() {
    mock::reset();
    test::set_callee::<Env>(me());
    assert_eq!(
        Bezent::new(roles(), 0, vec![]).err(),
        Some(Error::ZeroDelay)
    );
    let mut contract = setup();
    caller(governance());
    assert_eq!(contract.schedule_raise_delay(0), Err(Error::ZeroDelay));
    assert_eq!(contract.raise_delay(), (DELAY, None));
}

/// Block counts for Kreivo's ~2.07 s blocks (bloque-app/bezent#58): a 24-hour window is 41,739
/// blocks and the raise-delay cap, 28 days, is 1,168,692; the 6 s figures (14,400 and 403,200)
/// would be 8.6 hours and 9.7 days.
#[test]
fn req_gv_3_block_counts_are_pinned_to_kreivos_2_07_second_blocks() {
    assert_eq!(BLOCK_TIME_MS, 2_070);
    assert_eq!(BLOCKS_PER_DAY, 41_739);
    assert_eq!(MAX_RAISE_DELAY, 1_168_692);
    // Within a block of 24 h and 28 d at 2.07 s.
    assert_eq!(86_400_000 / BLOCK_TIME_MS, BLOCKS_PER_DAY);
    assert_eq!(MAX_RAISE_DELAY, 28 * BLOCKS_PER_DAY);
    // A 24 h window is a valid bound.
    let day = Bounds {
        window_blocks: BLOCKS_PER_DAY,
        ..BOUNDS
    };
    assert!(day.is_valid());
}

/// Review 2's S6 scenario: a raise delay of `BlockNumber::MAX - 10` made every later schedule
/// overflow, and a merely long one (ten years) would hold back raises, new assets, delay changes and
/// a governance handover just the same. Refused above `MAX_RAISE_DELAY`, 28 days of blocks
/// (`28 * BLOCKS_PER_DAY`), at instantiation and on change; at the cap, every delayed path still works (`REQ-GV-3`).
#[test]
fn req_gv_3_a_raise_delay_above_the_cap_is_refused_at_instantiation_and_on_change() {
    const TOO_LONG: [BlockNumber; 3] = [MAX_RAISE_DELAY + 1, 52_560_000, BlockNumber::MAX - 10];
    mock::reset();
    test::set_callee::<Env>(me());
    for delay in TOO_LONG {
        assert_eq!(
            Bezent::new(roles(), delay, vec![]).err(),
            Some(Error::DelayTooLong)
        );
    }
    assert!(Bezent::new(roles(), MAX_RAISE_DELAY, vec![]).is_ok());

    let mut contract = setup();
    caller(governance());
    for delay in TOO_LONG {
        assert_eq!(
            contract.schedule_raise_delay(delay),
            Err(Error::DelayTooLong)
        );
    }
    assert_eq!(contract.raise_delay(), (DELAY, None));
    contract.schedule_raise_delay(MAX_RAISE_DELAY).unwrap();
    at_block(START + DELAY);
    contract.apply_raise_delay().unwrap();
    assert_eq!(contract.raise_delay(), (MAX_RAISE_DELAY, None));
    let due = START + DELAY + MAX_RAISE_DELAY;
    contract.schedule_raise(BUSDC, config(BOUNDS)).unwrap();
    contract.schedule_raise_delay(DELAY).unwrap();
    contract.schedule_governance(stranger()).unwrap();
    assert_eq!(contract.scheduled_raise(BUSDC).unwrap().due, due);
    assert_eq!(contract.raise_delay().1.unwrap().due, due);
    assert_eq!(contract.scheduled_governance().unwrap().due, due);
}

// ---------------------------------------------------------------------------------------------
// Ordered events (T-006-08)
// ---------------------------------------------------------------------------------------------

#[test]
fn req_hn_7_every_event_carries_a_sequence_that_increases_by_one() {
    let mut contract = funded(1_000);
    redeem_evm(&mut contract, 500, r(0x20)).unwrap();
    caller(delivery());
    contract.refund_mint(holder(), r(0x20)).unwrap();
    caller(pauser());
    contract.pause(PauseScope::All).unwrap();
    caller(governance());
    contract.unpause(PauseScope::All).unwrap();

    // Every event's encoding starts with its sequence number.
    let seqs: Vec<EventSeq> = events()
        .iter()
        .map(|e| EventSeq::decode(&mut &e.data[..]).unwrap())
        .collect();
    let expected: Vec<EventSeq> = (0..seqs.len() as u64).collect();
    assert_eq!(seqs, expected);
    assert_eq!(contract.event_seq(), seqs.len() as u64);
}

#[test]
fn req_hn_7_mint_redemption_and_refund_events_carry_their_reference() {
    let mut contract = funded(1_000);
    redeem_evm(&mut contract, 500, r(0x20)).unwrap();
    caller(delivery());
    contract.refund_mint(holder(), r(0x20)).unwrap();
    let events = events();
    let n = events.len();
    let minted = Minted::decode(&mut &events[n - 3].data[..]).unwrap();
    let redeemed = Redeemed::decode(&mut &events[n - 2].data[..]).unwrap();
    let refunded = RefundMinted::decode(&mut &events[n - 1].data[..]).unwrap();
    assert_eq!(minted.reference, r(0x01));
    assert_eq!(redeemed.reference, r(0x20));
    assert_eq!(refunded.reference, r(0x20));
    assert!(minted.seq < redeemed.seq && redeemed.seq < refunded.seq);
}
