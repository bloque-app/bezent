// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! What an EVM `cancel` may be (PR #39 review S6 and second review SF2; `REQ-NX-4`, `INV-9`,
//! `REQ-KY-3`).
//!
//! A cancel carries no reference and moves nothing, so the one-time reference guard and the
//! window cap pass it whatever its bytes say. The signer therefore reads them: a cancel is signed
//! only when its message is an EIP-1559 transaction (`0x02 ‖ rlp([chain id, nonce, priority fee,
//! max fee, gas, to, value, data, access list])`) **to the signing key's own address**, of value
//! zero, with no calldata and no access list; **on the chain id the signer's policy declares for
//! the network** (so a cancel is never signed for another chain where the key may be a lane or
//! carry a delegation); with **gas 21000**, a bare transfer's; and with **fees within the
//! network's cap** (`cancel_max_fee_per_gas`), the priority fee no higher than the max fee (so one
//! cancel cannot pay the lane's balance away as fees). Anything else labelled `cancel` is refused.
//! The nonce is not read; the endpoint has compared it against the payout it replaces.

use k256::elliptic_curve::sec1::ToEncodedPoint;
use sha3::{Digest, Keccak256};

/// The gas of a transfer of nothing with no calldata.
pub const CANCEL_GAS: u64 = 21_000;

/// What the signer's policy bounds a cancel by, for its network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// The network's chain id.
    pub chain_id: u64,
    /// The most the cancel pays per gas, in wei.
    pub max_fee: u128,
}

/// One RLP item at the head of `b`: (whether it is a list, its payload, the rest). Strings longer
/// than 55 bytes are refused (no field of a cancel is), lists may be long.
fn item(b: &[u8]) -> Option<(bool, &[u8], &[u8])> {
    let (&h, rest) = b.split_first()?;
    let (list, start, len) = match h {
        0x00..=0x7f => return Some((false, &b[..1], rest)),
        0x80..=0xb7 => (false, 1, usize::from(h - 0x80)),
        0xc0..=0xf7 => (true, 1, usize::from(h - 0xc0)),
        0xf8..=0xfa => {
            let n = usize::from(h - 0xf7);
            let len = rest
                .get(..n)?
                .iter()
                .fold(0usize, |a, &d| (a << 8) | usize::from(d));
            (true, 1 + n, len)
        }
        _ => return None,
    };
    let end = start.checked_add(len)?;
    Some((list, b.get(start..end)?, b.get(end..)?))
}

/// A canonical RLP unsigned integer of at most 16 bytes: big-endian, no leading zero byte.
fn uint(payload: &[u8]) -> Option<u128> {
    if payload.len() > 16 || payload.first() == Some(&0) {
        return None;
    }
    Some(payload.iter().fold(0u128, |a, &d| (a << 8) | u128::from(d)))
}

/// The chain id of an EIP-1559 signing payload (`0x02 ‖ rlp([chain id, …])`): the first field of
/// its list, a canonical integer. `None` for anything else. Every EVM intent's bytes are read for
/// it, not only a cancel's (PR #39 third review SF-B).
pub fn chain_id(message: &[u8]) -> Option<u64> {
    let (&2, body) = message.split_first()? else {
        return None;
    };
    let (true, fields, []) = item(body)? else {
        return None;
    };
    let (false, chain_id, _) = item(fields)? else {
        return None;
    };
    u64::try_from(uint(chain_id)?).ok()
}

/// The EVM address of a secp256k1 public key (SEC1, compressed or not).
fn address(public_key: &[u8]) -> Option<[u8; 20]> {
    let key = k256::PublicKey::from_sec1_bytes(public_key).ok()?;
    let point = key.to_encoded_point(false);
    let hash = Keccak256::digest(&point.as_bytes()[1..]);
    hash[12..].try_into().ok()
}

/// Whether `message` is a cancel of the key `public_key` within `bounds`: a type-2 transaction of
/// nothing, to the key's own address, with no calldata and no access list, on the bounds' chain,
/// with gas 21000 and fees within the cap.
pub fn is_cancel(message: &[u8], public_key: &[u8], bounds: Bounds) -> bool {
    let (Some(own), Some((&2, body))) = (address(public_key), message.split_first()) else {
        return false;
    };
    let Some((true, mut fields, [])) = item(body) else {
        return false;
    };
    let mut items = Vec::with_capacity(9);
    while !fields.is_empty() {
        let Some((list, payload, rest)) = item(fields) else {
            return false;
        };
        items.push((list, payload));
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
        (true, access),
    ] = items.as_slice()
    else {
        return false;
    };
    let (Some(chain_id), Some(_), Some(priority), Some(max_fee), Some(gas)) = (
        uint(chain_id),
        uint(nonce),
        uint(priority),
        uint(max_fee),
        uint(gas),
    ) else {
        return false;
    };
    *to == own.as_slice()
        && value.is_empty()
        && data.is_empty()
        && access.is_empty()
        && chain_id == u128::from(bounds.chain_id)
        && gas == u128::from(CANCEL_GAS)
        && max_fee <= bounds.max_fee
        && priority <= max_fee
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: Bounds = Bounds {
        chain_id: 137,
        max_fee: 1_000,
    };

    /// The canonical RLP of an unsigned integer.
    fn int(n: u128) -> Vec<u8> {
        let b = n.to_be_bytes();
        let b = &b[b.iter().position(|&d| d != 0).unwrap_or(16)..];
        s(b)
    }

    fn s(b: &[u8]) -> Vec<u8> {
        match b {
            [x] if *x < 0x80 => vec![*x],
            _ => [vec![0x80 + u8::try_from(b.len()).unwrap()], b.to_vec()].concat(),
        }
    }

    /// `0x02 ‖ rlp([chain, 0, priority, max_fee, gas, to, value, data, []])`.
    fn tx_with(
        chain: u128,
        priority: u128,
        max_fee: u128,
        gas: u128,
        to: &[u8],
        value: &[u8],
        data: &[u8],
    ) -> Vec<u8> {
        let body = [
            int(chain),
            int(0),
            int(priority),
            int(max_fee),
            int(gas),
            s(to),
            s(value),
            s(data),
            vec![0xc0],
        ]
        .concat();
        let head = if body.len() < 56 {
            vec![0xc0 + u8::try_from(body.len()).unwrap()]
        } else {
            vec![0xf8, u8::try_from(body.len()).unwrap()]
        };
        [vec![2], head, body].concat()
    }

    /// `0x02 ‖ rlp([137, 0, 1, 2, 21000, to, value, data, []])`.
    fn tx(to: &[u8], value: &[u8], data: &[u8]) -> Vec<u8> {
        tx_with(137, 1, 2, 21_000, to, value, data)
    }

    fn key() -> (Vec<u8>, [u8; 20]) {
        let key = k256::ecdsa::SigningKey::from_slice(&[7u8; 32]).unwrap();
        let public = key
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .to_vec();
        let own = address(&public).unwrap();
        (public, own)
    }

    #[test]
    fn s6_req_nx_4_only_a_transfer_of_nothing_to_the_key_itself_is_a_cancel() {
        let (public, own) = key();
        assert!(is_cancel(&tx(&own, &[], &[]), &public, BOUNDS));
        assert!(
            !is_cancel(&tx(&[0x11; 20], &[], &[]), &public, BOUNDS),
            "to another"
        );
        assert!(
            !is_cancel(&tx(&own, &[1], &[]), &public, BOUNDS),
            "with value"
        );
        assert!(
            !is_cancel(&tx(&own, &[], &[0xab; 4]), &public, BOUNDS),
            "with data"
        );
        let mut legacy = tx(&own, &[], &[]);
        legacy[0] = 4;
        assert!(!is_cancel(&legacy, &public, BOUNDS), "another type");
        let mut trailing = tx(&own, &[], &[]);
        trailing.push(0);
        assert!(!is_cancel(&trailing, &public, BOUNDS), "trailing bytes");
        assert!(!is_cancel(&[2, 0xc0], &public, BOUNDS));
    }

    /// PR #39 second review SF2 (`REQ-NX-4`, `REQ-KY-3`): a cancel is signed only on its network's
    /// chain id, with a bare transfer's gas, and fees within the network's cap.
    #[test]
    fn sf2_req_nx_4_a_cancel_is_pinned_to_its_chain_and_its_fees_are_capped() {
        let (public, own) = key();
        let ok = |chain, priority, max_fee, gas| {
            is_cancel(
                &tx_with(chain, priority, max_fee, gas, &own, &[], &[]),
                &public,
                BOUNDS,
            )
        };
        assert!(ok(137, 1_000, 1_000, 21_000), "at the cap");
        assert!(!ok(1, 1, 2, 21_000), "another chain's id");
        assert!(!ok(80_002, 1, 2, 21_000), "another chain's id");
        assert!(!ok(137, 1, 1_001, 21_000), "a max fee above the cap");
        assert!(
            !ok(137, u128::from(u64::MAX), u128::from(u64::MAX), 21_000),
            "a fee that pays the lane away"
        );
        assert!(!ok(137, 3, 2, 21_000), "a priority fee above the max fee");
        assert!(!ok(137, 1, 2, 21_001), "more gas than a bare transfer");
        assert!(!ok(137, 1, 2, 1_000_000), "more gas than a bare transfer");
        assert!(!ok(137, 1, 2, 20_999), "less gas than a bare transfer");
        // A non-canonical chain id (a leading zero byte) is not read as 137.
        let mut padded = tx_with(137, 1, 2, 21_000, &own, &[], &[]);
        let at = padded.iter().position(|&b| b == 0x81).unwrap();
        padded.splice(at..=at, [0x82, 0x00]);
        padded[1] += 1;
        assert!(!is_cancel(&padded, &public, BOUNDS), "a padded chain id");
    }

    /// PR #39 third review SF-B (`REQ-KY-4`, `INV-9`): any EVM intent's chain id is read from its
    /// bytes, a payout's included, whatever the length of its calldata.
    #[test]
    fn sf_b_req_ky_4_the_chain_id_is_read_from_any_eip_1559_payload() {
        assert_eq!(chain_id(&tx(&[0x11; 20], &[], &[])), Some(137));
        assert_eq!(
            chain_id(&tx_with(
                8_453,
                1,
                2,
                300_000,
                &[0x11; 20],
                &[],
                &[0xab; 40]
            )),
            Some(8_453)
        );
        // A payout's calldata is longer than one byte of RLP length.
        let mut long = tx_with(80_002, 1, 2, 300_000, &[0x11; 20], &[], &[]);
        long.truncate(long.len() - 2);
        long.extend([0xb9, 0x01, 0x64]);
        long.extend([0xab; 356]);
        long.push(0xc0);
        let len = long.len() - 2;
        long.splice(
            1..2,
            [
                0xf9,
                u8::try_from(len >> 8).unwrap(),
                u8::try_from(len & 0xff).unwrap(),
            ],
        );
        assert_eq!(chain_id(&long), Some(80_002));
        assert_eq!(chain_id(&[0x01, 0xc0]), None, "another type");
        assert_eq!(chain_id(&[0x02, 0xc0]), None, "no fields");
        assert_eq!(
            chain_id(&[0x01, 0x02, 0x03, 0x04, 0x05]),
            None,
            "not a transaction"
        );
    }
}
