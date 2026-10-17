// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The EVM side's encoding: the vault's delivery call, EIP-1559 transactions, and the events the
//! endpoint reads (PLAN `DEC-3`, `DEC-10`; F-008 NOTES, "The two roles").
//!
//! A payout is one transaction from the delivery key to the vault's Roles modifier:
//!
//! ```text
//! Roles.execTransactionWithRole(
//!     to        = MultiSendCallOnly,
//!     value     = 0,
//!     data      = multiSend( entry(call, token,   0, transfer(beneficiary, amount))
//!                          ‖ entry(call, emitter, 0, emitReference(reference)) ),
//!     operation = DelegateCall,
//!     roleKey   = bytes32("bezent.delivery"),
//!     shouldRevert = true)
//! ```
//!
//! The batch is the 356-byte canonical `multiSend` that `ReferencedTransferUnwrapper` admits, so
//! the transfer and its reference execute in one transaction or not at all (`REQ-RN-5`,
//! `INV-14`). Decoding is strict: a call decodes only when re-encoding what it decoded gives the
//! same bytes, so what is compared is exactly what executes.

use alloy::consensus::transaction::RlpEcdsaDecodableTx;
use alloy::consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy::eips::eip2718::Encodable2718;
use alloy::primitives::{Address, B256, Bytes, Signature, TxKind, U256, keccak256};
use alloy::sol;
use alloy::sol_types::{SolCall, SolEvent};

sol! {
    /// ERC-20 `Transfer`.
    event Transfer(address indexed from, address indexed to, uint256 value);
    /// The reference emitter's only event (F-008, `ReferenceEmitter.sol`).
    event Reference(bytes32 indexed ref);
    /// ERC-20 `transfer`.
    function transfer(address to, uint256 amount) external returns (bool);
    /// ERC-20 `balanceOf`.
    function balanceOf(address owner) external view returns (uint256);
    /// The reference emitter's only function.
    function emitReference(bytes32 ref) external;
    /// MultiSendCallOnly 1.4.1.
    function multiSend(bytes transactions) external payable;
    /// A Zodiac modifier's avatar (the Safe whose module it is).
    function avatar() external view returns (address);
    /// A Zodiac modifier's target (the Safe it executes on).
    function target() external view returns (address);
    /// Safe: whether `module` is enabled.
    function isModuleEnabled(address module) external view returns (bool);
    /// Zodiac Roles v2.
    function execTransactionWithRole(
        address to,
        uint256 value,
        bytes data,
        uint8 operation,
        bytes32 roleKey,
        bool shouldRevert
    ) external returns (bool success);
}

/// `Transfer(address,address,uint256)`.
pub const TRANSFER_TOPIC: B256 = Transfer::SIGNATURE_HASH;
/// `Reference(bytes32)`.
pub const REFERENCE_TOPIC: B256 = Reference::SIGNATURE_HASH;

/// The EIP-2718 type of an EIP-1559 transaction.
pub const EIP1559: u8 = 2;

/// Safe's `Enum.Operation.DelegateCall`.
const DELEGATECALL: u8 = 1;

/// A Roles v2 role key: a short ASCII name, padded right (as F-008's `VaultRoles` encodes them).
pub fn role_key(name: &str) -> Option<B256> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return None;
    }
    let mut key = [0u8; 32];
    key[..bytes.len()].copy_from_slice(bytes);
    Some(B256::from(key))
}

/// The vault's contracts the delivery call names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Contracts {
    /// The Roles modifier, the transaction's target.
    pub roles: Address,
    /// MultiSendCallOnly, the only batch target admitted.
    pub multisend: Address,
    /// The reference emitter.
    pub emitter: Address,
    /// The delivery role's key.
    pub delivery_role: B256,
}

/// What a delivery moves: the origin token, to whom, how much, with which reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delivery {
    /// The origin token.
    pub token: Address,
    /// The beneficiary.
    pub to: Address,
    /// The amount, in the token's minor units (`INV-11`).
    pub amount: U256,
    /// The on-chain reference the emitter writes (`REQ-ID-2`).
    pub reference: B256,
}

fn entry(to: Address, data: &[u8]) -> Vec<u8> {
    let mut e = Vec::with_capacity(85 + data.len());
    e.push(0); // call
    e.extend_from_slice(to.as_slice());
    e.extend_from_slice(&U256::ZERO.to_be_bytes::<32>());
    e.extend_from_slice(&U256::from(data.len()).to_be_bytes::<32>());
    e.extend_from_slice(data);
    e
}

/// The `multiSend` calldata of a delivery: the transfer, then the reference.
pub fn delivery_batch(contracts: &Contracts, d: &Delivery) -> Vec<u8> {
    let transfer = transferCall {
        to: d.to,
        amount: d.amount,
    }
    .abi_encode();
    let emit = emitReferenceCall { r#ref: d.reference }.abi_encode();
    let mut transactions = entry(d.token, &transfer);
    transactions.extend_from_slice(&entry(contracts.emitter, &emit));
    multiSendCall {
        transactions: transactions.into(),
    }
    .abi_encode()
}

/// The calldata of a delivery: `execTransactionWithRole` on the Roles modifier.
pub fn delivery_call(contracts: &Contracts, d: &Delivery) -> Vec<u8> {
    execTransactionWithRoleCall {
        to: contracts.multisend,
        value: U256::ZERO,
        data: delivery_batch(contracts, d).into(),
        operation: DELEGATECALL,
        roleKey: contracts.delivery_role,
        shouldRevert: true,
    }
    .abi_encode()
}

/// Decodes calldata as a delivery through `contracts`, strictly: it decodes only if re-encoding
/// the delivery it names gives the same bytes (target, value, operation, role, the batch's two
/// entries, their order and padding).
pub fn decode_delivery_call(contracts: &Contracts, calldata: &[u8]) -> Option<Delivery> {
    let outer = execTransactionWithRoleCall::abi_decode(calldata).ok()?;
    let batch = multiSendCall::abi_decode(&outer.data).ok()?;
    let tx = batch.transactions.as_ref();
    // entry 0: op (1) | to (20) | value (32) | len (32) | transfer (68)
    // entry 1: op (1) | to (20) | value (32) | len (32) | emitReference (36)
    if tx.len() != 85 + 68 + 85 + 36 {
        return None;
    }
    let token = Address::from_slice(&tx[1..21]);
    let transfer = transferCall::abi_decode(&tx[85..153]).ok()?;
    let emit = emitReferenceCall::abi_decode(&tx[153 + 85..]).ok()?;
    let d = Delivery {
        token,
        to: transfer.to,
        amount: transfer.amount,
        reference: emit.r#ref,
    };
    (delivery_call(contracts, &d) == calldata).then_some(d)
}

/// Whether `calldata` carries `reference` anywhere, as a 32-byte word: a wrapper (a multicall, a
/// proxy) that carries a delivery cannot be told apart by decoding the outer call alone.
pub fn carries(calldata: &[u8], reference: &B256) -> bool {
    calldata.windows(32).any(|w| w == reference.as_slice())
}

/// The fee and gas fields of a transaction the endpoint builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gas {
    /// The gas limit.
    pub limit: u64,
    /// `maxFeePerGas`, in wei.
    pub max_fee: u128,
    /// `maxPriorityFeePerGas`, in wei.
    pub priority_fee: u128,
}

/// An unsigned EIP-1559 transaction with no value and no access list.
pub fn transaction(chain_id: u64, nonce: u64, to: Address, input: Vec<u8>, gas: Gas) -> TxEip1559 {
    TxEip1559 {
        chain_id,
        nonce,
        gas_limit: gas.limit,
        max_fee_per_gas: gas.max_fee,
        max_priority_fee_per_gas: gas.priority_fee,
        to: TxKind::Call(to),
        value: U256::ZERO,
        access_list: Default::default(),
        input: input.into(),
    }
}

/// The bytes the delivery key signs the Keccak-256 of: `0x02 ‖ rlp(fields)` (EIP-1559). The
/// signer hashes them itself (`secp256k1-evm`, F-005).
pub fn signing_payload(tx: &TxEip1559) -> Vec<u8> {
    let mut out = Vec::new();
    tx.encode_for_signing(&mut out);
    out
}

/// Decodes a signing payload, strictly: type 2, one RLP list, nothing after it, and re-encoding
/// gives the same bytes.
pub fn decode_signing_payload(payload: &[u8]) -> Option<TxEip1559> {
    let (&ty, rest) = payload.split_first()?;
    if ty != EIP1559 {
        return None;
    }
    let mut buf = rest;
    let header = alloy::rlp::Header::decode(&mut buf).ok()?;
    if !header.list || header.payload_length != buf.len() {
        return None;
    }
    let tx = TxEip1559::rlp_decode_fields(&mut buf).ok()?;
    (buf.is_empty() && signing_payload(&tx) == payload).then_some(tx)
}

/// A signed transaction: its network encoding (EIP-2718) and hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedTx {
    /// What is sent (`eth_sendRawTransaction`) and persisted (`REQ-EO-1`).
    pub bytes: Vec<u8>,
    /// The transaction hash.
    pub hash: B256,
    /// The address the signature recovers to.
    pub from: Address,
}

/// The signed transaction for `tx` and a 65-byte `r ‖ s ‖ v` signature (`v` the recovery id, 0
/// or 1, as the signer returns it), if the signature is well formed and low-`s`.
pub fn signed(tx: &TxEip1559, signature: &[u8]) -> Option<SignedTx> {
    let (&v, rs) = signature.split_last()?;
    if rs.len() != 64 || v > 1 {
        return None;
    }
    let sig = Signature::from_bytes_and_parity(rs, v == 1);
    if sig.normalize_s().is_some() {
        return None; // high s: not what the signer makes, and not valid on chain (EIP-2)
    }
    let from = sig
        .recover_address_from_prehash(&tx.signature_hash())
        .ok()?;
    let envelope = TxEnvelope::from(tx.clone().into_signed(sig));
    let bytes = envelope.encoded_2718();
    Some(SignedTx {
        hash: keccak256(&bytes),
        bytes,
        from,
    })
}

/// The address of a secp256k1 public key, compressed (33 bytes) or not (65 bytes).
pub fn address_of(public_key: &[u8]) -> Option<Address> {
    let key = k256::ecdsa::VerifyingKey::from_sec1_bytes(public_key).ok()?;
    Some(Address::from_public_key(&key))
}

/// A decoded ERC-20 `Transfer` log: three topics and one data word.
pub fn decode_transfer(topics: &[B256], data: &[u8]) -> Option<(Address, Address, U256)> {
    if topics.len() != 3 || topics[0] != TRANSFER_TOPIC || data.len() != 32 {
        return None;
    }
    let from = Address::from_word(topics[1]);
    let to = Address::from_word(topics[2]);
    // An address topic carries twelve zero bytes first.
    if from.into_word() != topics[1] || to.into_word() != topics[2] {
        return None;
    }
    Some((from, to, U256::from_be_slice(data)))
}

/// A decoded `Reference(bytes32)` log.
pub fn decode_reference(topics: &[B256], data: &[u8]) -> Option<B256> {
    (topics.len() == 2 && topics[0] == REFERENCE_TOPIC && data.is_empty()).then_some(topics[1])
}

/// A revert's data decoded as the Roles modifier's `ConditionViolation(uint8 status, bytes32
/// info)`, strictly: the selector, then exactly two ABI words, the first a `uint8`. Anything else
/// is not one (PR #52 review S-2: the core names `CND-PayoutWindow` from this, never from text).
pub fn condition_violation(data: Option<&Bytes>) -> Option<(u8, B256)> {
    let d = data?;
    let selector = alloy::primitives::keccak256(b"ConditionViolation(uint8,bytes32)");
    (d.len() == 68 && d[..4] == selector[..4] && d[4..35].iter().all(|b| *b == 0))
        .then(|| (d[35], B256::from_slice(&d[36..68])))
}

/// A revert's data, read: a Roles condition violation, a Solidity `Error(string)`, or the raw
/// bytes. The text is the network's own (`REQ-ST-3`).
pub fn revert_reason(message: &str, data: Option<&Bytes>) -> String {
    const ERROR: [u8; 4] = [0x08, 0xc3, 0x79, 0xa0];
    let Some(d) = data.filter(|d| d.len() >= 4) else {
        return message.to_owned();
    };
    let violation = alloy::primitives::keccak256(b"ConditionViolation(uint8,bytes32)");
    if d[..4] == violation[..4] && d.len() >= 68 {
        return format!(
            "Roles: ConditionViolation(status {}, 0x{}) ({message})",
            d[35],
            hex::encode(&d[36..68])
        );
    }
    if d[..4] == ERROR
        && let Ok(text) =
            <alloy::sol_types::sol_data::String as alloy::sol_types::SolType>::abi_decode(&d[4..])
    {
        return format!("{text} ({message})");
    }
    format!("{message}: 0x{}", hex::encode(d))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contracts() -> Contracts {
        Contracts {
            roles: Address::repeat_byte(0x11),
            multisend: "0x9641d764fc13c8B624c04430C7356C1C7C8102e2"
                .parse()
                .unwrap(),
            emitter: Address::repeat_byte(0x22),
            delivery_role: role_key("bezent.delivery").unwrap(),
        }
    }

    fn delivery() -> Delivery {
        Delivery {
            token: "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359"
                .parse()
                .unwrap(),
            to: Address::repeat_byte(0x33),
            amount: U256::from(700u64),
            reference: B256::repeat_byte(0x44),
        }
    }

    #[test]
    fn inv_14_the_batch_is_the_canonical_356_bytes_the_unwrapper_admits() {
        let batch = delivery_batch(&contracts(), &delivery());
        assert_eq!(batch.len(), 356);
        assert_eq!(batch[..4], [0x8d, 0x80, 0xff, 0x0a]);
        // offset 0x20, length 274
        assert_eq!(U256::from_be_slice(&batch[4..36]), U256::from(32));
        assert_eq!(U256::from_be_slice(&batch[36..68]), U256::from(274));
        assert!(batch[342..].iter().all(|b| *b == 0));
    }

    #[test]
    fn dec_3_a_delivery_call_decodes_to_what_it_was_built_from() {
        let c = contracts();
        let call = delivery_call(&c, &delivery());
        assert_eq!(decode_delivery_call(&c, &call), Some(delivery()));
        // Another role key, another target or a flipped byte anywhere does not decode.
        let other = Contracts {
            delivery_role: role_key("bezent.conversion").unwrap(),
            ..c
        };
        assert_eq!(decode_delivery_call(&other, &call), None);
        for i in [4, 40, 200, call.len() - 1] {
            let mut bad = call.clone();
            bad[i] ^= 1;
            let decoded = decode_delivery_call(&c, &bad);
            assert!(decoded.is_none() || decoded != Some(delivery()), "byte {i}");
        }
    }

    #[test]
    fn req_eo_1_a_signing_payload_decodes_strictly() {
        let tx = transaction(
            137,
            7,
            Address::repeat_byte(0x11),
            vec![1, 2, 3],
            Gas {
                limit: 300_000,
                max_fee: 100_000_000_000,
                priority_fee: 30_000_000_000,
            },
        );
        let payload = signing_payload(&tx);
        assert_eq!(payload[0], EIP1559);
        assert_eq!(decode_signing_payload(&payload), Some(tx));
        let mut trailing = payload.clone();
        trailing.push(0);
        assert_eq!(decode_signing_payload(&trailing), None);
        let mut typed = payload.clone();
        typed[0] = 1;
        assert_eq!(decode_signing_payload(&typed), None);
    }

    #[test]
    fn inv_14_transfer_and_reference_logs_decode_only_in_their_shape() {
        let from = Address::repeat_byte(1);
        let to = Address::repeat_byte(2);
        let topics = [TRANSFER_TOPIC, from.into_word(), to.into_word()];
        let data = U256::from(5).to_be_bytes::<32>();
        assert_eq!(
            decode_transfer(&topics, &data),
            Some((from, to, U256::from(5)))
        );
        // ERC-721 shape (four topics, no data) is not an ERC-20 transfer.
        assert_eq!(
            decode_transfer(&[topics[0], topics[1], topics[2], B256::ZERO], &[]),
            None
        );
        let r = B256::repeat_byte(9);
        assert_eq!(decode_reference(&[REFERENCE_TOPIC, r], &[]), Some(r));
        assert_eq!(decode_reference(&[REFERENCE_TOPIC, r], &[0]), None);
    }

    /// The Roles modifier's `ConditionViolation(uint8, bytes32)` revert data.
    fn violation_data(status: u8, key: B256) -> Bytes {
        let mut d = keccak256(b"ConditionViolation(uint8,bytes32)")[..4].to_vec();
        d.extend_from_slice(&U256::from(status).to_be_bytes::<32>());
        d.extend_from_slice(key.as_slice());
        d.into()
    }

    /// CND-PayoutWindow, PR #52 review S-2: only the exact `ConditionViolation` shape decodes;
    /// the same words in an `Error(string)`, extra or missing bytes, or a status that is not a
    /// `uint8` do not.
    #[test]
    fn cnd_payout_window_s2_a_condition_violation_decodes_only_in_its_shape() {
        let key = B256::repeat_byte(0xfe);
        let data = violation_data(17, key);
        assert_eq!(condition_violation(Some(&data)), Some((17, key)));
        assert_eq!(condition_violation(None), None);
        let longer: Bytes = [data.as_ref(), &[0]].concat().into();
        assert_eq!(condition_violation(Some(&longer)), None);
        let shorter: Bytes = data[..67].to_vec().into();
        assert_eq!(condition_violation(Some(&shorter)), None);
        let mut wide = data.to_vec();
        wide[34] = 1;
        assert_eq!(condition_violation(Some(&wide.into())), None);
        let text = format!("Roles: ConditionViolation(status 17, {key})");
        let error: Bytes = [
            &[0x08, 0xc3, 0x79, 0xa0][..],
            &<alloy::sol_types::sol_data::String as alloy::sol_types::SolType>::abi_encode(&text),
        ]
        .concat()
        .into();
        assert_eq!(condition_violation(Some(&error)), None);
    }
}
