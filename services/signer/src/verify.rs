// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Verifying a signature against the exact bytes the core recorded (`INV-9`, `AC-I1.2`; review M1).
//!
//! Who wrote a row in `signatures` proves nothing: the signer writes them, and the signer is what
//! the compromise path must catch. A signature is bezent's only if it verifies, with the key's
//! public key, over the `message` of an intent the core recorded. The core checks that twice: when
//! the signer returns a signature ([`crate::link::Client::sign`]) and when one is seen on chain
//! ([`crate::compromise::check`]).
//!
//! The schemes are those of [`crate::keys`]: `sr25519` over the message in the `substrate`
//! context; `ecdsa` over its BLAKE2b-256; `secp256k1-evm` over its Keccak-256; `ed25519` (the
//! statement key, `REQ-PR-1`) over the message itself, strictly (RFC 8032, no malleable encodings).

use k256::ecdsa::signature::hazmat::PrehashVerifier;
use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
use sha3::{Digest, Keccak256};

/// The 64 bytes a secp256k1 signature is matched on, `r ‖ s`, with `s` normalised to the low
/// half. bezent's signatures are low-`s`; a third party may flip a pending transaction's `s` to
/// `n − s` (review N5), which must not read as another signature. sr25519 signatures, and anything
/// that is not a secp256k1 signature, are returned as they are.
pub fn canonical_prefix(public_key: &[u8], signature: &[u8]) -> Option<Vec<u8>> {
    let prefix = signature.get(..64)?;
    if public_key.len() == 33
        && let Ok(sig) = Signature::from_slice(prefix)
    {
        return Some(sig.normalize_s().unwrap_or(sig).to_bytes().to_vec());
    }
    Some(prefix.to_vec())
}

fn digest(scheme: &str, message: &[u8]) -> Option<Vec<u8>> {
    match scheme {
        "ecdsa" => Some(crate::keys::ecdsa_digest(message).to_vec()),
        "secp256k1-evm" => Some(Keccak256::digest(message).to_vec()),
        _ => None,
    }
}

/// Whether `signature` is `public_key`'s signature over `message` under `scheme`. Only the first
/// 64 bytes count (`r ‖ s`, or the whole sr25519 signature): a secp256k1 recovery byte is written
/// differently on chain. A high-`s` secp256k1 signature is accepted as its low-`s` twin.
pub fn verifies(scheme: &str, public_key: &[u8], message: &[u8], signature: &[u8]) -> bool {
    let Some(rs) = signature.get(..64) else {
        return false;
    };
    match scheme {
        "sr25519" => {
            let (Ok(public), Ok(sig)) = (
                schnorrkel::PublicKey::from_bytes(public_key),
                schnorrkel::Signature::from_bytes(rs),
            ) else {
                return false;
            };
            public
                .verify(
                    schnorrkel::signing_context(b"substrate").bytes(message),
                    &sig,
                )
                .is_ok()
        }
        "ed25519" => {
            let (Ok(public), Ok(sig)) = (
                <[u8; 32]>::try_from(public_key),
                ed25519_dalek::Signature::from_slice(rs),
            ) else {
                return false;
            };
            ed25519_dalek::VerifyingKey::from_bytes(&public)
                .is_ok_and(|k| k.verify_strict(message, &sig).is_ok())
        }
        "ecdsa" | "secp256k1-evm" => {
            let (Ok(key), Ok(sig), Some(digest)) = (
                VerifyingKey::from_sec1_bytes(public_key),
                Signature::from_slice(rs),
                digest(scheme, message),
            ) else {
                return false;
            };
            let sig = sig.normalize_s().unwrap_or(sig);
            key.verify_prehash(&digest, &sig).is_ok()
        }
        _ => false,
    }
}

/// Whether `signature` is exactly what the signer must return for `message`: [`verifies`], of the
/// scheme's length (64 bytes for sr25519, 65 for secp256k1), and for secp256k1 low-`s` with a
/// recovery byte that recovers `public_key`, since the transaction carries it as it is.
pub fn verifies_exactly(scheme: &str, public_key: &[u8], message: &[u8], signature: &[u8]) -> bool {
    if !verifies(scheme, public_key, message, signature) {
        return false;
    }
    match scheme {
        "sr25519" | "ed25519" => signature.len() == 64,
        "ecdsa" | "secp256k1-evm" => {
            let (Some(digest), Ok(sig), Some(recovery)) = (
                digest(scheme, message),
                Signature::from_slice(&signature[..64]),
                signature
                    .get(64)
                    .filter(|_| signature.len() == 65)
                    .and_then(|&v| RecoveryId::from_byte(v)),
            ) else {
                return false;
            };
            sig.normalize_s().is_none()
                && VerifyingKey::recover_from_prehash(&digest, &sig, recovery)
                    .is_ok_and(|k| k.to_encoded_point(true).as_bytes() == public_key)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{FileBackend, SigningEngine};

    const SEED: &str = "0x0202020202020202020202020202020202020202020202020202020202020202";

    fn backend(scheme: &str) -> FileBackend {
        FileBackend::parse(
            format!(
                r#"{{"environment": "test", "keys": [{{"id": "k", "scheme": "{scheme}", "seed": "{SEED}"}}]}}"#
            )
            .as_bytes(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn inv_9_a_signature_verifies_only_over_its_own_bytes() {
        for scheme in ["sr25519", "ecdsa", "secp256k1-evm"] {
            let b = backend(scheme);
            let public = b.public_key("k").unwrap();
            let signature = b.sign("k", b"recorded").await.unwrap();
            assert!(
                verifies_exactly(scheme, &public, b"recorded", &signature),
                "{scheme}"
            );
            assert!(!verifies(scheme, &public, b"rogue", &signature), "{scheme}");
            assert!(
                !verifies_exactly(scheme, &public, b"rogue", &signature),
                "{scheme}"
            );
            // Another key's public key.
            let other = backend(if scheme == "sr25519" {
                "ecdsa"
            } else {
                "sr25519"
            });
            assert!(
                !verifies(
                    scheme,
                    &other.public_key("k").unwrap(),
                    b"recorded",
                    &signature
                ),
                "{scheme}"
            );
        }
    }

    #[tokio::test]
    async fn ac_i1_2_a_malleated_secp256k1_signature_is_still_bezents() {
        let b = backend("secp256k1-evm");
        let public = b.public_key("k").unwrap();
        let signature = b.sign("k", b"recorded").await.unwrap();
        // Flip s to n − s, as a third party could in a pending transaction.
        let sig = Signature::from_slice(&signature[..64]).unwrap();
        let (r, s) = sig.split_scalars();
        let high = Signature::from_scalars(r, -*s).unwrap();
        let mut malleated = high.to_bytes().to_vec();
        malleated.push(signature[64] ^ 1);
        assert!(verifies("secp256k1-evm", &public, b"recorded", &malleated));
        assert!(!verifies_exactly(
            "secp256k1-evm",
            &public,
            b"recorded",
            &malleated
        ));
        assert_eq!(
            canonical_prefix(&public, &malleated),
            canonical_prefix(&public, &signature)
        );
    }
}
