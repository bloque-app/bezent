// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The conformance suite every signing engine must pass (`services/signer/src/engine.rs`): run
//! against each engine with a local stand-in for its service (`tests/engines.rs`) and against the
//! real Vault signing plugin in CI (`tests/vault_plugin.rs`). An engine is accepted when it passes
//! this. Each check names the requirement it holds the engine to:
//!
//! - `REQ-CU-1`, `INV-9`: every signature verifies, independently of the engine, over what the key's
//!   scheme signs; ECDSA ones are 65 bytes `r ‖ s ‖ v` with a low `s` and recover the key.
//! - `REQ-KY-3`: the key's public key and address are the ones declared.
//! - `INV-9`: a key signs only as its own scheme and only its scheme's input; an unknown key signs
//!   nothing.
//! - `REQ-KY-1`: no error and no `Debug` carries a secret, the payload, its digest or a signature.
//! - The engine is ready.

#![allow(dead_code)]

pub mod doubles;

use std::fmt::Debug;

use bezent_signer::engine::{EngineKind, Signable, SigningEngine, address_of, prepare};
use bezent_signer::keys::{KeyError, Scheme};
use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};

/// The payload every case signs. Distinctive, so a leak is found.
pub const PAYLOAD: &[u8] = b"bezent conformance payload 7f3a9c";

/// A key the engine is expected to hold.
pub struct Case {
    /// Its id.
    pub id: &'static str,
    /// Its scheme.
    pub scheme: Scheme,
    /// The public key it must publish.
    pub public_key: Vec<u8>,
    /// Whether a signing service holds it (never released) or it is in the signer's memory.
    pub non_exportable: bool,
}

/// What a run needs besides the cases.
pub struct Setup<'a, E> {
    /// The engine.
    pub engine: &'a E,
    /// Its keys.
    pub cases: Vec<Case>,
    /// Bytes that must appear in no error and no `Debug`, hex-encoded there or not: the seeds.
    pub secrets: Vec<Vec<u8>>,
}

/// Checks that `signature` is a signature by `public_key` of `scheme` over `message`, with a
/// library of the scheme rather than the engine's own code path.
pub fn verify(scheme: Scheme, public_key: &[u8], message: &[u8], signature: &[u8]) {
    match scheme {
        Scheme::Sr25519 => {
            let public = schnorrkel::PublicKey::from_bytes(public_key).unwrap();
            let signature = schnorrkel::Signature::from_bytes(signature).unwrap();
            public
                .verify(
                    schnorrkel::signing_context(b"substrate").bytes(message),
                    &signature,
                )
                .expect("an sr25519 signature in the substrate context");
        }
        Scheme::Ed25519 => {
            let public: [u8; 32] = public_key.try_into().unwrap();
            let public = ed25519_dalek::VerifyingKey::from_bytes(&public).unwrap();
            public
                .verify_strict(
                    message,
                    &ed25519_dalek::Signature::from_slice(signature).unwrap(),
                )
                .expect("RFC 8032 over the message itself");
        }
        Scheme::Ecdsa | Scheme::Secp256k1Evm => {
            let Signable::Digest(digest) = prepare(scheme, message) else {
                unreachable!()
            };
            assert_eq!(signature.len(), 65, "r ‖ s ‖ v");
            let sig = Signature::from_slice(&signature[..64]).unwrap();
            assert!(sig.normalize_s().is_none(), "low s");
            assert!(signature[64] <= 1, "v is the recovery id, 0 or 1");
            let recovered = VerifyingKey::recover_from_prehash(
                &digest,
                &sig,
                RecoveryId::from_byte(signature[64]).unwrap(),
            )
            .unwrap();
            assert_eq!(
                recovered.to_encoded_point(true).as_bytes(),
                public_key,
                "v recovers the key"
            );
        }
    }
}

/// Fails if `text` carries any of `secrets`, the payload, its digests or `extra`.
pub fn assert_clean(text: &str, secrets: &[Vec<u8>], extra: &[&[u8]]) {
    let mut forbidden: Vec<Vec<u8>> = secrets.to_vec();
    forbidden.push(PAYLOAD.to_vec());
    for scheme in [Scheme::Ecdsa, Scheme::Secp256k1Evm] {
        if let Signable::Digest(d) = prepare(scheme, PAYLOAD) {
            forbidden.push(d.to_vec());
        }
    }
    forbidden.extend(extra.iter().map(|e| e.to_vec()));
    let lower = text.to_lowercase();
    for bytes in forbidden {
        assert!(
            !lower.contains(&hex::encode(&bytes)),
            "hex of a secret, payload or signature in: {text}"
        );
        if let Ok(as_text) = std::str::from_utf8(&bytes) {
            assert!(!text.contains(as_text), "a secret or payload in: {text}");
        }
    }
}

fn wrong_input(scheme: Scheme) -> Signable<'static> {
    if scheme.signs_digest() {
        Signable::Message(PAYLOAD)
    } else {
        Signable::Digest([7; 32])
    }
}

fn other_scheme(scheme: Scheme) -> Scheme {
    match scheme {
        Scheme::Ed25519 => Scheme::Sr25519,
        _ => Scheme::Ed25519,
    }
}

/// Runs the suite (module documentation).
pub async fn run<E: SigningEngine + Debug>(s: Setup<'_, E>) {
    let e = s.engine;
    let ids = e.key_ids();
    let mut errors = Vec::new();
    let mut signatures: Vec<Vec<u8>> = Vec::new();
    for case in &s.cases {
        let id = case.id;
        // REQ-KY-3: the key is the declared one.
        assert!(ids.iter().any(|k| k == id), "{id} is listed");
        assert_eq!(e.scheme(id).unwrap(), case.scheme, "{id}: scheme");
        assert_eq!(
            e.public_key(id).unwrap(),
            case.public_key,
            "{id}: public key"
        );
        assert_eq!(
            e.address(id).unwrap(),
            address_of(case.scheme, &case.public_key).unwrap(),
            "{id}: address"
        );
        assert_eq!(e.non_exportable(id), case.non_exportable, "{id}: custody");
        if case.non_exportable {
            assert_eq!(e.kind(), EngineKind::SigningService, "{id}: kind");
        }

        // REQ-CU-1, INV-9: a signature over the payload, through both entry points, verifies.
        let signature = e.sign(id, PAYLOAD).await.unwrap();
        verify(case.scheme, &case.public_key, PAYLOAD, &signature);
        let prepared = e
            .sign_prepared(id, case.scheme, prepare(case.scheme, PAYLOAD))
            .await
            .unwrap();
        verify(case.scheme, &case.public_key, PAYLOAD, &prepared);
        if case.scheme != Scheme::Sr25519 {
            // RFC 6979 and RFC 8032: deterministic.
            assert_eq!(signature, prepared, "{id}: deterministic");
        }
        signatures.push(signature);

        // INV-9: only the key's own scheme and input.
        let err = e
            .sign_prepared(id, case.scheme, wrong_input(case.scheme))
            .await
            .unwrap_err();
        assert!(matches!(err, KeyError::Refused(_)), "{id}: {err}");
        assert!(
            !err.is_transient(),
            "{id}: a refusal is not worth waiting for"
        );
        errors.push(err);
        let other = other_scheme(case.scheme);
        let err = e
            .sign_prepared(id, other, prepare(other, PAYLOAD))
            .await
            .unwrap_err();
        assert!(matches!(err, KeyError::Refused(_)), "{id}: {err}");
        errors.push(err);
    }

    // An unknown key has no scheme, no public key and signs nothing.
    for err in [
        e.scheme("no-such-key").unwrap_err(),
        e.public_key("no-such-key").unwrap_err(),
        e.address("no-such-key").unwrap_err(),
        e.sign("no-such-key", PAYLOAD).await.unwrap_err(),
    ] {
        assert!(matches!(err, KeyError::UnknownKey(_)), "{err}");
        errors.push(err);
    }

    // REQ-KY-1: nothing secret in an error or the engine's Debug.
    let extra: Vec<&[u8]> = signatures.iter().map(Vec::as_slice).collect();
    for err in &errors {
        assert_clean(&format!("{err} {err:?}"), &s.secrets, &extra);
    }
    assert_clean(&format!("{e:?}"), &s.secrets, &extra);

    let health = e.health().await;
    assert!(health.ready, "{}", health.detail);
}

/// A service that does not answer: the signature fails, worth retrying, and carries nothing
/// (`REQ-KY-1`).
pub async fn fails_closed_when_down<E: SigningEngine + Debug>(engine: &E, id: &str) {
    let err = engine.sign(id, PAYLOAD).await.unwrap_err();
    assert!(err.is_transient(), "{err}");
    assert_clean(&format!("{err} {err:?}"), &[], &[]);
    assert!(!engine.health().await.ready, "not ready while down");
}

/// A service that signs with another key: nothing is returned (`INV-9`), and the error is not
/// worth waiting for.
pub async fn refuses_a_rogue<E: SigningEngine + Debug>(engine: &E, id: &str) {
    let err = engine.sign(id, PAYLOAD).await.unwrap_err();
    assert!(matches!(err, KeyError::Backend(_)), "{err}");
    assert!(err.to_string().contains("INV-9"), "{err}");
    assert_clean(&format!("{err} {err:?}"), &[], &[]);
}
