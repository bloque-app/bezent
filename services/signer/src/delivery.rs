// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The exact bytes the signer is asked to sign, decoded by the signer itself (the audit of PR #56,
//! must-fix 2; `REQ-AR-3`, `INV-9`, `DEC-3`, issue #23).
//!
//! The signer used to check the bytes only against the endpoint's own preparation record: a
//! compromised endpoint could prepare a mint or a payout of any amount to any recipient, and the
//! signer signed it, bounded only on chain. Now the signer decodes the bytes, strictly (a
//! transaction decodes only when encoding what was decoded gives the same bytes, so what is
//! compared is exactly what executes), and the caller compares what they say with the core's
//! delivery order. This module holds the formats, both ways: the decoders the signer uses, and the
//! encoders that build the same bytes (the endpoints build them with their own code; the tests and
//! the sidecar's stand-in build them with these).
//!
//! **EVM** (F-008, F-009): an EIP-1559 signing payload, `0x02 ‖ rlp([chain id, nonce, priority fee,
//! max fee, gas, to, value, data, access list])`. A payout is a call to the vault's Roles modifier:
//!
//! ```text
//! Roles.execTransactionWithRole(MultiSendCallOnly, 0,
//!     multiSend( entry(call, token,   0, transfer(beneficiary, amount))
//!              ‖ entry(call, emitter, 0, emitReference(reference)) ),
//!     DelegateCall, roleKey, true)
//! ```
//!
//! A cancel is a transfer of nothing to the key's own address ([`crate::cancel`]).
//!
//! **Kreivo** (`DEC-5`, `DEC-6`): a signed payload `call ‖ extra ‖ implicit`, where the call is
//! `Contracts::call { dest: Id(contract), value, gas_limit, storage_deposit_limit, data }` and the
//! data is the contract's `mint(asset, to, amount, reference)` or `refund_mint(redeemer,
//! reference)`; `extra = 0x00 ‖ era ‖ compact(nonce) ‖ compact(tip) ‖ 0x00` and `implicit =
//! spec_version ‖ transaction_version ‖ genesis ‖ birth hash`. The key signs the payload, or its
//! BLAKE2b-256 when it is longer than 256 bytes.

use sha3::{Digest, Keccak256};

// ------------------------------------------------------------------------------ hashes

/// Keccak-256.
pub fn keccak256(bytes: &[u8]) -> [u8; 32] {
    Keccak256::digest(bytes).into()
}

/// BLAKE2b-256: Kreivo's hash.
pub fn blake2_256(bytes: &[u8]) -> [u8; 32] {
    use blake2::Digest as _;
    blake2::Blake2b::<blake2::digest::consts::U32>::digest(bytes).into()
}

fn selector(signature: &str) -> [u8; 4] {
    let h = keccak256(signature.as_bytes());
    [h[0], h[1], h[2], h[3]]
}

// ------------------------------------------------------------------------------ RLP

/// One canonical RLP item at the head of `b`: (whether it is a list, its payload, the rest).
fn item(b: &[u8]) -> Option<(bool, &[u8], &[u8])> {
    let (&h, rest) = b.split_first()?;
    let (list, start, len) = match h {
        0x00..=0x7f => return Some((false, &b[..1], rest)),
        0x80..=0xb7 => (false, 1, usize::from(h - 0x80)),
        0xb8..=0xbf | 0xf8..=0xff => {
            let base = if h >= 0xf8 { 0xf7 } else { 0xb7 };
            let n = usize::from(h - base);
            let digits = rest.get(..n)?;
            // Canonical: no leading zero, and a long form only past 55 bytes.
            if digits.first() == Some(&0) || n > std::mem::size_of::<usize>() {
                return None;
            }
            let len = digits
                .iter()
                .fold(0usize, |a, &d| (a << 8) | usize::from(d));
            if len < 56 {
                return None;
            }
            (h >= 0xf8, 1 + n, len)
        }
        0xc0..=0xf7 => (true, 1, usize::from(h - 0xc0)),
    };
    let end = start.checked_add(len)?;
    let payload = b.get(start..end)?;
    // Canonical: a single byte below 0x80 is its own encoding.
    if !list && len == 1 && payload[0] < 0x80 {
        return None;
    }
    Some((list, payload, b.get(end..)?))
}

/// A canonical RLP unsigned integer of at most 16 bytes.
fn uint(payload: &[u8]) -> Option<u128> {
    if payload.len() > 16 || payload.first() == Some(&0) {
        return None;
    }
    Some(payload.iter().fold(0u128, |a, &d| (a << 8) | u128::from(d)))
}

fn rlp_header(list: bool, len: usize, out: &mut Vec<u8>) {
    let (short, long) = if list { (0xc0u8, 0xf7u8) } else { (0x80, 0xb7) };
    if len < 56 {
        out.push(short + u8::try_from(len).unwrap_or(0));
    } else {
        let be = len.to_be_bytes();
        let digits = &be[be.iter().position(|&d| d != 0).unwrap_or(be.len() - 1)..];
        out.push(long + u8::try_from(digits.len()).unwrap_or(0));
        out.extend_from_slice(digits);
    }
}

fn rlp_bytes(b: &[u8], out: &mut Vec<u8>) {
    if b.len() == 1 && b[0] < 0x80 {
        out.push(b[0]);
    } else {
        rlp_header(false, b.len(), out);
        out.extend_from_slice(b);
    }
}

fn rlp_uint(n: u128, out: &mut Vec<u8>) {
    let be = n.to_be_bytes();
    let digits = &be[be.iter().position(|&d| d != 0).unwrap_or(be.len())..];
    rlp_bytes(digits, out);
}

// ------------------------------------------------------------------------------ EVM

/// An EIP-1559 transaction as the signer reads it: every field of its signing payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvmTx {
    /// The chain id.
    pub chain_id: u64,
    /// The nonce.
    pub nonce: u64,
    /// `maxPriorityFeePerGas`, in wei.
    pub priority_fee: u128,
    /// `maxFeePerGas`, in wei.
    pub max_fee: u128,
    /// The gas limit.
    pub gas: u64,
    /// The target (never a contract creation).
    pub to: [u8; 20],
    /// The value, in wei.
    pub value: u128,
    /// The calldata.
    pub data: Vec<u8>,
}

/// The signing payload of `tx`: `0x02 ‖ rlp([…, []])`, with an empty access list.
pub fn evm_signing_payload(tx: &EvmTx) -> Vec<u8> {
    let mut body = Vec::new();
    rlp_uint(u128::from(tx.chain_id), &mut body);
    rlp_uint(u128::from(tx.nonce), &mut body);
    rlp_uint(tx.priority_fee, &mut body);
    rlp_uint(tx.max_fee, &mut body);
    rlp_uint(u128::from(tx.gas), &mut body);
    rlp_bytes(&tx.to, &mut body);
    rlp_uint(tx.value, &mut body);
    rlp_bytes(&tx.data, &mut body);
    body.push(0xc0);
    let mut out = vec![2];
    rlp_header(true, body.len(), &mut out);
    out.extend_from_slice(&body);
    out
}

/// Decodes an EIP-1559 signing payload, strictly: type 2, one list of exactly the nine fields, a
/// call (not a creation), an empty access list, nothing after it, and encoding what was decoded
/// gives the same bytes.
pub fn decode_evm(payload: &[u8]) -> Option<EvmTx> {
    let (&2, body) = payload.split_first()? else {
        return None;
    };
    let (true, mut fields, []) = item(body)? else {
        return None;
    };
    let mut items = Vec::with_capacity(9);
    while !fields.is_empty() {
        let (list, p, rest) = item(fields)?;
        items.push((list, p));
        fields = rest;
    }
    let [
        (false, chain_id),
        (false, nonce),
        (false, priority),
        (false, max_fee),
        (false, gas),
        (false, to),
        (false, value),
        (false, data),
        (true, []),
    ] = items.as_slice()
    else {
        return None;
    };
    let tx = EvmTx {
        chain_id: u64::try_from(uint(chain_id)?).ok()?,
        nonce: u64::try_from(uint(nonce)?).ok()?,
        priority_fee: uint(priority)?,
        max_fee: uint(max_fee)?,
        gas: u64::try_from(uint(gas)?).ok()?,
        to: (*to).try_into().ok()?,
        value: uint(value)?,
        data: data.to_vec(),
    };
    (evm_signing_payload(&tx) == payload).then_some(tx)
}

/// The vault's contracts a payout names, as the signer's policy declares them for its network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvmContracts {
    /// The Roles modifier: the transaction's target.
    pub roles: [u8; 20],
    /// MultiSendCallOnly: the only batch target.
    pub multisend: [u8; 20],
    /// The reference emitter.
    pub emitter: [u8; 20],
    /// The delivery role's key.
    pub role_key: [u8; 32],
}

/// A Roles v2 role key: a short ASCII name, padded right (F-008's `VaultRoles`).
pub fn role_key(name: &str) -> Option<[u8; 32]> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return None;
    }
    let mut key = [0u8; 32];
    key[..bytes.len()].copy_from_slice(bytes);
    Some(key)
}

/// What a payout moves: the origin token, to whom, how much, under which reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvmPayout {
    /// The origin token.
    pub token: [u8; 20],
    /// The beneficiary.
    pub to: [u8; 20],
    /// The amount, in the token's minor units.
    pub amount: u128,
    /// The on-chain reference the emitter writes.
    pub reference: [u8; 32],
}

fn word_address(a: &[u8; 20]) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    w
}

fn word_uint(n: u128) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&n.to_be_bytes());
    w
}

fn word_len(n: usize) -> [u8; 32] {
    word_uint(u128::try_from(n).unwrap_or(u128::MAX))
}

fn padded(b: &[u8]) -> Vec<u8> {
    let mut out = b.to_vec();
    out.resize(b.len().div_ceil(32) * 32, 0);
    out
}

fn entry(to: &[u8; 20], data: &[u8]) -> Vec<u8> {
    let mut e = vec![0u8]; // call
    e.extend_from_slice(to);
    e.extend_from_slice(&[0u8; 32]);
    e.extend_from_slice(&word_len(data.len()));
    e.extend_from_slice(data);
    e
}

/// The calldata of a payout: `execTransactionWithRole` on the Roles modifier, carrying the
/// canonical 356-byte `multiSend` of the transfer and its reference.
pub fn evm_payout_call(c: &EvmContracts, p: &EvmPayout) -> Vec<u8> {
    let mut transfer = selector("transfer(address,uint256)").to_vec();
    transfer.extend_from_slice(&word_address(&p.to));
    transfer.extend_from_slice(&word_uint(p.amount));
    let mut emit = selector("emitReference(bytes32)").to_vec();
    emit.extend_from_slice(&p.reference);
    let mut transactions = entry(&p.token, &transfer);
    transactions.extend_from_slice(&entry(&c.emitter, &emit));

    let mut batch = selector("multiSend(bytes)").to_vec();
    batch.extend_from_slice(&word_len(32));
    batch.extend_from_slice(&word_len(transactions.len()));
    batch.extend_from_slice(&padded(&transactions));

    let mut call =
        selector("execTransactionWithRole(address,uint256,bytes,uint8,bytes32,bool)").to_vec();
    call.extend_from_slice(&word_address(&c.multisend));
    call.extend_from_slice(&[0u8; 32]);
    call.extend_from_slice(&word_len(6 * 32));
    call.extend_from_slice(&word_uint(1)); // DelegateCall
    call.extend_from_slice(&c.role_key);
    call.extend_from_slice(&word_uint(1)); // shouldRevert
    call.extend_from_slice(&word_len(batch.len()));
    call.extend_from_slice(&padded(&batch));
    call
}

/// Decodes the calldata of a payout through `c`, strictly: only the exact bytes
/// [`evm_payout_call`] builds for what it names decode. An amount past `u128` does not.
pub fn decode_evm_payout(c: &EvmContracts, calldata: &[u8]) -> Option<EvmPayout> {
    // selector 4 | six head words | data length | data: selector 4 | offset | length | the two
    // entries, 274 bytes (85 + 68, 85 + 36), padded: 356 bytes of `multiSend` calldata in all
    let tx = calldata.get(4 + 7 * 32 + 4 + 2 * 32..)?.get(..274)?;
    let token: [u8; 20] = tx.get(1..21)?.try_into().ok()?;
    let transfer = tx.get(85..153)?;
    let to: [u8; 20] = transfer.get(4 + 12..36)?.try_into().ok()?;
    let amount_word = transfer.get(36..68)?;
    if amount_word[..16].iter().any(|&b| b != 0) {
        return None;
    }
    let amount = u128::from_be_bytes(amount_word[16..].try_into().ok()?);
    let reference: [u8; 32] = tx.get(153 + 85 + 4..)?.try_into().ok()?;
    let p = EvmPayout {
        token,
        to,
        amount,
        reference,
    };
    (evm_payout_call(c, &p) == calldata).then_some(p)
}

// ------------------------------------------------------------------------------ SCALE

fn compact(n: u128, out: &mut Vec<u8>) {
    if n < 1 << 6 {
        out.push(u8::try_from(n << 2).unwrap_or(0));
    } else if n < 1 << 14 {
        out.extend_from_slice(&(u16::try_from(n << 2).unwrap_or(0) | 1).to_le_bytes());
    } else if n < 1 << 30 {
        out.extend_from_slice(&(u32::try_from(n << 2).unwrap_or(0) | 2).to_le_bytes());
    } else {
        let le = n.to_le_bytes();
        let len = le.iter().rposition(|&b| b != 0).map_or(1, |i| i + 1).max(4);
        out.push(u8::try_from((len - 4) << 2).unwrap_or(0) | 3);
        out.extend_from_slice(&le[..len]);
    }
}

/// A SCALE compact integer at the head of `b`, advanced past it. Canonical form is checked by
/// the caller's re-encoding.
fn take_compact(b: &mut &[u8]) -> Option<u128> {
    let first = *b.first()?;
    let (n, len) = match first & 3 {
        0 => (u128::from(first >> 2), 1),
        1 => (
            u128::from(u16::from_le_bytes(b.get(..2)?.try_into().ok()?) >> 2),
            2,
        ),
        2 => (
            u128::from(u32::from_le_bytes(b.get(..4)?.try_into().ok()?) >> 2),
            4,
        ),
        _ => {
            let len = usize::from(first >> 2) + 4;
            if len > 16 {
                return None;
            }
            let digits = b.get(1..1 + len)?;
            let mut le = [0u8; 16];
            le[..len].copy_from_slice(digits);
            (u128::from_le_bytes(le), 1 + len)
        }
    };
    *b = b.get(len..)?;
    Some(n)
}

fn take<const N: usize>(b: &mut &[u8]) -> Option<[u8; N]> {
    let head: [u8; N] = b.get(..N)?.try_into().ok()?;
    *b = b.get(N..)?;
    Some(head)
}

// ------------------------------------------------------------------------------ Kreivo

/// `Contracts` in Kreivo's runtime (`DEC-6`: `#[runtime::pallet_index(80)]`).
pub const CONTRACTS_PALLET: u8 = 80;
/// `pallet_contracts::Call::call` (`#[pallet::call_index(6)]`).
pub const CONTRACTS_CALL: u8 = 6;
/// The longest era the endpoint signs with, in blocks (`DEC-5`: `max_era` 64).
pub const MAX_ERA: u64 = 64;
/// A signed payload longer than this is signed as its BLAKE2b-256.
pub const MAX_UNHASHED_PAYLOAD: usize = 256;

/// A Kreivo signed payload as the signer reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KreivoTx {
    /// The contract the call is to.
    pub contract: [u8; 32],
    /// The value it transfers.
    pub value: u128,
    /// The gas limit's computation time.
    pub ref_time: u64,
    /// The gas limit's proof size.
    pub proof_size: u64,
    /// The storage deposit limit.
    pub storage_deposit_limit: Option<u128>,
    /// The contract message's data.
    pub data: Vec<u8>,
    /// The era's two bytes (mortal).
    pub era: [u8; 2],
    /// The nonce.
    pub nonce: u32,
    /// The tip.
    pub tip: u128,
    /// `CheckSpecVersion`.
    pub spec_version: u32,
    /// `CheckTxVersion`.
    pub transaction_version: u32,
    /// `CheckGenesis`: the network's genesis hash.
    pub genesis: [u8; 32],
    /// The hash of the era's birth block.
    pub birth_hash: [u8; 32],
}

impl KreivoTx {
    /// The era's length in blocks (`Era::Mortal`'s period).
    pub fn era_period(&self) -> u64 {
        2u64 << (u16::from_le_bytes(self.era) % 16)
    }
}

/// The signed payload of `tx`: `call ‖ extra ‖ implicit`.
pub fn kreivo_signing_payload(tx: &KreivoTx) -> Vec<u8> {
    let mut out = vec![CONTRACTS_PALLET, CONTRACTS_CALL, 0];
    out.extend_from_slice(&tx.contract);
    compact(tx.value, &mut out);
    compact(u128::from(tx.ref_time), &mut out);
    compact(u128::from(tx.proof_size), &mut out);
    match tx.storage_deposit_limit {
        None => out.push(0),
        Some(l) => {
            out.push(1);
            compact(l, &mut out);
        }
    }
    compact(u128::try_from(tx.data.len()).unwrap_or(u128::MAX), &mut out);
    out.extend_from_slice(&tx.data);
    out.push(0); // PassAuthenticate: None
    out.extend_from_slice(&tx.era);
    compact(u128::from(tx.nonce), &mut out);
    compact(tx.tip, &mut out);
    out.push(0); // no fee asset
    out.extend_from_slice(&tx.spec_version.to_le_bytes());
    out.extend_from_slice(&tx.transaction_version.to_le_bytes());
    out.extend_from_slice(&tx.genesis);
    out.extend_from_slice(&tx.birth_hash);
    out
}

/// What the key signs for a Kreivo payload: the payload, or its BLAKE2b-256 past 256 bytes.
pub fn kreivo_message_to_sign(payload: &[u8]) -> Vec<u8> {
    if payload.len() > MAX_UNHASHED_PAYLOAD {
        blake2_256(payload).to_vec()
    } else {
        payload.to_vec()
    }
}

/// Decodes a Kreivo signed payload, strictly: a `Contracts::call` to an account id, a mortal era,
/// no fee asset, nothing left over, and encoding what was decoded gives the same bytes.
pub fn decode_kreivo(payload: &[u8]) -> Option<KreivoTx> {
    let mut b = payload;
    if take::<3>(&mut b)? != [CONTRACTS_PALLET, CONTRACTS_CALL, 0] {
        return None;
    }
    let contract = take::<32>(&mut b)?;
    let value = take_compact(&mut b)?;
    let ref_time = u64::try_from(take_compact(&mut b)?).ok()?;
    let proof_size = u64::try_from(take_compact(&mut b)?).ok()?;
    let storage_deposit_limit = match take::<1>(&mut b)? {
        [0] => None,
        [1] => Some(take_compact(&mut b)?),
        _ => return None,
    };
    let len = usize::try_from(take_compact(&mut b)?).ok()?;
    let data = b.get(..len)?.to_vec();
    b = b.get(len..)?;
    if take::<1>(&mut b)? != [0] {
        return None;
    }
    let era = take::<2>(&mut b)?;
    if era[0] == 0 {
        return None;
    }
    let nonce = u32::try_from(take_compact(&mut b)?).ok()?;
    let tip = take_compact(&mut b)?;
    if take::<1>(&mut b)? != [0] {
        return None;
    }
    let spec_version = u32::from_le_bytes(take::<4>(&mut b)?);
    let transaction_version = u32::from_le_bytes(take::<4>(&mut b)?);
    let genesis = take::<32>(&mut b)?;
    let birth_hash = take::<32>(&mut b)?;
    if !b.is_empty() {
        return None;
    }
    let tx = KreivoTx {
        contract,
        value,
        ref_time,
        proof_size,
        storage_deposit_limit,
        data,
        era,
        nonce,
        tip,
        spec_version,
        transaction_version,
        genesis,
        birth_hash,
    };
    (kreivo_signing_payload(&tx) == payload).then_some(tx)
}

/// An ink! 5 message selector: the first four bytes of the BLAKE2b-256 of its name.
fn ink_selector(name: &str) -> [u8; 4] {
    let h = blake2_256(name.as_bytes());
    [h[0], h[1], h[2], h[3]]
}

/// A message to bezent's contract (F-006), as its input names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KreivoMessage {
    /// `mint(AssetId::Here(asset), to, amount, reference)`.
    Mint {
        /// The asset's index (`AssetId::Here`).
        asset: u32,
        /// The destination account.
        to: [u8; 32],
        /// The amount.
        amount: u128,
        /// The on-chain reference.
        reference: [u8; 32],
    },
    /// `refund_mint(redeemer, redemption_reference)`.
    RefundMint {
        /// The redeemer.
        redeemer: [u8; 32],
        /// The redemption's reference.
        reference: [u8; 32],
    },
}

impl KreivoMessage {
    /// The message's input bytes.
    pub fn data(&self) -> Vec<u8> {
        match self {
            Self::Mint {
                asset,
                to,
                amount,
                reference,
            } => {
                let mut out = ink_selector("mint").to_vec();
                out.push(0); // AssetId::Here
                out.extend_from_slice(&asset.to_le_bytes());
                out.extend_from_slice(to);
                out.extend_from_slice(&amount.to_le_bytes());
                out.extend_from_slice(reference);
                out
            }
            Self::RefundMint {
                redeemer,
                reference,
            } => {
                let mut out = ink_selector("refund_mint").to_vec();
                out.extend_from_slice(redeemer);
                out.extend_from_slice(reference);
                out
            }
        }
    }

    /// Decodes a message's input, strictly; `None` for anything but `mint` and `refund_mint`.
    pub fn decode(data: &[u8]) -> Option<Self> {
        let (sel, mut b) = (data.get(..4)?, data.get(4..)?);
        let m = if sel == ink_selector("mint") {
            if take::<1>(&mut b)? != [0] {
                return None;
            }
            Self::Mint {
                asset: u32::from_le_bytes(take::<4>(&mut b)?),
                to: take::<32>(&mut b)?,
                amount: u128::from_le_bytes(take::<16>(&mut b)?),
                reference: take::<32>(&mut b)?,
            }
        } else if sel == ink_selector("refund_mint") {
            Self::RefundMint {
                redeemer: take::<32>(&mut b)?,
                reference: take::<32>(&mut b)?,
            }
        } else {
            return None;
        };
        (b.is_empty() && m.data() == data).then_some(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contracts() -> EvmContracts {
        EvmContracts {
            roles: [0x11; 20],
            multisend: [0x22; 20],
            emitter: [0x33; 20],
            role_key: role_key("bezent.delivery").unwrap(),
        }
    }

    #[test]
    fn must_fix_2_an_evm_payout_decodes_to_exactly_what_it_was_built_from() {
        let p = EvmPayout {
            token: [0x44; 20],
            to: [0x55; 20],
            amount: 1_000_000,
            reference: [0x66; 32],
        };
        let call = evm_payout_call(&contracts(), &p);
        assert_eq!(call.len(), 4 + 7 * 32 + 384, "the canonical layout");
        assert_eq!(
            &call[4 + 7 * 32 - 32..4 + 7 * 32],
            &word_len(356),
            "a 356-byte multiSend"
        );
        assert_eq!(decode_evm_payout(&contracts(), &call), Some(p));
        // Another emitter, multisend or role: not a payout through these contracts.
        for other in [
            EvmContracts {
                emitter: [0x34; 20],
                ..contracts()
            },
            EvmContracts {
                multisend: [0x23; 20],
                ..contracts()
            },
            EvmContracts {
                role_key: role_key("other").unwrap(),
                ..contracts()
            },
        ] {
            assert_eq!(decode_evm_payout(&other, &call), None);
        }
        // Any byte changed outside the decoded fields, or bytes appended: refused.
        let mut flipped = call.clone();
        flipped[4 + 3 * 32 + 31] = 0; // the operation: a call, not a delegatecall
        assert_eq!(decode_evm_payout(&contracts(), &flipped), None);
        let mut longer = call.clone();
        longer.push(0);
        assert_eq!(decode_evm_payout(&contracts(), &longer), None);

        let tx = EvmTx {
            chain_id: 80_002,
            nonce: 7,
            priority_fee: 1,
            max_fee: 2,
            gas: 200_000,
            to: [0x11; 20],
            value: 0,
            data: call,
        };
        let payload = evm_signing_payload(&tx);
        assert_eq!(decode_evm(&payload), Some(tx));
        let mut trailing = payload.clone();
        trailing.push(0);
        assert_eq!(decode_evm(&trailing), None);
    }

    #[test]
    fn must_fix_2_a_kreivo_mint_decodes_to_exactly_what_it_was_built_from() {
        let message = KreivoMessage::Mint {
            asset: 101,
            to: [2; 32],
            amount: 1_000_000,
            reference: [1; 32],
        };
        assert_eq!(KreivoMessage::decode(&message.data()), Some(message));
        let refund = KreivoMessage::RefundMint {
            redeemer: [3; 32],
            reference: [4; 32],
        };
        assert_eq!(KreivoMessage::decode(&refund.data()), Some(refund));
        let tx = KreivoTx {
            contract: [9; 32],
            value: 0,
            ref_time: 1_000_000_000,
            proof_size: 100_000,
            storage_deposit_limit: Some(10_000_000_000),
            data: message.data(),
            era: [0x05, 0x00],
            nonce: 300,
            tip: 0,
            spec_version: 134,
            transaction_version: 13,
            genesis: [7; 32],
            birth_hash: [8; 32],
        };
        let payload = kreivo_signing_payload(&tx);
        assert_eq!(decode_kreivo(&payload), Some(tx.clone()));
        assert_eq!(tx.era_period(), 64);
        let mut trailing = payload.clone();
        trailing.push(0);
        assert_eq!(decode_kreivo(&trailing), None);
        // A non-canonical compact (the nonce 300 in four bytes) does not decode.
        let canonical = [0xb1, 0x04];
        let at = payload
            .windows(2)
            .position(|w| w == canonical)
            .expect("the nonce");
        let mut long = payload[..at].to_vec();
        long.extend_from_slice(&(300u32 << 2 | 2).to_le_bytes());
        long.extend_from_slice(&payload[at + 2..]);
        assert_eq!(decode_kreivo(&long), None);
    }
}
