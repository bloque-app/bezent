// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
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
