// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Bridge references, body digests and commitments.
//!
//! A bridge reference is bezent's own, derived deterministically, distinct from the request
//! reference (`REQ-ID-3`):
//!
//! - a deposit that binds an arrival: from the arrival's coordinate (`REQ-IN-3`), `bz-d-…`;
//! - a refund of an unassociated arrival: from the arrival's coordinate, in its own domain, `bz-u-…`;
//! - a request refused before it bound anything: from (requester, request reference), `bz-r-…`, so
//!   it never takes a coordinate's reference.
//!
//! Every derivation is SHA-256 over a domain tag and length-prefixed fields.

use bezent_core::{Coordinate, Position};
use blake2::Blake2b;
use blake2::digest::consts::U32;
use sha2::{Digest, Sha256};

/// SHA-256 over `tag` and each field, each length-prefixed.
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

fn coordinate_fields(c: &Coordinate) -> [u8; 32] {
    let (kind, index) = match c.position {
        Position::Transaction(i) => (b"transaction".as_slice(), i),
        Position::Log(i) => (b"log".as_slice(), i),
    };
    tagged(
        "bezent/coordinate/v0",
        &[
            c.network.as_str().as_bytes(),
            &c.block_number.to_be_bytes(),
            c.block_hash.as_bytes(),
            kind,
            &index.to_be_bytes(),
        ],
    )
}

fn short(prefix: &str, digest: [u8; 32]) -> String {
    format!("{prefix}{}", hex::encode(&digest[..20]))
}

/// The bridge reference of the deposit that binds the arrival at `c` (`REQ-IN-3`).
pub fn deposit(c: &Coordinate) -> String {
    short(
        "bz-d-",
        tagged("bezent/deposit/v0", &[&coordinate_fields(c)]),
    )
}

/// The bridge reference of the refund of the unassociated arrival at `c` (`REQ-RQ-6`).
pub fn unassociated_refund(c: &Coordinate) -> String {
    short(
        "bz-u-",
        tagged("bezent/refund/v0", &[&coordinate_fields(c)]),
    )
}

/// The bridge reference of a request refused before it bound anything.
pub fn refused(requester: &str, request_reference: &str) -> String {
    short(
        "bz-r-",
        tagged(
            "bezent/refused/v0",
            &[requester.as_bytes(), request_reference.as_bytes()],
        ),
    )
}

/// The digest a repeat is compared by (`REQ-ID-1`): the request's kind and its canonical body.
pub fn body_digest(kind: &str, canonical_body: &[u8]) -> [u8; 32] {
    tagged("bezent/request/v0", &[kind.as_bytes(), canonical_body])
}

/// The commitment of relayed payout instructions: BLAKE2b-256 of their bytes (`REQ-RQ-9`, PLAN
/// `DEC-14`).
pub fn commitment(instructions: &[u8]) -> [u8; 32] {
    Blake2b::<U32>::digest(instructions).into()
}

/// Compares two digests in constant time.
pub fn same_digest(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use bezent_core::{BlockHash, NetworkId};

    fn coordinate(index: u32) -> Coordinate {
        Coordinate {
            network: NetworkId::new("origin-a").unwrap(),
            block_number: 7,
            block_hash: BlockHash::new(vec![0xaa]),
            position: Position::Log(index),
        }
    }

    #[test]
    fn req_in_3_deposit_bridge_reference_is_derived_from_the_coordinate() {
        assert_eq!(deposit(&coordinate(0)), deposit(&coordinate(0)));
        assert_ne!(deposit(&coordinate(0)), deposit(&coordinate(1)));
        assert_ne!(deposit(&coordinate(0)), unassociated_refund(&coordinate(0)));
    }

    /// `REQ-IN-3`, `REQ-IN-10`: the store derives a directed deposit's bridge reference in SQL
    /// (`deposit_reference()`, migration 0144, which `directed_deposit()` writes). These vectors are
    /// pinned in `crates/store/tests/directed_deposits.rs` as well, so the two never drift.
    #[test]
    fn req_in_10_deposit_reference_vectors_the_store_derives_the_same() {
        let at = |network: &str, block: u64, hash: Vec<u8>, position: Position| Coordinate {
            network: NetworkId::new(network).unwrap(),
            block_number: block,
            block_hash: BlockHash::new(hash),
            position,
        };
        assert_eq!(
            deposit(&at("origin-a", 7, vec![0xaa], Position::Log(0))),
            "bz-d-f974333c8ffbef8f6e0e9b3bd970def281dd7767"
        );
        // A 32-byte hash, a large block number and the largest log index (a u32's four bytes).
        assert_eq!(
            deposit(&at(
                "polygon",
                96_983_980,
                vec![0x11; 32],
                Position::Log(u32::MAX)
            )),
            "bz-d-668330a314e9d999ee65c91f51fb6dedd62becfe"
        );
        assert_eq!(
            deposit(&at("origin-a", 7, vec![0xaa], Position::Log(1))),
            "bz-d-071b8b19756a50ad5929639577cdea067cc85a88"
        );
        assert_eq!(
            deposit(&at("origin-a", 1, vec![0xab, 1], Position::Transaction(3))),
            "bz-d-d1321a13639f570b72f526e3b141f8770dbcd76b"
        );
    }

    /// `REQ-RQ-6`, `REQ-IN-11`: the store derives the return of a directed arrival's bridge reference
    /// in SQL (`refund_reference()`, migration 0144, which the guard checks a `bz-u-` row against).
    /// The same vectors, coordinates as for `bz-d-`, are pinned in
    /// `crates/store/tests/directed_deposits.rs`, so the two never drift.
    #[test]
    fn req_in_11_refund_reference_vectors_the_store_derives_the_same() {
        let at = |network: &str, block: u64, hash: Vec<u8>, position: Position| Coordinate {
            network: NetworkId::new(network).unwrap(),
            block_number: block,
            block_hash: BlockHash::new(hash),
            position,
        };
        assert_eq!(
            unassociated_refund(&at("origin-a", 7, vec![0xaa], Position::Log(0))),
            "bz-u-785300f9639f00608a29c3f84a185865db732118"
        );
        assert_eq!(
            unassociated_refund(&at("origin-a", 7, vec![0xaa], Position::Log(1))),
            "bz-u-38661c655e8905bb6befb155263552f8bd0d1000"
        );
        assert_eq!(
            unassociated_refund(&at(
                "polygon",
                96_983_980,
                vec![0x11; 32],
                Position::Log(u32::MAX)
            )),
            "bz-u-32a768ecd707102dccbf4e9ddc6156624b235afe"
        );
        assert_eq!(
            unassociated_refund(&at("origin-a", 1, vec![0xab, 1], Position::Transaction(3))),
            "bz-u-00c2a6caea37e16941afee596e77c4dc6dee27a5"
        );
    }

    #[test]
    fn req_id_4_refused_bridge_reference_is_scoped_to_the_requester() {
        assert_ne!(refused("a", "r1"), refused("b", "r1"));
        assert_ne!(refused("a", "r1"), refused("a", "r2"));
        // Length prefixes keep ("ab", "c") apart from ("a", "bc").
        assert_ne!(refused("ab", "c"), refused("a", "bc"));
    }

    #[test]
    fn req_rq_9_commitment_is_blake2b_256() {
        // BLAKE2b-256 of the empty input (RFC 7693 parameters, 32-byte output).
        assert_eq!(
            hex::encode(commitment(b"")),
            "0e5751c026e543b2e8ab2eb06099daa1d1e5df47778f7787faab45cdf12fe3a8"
        );
        assert!(same_digest(&commitment(b"x"), &commitment(b"x")));
        assert!(!same_digest(&commitment(b"x"), &commitment(b"y")));
    }
}
