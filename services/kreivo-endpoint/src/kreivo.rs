// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Kreivo, as the endpoint speaks to it: plain SCALE, written from Kreivo's source at the pin of
//! `DEC-5` and `DEC-6` (Kreivo `0.17.1`, `87f8728`, the merge of kreivo#512; spec 134, transaction
//! version 13), and from bezent's contract (`contracts/bezent`, F-006).
//!
//! - **The contract's messages** the endpoint sends (`mint`, `refund_mint`), by their ink! 5
//!   selectors, and **its events**, by their signature topics. Every event's first field is the
//!   contract's sequence number (`REQ-HN-7`), so a gap in it is visible (`AC-D2.3`).
//! - **`Contracts::call`**, which carries a message, and `ContractsApi_call`, the runtime API that
//!   dry-runs it at a block (`pallet-contracts` 49).
//! - **The transaction**: a signed v4 extrinsic from the delivery key's own account, with Kreivo's
//!   extensions: `PassAuthenticate` (none: the account signs for itself), `CheckNonZeroSender`,
//!   `CheckSpecVersion`, `CheckTxVersion`, `CheckGenesis`, `CheckEra` (mortal), `CheckNonce`,
//!   `CheckWeight` and `SkipCheckIfFeeless<ChargeGasTxPayment<ChargeAssetTxPayment>>`, all wrapped in
//!   `DynamicMaxBlockWeight`, which adds no data:
//!
//! ```text
//! extrinsic = compact(len) ‖ 0x84 ‖ MultiAddress::Id(account) ‖ MultiSignature ‖ extra ‖ call
//! extra     = 0x00 (PassAuthenticate: None) ‖ era ‖ compact(nonce) ‖ compact(tip) ‖ 0x00 (no fee asset)
//! implicit  = spec_version ‖ transaction_version ‖ genesis ‖ hash of the era's birth block
//! payload   = call ‖ extra ‖ implicit; the key signs it, or its BLAKE2b-256 when longer than 256 bytes
//! ```
//!
//! **Expiry** (`REQ-HN-3`): every transaction is mortal, born at the finalized head, at most
//! [`MAX_ERA`] blocks long (`DEC-5`). It cannot be included after its last valid block, which is
//! what makes its non-execution provable (`REQ-NX-1`).

use blake2::Digest;
use parity_scale_codec::{Compact, Decode, Encode};

/// A 32-byte Kreivo account.
pub type AccountId32 = [u8; 32];

/// An on-chain reference as the contract takes it (SPEC §2): 32 bytes.
pub type Reference = [u8; 32];

/// The longest era the endpoint signs with, in blocks (`DEC-5`: `max_era` 64).
pub const MAX_ERA: u64 = 64;

/// `Contracts` in Kreivo's runtime (`#[runtime::pallet_index(80)]` at `87f8728`).
pub const CONTRACTS_PALLET: u8 = 80;
/// `pallet_contracts::Call::call` (`#[pallet::call_index(6)]`, `pallet-contracts` 49).
pub const CONTRACTS_CALL: u8 = 6;
/// A signed extrinsic, format version 4.
pub const SIGNED_V4: u8 = 0x84;
/// `MultiAddress::Id`.
pub const ADDRESS_ID: u8 = 0;
/// A signed payload longer than this is signed as its BLAKE2b-256 (`sp_runtime::generic::SignedPayload`).
pub const MAX_UNHASHED_PAYLOAD: usize = 256;

/// The runtime version the endpoint's transaction format is written for (Kreivo `0.17.1`). A
/// runtime of another `spec_version` is refused, not guessed at.
pub const SPEC_VERSIONS: &[u32] = &[134];

/// The `transaction_version` of each spec in [`SPEC_VERSIONS`] (Kreivo `0.17.1`: 13).
pub const RUNTIME_VERSIONS: &[(u32, u32)] = &[(134, 13)];

/// The `transaction_version` the endpoint knows for `spec_version`, if it has a format for it.
pub fn known_transaction_version(spec_version: u32) -> Option<u32> {
    RUNTIME_VERSIONS
        .iter()
        .find(|(spec, _)| *spec == spec_version)
        .map(|(_, tx)| *tx)
}

/// BLAKE2b-256: Kreivo's hash.
pub fn blake2_256(bytes: &[u8]) -> [u8; 32] {
    blake2::Blake2b::<blake2::digest::consts::U32>::digest(bytes).into()
}

// ------------------------------------------------------------------------------ keys

/// How the delivery key signs (`signatures.scheme`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    /// Schnorrkel over the payload, in the `substrate` context. The account is the public key.
    Sr25519,
    /// ECDSA over secp256k1, over the payload's BLAKE2b-256, recoverable. The account is the
    /// BLAKE2b-256 of the compressed public key.
    Ecdsa,
}

impl Scheme {
    /// From the signer's scheme name; `None` for a scheme Kreivo accounts do not use here.
    pub fn parse(name: &str) -> Option<Scheme> {
        match name {
            "sr25519" => Some(Scheme::Sr25519),
            "ecdsa" => Some(Scheme::Ecdsa),
            _ => None,
        }
    }

    /// The signer's name for it.
    pub fn name(self) -> &'static str {
        match self {
            Scheme::Sr25519 => "sr25519",
            Scheme::Ecdsa => "ecdsa",
        }
    }

    /// Its `MultiSignature` variant.
    pub fn variant(self) -> u8 {
        match self {
            Scheme::Sr25519 => 1,
            Scheme::Ecdsa => 2,
        }
    }

    /// The length of its signature.
    pub fn signature_len(self) -> usize {
        match self {
            Scheme::Sr25519 => 64,
            Scheme::Ecdsa => 65,
        }
    }

    /// The account of `public_key`, if it has this scheme's length.
    pub fn account(self, public_key: &[u8]) -> Option<AccountId32> {
        match self {
            Scheme::Sr25519 => public_key.try_into().ok(),
            Scheme::Ecdsa => (public_key.len() == 33).then(|| blake2_256(public_key)),
        }
    }
}

// ------------------------------------------------------------------------------ the contract

/// A bridged asset's id on Kreivo, as the contract encodes it (`AssetId::Here(u32)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum AssetId {
    /// A local asset, by its index in `pallet-assets`.
    #[codec(index = 0)]
    Here(u32),
}

impl AssetId {
    /// Its text form, `Here(101)`.
    pub fn text(&self) -> String {
        let AssetId::Here(id) = self;
        format!("Here({id})")
    }

    /// Parses its text form, `Here(101)`, exactly as [`AssetId::text`] writes it.
    pub fn parse(text: &str) -> Option<AssetId> {
        let digits = text.strip_prefix("Here(")?.strip_suffix(')')?;
        if digits.is_empty()
            || !digits.bytes().all(|b| b.is_ascii_digit())
            || (digits.len() > 1 && digits.starts_with('0'))
        {
            return None;
        }
        digits.parse().ok().map(AssetId::Here)
    }
}

/// A direction of the contract's value flow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum Direction {
    /// Mints.
    Mint,
    /// Redemptions.
    Redeem,
}

/// What a pause covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum PauseScope {
    /// Everything.
    All,
    /// One direction of every asset.
    Direction(Direction),
    /// One asset.
    Asset(AssetId),
    /// One asset, one direction.
    AssetDirection(AssetId, Direction),
}

/// An asset's bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Bounds {
    /// Supply ceiling.
    pub supply_ceiling: u128,
    /// Mint window amount.
    pub window_amount: u128,
    /// Mint window length, in blocks.
    pub window_blocks: u32,
    /// Per-transfer maximum.
    pub per_transfer_max: u128,
    /// Redemption minimum.
    pub redeem_minimum: u128,
}

/// The backend a redemption names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum Backend {
    /// An EVM origin network, by chain id.
    Evm {
        /// The chain id.
        chain_id: u64,
    },
    /// An attested backend, by its id.
    Attested {
        /// The id.
        id: u32,
    },
}

/// A redemption's payout instructions, or their commitment (`REQ-RQ-9`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum Payout {
    /// An EVM address.
    Address([u8; 20]),
    /// A commitment to instructions held off chain.
    Commitment([u8; 32]),
}

/// An ink! 5 message selector: the first four bytes of the BLAKE2b-256 of its name.
pub fn selector(name: &str) -> [u8; 4] {
    let h = blake2_256(name.as_bytes());
    [h[0], h[1], h[2], h[3]]
}

/// The input of `mint(asset, to, amount, reference)`.
pub fn mint_data(asset: AssetId, to: &AccountId32, amount: u128, reference: &Reference) -> Vec<u8> {
    let mut out = selector("mint").to_vec();
    (asset, to, amount, reference).encode_to(&mut out);
    out
}

/// The input of `refund_mint(redeemer, redemption_reference)`.
pub fn refund_mint_data(redeemer: &AccountId32, reference: &Reference) -> Vec<u8> {
    let mut out = selector("refund_mint").to_vec();
    (redeemer, reference).encode_to(&mut out);
    out
}

/// A message to the contract, decoded from its input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// `mint`.
    Mint {
        /// The asset.
        asset: AssetId,
        /// The destination.
        to: AccountId32,
        /// The amount.
        amount: u128,
        /// The on-chain reference.
        reference: Reference,
    },
    /// `refund_mint`.
    RefundMint {
        /// The redeemer.
        redeemer: AccountId32,
        /// The redemption's reference.
        reference: Reference,
    },
}

impl Message {
    /// The message's input bytes.
    pub fn data(&self) -> Vec<u8> {
        match self {
            Message::Mint {
                asset,
                to,
                amount,
                reference,
            } => mint_data(*asset, to, *amount, reference),
            Message::RefundMint {
                redeemer,
                reference,
            } => refund_mint_data(redeemer, reference),
        }
    }

    /// Decodes a message's input; `None` for anything but `mint` and `refund_mint`, or trailing
    /// bytes.
    pub fn decode(data: &[u8]) -> Option<Message> {
        let (sel, mut rest) = (data.get(..4)?, data.get(4..)?);
        let m = if sel == selector("mint") {
            let (asset, to, amount, reference) =
                <(AssetId, AccountId32, u128, Reference)>::decode(&mut rest).ok()?;
            Message::Mint {
                asset,
                to,
                amount,
                reference,
            }
        } else if sel == selector("refund_mint") {
            let (redeemer, reference) = <(AccountId32, Reference)>::decode(&mut rest).ok()?;
            Message::RefundMint {
                redeemer,
                reference,
            }
        } else {
            return None;
        };
        rest.is_empty().then_some(m)
    }
}

/// The contract's errors, by their SCALE index (`contracts/bezent/src/types.rs`, `Error`).
pub const CONTRACT_ERRORS: &[&str] = &[
    "Unauthorised",
    "UnknownAsset",
    "Paused",
    "ReferenceUsed",
    "ZeroAmount",
    "AboveTransferMaximum",
    "AboveWindow",
    "AboveCeiling",
    "BelowMinimum",
    "PayoutDoesNotFitBackend",
    "BackendNotDeclared",
    "InvalidBackends",
    "SupplyOfAKnownAsset",
    "NoSuchRedemption",
    "AlreadyRefunded",
    "NotALowering",
    "InvalidBounds",
    "NoRaiseScheduled",
    "RaiseNotDue",
    "NoDelayScheduled",
    "DelayNotDue",
    "ZeroDelay",
    "DelayTooLong",
    "RolesNotDistinct",
    "InvalidRoleAccount",
    "GovernanceChangeIsDelayed",
    "NoGovernanceScheduled",
    "GovernanceNotDue",
    "Overflow",
    "Kreivo",
    "DepositMismatch",
    "MintMismatch",
    "BurnShort",
];

// ------------------------------------------------------------------------------ events

/// An ink! 5 event's signature topic: the BLAKE2b-256 of `Name(type,type,…)`, with the field
/// types as the contract's source writes them (`ink_macro` 5.1.1, `event::signature_topic`).
pub fn signature_topic(signature: &str) -> [u8; 32] {
    blake2_256(signature.as_bytes())
}

/// Every event the contract emits, by its signature as ink! hashes it.
pub const EVENT_SIGNATURES: &[(&str, &str)] = &[
    (
        "Minted",
        "Minted(EventSeq,Reference,AssetId,AccountId,Balance)",
    ),
    (
        "Redeemed",
        "Redeemed(EventSeq,Reference,AccountId,AssetId,Balance,Backend,Payout)",
    ),
    (
        "RefundMinted",
        "RefundMinted(EventSeq,Reference,AccountId,AssetId,Balance)",
    ),
    ("Paused", "Paused(EventSeq,PauseScope,AccountId)"),
    ("Unpaused", "Unpaused(EventSeq,PauseScope)"),
    ("BoundsSet", "BoundsSet(EventSeq,AssetId,Bounds)"),
    ("BackendsSet", "BackendsSet(EventSeq,AssetId,Vec<Backend>)"),
    ("SupplyCounted", "SupplyCounted(EventSeq,AssetId,Balance)"),
    (
        "RaiseScheduled",
        "RaiseScheduled(EventSeq,AssetId,AssetConfig,BlockNumber)",
    ),
    ("RaiseCancelled", "RaiseCancelled(EventSeq,AssetId)"),
    ("RolesSet", "RolesSet(EventSeq,Roles)"),
    (
        "DelayScheduled",
        "DelayScheduled(EventSeq,BlockNumber,BlockNumber)",
    ),
    (
        "GovernanceScheduled",
        "GovernanceScheduled(EventSeq,AccountId,BlockNumber)",
    ),
    ("DelaySet", "DelaySet(EventSeq,BlockNumber)"),
];

/// A contract event, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContractEvent {
    /// A mint to an order's destination.
    Minted {
        /// Sequence number.
        seq: u64,
        /// On-chain reference.
        reference: Reference,
        /// Asset.
        asset: AssetId,
        /// Destination.
        to: AccountId32,
        /// Amount.
        amount: u128,
    },
    /// A redemption: the amount was taken from the redeemer and destroyed.
    Redeemed {
        /// Sequence number.
        seq: u64,
        /// The redemption's reference, scoped by its redeemer.
        reference: Reference,
        /// The redeemer.
        redeemer: AccountId32,
        /// Asset.
        asset: AssetId,
        /// Amount destroyed.
        amount: u128,
        /// The backend that pays it out.
        backend: Backend,
        /// The payout instructions or their commitment.
        payout: Payout,
    },
    /// A refund mint to a redemption's redeemer.
    RefundMinted {
        /// Sequence number.
        seq: u64,
        /// The redemption's reference.
        reference: Reference,
        /// The redeemer.
        to: AccountId32,
        /// Asset.
        asset: AssetId,
        /// Amount.
        amount: u128,
    },
    /// A pause.
    Paused {
        /// Sequence number.
        seq: u64,
        /// What was paused.
        scope: PauseScope,
        /// Who paused it.
        by: AccountId32,
    },
    /// An unpause.
    Unpaused {
        /// Sequence number.
        seq: u64,
        /// What was unpaused.
        scope: PauseScope,
    },
    /// Bounds took effect.
    BoundsSet {
        /// Sequence number.
        seq: u64,
        /// Asset.
        asset: AssetId,
        /// Its bounds.
        bounds: Bounds,
    },
    /// Any other event of the contract (governance and scheduling), by name, with its bytes.
    Other {
        /// Sequence number.
        seq: u64,
        /// The event's name.
        name: &'static str,
        /// Its SCALE bytes.
        data: Vec<u8>,
    },
}

impl ContractEvent {
    /// Its sequence number.
    pub fn seq(&self) -> u64 {
        match self {
            ContractEvent::Minted { seq, .. }
            | ContractEvent::Redeemed { seq, .. }
            | ContractEvent::RefundMinted { seq, .. }
            | ContractEvent::Paused { seq, .. }
            | ContractEvent::Unpaused { seq, .. }
            | ContractEvent::BoundsSet { seq, .. }
            | ContractEvent::Other { seq, .. } => *seq,
        }
    }

    /// The kind of its observation, from the store's closed set (`observations.kind`, migration
    /// 0034; `REQ-OB-6`, `INV-2`): what the issuing network produces. An event the endpoint does
    /// not decode is `other`, which nothing binds.
    pub fn kind(&self) -> &'static str {
        match self {
            ContractEvent::Minted { .. } => "mint",
            ContractEvent::Redeemed { .. } => "redemption",
            ContractEvent::RefundMinted { .. } => "refund_mint",
            ContractEvent::Paused { .. }
            | ContractEvent::Unpaused { .. }
            | ContractEvent::BoundsSet { .. } => "control",
            ContractEvent::Other { .. } => "other",
        }
    }

    /// Its name.
    pub fn name(&self) -> &'static str {
        match self {
            ContractEvent::Minted { .. } => "Minted",
            ContractEvent::Redeemed { .. } => "Redeemed",
            ContractEvent::RefundMinted { .. } => "RefundMinted",
            ContractEvent::Paused { .. } => "Paused",
            ContractEvent::Unpaused { .. } => "Unpaused",
            ContractEvent::BoundsSet { .. } => "BoundsSet",
            ContractEvent::Other { name, .. } => name,
        }
    }

    /// Its SCALE bytes, as the contract emits them (the event's fields in order).
    pub fn encode_data(&self) -> Vec<u8> {
        match self {
            ContractEvent::Minted {
                seq,
                reference,
                asset,
                to,
                amount,
            } => (seq, reference, asset, to, amount).encode(),
            ContractEvent::Redeemed {
                seq,
                reference,
                redeemer,
                asset,
                amount,
                backend,
                payout,
            } => (seq, reference, redeemer, asset, amount, backend, payout).encode(),
            ContractEvent::RefundMinted {
                seq,
                reference,
                to,
                asset,
                amount,
            } => (seq, reference, to, asset, amount).encode(),
            ContractEvent::Paused { seq, scope, by } => (seq, scope, by).encode(),
            ContractEvent::Unpaused { seq, scope } => (seq, scope).encode(),
            ContractEvent::BoundsSet { seq, asset, bounds } => (seq, asset, bounds).encode(),
            ContractEvent::Other { data, .. } => data.clone(),
        }
    }

    /// Its topics, as ink! 5 emits them: the signature topic, then each `#[ink(topic)]` field
    /// (a value of 32 bytes or fewer is its own encoding, zero-padded).
    pub fn topics(&self) -> Vec<[u8; 32]> {
        let signature = EVENT_SIGNATURES
            .iter()
            .find(|(n, _)| *n == self.name())
            .map(|(_, s)| signature_topic(s))
            .unwrap_or_default();
        let mut topics = vec![signature];
        match self {
            ContractEvent::Minted { reference, .. } => topics.push(*reference),
            ContractEvent::Redeemed {
                reference,
                redeemer,
                ..
            } => topics.extend([*reference, *redeemer]),
            ContractEvent::RefundMinted { reference, to, .. } => {
                topics.extend([*reference, *to]);
            }
            _ => {}
        }
        topics
    }
}

/// Decodes one event the contract emitted, from its topics and data. An event whose signature
/// topic is not one of the contract's, or whose bytes do not decode exactly, is an error: never
/// guessed at (`REQ-NX-3`).
pub fn decode_event(topics: &[[u8; 32]], data: &[u8]) -> Result<ContractEvent, String> {
    let signature = topics
        .first()
        .ok_or("a contract event without a signature topic")?;
    let name = EVENT_SIGNATURES
        .iter()
        .find(|(_, s)| signature_topic(s) == *signature)
        .map(|(n, _)| *n)
        .ok_or_else(|| {
            format!(
                "a contract event with an unknown signature topic 0x{}",
                hex::encode(signature)
            )
        })?;
    let mut b = data;
    let bad = |e: parity_scale_codec::Error| format!("{name} does not decode: {e}");
    let event = match name {
        "Minted" => {
            let (seq, reference, asset, to, amount) =
                <(u64, Reference, AssetId, AccountId32, u128)>::decode(&mut b).map_err(bad)?;
            ContractEvent::Minted {
                seq,
                reference,
                asset,
                to,
                amount,
            }
        }
        "Redeemed" => {
            let (seq, reference, redeemer, asset, amount, backend, payout) =
                <(u64, Reference, AccountId32, AssetId, u128, Backend, Payout)>::decode(&mut b)
                    .map_err(bad)?;
            ContractEvent::Redeemed {
                seq,
                reference,
                redeemer,
                asset,
                amount,
                backend,
                payout,
            }
        }
        "RefundMinted" => {
            let (seq, reference, to, asset, amount) =
                <(u64, Reference, AccountId32, AssetId, u128)>::decode(&mut b).map_err(bad)?;
            ContractEvent::RefundMinted {
                seq,
                reference,
                to,
                asset,
                amount,
            }
        }
        "Paused" => {
            let (seq, scope, by) = <(u64, PauseScope, AccountId32)>::decode(&mut b).map_err(bad)?;
            ContractEvent::Paused { seq, scope, by }
        }
        "Unpaused" => {
            let (seq, scope) = <(u64, PauseScope)>::decode(&mut b).map_err(bad)?;
            ContractEvent::Unpaused { seq, scope }
        }
        "BoundsSet" => {
            let (seq, asset, bounds) = <(u64, AssetId, Bounds)>::decode(&mut b).map_err(bad)?;
            ContractEvent::BoundsSet { seq, asset, bounds }
        }
        other => {
            // Governance and scheduling events: their sequence number is read, the rest kept
            // as bytes.
            let seq = u64::decode(&mut b).map_err(bad)?;
            b = &[];
            ContractEvent::Other {
                seq,
                name: other,
                data: data.to_vec(),
            }
        }
    };
    if !b.is_empty() {
        return Err(format!("{name} has {} bytes past its end", b.len()));
    }
    Ok(event)
}

// ------------------------------------------------------------------------------ Contracts

/// A weight (`sp_weights::Weight`): both parts compact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct Weight {
    /// Computation time, in picoseconds.
    #[codec(compact)]
    pub ref_time: u64,
    /// Proof size, in bytes.
    #[codec(compact)]
    pub proof_size: u64,
}

impl Weight {
    /// `self` with `percent` / 100 of each part, rounded up; saturating.
    pub fn with_margin(self, percent: u64) -> Weight {
        let scale = |v: u64| {
            let v = u128::from(v) * u128::from(percent);
            u64::try_from(v.div_ceil(100)).unwrap_or(u64::MAX)
        };
        Weight {
            ref_time: scale(self.ref_time),
            proof_size: scale(self.proof_size),
        }
    }
}

/// What a `Contracts::call` pays for: its gas limit and storage deposit limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallLimits {
    /// The gas limit.
    pub gas_limit: Weight,
    /// The most it may put down as storage deposit; `None` for no limit.
    pub storage_deposit_limit: Option<u128>,
}

/// `Contracts::call { dest: MultiAddress::Id(contract), value: 0, gas_limit,
/// storage_deposit_limit, data }`, as `RuntimeCall` bytes.
pub fn contracts_call(contract: &AccountId32, limits: &CallLimits, data: &[u8]) -> Vec<u8> {
    let mut out = vec![CONTRACTS_PALLET, CONTRACTS_CALL, ADDRESS_ID];
    out.extend_from_slice(contract);
    Compact(0u128).encode_to(&mut out);
    limits.gas_limit.encode_to(&mut out);
    limits
        .storage_deposit_limit
        .map(Compact)
        .encode_to(&mut out);
    data.to_vec().encode_to(&mut out);
    out
}

/// A `Contracts::call` decoded: `(contract, value, limits, data)`. `None` for anything else.
pub fn decode_contracts_call(call: &[u8]) -> Option<(AccountId32, u128, CallLimits, Vec<u8>)> {
    let mut b = call;
    let decoded = decode_contracts_call_prefix(&mut b)?;
    b.is_empty().then_some(decoded)
}

/// A `Contracts::call` at the start of `b`, which is advanced past it.
fn decode_contracts_call_prefix(b: &mut &[u8]) -> Option<(AccountId32, u128, CallLimits, Vec<u8>)> {
    if u8::decode(b).ok()? != CONTRACTS_PALLET
        || u8::decode(b).ok()? != CONTRACTS_CALL
        || u8::decode(b).ok()? != ADDRESS_ID
    {
        return None;
    }
    let dest = <[u8; 32]>::decode(b).ok()?;
    let value = Compact::<u128>::decode(b).ok()?.0;
    let gas_limit = Weight::decode(b).ok()?;
    let storage_deposit_limit = Option::<Compact<u128>>::decode(b).ok()?.map(|c| c.0);
    let data = Vec::<u8>::decode(b).ok()?;
    Some((
        dest,
        value,
        CallLimits {
            gas_limit,
            storage_deposit_limit,
        },
        data,
    ))
}

/// The arguments of `ContractsApi_call(origin, dest, value, gas_limit, storage_deposit_limit,
/// input_data)`: a dry run of `input` at a block, with no gas or deposit limit.
pub fn contracts_api_call_args(origin: &AccountId32, dest: &AccountId32, input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    origin.encode_to(&mut out);
    dest.encode_to(&mut out);
    0u128.encode_to(&mut out);
    None::<Weight>.encode_to(&mut out);
    None::<u128>.encode_to(&mut out);
    input.to_vec().encode_to(&mut out);
    out
}

/// What a dry run of a message found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DryRun {
    /// The gas the runtime says the call requires.
    pub gas_required: Weight,
    /// The storage deposit it would put down (zero for a refund).
    pub storage_deposit: u128,
    /// Whether it would apply, and if not, why.
    pub outcome: DryRunOutcome,
}

/// Whether a dry run applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DryRunOutcome {
    /// The message returned `Ok(())`.
    Passed,
    /// The contract refused it, with this error.
    Refused(String),
    /// The call failed to dispatch (trapped, out of gas, …), or returned something else.
    Failed(String),
}

impl DryRun {
    /// Whether it passed.
    pub fn passed(&self) -> bool {
        self.outcome == DryRunOutcome::Passed
    }

    /// The call's limits: what it required, with a margin (`percent` / 100).
    pub fn limits(&self, percent: u64) -> CallLimits {
        CallLimits {
            gas_limit: self.gas_required.with_margin(percent),
            storage_deposit_limit: Some(
                self.storage_deposit
                    .saturating_mul(u128::from(percent))
                    .div_ceil(100),
            ),
        }
    }

    /// As JSON, for the record.
    pub fn to_json(&self) -> serde_json::Value {
        let (outcome, reason) = match &self.outcome {
            DryRunOutcome::Passed => ("passed", None),
            DryRunOutcome::Refused(e) => ("refused", Some(e.clone())),
            DryRunOutcome::Failed(e) => ("failed", Some(e.clone())),
        };
        serde_json::json!({
            "outcome": outcome,
            "reason": reason,
            "gas_required": {"ref_time": self.gas_required.ref_time, "proof_size": self.gas_required.proof_size},
            "storage_deposit": self.storage_deposit.to_string(),
        })
    }
}

/// A redemption as the contract's `redemption(redeemer, reference)` returns it
/// (`contracts/bezent/src/types.rs`, `Redemption`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Redemption {
    /// Who redeemed.
    pub redeemer: AccountId32,
    /// The asset.
    pub asset: AssetId,
    /// The amount destroyed.
    pub amount: u128,
    /// The backend that pays it out.
    pub backend: Backend,
    /// The payout instructions or their commitment.
    pub payout: Payout,
    /// Whether a refund mint refunded it. Set once, never cleared.
    pub refunded: bool,
}

/// The input of the contract's `mint_reference_used(reference)` (review M1-bis, `INV-8`).
pub fn mint_reference_used_data(reference: &Reference) -> Vec<u8> {
    let mut out = selector("mint_reference_used").to_vec();
    reference.encode_to(&mut out);
    out
}

/// The input of the contract's `supply(asset)`: the asset's supply as the contract counts it
/// (issue #42).
pub fn supply_data(asset: AssetId) -> Vec<u8> {
    let mut out = selector("supply").to_vec();
    asset.encode_to(&mut out);
    out
}

/// The input of the contract's `redemption(redeemer, reference)`.
pub fn redemption_data(redeemer: &AccountId32, reference: &Reference) -> Vec<u8> {
    let mut out = selector("redemption").to_vec();
    (redeemer, reference).encode_to(&mut out);
    out
}

/// What a read-only message returned, from its `ContractsApi_call` result: the SCALE bytes of
/// its value, `Result<T, LangError>` unwrapped. A failed dispatch, a revert or a `LangError` is
/// `Err`: never read as an answer.
pub fn decode_getter(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut b = bytes;
    let bad = |what: &str| format!("the contract's answer does not decode: {what}");
    Weight::decode(&mut b).map_err(|_| bad("gas consumed"))?;
    Weight::decode(&mut b).map_err(|_| bad("gas required"))?;
    u8::decode(&mut b).map_err(|_| bad("storage deposit"))?;
    u128::decode(&mut b).map_err(|_| bad("storage deposit"))?;
    Vec::<u8>::decode(&mut b).map_err(|_| bad("debug message"))?;
    if u8::decode(&mut b).map_err(|_| bad("result"))? != 0 {
        return Err("the contract's getter failed to dispatch".into());
    }
    let flags = u32::decode(&mut b).map_err(|_| bad("flags"))?;
    let data = Vec::<u8>::decode(&mut b).map_err(|_| bad("data"))?;
    match (flags & 1, data.split_first()) {
        (0, Some((0, value))) => Ok(value.to_vec()),
        _ => Err(format!(
            "the contract's getter returned 0x{} (flags {flags})",
            hex::encode(&data)
        )),
    }
}

/// `T`, SCALE, with no trailing bytes.
pub fn decode_exact<T: Decode>(bytes: &[u8]) -> Option<T> {
    let mut b = bytes;
    let v = T::decode(&mut b).ok()?;
    b.is_empty().then_some(v)
}

/// A `ContractsApi_call` result (`ContractResult { gas_consumed, gas_required, storage_deposit,
/// debug_message, result, events }`, `pallet-contracts` 49) of a message that returns
/// `Result<(), Error>`, decoded.
pub fn decode_dry_run(bytes: &[u8]) -> Result<DryRun, String> {
    let mut b = bytes;
    let bad = |what: &str| format!("the dry run does not decode: {what}");
    Weight::decode(&mut b).map_err(|_| bad("gas consumed"))?;
    let gas_required = Weight::decode(&mut b).map_err(|_| bad("gas required"))?;
    // StorageDeposit::{Refund = 0, Charge = 1}(Balance).
    let charged = u8::decode(&mut b).map_err(|_| bad("storage deposit"))? == 1;
    let deposit = u128::decode(&mut b).map_err(|_| bad("storage deposit"))?;
    Vec::<u8>::decode(&mut b).map_err(|_| bad("debug message"))?;
    let outcome = match u8::decode(&mut b).map_err(|_| bad("result"))? {
        0 => {
            let flags = u32::decode(&mut b).map_err(|_| bad("flags"))?;
            let data = Vec::<u8>::decode(&mut b).map_err(|_| bad("data"))?;
            let reverted = flags & 1 == 1;
            // Result<Result<(), Error>, LangError>.
            match (reverted, data.as_slice()) {
                (false, [0, 0]) => DryRunOutcome::Passed,
                (true, [0, 1, index, ..]) => DryRunOutcome::Refused(
                    CONTRACT_ERRORS
                        .get(usize::from(*index))
                        .map_or_else(|| format!("error {index}"), |e| (*e).to_owned()),
                ),
                (_, [1, ..]) => {
                    DryRunOutcome::Failed("the contract refused to dispatch the message".into())
                }
                _ => DryRunOutcome::Failed(format!(
                    "the message returned 0x{} (flags {flags})",
                    hex::encode(&data)
                )),
            }
        }
        _ => DryRunOutcome::Failed(format!(
            "the call failed to dispatch: 0x{}",
            hex::encode(&b[..b.len().min(64)])
        )),
    };
    Ok(DryRun {
        gas_required,
        storage_deposit: if charged { deposit } else { 0 },
        outcome,
    })
}

/// A `ContractsApi_call` result's bytes, as the runtime writes them: for the in-memory chain and
/// the tests. `returned` is `Ok(data)` with the REVERT flag when `reverted`, or `Err(())` for a
/// failed dispatch.
pub fn encode_dry_run(
    gas_required: Weight,
    storage_deposit: u128,
    returned: Result<(bool, Vec<u8>), ()>,
) -> Vec<u8> {
    let mut out = Vec::new();
    gas_required.encode_to(&mut out);
    gas_required.encode_to(&mut out);
    1u8.encode_to(&mut out);
    storage_deposit.encode_to(&mut out);
    Vec::<u8>::new().encode_to(&mut out);
    match returned {
        Ok((reverted, data)) => {
            0u8.encode_to(&mut out);
            u32::from(reverted).encode_to(&mut out);
            data.encode_to(&mut out);
        }
        Err(()) => {
            // DispatchError::Other.
            out.extend_from_slice(&[1, 0]);
        }
    }
    // events: None.
    0u8.encode_to(&mut out);
    out
}

// ------------------------------------------------------------------------------ eras

/// A mortal era, as Substrate's `CheckMortality` computes it: valid in blocks `[birth, death)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mortality {
    /// The first block it is valid in.
    pub birth: u64,
    /// Its length, a power of two.
    pub period: u64,
}

impl Mortality {
    /// The era Substrate builds for `period` blocks from block `current` (`Era::mortal`).
    pub fn at(period: u64, current: u64) -> Mortality {
        let period = period
            .checked_next_power_of_two()
            .unwrap_or(1 << 16)
            .clamp(4, 1 << 16);
        let phase = current % period;
        let quantize = (period >> 12).max(1);
        let phase = phase / quantize * quantize;
        let birth = (current.max(phase) - phase) / period * period + phase;
        Mortality { birth, period }
    }

    /// The era the endpoint signs with (`DEC-5`): [`MAX_ERA`] blocks, born at the finalized head
    /// `finalized`. Every era of at most 4 096 blocks is born where it is built, so its birth is
    /// the finalized head, whose hash the endpoint already holds and which no reorganisation
    /// retracts.
    pub fn born_at(finalized: u64) -> Mortality {
        Mortality::at(MAX_ERA, finalized)
    }

    /// The first block it is no longer valid in.
    pub const fn death(&self) -> u64 {
        self.birth + self.period
    }

    /// The last block it can be included in.
    pub const fn last_valid(&self) -> u64 {
        self.death() - 1
    }

    /// Its two bytes (`sp_runtime::generic::Era::Mortal`).
    pub fn bytes(&self) -> [u8; 2] {
        let period = self.period;
        let phase = self.birth % period;
        let quantize = (period >> 12).max(1);
        let low = u64::from(period.trailing_zeros().saturating_sub(1).clamp(1, 15));
        let encoded = low | ((phase / quantize) << 4);
        u16::try_from(encoded).unwrap_or(u16::MAX).to_le_bytes()
    }

    /// `(period, phase)` from an era's two bytes.
    pub fn period_phase(bytes: [u8; 2]) -> (u64, u64) {
        let encoded = u64::from(u16::from_le_bytes(bytes));
        let period = 2u64 << (encoded % (1 << 4));
        let quantize = (period >> 12).max(1);
        let phase = (encoded >> 4) * quantize;
        (period, phase)
    }

    /// The era's birth for an extrinsic seen at block `current` (`Era::birth`).
    pub fn birth_at(bytes: [u8; 2], current: u64) -> u64 {
        let (period, phase) = Self::period_phase(bytes);
        (current.max(phase) - phase) / period * period + phase
    }
}

// ------------------------------------------------------------------------------ transactions

/// What the transaction binds besides its call: everything its extensions check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxParams {
    /// The genesis hash (`CheckGenesis`).
    pub genesis: [u8; 32],
    /// `CheckSpecVersion`.
    pub spec_version: u32,
    /// `CheckTxVersion`.
    pub transaction_version: u32,
    /// `CheckEra`.
    pub era: Mortality,
    /// The hash of the era's birth block (`CheckEra`'s implicit).
    pub birth_hash: [u8; 32],
    /// `CheckNonce` (Kreivo's `Nonce` is `u32`).
    pub nonce: u32,
    /// The tip, in the native asset. The endpoint tips nothing.
    pub tip: u128,
}

/// The extensions' explicit data, in order.
pub fn extra(p: &TxParams) -> Vec<u8> {
    let mut out = vec![0u8]; // PassAuthenticate: None.
    // CheckNonZeroSender, CheckSpecVersion, CheckTxVersion, CheckGenesis: ().
    out.extend_from_slice(&p.era.bytes());
    Compact(p.nonce).encode_to(&mut out);
    // CheckWeight: (). SkipCheckIfFeeless<ChargeGasTxPayment<ChargeAssetTxPayment>>:
    // `{ #[codec(compact)] tip, asset_id: Option<_> }`, with no fee asset.
    Compact(p.tip).encode_to(&mut out);
    out.push(0);
    out
}

/// The extensions' implicit data, in order (`PassAuthenticate` has none).
pub fn implicit(p: &TxParams) -> Vec<u8> {
    let mut out = Vec::new();
    p.spec_version.encode_to(&mut out);
    p.transaction_version.encode_to(&mut out);
    out.extend_from_slice(&p.genesis);
    out.extend_from_slice(&p.birth_hash);
    out
}

/// The signed payload: `call ‖ extra ‖ implicit`.
pub fn signing_payload(call: &[u8], p: &TxParams) -> Vec<u8> {
    let mut out = call.to_vec();
    out.extend_from_slice(&extra(p));
    out.extend_from_slice(&implicit(p));
    out
}

/// What the key signs for `payload`: the payload, or its BLAKE2b-256 when it is longer than 256
/// bytes. The signer signs the intent's `message` as it is (F-005 NOTES, N7), so the endpoint
/// hashes here.
pub fn message_to_sign(payload: &[u8]) -> Vec<u8> {
    if payload.len() > MAX_UNHASHED_PAYLOAD {
        blake2_256(payload).to_vec()
    } else {
        payload.to_vec()
    }
}

/// The extrinsic, as broadcast.
pub fn extrinsic(
    account: &AccountId32,
    scheme: Scheme,
    signature: &[u8],
    p: &TxParams,
    call: &[u8],
) -> Vec<u8> {
    let mut body = vec![SIGNED_V4, ADDRESS_ID];
    body.extend_from_slice(account);
    body.push(scheme.variant());
    body.extend_from_slice(signature);
    body.extend_from_slice(&extra(p));
    body.extend_from_slice(call);
    let mut out = Compact(u32::try_from(body.len()).unwrap_or(u32::MAX)).encode();
    out.extend_from_slice(&body);
    out
}

/// A signing payload the endpoint builds, decoded whole (`REQ-EO-1`; PLAN `DEC-3`: the endpoint
/// decodes the bytes to check they match the order): the call, every extension's explicit data and
/// every implicit. Only the format of [`signing_payload`] decodes: a `Contracts::call`,
/// `PassAuthenticate: None`, a mortal era, no fee asset, and nothing left over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedPayload {
    /// The call's bytes.
    pub call: Vec<u8>,
    /// The call's contract.
    pub contract: AccountId32,
    /// The value it transfers.
    pub value: u128,
    /// Its gas and storage deposit limits.
    pub limits: CallLimits,
    /// The contract message's data.
    pub data: Vec<u8>,
    /// The era's two bytes (mortal).
    pub era: [u8; 2],
    /// The nonce.
    pub nonce: u32,
    /// The tip.
    pub tip: u128,
    /// `CheckSpecVersion`'s implicit.
    pub spec_version: u32,
    /// `CheckTxVersion`'s implicit.
    pub transaction_version: u32,
    /// `CheckGenesis`'s implicit.
    pub genesis: [u8; 32],
    /// `CheckEra`'s implicit: the hash of the era's birth block.
    pub birth_hash: [u8; 32],
}

/// Decodes a signing payload whole; `None` for anything but the format of [`signing_payload`].
pub fn decode_signing_payload(payload: &[u8]) -> Option<SignedPayload> {
    let mut b = payload;
    let (contract, value, limits, data) = decode_contracts_call_prefix(&mut b)?;
    let call = payload[..payload.len() - b.len()].to_vec();
    if u8::decode(&mut b).ok()? != 0 {
        return None; // PassAuthenticate: Some
    }
    let era = <[u8; 2]>::decode(&mut b).ok()?;
    if era[0] == 0 {
        return None; // immortal
    }
    let nonce = Compact::<u32>::decode(&mut b).ok()?.0;
    let tip = Compact::<u128>::decode(&mut b).ok()?.0;
    if u8::decode(&mut b).ok()? != 0 {
        return None; // a fee asset
    }
    let spec_version = u32::decode(&mut b).ok()?;
    let transaction_version = u32::decode(&mut b).ok()?;
    let genesis = <[u8; 32]>::decode(&mut b).ok()?;
    let birth_hash = <[u8; 32]>::decode(&mut b).ok()?;
    b.is_empty().then_some(SignedPayload {
        call,
        contract,
        value,
        limits,
        data,
        era,
        nonce,
        tip,
        spec_version,
        transaction_version,
        genesis,
        birth_hash,
    })
}

/// A signed extrinsic, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    /// The sender.
    pub account: AccountId32,
    /// The signature's scheme.
    pub scheme: Scheme,
    /// The signature.
    pub signature: Vec<u8>,
    /// The era's two bytes.
    pub era: [u8; 2],
    /// The nonce.
    pub nonce: u32,
    /// The tip.
    pub tip: u128,
    /// The call.
    pub call: Vec<u8>,
}

impl Decoded {
    /// Whether `other` is the same transaction, whatever its signature: the same sender, era,
    /// nonce, tip and call, so the same signed payload on this chain (`REQ-NX-1`: a re-signed copy
    /// is never "a different transaction").
    pub fn same_payload(&self, other: &Decoded) -> bool {
        self.account == other.account
            && self.era == other.era
            && self.nonce == other.nonce
            && self.tip == other.tip
            && self.call == other.call
    }
}

/// The sender of a signed extrinsic whose address is `MultiAddress::Id`, read even when the rest
/// does not decode; `None` for an unsigned extrinsic or another kind of address.
pub fn signed_sender(bytes: &[u8]) -> Option<AccountId32> {
    let mut b = bytes;
    Compact::<u32>::decode(&mut b).ok()?;
    let version = u8::decode(&mut b).ok()?;
    if version & 0x80 == 0 || u8::decode(&mut b).ok()? != ADDRESS_ID {
        return None;
    }
    <[u8; 32]>::decode(&mut b).ok()
}

/// Decodes a signed extrinsic of the format above; `None` for anything else.
pub fn decode_extrinsic(bytes: &[u8]) -> Option<Decoded> {
    let mut b = bytes;
    let len = Compact::<u32>::decode(&mut b).ok()?.0;
    if usize::try_from(len).ok()? != b.len() {
        return None;
    }
    if u8::decode(&mut b).ok()? != SIGNED_V4 || u8::decode(&mut b).ok()? != ADDRESS_ID {
        return None;
    }
    let account = <[u8; 32]>::decode(&mut b).ok()?;
    let scheme = match u8::decode(&mut b).ok()? {
        1 => Scheme::Sr25519,
        2 => Scheme::Ecdsa,
        _ => return None,
    };
    let signature = b.get(..scheme.signature_len())?.to_vec();
    b = &b[scheme.signature_len()..];
    if u8::decode(&mut b).ok()? != 0 {
        return None; // PassAuthenticate: Some
    }
    let era = <[u8; 2]>::decode(&mut b).ok()?;
    let nonce = Compact::<u32>::decode(&mut b).ok()?.0;
    let tip = Compact::<u128>::decode(&mut b).ok()?.0;
    if u8::decode(&mut b).ok()? != 0 {
        return None; // a fee asset
    }
    Some(Decoded {
        account,
        scheme,
        signature,
        era,
        nonce,
        tip,
        call: b.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The signature topics `contracts/bezent` emits, as ink! 5.1.1 computes them
    /// (`<Event as ink::env::Event>::SIGNATURE_TOPIC`, printed from the contract crate built at
    /// this branch's base). A change to an event's fields must change both.
    #[test]
    fn req_hn_7_signature_topics_are_the_contracts() {
        let pinned: &[(&str, &str)] = &[
            (
                "Minted",
                "3de1a43d342f1f7dfde367f38a4372fce3bce321de372fe7c3de1f817ed19065",
            ),
            (
                "Redeemed",
                "be26cc5f87fbbc93cbc0489ba1fa0b5888e0a90e09b22f3ac8cedfdd45de70d7",
            ),
            (
                "RefundMinted",
                "dc4ebbe56a1648ca16bc14c50bc9cbf6a3a2ed13f25d7a06685ceb0f74889c8f",
            ),
            (
                "Paused",
                "d21324ba4ef5d05bf736fc59495cb5c54f5f68e615892b10392665553f52c28e",
            ),
            (
                "Unpaused",
                "7b4ae63845db6f472dc1f53bf9369b5a7e24453fc7d0dc08dc2139b53a94c52a",
            ),
            (
                "BoundsSet",
                "b772e825cb2b824b0655cb3415103c85d85b2096a072240782c20548d0ea912f",
            ),
            (
                "BackendsSet",
                "73885dc8ad85b653b65bcb9dd0469d9e45c3b0baaee346e7f3ffc053a4118b1f",
            ),
            (
                "SupplyCounted",
                "83f54a0682ee1921ef7106bb2c5d385815abb28872606045d6cf6b074ddc61c6",
            ),
            (
                "RaiseScheduled",
                "a9384dfcb0fffccb5717223b9f2a9676d4f25ee5348d73ce8e70fad421e7499d",
            ),
            (
                "RaiseCancelled",
                "70a057368a3a21a862b77006e9875a200eeef2a421d142c20fa7223e327950cf",
            ),
            (
                "RolesSet",
                "3e904c82b0e39d3451eca8d4fc927490845b1584df3b54009aef678c432884e0",
            ),
            (
                "DelayScheduled",
                "027cd4cef348a01e52f1a64f5e14189c99495b95046f52c99be92743d5f3baaf",
            ),
            (
                "GovernanceScheduled",
                "83681059573567e55bd93115d32d0b63bacbc6bf4ae5c6ca4893a8040570b8fa",
            ),
            (
                "DelaySet",
                "a475749a28cad352488640cb4cbc44d616558543b994b89b9422553fd12ef49b",
            ),
        ];
        assert_eq!(pinned.len(), EVENT_SIGNATURES.len());
        for (name, hex_topic) in pinned {
            let (_, sig) = EVENT_SIGNATURES.iter().find(|(n, _)| n == name).unwrap();
            assert_eq!(hex::encode(signature_topic(sig)), *hex_topic, "{name}");
        }
    }

    #[test]
    fn dec_5_an_era_born_at_the_finalized_head_lasts_64_blocks() {
        for f in [0u64, 1, 63, 64, 1000, 6_543_210] {
            let m = Mortality::born_at(f);
            assert_eq!((m.birth, m.period, m.last_valid()), (f, 64, f + 63));
            let bytes = m.bytes();
            assert_eq!(Mortality::period_phase(bytes), (64, f % 64));
            // Seen anywhere in its window, its birth is the same block.
            for seen in [f, f + 30, f + 63] {
                assert_eq!(Mortality::birth_at(bytes, seen), f);
            }
        }
        // sp_runtime's own vectors: Era::mortal(64, 42) has phase 42; its encoding is 0xa5 0x02.
        assert_eq!(Mortality::at(64, 42).bytes(), [0xa5, 0x02]);
    }

    #[test]
    fn dec_6_messages_round_trip_by_selector() {
        let m = Message::Mint {
            asset: AssetId::Here(101),
            to: [7; 32],
            amount: 1_000_000,
            reference: [9; 32],
        };
        assert_eq!(Message::decode(&m.data()), Some(m.clone()));
        let r = Message::RefundMint {
            redeemer: [3; 32],
            reference: [4; 32],
        };
        assert_eq!(Message::decode(&r.data()), Some(r));
        let mut trailing = m.data();
        trailing.push(0);
        assert_eq!(Message::decode(&trailing), None);
        // 4 + (1 + 4) + 32 + 16 + 32
        assert_eq!(m.data().len(), 89);
    }

    #[test]
    fn req_hn_7_every_event_decodes_from_its_topics_with_its_sequence_number() {
        let events = [
            ContractEvent::Minted {
                seq: 4,
                reference: [1; 32],
                asset: AssetId::Here(101),
                to: [2; 32],
                amount: 5,
            },
            ContractEvent::Redeemed {
                seq: 5,
                reference: [1; 32],
                redeemer: [3; 32],
                asset: AssetId::Here(101),
                amount: 9,
                backend: Backend::Evm { chain_id: 137 },
                payout: Payout::Address([8; 20]),
            },
            ContractEvent::RefundMinted {
                seq: 6,
                reference: [1; 32],
                to: [3; 32],
                asset: AssetId::Here(101),
                amount: 9,
            },
            ContractEvent::Paused {
                seq: 7,
                scope: PauseScope::AssetDirection(AssetId::Here(101), Direction::Mint),
                by: [4; 32],
            },
        ];
        for e in events {
            let decoded = decode_event(&e.topics(), &e.encode_data()).unwrap();
            assert_eq!(decoded, e);
        }
        // An unknown signature topic and trailing bytes are errors, never guesses (REQ-NX-3).
        assert!(decode_event(&[[0; 32]], &[0; 8]).is_err());
        let e = ContractEvent::Unpaused {
            seq: 1,
            scope: PauseScope::All,
        };
        let mut data = e.encode_data();
        data.push(1);
        assert!(decode_event(&e.topics(), &data).is_err());
    }

    #[test]
    fn dec_5_a_dry_run_passes_only_on_ok() {
        let w = Weight {
            ref_time: 1_000,
            proof_size: 2_000,
        };
        let passed = decode_dry_run(&encode_dry_run(w, 10, Ok((false, vec![0, 0])))).unwrap();
        assert!(passed.passed());
        assert_eq!(passed.limits(110).gas_limit.proof_size, 2_200);
        assert_eq!(passed.limits(110).storage_deposit_limit, Some(11));
        let refused = decode_dry_run(&encode_dry_run(w, 0, Ok((true, vec![0, 1, 3])))).unwrap();
        assert_eq!(
            refused.outcome,
            DryRunOutcome::Refused("ReferenceUsed".into())
        );
        let trapped = decode_dry_run(&encode_dry_run(w, 0, Err(()))).unwrap();
        assert!(matches!(trapped.outcome, DryRunOutcome::Failed(_)));
        assert!(decode_dry_run(&[1, 2]).is_err());
    }

    #[test]
    fn req_hn_3_an_extrinsic_round_trips_and_binds_its_era() {
        let p = TxParams {
            genesis: [1; 32],
            spec_version: 134,
            transaction_version: 13,
            era: Mortality::born_at(1000),
            birth_hash: [2; 32],
            nonce: 7,
            tip: 0,
        };
        let call = contracts_call(
            &[5; 32],
            &CallLimits {
                gas_limit: Weight {
                    ref_time: 9,
                    proof_size: 10,
                },
                storage_deposit_limit: Some(11),
            },
            &mint_data(AssetId::Here(101), &[6; 32], 1, &[7; 32]),
        );
        let x = extrinsic(&[3; 32], Scheme::Sr25519, &[4; 64], &p, &call);
        let d = decode_extrinsic(&x).unwrap();
        assert_eq!(d.account, [3; 32]);
        assert_eq!(d.nonce, 7);
        assert_eq!(d.call, call);
        assert_eq!(Mortality::birth_at(d.era, 1063), 1000);
        let (dest, value, limits, data) = decode_contracts_call(&d.call).unwrap();
        assert_eq!(
            (dest, value, limits.storage_deposit_limit),
            ([5; 32], 0, Some(11))
        );
        assert!(Message::decode(&data).is_some());
        // The payload of a mint is longer than 256 bytes or not; either way the key signs at most
        // 256 bytes.
        let payload = signing_payload(&call, &p);
        assert!(message_to_sign(&payload).len() <= MAX_UNHASHED_PAYLOAD);
        assert_eq!(
            message_to_sign(&[0; 257]),
            blake2_256(&[0; 257]).to_vec(),
            "hashed past 256 bytes"
        );
        assert_eq!(message_to_sign(&[0; 256]), vec![0; 256]);
    }

    #[test]
    fn req_eo_1_a_signing_payload_decodes_whole_or_not_at_all() {
        let limits = CallLimits {
            gas_limit: Weight {
                ref_time: 7,
                proof_size: 9,
            },
            storage_deposit_limit: Some(11),
        };
        let call = contracts_call(&[5; 32], &limits, &refund_mint_data(&[1; 32], &[2; 32]));
        let p = TxParams {
            genesis: [1; 32],
            spec_version: 134,
            transaction_version: 13,
            era: Mortality::born_at(500),
            birth_hash: [2; 32],
            nonce: 300,
            tip: 4,
        };
        let payload = signing_payload(&call, &p);
        let d = decode_signing_payload(&payload).unwrap();
        assert_eq!(
            (d.call.as_slice(), d.contract, d.value, d.limits),
            (call.as_slice(), [5; 32], 0, limits)
        );
        assert_eq!((d.era, d.nonce, d.tip), (p.era.bytes(), 300, 4));
        assert_eq!(
            (
                d.spec_version,
                d.transaction_version,
                d.genesis,
                d.birth_hash
            ),
            (134, 13, [1; 32], [2; 32])
        );
        // A byte left over, a byte short, or an immortal era: nothing.
        assert_eq!(
            decode_signing_payload(&[payload.clone(), vec![0]].concat()),
            None
        );
        assert_eq!(decode_signing_payload(&payload[..payload.len() - 1]), None);
        let mut immortal = call.clone();
        immortal.extend_from_slice(&[0, 0, 0, 0, 0]);
        immortal.extend_from_slice(&implicit(&p));
        assert_eq!(decode_signing_payload(&immortal), None);
    }
}
