// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! References the core derives, and the issuing network's account encoding.
//!
//! Every derivation is SHA-256 over a domain tag and length-prefixed fields, as the ingress derives
//! its own (`services/ingress/src/refs.rs`), and the coordinate digest is the same one, so a
//! withdrawal's bridge reference sits beside a deposit's in one scheme:
//!
//! - a withdrawal bound to the redemption (redeemer, reference): `bz-w-…` (`REQ-ID-3`,
//!   `REQ-OT-2`), and the 32 bytes its payout writes on the origin network, derived from that
//!   bridge reference, so unique to the withdrawal (PLAN `DEC-10`, the reference emitter);
//! - the refund of a transfer: `bz-f-…`, from the original's bridge reference (`REQ-RF-3`);
//! - a deposit's mint reference, the 32 bytes the mint writes on chain: from the arrival's
//!   coordinate, so it is fixed by the arrival and consumed once (`REQ-IN-4`, `INV-2`). It is
//!   derivable from public data, which is why a redemption's reference is scoped to its redeemer
//!   and never shares a namespace with it (amendment 0008).

use blake2::Blake2b512;
use sha2::{Digest, Sha256};

/// A coordinate as the store holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coordinate {
    /// The network.
    pub network: String,
    /// The block number.
    pub block_number: i64,
    /// The block hash.
    pub block_hash: Vec<u8>,
    /// `transaction` or `log`.
    pub position_kind: String,
    /// The index.
    pub position_index: i64,
}

fn tagged(tag: &str, fields: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update((tag.len() as u64).to_be_bytes());
    h.update(tag.as_bytes());
    for f in fields {
        h.update((f.len() as u64).to_be_bytes());
        h.update(f);
    }
    h.finalize().into()
}

/// The coordinate digest the ingress uses (`bezent/coordinate/v0`).
fn coordinate_fields(c: &Coordinate) -> [u8; 32] {
    let block = u64::try_from(c.block_number).unwrap_or(0);
    let index = u32::try_from(c.position_index).unwrap_or(0);
    tagged(
        "bezent/coordinate/v0",
        &[
            c.network.as_bytes(),
            &block.to_be_bytes(),
            &c.block_hash,
            c.position_kind.as_bytes(),
            &index.to_be_bytes(),
        ],
    )
}

fn short(prefix: &str, digest: [u8; 32]) -> String {
    format!("{prefix}{}", hex::encode(&digest[..20]))
}

/// The bytes a redeemer is identified by: its 32-byte account when the address decodes to one, the
/// lowercased address otherwise, so one account written two ways is one redeemer.
fn account_bytes(address: &str) -> Vec<u8> {
    account32(address).map_or_else(|| address.to_ascii_lowercase().into_bytes(), |a| a.to_vec())
}

/// The bridge reference of the withdrawal bound to the redemption (`redeemer`, `reference`): the
/// redemption's on-chain reference (`REQ-ID-3`, `REQ-OT-2`, amendment 0008).
pub fn withdrawal(redeemer: &str, reference: &[u8]) -> String {
    short(
        "bz-w-",
        tagged(
            "bezent/withdrawal/v1",
            &[&account_bytes(redeemer), reference],
        ),
    )
}

/// The 32-byte reference the payout of the withdrawal whose bridge reference is `bridge_reference`
/// writes on its origin network (PLAN `DEC-10`: the emitter's `Reference(bytes32)`). It is that
/// bridge reference as the emitter carries it, so no other withdrawal's payout carries it
/// (`REQ-OT-2`, `INV-2`).
pub fn payout_reference(bridge_reference: &str) -> [u8; 32] {
    tagged("bezent/payout-reference/v0", &[bridge_reference.as_bytes()])
}

/// Whether two addresses name one account: as 32-byte accounts when both decode to one, otherwise
/// case-insensitively (an EVM address).
pub fn same_account(a: &str, b: &str) -> bool {
    match (account32(a), account32(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a.eq_ignore_ascii_case(b),
    }
}

/// The bridge reference of the refund of the transfer whose bridge reference is `original`
/// (`REQ-RF-3`: derived deterministically from the original's).
pub fn refund(original: &str) -> String {
    short(
        "bz-f-",
        tagged("bezent/refund-of/v0", &[original.as_bytes()]),
    )
}

/// The 32-byte on-chain reference the mint of the deposit bound to the arrival at `c` carries
/// (`REQ-IN-4`, `INV-2`).
pub fn mint_reference(c: &Coordinate) -> [u8; 32] {
    tagged("bezent/mint-reference/v0", &[&coordinate_fields(c)])
}

/// The 32-byte on-chain reference the payout of the refund of the unassociated arrival at `c`
/// carries (`REQ-RQ-6`, `REQ-ID-2`), in a domain of its own.
pub fn unassociated_refund_reference(c: &Coordinate) -> [u8; 32] {
    tagged(
        "bezent/unassociated-refund-reference/v0",
        &[&coordinate_fields(c)],
    )
}

/// The 32-byte account of an issuing-network address: SS58 (checksum verified) or `0x` and 64 hex
/// digits.
pub fn account32(address: &str) -> Option<[u8; 32]> {
    if let Some(digits) = address.strip_prefix("0x") {
        return hex::decode(digits).ok()?.try_into().ok();
    }
    let bytes = bs58::decode(address).into_vec().ok()?;
    let prefix_len = match (bytes.len(), bytes.first()) {
        (35, Some(b)) if *b < 64 => 1,
        (36, Some(b)) if (64..128).contains(b) => 2,
        _ => return None,
    };
    let (body, checksum) = bytes.split_at(bytes.len() - 2);
    let mut h = Blake2b512::new();
    h.update(b"SS58PRE");
    h.update(body);
    if h.finalize()[..2] != *checksum {
        return None;
    }
    body[prefix_len..].try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(index: i64) -> Coordinate {
        Coordinate {
            network: "origin-a".into(),
            block_number: 7,
            block_hash: vec![0xaa],
            position_kind: "log".into(),
            position_index: index,
        }
    }

    #[test]
    fn req_id_3_derived_references_are_deterministic_and_distinct() {
        assert_eq!(withdrawal("0xaa", &[1; 32]), withdrawal("0xAA", &[1; 32]));
        assert_ne!(withdrawal("0xaa", &[1; 32]), withdrawal("0xbb", &[1; 32]));
        assert_ne!(withdrawal("0xaa", &[1; 32]), withdrawal("0xaa", &[2; 32]));
        assert_ne!(
            payout_reference(&withdrawal("0xaa", &[1; 32])),
            payout_reference(&withdrawal("0xbb", &[1; 32]))
        );
        assert_ne!(refund("bz-w-1"), refund("bz-w-2"));
        assert_eq!(mint_reference(&c(0)), mint_reference(&c(0)));
        assert_ne!(mint_reference(&c(0)), mint_reference(&c(1)));
        assert_ne!(mint_reference(&c(0)), unassociated_refund_reference(&c(0)));
    }

    #[test]
    fn req_in_5_issuing_addresses_decode_to_their_account() {
        // Alice, prefix 42.
        let alice = account32("5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY").unwrap();
        assert_eq!(
            hex::encode(alice),
            "d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d"
        );
        assert_eq!(account32(&format!("0x{}", hex::encode(alice))), Some(alice));
        // A broken checksum.
        assert_eq!(
            account32("5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQZ"),
            None
        );
    }
}
