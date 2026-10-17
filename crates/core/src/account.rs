// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! One canonical form for an account, wherever bezent compares accounts (`REQ-IN-5`, DEC-15).
//!
//! The ingress, the core and the store's declared list of ledger accounts all compare the
//! canonical bytes of an account, never its spelling:
//!
//! - `evm`: the 20 bytes of `0x` and 40 hex digits, in any case;
//! - `ss58`: the 32-byte public key, from an SS58 address under any prefix (checksum verified) or
//!   from `0x` and 64 hex digits;
//! - `opaque`: the address byte for byte.

use blake2::{Blake2b512, Digest};

/// The canonical bytes of `address` on a network whose address format is `format`; `None` when it
/// is not an address of that format.
#[must_use]
pub fn canonical_account(format: &str, address: &str) -> Option<Vec<u8>> {
    match format {
        "evm" => {
            let digits = address.strip_prefix("0x")?;
            (digits.len() == 40)
                .then(|| hex::decode(digits).ok())
                .flatten()
        }
        "ss58" => ss58_account(address).map(|a| a.to_vec()),
        "opaque" => (!address.is_empty()).then(|| address.as_bytes().to_vec()),
        _ => None,
    }
}

/// The 32-byte account of an SS58 address (checksum verified) or of `0x` and 64 hex digits.
fn ss58_account(address: &str) -> Option<[u8; 32]> {
    if let Some(digits) = address.strip_prefix("0x") {
        return (digits.len() == 64)
            .then(|| hex::decode(digits).ok()?.try_into().ok())
            .flatten();
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

/// What the declared list of ledger accounts says of a destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Listing {
    /// Not on the list: the destination may be used.
    Clear,
    /// On the list: a contract account of a ledger.
    Listed,
    /// The destination does not parse for its network, or an entry on the list does not agree with
    /// its own canonical bytes: nothing can be said, so nothing is delivered (never fails open).
    Unknown,
}

/// Whether `destination` is on the list `entries` (each an address as declared and the canonical
/// bytes the store set for it), on a network whose address format is `format`.
#[must_use]
pub fn listing(format: &str, destination: &str, entries: &[(String, Vec<u8>)]) -> Listing {
    let Some(account) = canonical_account(format, destination) else {
        return Listing::Unknown;
    };
    let mut listed = false;
    for (address, bytes) in entries {
        if canonical_account(format, address).as_deref() != Some(bytes.as_slice()) {
            return Listing::Unknown;
        }
        listed |= *bytes == account;
    }
    if listed {
        Listing::Listed
    } else {
        Listing::Clear
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALICE: &str = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";
    const ALICE_POLKADOT: &str = "15oF4uVJwmo4TdGW7VfQxNLavjCXviqxT9S1MgbjMNHr6Sp5";
    const ALICE_RAW: &str = "0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d";
    const BOB: &str = "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty";

    #[test]
    fn req_in_5_one_canonical_form_for_every_spelling_of_an_account() {
        let alice = canonical_account("ss58", ALICE).unwrap();
        assert_eq!(alice.len(), 32);
        assert_eq!(
            canonical_account("ss58", ALICE_POLKADOT),
            Some(alice.clone())
        );
        assert_eq!(canonical_account("ss58", ALICE_RAW), Some(alice.clone()));
        assert_ne!(canonical_account("ss58", BOB), Some(alice));
        let mixed = "0x52908400098527886E0F7030069857D2E4169EE7";
        let lower = canonical_account("evm", &mixed.to_ascii_lowercase()).unwrap();
        assert_eq!(lower.len(), 20);
        assert_eq!(canonical_account("evm", mixed), Some(lower.clone()));
        let upper = format!("0x{}", mixed[2..].to_ascii_uppercase());
        assert_eq!(canonical_account("evm", &upper), Some(lower));
    }

    #[test]
    fn req_in_5_what_is_not_an_account_has_no_canonical_form() {
        assert_eq!(canonical_account("ss58", "5Grw"), None);
        // A bad checksum.
        assert_eq!(canonical_account("ss58", &ALICE.replace('Q', "R")), None);
        assert_eq!(canonical_account("ss58", "0x01"), None);
        assert_eq!(canonical_account("evm", "0x01"), None);
        assert_eq!(canonical_account("evm", ALICE), None);
        assert_eq!(canonical_account("opaque", ""), None);
        assert_eq!(canonical_account("other", "x"), None);
    }

    #[test]
    fn req_in_5_the_list_matches_by_bytes_in_both_directions_and_never_fails_open() {
        let entry = |a: &str| (a.to_owned(), canonical_account("ss58", a).unwrap());
        // Listed as SS58: a raw key and another prefix name it; listed raw: SS58 names it.
        for (listed, asked) in [
            (ALICE, ALICE_RAW),
            (ALICE, ALICE_POLKADOT),
            (ALICE_RAW, ALICE),
            (ALICE_POLKADOT, ALICE_RAW),
            (ALICE_RAW, ALICE_POLKADOT),
        ] {
            assert_eq!(listing("ss58", asked, &[entry(listed)]), Listing::Listed);
        }
        assert_eq!(listing("ss58", BOB, &[entry(ALICE)]), Listing::Clear);
        assert_eq!(listing("ss58", BOB, &[]), Listing::Clear);
        // A destination that does not parse, or an entry that disagrees with its bytes.
        assert_eq!(
            listing("ss58", "nonsense", &[entry(ALICE)]),
            Listing::Unknown
        );
        assert_eq!(
            listing("ss58", BOB, &[(ALICE.into(), vec![0; 32])]),
            Listing::Unknown
        );
        assert_eq!(
            listing(
                "ss58",
                BOB,
                &[(
                    ALICE.replace('Q', "R"),
                    canonical_account("ss58", ALICE).unwrap()
                )]
            ),
            Listing::Unknown
        );
    }
}
