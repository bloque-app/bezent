// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Whether a destination is well formed for its network (`ERR-InvalidDestination`).
//!
//! The check is of form only. Whether the issuing network will accept a mint to it is never
//! checked (`REQ-IN-9`).

use blake2::{Blake2b512, Digest};

/// The longest destination taken, in bytes.
const MAX_LEN: usize = 256;

/// Whether `address` is well formed for a network whose address format is `format`.
pub fn well_formed(format: &str, address: &str) -> bool {
    match format {
        "evm" => evm(address),
        "ss58" => ss58(address),
        "opaque" => opaque(address),
        _ => false,
    }
}

/// `0x` and 20 bytes in hexadecimal. A mixed-case address carries the EIP-55 checksum, and is
/// well formed only when the checksum holds; an all-lower or all-upper one carries none.
fn evm(address: &str) -> bool {
    let well_formed = address.len() == 42
        && address.starts_with("0x")
        && address[2..].bytes().all(|b| b.is_ascii_hexdigit());
    if !well_formed {
        return false;
    }
    let hex = &address[2..];
    let has_lower = hex.bytes().any(|b| b.is_ascii_lowercase());
    let has_upper = hex.bytes().any(|b| b.is_ascii_uppercase());
    !(has_lower && has_upper) || eip55(hex)
}

/// Whether each letter of `hex` is upper case exactly when the matching nibble of the Keccak-256
/// of the lower-case address is 8 or more (EIP-55).
fn eip55(hex: &str) -> bool {
    use sha3::{Digest, Keccak256};
    let lower = hex.to_ascii_lowercase();
    let hash = Keccak256::digest(lower.as_bytes());
    hex.bytes().enumerate().all(|(i, b)| {
        let nibble = (hash[i / 2] >> (if i % 2 == 0 { 4 } else { 0 })) & 0x0f;
        !b.is_ascii_alphabetic() || b.is_ascii_uppercase() == (nibble >= 8)
    })
}

/// An SS58 address of a 32-byte account: a one- or two-byte network prefix, the account, and the
/// two-byte checksum (BLAKE2b-512 of `SS58PRE` and the rest).
fn ss58(address: &str) -> bool {
    let Ok(bytes) = bs58::decode(address).into_vec() else {
        return false;
    };
    let prefix_len = match (bytes.len(), bytes.first()) {
        (35, Some(b)) if *b < 64 => 1,
        (36, Some(b)) if (64..128).contains(b) => 2,
        _ => return false,
    };
    let (body, checksum) = bytes.split_at(bytes.len() - 2);
    debug_assert_eq!(body.len(), prefix_len + 32);
    let mut h = Blake2b512::new();
    h.update(b"SS58PRE");
    h.update(body);
    h.finalize()[..2] == *checksum
}

/// Anything printable, without spaces, of bounded length.
fn opaque(address: &str) -> bool {
    !address.is_empty()
        && address.len() <= MAX_LEN
        && address
            .chars()
            .all(|c| !c.is_whitespace() && !c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Alice's well-known development account, generic Substrate prefix 42.
    const ALICE: &str = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";

    #[test]
    fn err_invalid_destination_ss58() {
        assert!(well_formed("ss58", ALICE));
        let mut tampered = ALICE.to_owned();
        tampered.replace_range(10..11, "x");
        assert!(!well_formed("ss58", &tampered));
        assert!(!well_formed(
            "ss58",
            "0x0000000000000000000000000000000000000001"
        ));
    }

    #[test]
    fn err_invalid_destination_evm() {
        assert!(well_formed(
            "evm",
            "0x52908400098527886E0F7030069857D2E4169EE7"
        ));
        assert!(!well_formed("evm", "0x1234"));
        assert!(!well_formed(
            "evm",
            "0xZZ908400098527886E0F7030069857D2E4169EE7"
        ));
        assert!(!well_formed("evm", ALICE));
        // EIP-55: a mixed-case address whose checksum is wrong is malformed (a one-letter typo);
        // one written in a single case carries no checksum.
        assert!(!well_formed(
            "evm",
            "0x52908400098527886E0F7030069857D2E4169Ee7"
        ));
        assert!(well_formed(
            "evm",
            "0x52908400098527886e0f7030069857d2e4169ee7"
        ));
        assert!(well_formed(
            "evm",
            "0xde709f2102306220921060314715629080e2fb77"
        ));
        assert!(well_formed(
            "evm",
            "0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359"
        ));
    }

    #[test]
    fn err_invalid_destination_opaque_and_unknown_formats() {
        assert!(well_formed("opaque", "acct-123"));
        assert!(!well_formed("opaque", "has space"));
        assert!(!well_formed("opaque", ""));
        assert!(!well_formed("unknown", "acct-123"));
    }
}
