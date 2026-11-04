// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! What the ingress and the core must agree on about an attested backend's reports (`REQ-BE-4`,
//! `REQ-BE-5`; PLAN `DEC-14`): what a party signs, and the coordinate a report becomes.

use blake2::digest::consts::U32;
use blake2::{Blake2b, Digest};

/// What separates a report's signature from anything else the key might sign.
const DOMAIN: &[u8] = b"bezent-attested-report-v1\0";

/// The position a report's observation has in its block: every report is alone in its own.
pub const REPORT_POSITION_INDEX: u32 = 0;

/// The message a party signs for a report: the domain, then the document's bytes, exactly as sent.
/// There is no canonical form to reproduce (F-011 NOTES 3).
pub fn report_message(document: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(DOMAIN.len() + document.len());
    message.extend_from_slice(DOMAIN);
    message.extend_from_slice(document);
    message
}

/// The block hash of a report's coordinate: BLAKE2b-256 of what was signed, so anyone holding the
/// report can recompute it.
pub fn report_digest(document: &[u8]) -> [u8; 32] {
    Blake2b::<U32>::digest(report_message(document)).into()
}

/// The commitment of relayed payout instructions: BLAKE2b-256 of their bytes (`REQ-RQ-9`, PLAN
/// `DEC-14`).
pub fn instructions_commitment(instructions: &[u8]) -> [u8; 32] {
    Blake2b::<U32>::digest(instructions).into()
}

/// A withdrawal's destination on an attested backend: its commitment, marked, never an address
/// (`REQ-RQ-9`). It names where the instructions are bound, and nothing is delivered to it.
pub fn commitment_destination(commitment: &str) -> String {
    format!("commitment:{commitment}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req_be_4_a_reports_digest_covers_its_domain_and_its_bytes() {
        assert_ne!(report_digest(b"{}"), report_digest(b"{ }"));
        assert_eq!(&report_message(b"{}")[..DOMAIN.len()], DOMAIN);
        assert_eq!(&report_message(b"{}")[DOMAIN.len()..], b"{}");
        let plain: [u8; 32] = Blake2b::<U32>::digest(b"{}").into();
        assert_ne!(report_digest(b"{}"), plain);
    }
}
