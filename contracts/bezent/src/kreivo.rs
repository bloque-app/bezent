// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! What the contract knows about Kreivo: its environment and the chain extension it mints, moves,
//! reads and destroys assets through.
//!
//! All of it is read off Kreivo's source at the kreivo#512 merge (`87f8728`, tag 0.17.1, spec 134):
//!
//! - **The chain extension is `KreivoChainExtensions`**, extension 0 in the contract's id (the runtime
//!   routes by function id). Its assets API (`apis/src/runtime/api_impls/assets.rs`):
//!   - `assets__balance` (`0x0000`, `(asset, who)`): `T::Assets::balance(asset, who)`.
//!   - `assets__deposit` (`0x0001`, `(asset, amount)`): `T::Assets::transfer(asset, caller,
//!     contract, amount, Preserve)`, where `caller` is the account that called this contract. It keeps
//!     the caller's asset account alive.
//!   - `assets__mint` (`0x0003`, `(asset, amount, beneficiary)`): `T::Assets::mint_into`, allowed only
//!     for a local asset whose Issuer is the account of the contract's community.
//!   - `assets__burn` (`0x0004`, `(asset, amount, who)`): `T::Assets::burn_from`, best effort and
//!     expendable, allowed only for a local asset whose Admin is the account of the contract's
//!     community. **It can burn from any account**; this contract only ever names its own
//!     (`REQ-HN-8`, `INV-4`).
//!
//!   On success the runtime writes the SCALE-encoded amount and returns status 0; on failure it
//!   writes nothing and returns a status code (`KreivoApisErrorCode`), which [`KreivoApiError`]
//!   decodes as Kreivo's own `From<KreivoApisErrorCode> for KreivoApisError` does.
//!
//! - **`call_runtime` with `Assets::transfer_approved`**: Kreivo's contracts `CallFilter`
//!   (`runtime/kreivo/src/config/contracts/mod.rs`) lets a contract dispatch any `RuntimeCall::Assets`
//!   call, with the contract as the signed origin. `transfer_approved` (pallet-assets call 25, in
//!   pallet index 13, `runtime/kreivo/src/lib.rs`) moves from an owner who approved the contract,
//!   with `keep_alive: false` (`do_transfer_approved`), so a holder can redeem their whole balance.
//!   A failed dispatch changes nothing (every FRAME call runs in its own storage layer) and reports
//!   only failure.
//!
//! The binding is written here rather than taken from Kreivo's `kreivo-apis` crate (its `contract`
//! feature): that crate depends on `frame-support` unconditionally, which does not build into the
//! contract's WASM with the pinned toolchain, and it fixes the contract's environment and asset id
//! type to its own. Tests pin this binding against Kreivo's own types at the pinned commit.

// The codec derive's variant-index casts, reported against these definitions.
#![allow(clippy::cast_possible_truncation)]

use crate::types::{AccountId, AssetId, Balance};

/// The chain extension's id, as the contract passes it: extension 0, function in the low half.
pub const EXTENSION_ID: u16 = 0;
/// `assets__balance`.
pub const FN_BALANCE: u16 = 0x0000;
/// `assets__deposit`.
pub const FN_DEPOSIT: u16 = 0x0001;
/// `assets__mint` (kreivo#512).
pub const FN_MINT: u16 = 0x0003;
/// `assets__burn` (kreivo#512).
pub const FN_BURN: u16 = 0x0004;

/// `Assets`' index in Kreivo's `RuntimeCall` (`#[runtime::pallet_index(13)]`).
pub const ASSETS_PALLET: u8 = 13;
/// `transfer_approved`'s index in pallet-assets' calls (`#[pallet::call_index(25)]`).
pub const TRANSFER_APPROVED: u8 = 25;

/// The part of Kreivo's `RuntimeCall` the contract dispatches through `call_runtime`.
#[ink::scale_derive(Encode)]
pub enum RuntimeCall {
    /// `pallet_assets::<Instance1>` (`Assets`).
    #[codec(index = 13)]
    Assets(AssetsCall),
}

/// The pallet-assets call the contract dispatches.
#[ink::scale_derive(Encode)]
pub enum AssetsCall {
    /// Move `amount` of `id` from `owner` to `destination`, out of `owner`'s approval for the
    /// origin (the contract). `owner` may be reaped.
    #[codec(index = 25)]
    TransferApproved {
        /// The asset (`AssetIdParameter`: `FungibleAssetLocation`).
        id: AssetId,
        /// Who approved the contract.
        owner: MultiAddress,
        /// Where the value goes.
        destination: MultiAddress,
        /// How much.
        #[codec(compact)]
        amount: Balance,
    },
}

/// Kreivo's `Address` (`MultiAddress<AccountId, CommunityId>`), in the one form the contract uses.
#[ink::scale_derive(Encode)]
pub enum MultiAddress {
    /// An account, by its id.
    #[codec(index = 0)]
    Id(AccountId),
}

/// Kreivo's contract environment. The types are Kreivo's (`parachains_common`): `AccountId32`,
/// `u128` balances, `H256` hashes, `u32` block numbers, `u64` milliseconds. The chain extension is
/// the assets API of `KreivoChainExtensions`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[ink::scale_derive(TypeInfo)]
pub enum KreivoEnvironment {}

impl ink::env::Environment for KreivoEnvironment {
    const MAX_EVENT_TOPICS: usize =
        <ink::env::DefaultEnvironment as ink::env::Environment>::MAX_EVENT_TOPICS;

    type AccountId = AccountId;
    type Balance = Balance;
    type Hash = ink::primitives::Hash;
    type Timestamp = u64;
    type BlockNumber = u32;
    type ChainExtension = KreivoAssets;
}

/// The part of Kreivo's assets API this contract uses (`kreivo-apis`,
/// `apis/src/contract/chain_extension.rs`, declares the same functions with the same ids and
/// arguments).
#[ink::chain_extension(extension = 0)]
pub trait KreivoAssets {
    /// A non-zero status code the extension returned.
    type ErrorCode = KreivoApiStatus;

    /// `T::Assets::balance(asset, who)`.
    #[ink(function = 0x0000, handle_status = false)]
    fn balance(asset: AssetId, who: AccountId) -> Balance;

    /// Move `amount` of `asset` from the contract's caller to the contract, keeping the caller's
    /// asset account alive. Returns the amount moved.
    #[ink(function = 0x0001)]
    fn deposit(asset: AssetId, amount: Balance) -> Result<Balance, KreivoApiError>;

    /// Mint `amount` of `asset` into `beneficiary`. Returns the amount minted.
    #[ink(function = 0x0003)]
    fn mint(
        asset: AssetId,
        amount: Balance,
        beneficiary: AccountId,
    ) -> Result<Balance, KreivoApiError>;

    /// Burn up to `amount` of `asset` from `who`, best effort; `who`'s account may be reaped, its
    /// rest burnt too. Returns the amount burnt.
    #[ink(function = 0x0004)]
    fn burn(asset: AssetId, amount: Balance, who: AccountId) -> Result<Balance, KreivoApiError>;
}

/// A status code from Kreivo's chain extension, as the runtime returns it (`KreivoApisErrorCode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[ink::scale_derive(Encode, Decode, TypeInfo)]
pub struct KreivoApiStatus(pub u32);

impl ink::env::chain_extension::FromStatusCode for KreivoApiStatus {
    fn from_status_code(code: u32) -> Result<(), Self> {
        match code {
            0 => Ok(()),
            code => Err(Self(code)),
        }
    }
}

/// Why Kreivo's assets API refused, as Kreivo names it (`kreivo_apis::KreivoApisError`), for the
/// codes the assets API can return. A test proves the decoding against Kreivo's own crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[ink::scale_derive(Encode, Decode, TypeInfo)]
pub enum KreivoApiError {
    /// A code Kreivo does not name (status 1, or one outside the assets API's range).
    #[codec(index = 0)]
    UnknownError,
    /// The runtime could not read the call's context (status 2).
    #[codec(index = 1)]
    ExtQueryError,
    /// The assets API refused (status `0x0001_xxxx`).
    #[codec(index = 2)]
    Assets(AssetsApiError),
}

/// The assets API's own errors (`kreivo_apis::AssetsApiError`). Their order is Kreivo's ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[ink::scale_derive(Encode, Decode, TypeInfo)]
pub enum AssetsApiError {
    /// The transfer from the caller was refused (`0x0001_0000`).
    #[codec(index = 0)]
    CannotDeposit,
    /// The transfer from the contract was refused (`0x0001_0001`).
    #[codec(index = 1)]
    CannotTransfer,
    /// The mint was refused (`0x0001_0002`).
    #[codec(index = 2)]
    CannotMint,
    /// The burn was refused (`0x0001_0003`).
    #[codec(index = 3)]
    CannotBurn,
    /// The contract does not belong to a community (`0x0001_0004`).
    #[codec(index = 4)]
    NotAGroupContract,
    /// The contract's community does not hold the role the operation needs (`0x0001_0005`).
    #[codec(index = 5)]
    NotAuthorised,
    /// Contracts may not mint or burn this asset: it is not local (`0x0001_0006`).
    #[codec(index = 6)]
    NotALocalAsset,
}

impl From<KreivoApiStatus> for KreivoApiError {
    /// Kreivo's own decoding (`apis/src/apis/error/mod.rs`): `2` is `ExtQueryError`; the assets range
    /// `0x0001_0000..0x0002_0000` carries the assets error in its low half; anything else, including
    /// an assets code Kreivo does not define, is `UnknownError`.
    fn from(KreivoApiStatus(code): KreivoApiStatus) -> Self {
        use AssetsApiError::*;
        let assets = |e| KreivoApiError::Assets(e);
        match code {
            0x0000_0002 => KreivoApiError::ExtQueryError,
            0x0001_0000 => assets(CannotDeposit),
            0x0001_0001 => assets(CannotTransfer),
            0x0001_0002 => assets(CannotMint),
            0x0001_0003 => assets(CannotBurn),
            0x0001_0004 => assets(NotAGroupContract),
            0x0001_0005 => assets(NotAuthorised),
            0x0001_0006 => assets(NotALocalAsset),
            _ => KreivoApiError::UnknownError,
        }
    }
}

impl From<scale::Error> for KreivoApiError {
    /// The runtime reported success but its output (the amount) does not decode. Value may have
    /// moved, so this cannot be a refusal: the contract traps and the whole message reverts.
    fn from(_: scale::Error) -> Self {
        panic!("Kreivo's assets API returned an amount that does not decode")
    }
}
