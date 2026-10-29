// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Signing engines (`REQ-CU-1`, `REQ-KY-1`, `INV-9`): one interface to every way the signer holds a
//! key and signs with it.
//!
//! A [`SigningEngine`] answers, for each key id it holds, the key's [`Scheme`], its public key and
//! the address or account a network knows it by, and signs a [`Signable`] for a scheme: the
//! message itself (sr25519, ed25519) or a 32-byte digest the signer computed ([`prepare`]: the
//! BLAKE2b-256 a Kreivo ECDSA account signs, the Keccak-256 an EVM key signs). It has no way to be
//! asked for a secret. The engines:
//!
//! | Engine | [`EngineKind`] | Schemes | Module |
//! |---|---|---|---|
//! | test keys from a file; the other keys read from Vault KV | [`EngineKind::InProcess`] | all four | [`crate::keys`], [`crate::vault`] |
//! | AWS KMS | [`EngineKind::SigningService`] | `ecdsa`, `secp256k1-evm` | [`crate::kms`] |
//! | Vault Transit | [`EngineKind::SigningService`] | `ed25519` | [`crate::transit`] |
//! | a Vault signing plugin | [`EngineKind::SigningService`] | `secp256k1-evm` | [`crate::vault_plugin`] |
//!
//! The configuration names which engine holds which key (`keys.backend`, [`crate::config`]); a
//! signer runs one signing service and, optionally, the other keys from Vault KV, joined by
//! [`crate::custody::Mixed`] into one engine.
//!
//! The rules every engine keeps, and that the conformance suite (`services/signer/tests/engines.rs`)
//! holds each one to:
//!
//! - A key signs only as its own scheme and only its scheme's input: a digest for a secp256k1 key,
//!   the message for the others ([`check_input`]). Anything else is [`KeyError::Refused`].
//! - What a signing service returns is checked before it leaves: an ECDSA signature is turned into
//!   `r ‖ s ‖ v` with a low `s`, `v` found by recovering the key, and verified against the public
//!   key ([`recoverable_from_der`]); an ed25519 one is verified strictly (`INV-9`).
//! - Errors say which key and what went wrong, never a key, a token, the message, the digest or a
//!   signature (`REQ-KY-1`). A service that did not answer is [`KeyError::Unavailable`], which is
//!   worth retrying; an answer that is wrong is [`KeyError::Backend`], which is not.
//! - `Debug` shows key ids, schemes and public keys only.

use std::fmt;
use std::future::Future;

use k256::ecdsa::signature::hazmat::PrehashVerifier;
use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};

use crate::keys::{Environment, KeyError, Scheme, ecdsa_digest, evm_digest};

/// Where an engine's keys are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    /// Read into the signer's own memory: the file of test keys, or Vault KV.
    InProcess,
    /// In a service that signs on request and never releases the key: AWS KMS, Vault Transit, a
    /// Vault signing plugin.
    SigningService,
}

impl EngineKind {
    /// Its name in a message.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InProcess => "in-process",
            Self::SigningService => "signing service",
        }
    }
}

/// Whether an engine can sign now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Health {
    /// Ready, or not.
    pub ready: bool,
    /// Why not, in words; empty when ready. Never a secret.
    pub detail: String,
}

impl Health {
    /// Ready.
    pub fn ready() -> Self {
        Self {
            ready: true,
            detail: String::new(),
        }
    }

    /// Not ready, for `why`.
    pub fn not_ready(why: impl Into<String>) -> Self {
        Self {
            ready: false,
            detail: why.into(),
        }
    }

    /// Both ready, or the first reason why not.
    pub fn and(self, other: Self) -> Self {
        if self.ready { other } else { self }
    }
}

/// What a key signs, as the signer prepared it ([`prepare`]). Its `Debug` shows the kind and the
/// length, never the bytes.
#[derive(Clone, Copy)]
pub enum Signable<'a> {
    /// The message itself: sr25519 (the `substrate` context) and ed25519 (RFC 8032).
    Message(&'a [u8]),
    /// A 32-byte digest the signer computed: ECDSA over secp256k1.
    Digest([u8; 32]),
}

impl fmt::Debug for Signable<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message(m) => write!(f, "a message of {} bytes", m.len()),
            Self::Digest(_) => f.write_str("a 32-byte digest"),
        }
    }
}

impl Scheme {
    /// Whether a key of this scheme signs a digest the signer computes (ECDSA) rather than the
    /// message itself.
    pub fn signs_digest(self) -> bool {
        matches!(self, Self::Ecdsa | Self::Secp256k1Evm)
    }
}

/// What a key of `scheme` signs for `message`, the network's signing payload as the core recorded
/// it: the BLAKE2b-256 a Kreivo ECDSA account signs ([`ecdsa_digest`]), the Keccak-256 an EVM key
/// signs ([`evm_digest`]), or the message itself. The signer computes every digest; no engine is
/// asked to hash.
pub fn prepare(scheme: Scheme, message: &[u8]) -> Signable<'_> {
    match scheme {
        Scheme::Ecdsa => Signable::Digest(ecdsa_digest(message)),
        Scheme::Secp256k1Evm => Signable::Digest(evm_digest(message)),
        Scheme::Sr25519 | Scheme::Ed25519 => Signable::Message(message),
    }
}

/// Refuses to sign `input` as `asked` with a key of scheme `held`: another scheme, or the wrong kind
/// of input for the scheme.
pub fn check_input(
    id: &str,
    held: Scheme,
    asked: Scheme,
    input: &Signable<'_>,
) -> Result<(), KeyError> {
    if held != asked {
        return Err(KeyError::Refused(format!(
            "key {id} is a {} key and was asked to sign as {}",
            held.as_str(),
            asked.as_str()
        )));
    }
    match (held.signs_digest(), input) {
        (true, Signable::Digest(_)) | (false, Signable::Message(_)) => Ok(()),
        (true, Signable::Message(_)) => Err(KeyError::Refused(format!(
            "key {id}: a {} key signs the 32-byte digest the signer computes, not a message",
            held.as_str()
        ))),
        (false, Signable::Digest(_)) => Err(KeyError::Refused(format!(
            "key {id}: a {} key signs the message itself, not a digest",
            held.as_str()
        ))),
    }
}

/// The address or account a network knows a key by: the 20-byte EVM address of a `secp256k1-evm`
/// key, the 32-byte Kreivo account (BLAKE2b-256 of the compressed key) of an `ecdsa` one, and the
/// public key itself for sr25519 and ed25519.
pub fn address_of(scheme: Scheme, public_key: &[u8]) -> Result<Vec<u8>, KeyError> {
    let secp = || {
        k256::PublicKey::from_sec1_bytes(public_key)
            .map_err(|_| KeyError::Source("the public key is not a secp256k1 key".to_owned()))
    };
    Ok(match scheme {
        Scheme::Secp256k1Evm => crate::kms::evm_address(&secp()?).to_vec(),
        Scheme::Ecdsa => crate::kms::kreivo_account(&secp()?).to_vec(),
        Scheme::Sr25519 | Scheme::Ed25519 => public_key.to_vec(),
    })
}

/// A DER ECDSA signature a signing service made over `digest`, as the 65 bytes `r ‖ s ‖ v` a
/// network takes: `s` made low, `v` the recovery id (0 or 1) whose recovered key `is_the_key`
/// accepts, and the result verified against `expected` before it is returned (`INV-9`). `service`
/// names the engine in an error; nothing of the signature or the digest is put in one.
pub fn recoverable_from_der(
    id: &str,
    service: &str,
    der: &[u8],
    digest: &[u8; 32],
    expected: &VerifyingKey,
    is_the_key: impl Fn(&VerifyingKey) -> bool,
) -> Result<Vec<u8>, KeyError> {
    let signature = Signature::from_der(der).map_err(|_| {
        KeyError::Backend(format!(
            "key {id}: {service} returned an unusable signature"
        ))
    })?;
    let signature = signature.normalize_s().unwrap_or(signature);
    let found = (0..=1u8).find(|&v| {
        RecoveryId::from_byte(v)
            .and_then(|recovery| {
                VerifyingKey::recover_from_prehash(digest, &signature, recovery).ok()
            })
            .is_some_and(|key| is_the_key(&key))
    });
    let Some(v) = found else {
        // The service signed with another key than the one the signer knows.
        return Err(KeyError::Backend(format!(
            "key {id}: {service}'s signature does not recover the key (INV-9)"
        )));
    };
    if expected.verify_prehash(digest, &signature).is_err() {
        return Err(KeyError::Backend(format!(
            "key {id}: {service}'s signature does not verify against the key (INV-9)"
        )));
    }
    let mut out = signature.to_bytes().to_vec();
    out.push(v);
    Ok(out)
}

/// A Vault client error as a key error: a Vault that did not answer, or answered 5xx or 429, is
/// [`KeyError::Unavailable`]; anything else is [`KeyError::Backend`]. Neither carries a value.
pub fn vault_failure(e: bezent_vault::Error) -> KeyError {
    match e {
        bezent_vault::Error::Unreachable(_) => KeyError::Unavailable(e.to_string()),
        bezent_vault::Error::Status { status, .. } if status >= 500 || status == 429 => {
            KeyError::Unavailable(e.to_string())
        }
        _ => KeyError::Backend(e.to_string()),
    }
}

/// One way of holding keys and signing with them (module documentation).
pub trait SigningEngine: Send + Sync + 'static {
    /// Where its keys are.
    fn kind(&self) -> EngineKind;

    /// What the configuration calls it, for a message: `file`, `Vault KV`, `AWS KMS`, `Vault
    /// Transit`, `Vault signing plugin`.
    fn name(&self) -> &'static str;

    /// The environment every key of this engine belongs to (`REQ-KY-6`).
    fn environment(&self) -> Environment;

    /// The ids of the keys it holds.
    fn key_ids(&self) -> Vec<String>;

    /// The scheme of key `id`.
    fn scheme(&self, id: &str) -> Result<Scheme, KeyError>;

    /// The public key of key `id`: 32 bytes for sr25519 and ed25519, 33 (compressed) for secp256k1.
    fn public_key(&self, id: &str) -> Result<Vec<u8>, KeyError>;

    /// The address or account a network knows key `id` by ([`address_of`]).
    fn address(&self, id: &str) -> Result<Vec<u8>, KeyError> {
        address_of(self.scheme(id)?, &self.public_key(id)?)
    }

    /// Whether key `id` is held where it signs on request and is never released (a signing
    /// service). A key read into this process, from a file or from Vault KV, is not. Only a key
    /// that is can be the ed25519 key of a delivery lane (the plan's `d-delivery-keys`).
    fn non_exportable(&self, _id: &str) -> bool {
        self.kind() == EngineKind::SigningService
    }

    /// Signs `input` with key `id` as `scheme`. Refuses another scheme than the key's, and the
    /// wrong kind of input for it ([`check_input`]).
    fn sign_prepared(
        &self,
        id: &str,
        scheme: Scheme,
        input: Signable<'_>,
    ) -> impl Future<Output = Result<Vec<u8>, KeyError>> + Send;

    /// Signs `message`, the network's signing payload, with key `id`: [`prepare`]s it for the
    /// key's scheme and signs that.
    fn sign(
        &self,
        id: &str,
        message: &[u8],
    ) -> impl Future<Output = Result<Vec<u8>, KeyError>> + Send {
        let scheme = self.scheme(id);
        async move {
            let scheme = scheme?;
            self.sign_prepared(id, scheme, prepare(scheme, message))
                .await
        }
    }

    /// Whether the engine can sign now. A signing service asks the service, without signing;
    /// an in-process engine is ready.
    fn health(&self) -> impl Future<Output = Health> + Send {
        std::future::ready(Health::ready())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req_ky_1_a_signable_shows_its_kind_and_length_never_its_bytes() {
        let shown = format!(
            "{:?} {:?}",
            Signable::Message(b"secret payload"),
            Signable::Digest([0xab; 32])
        );
        assert_eq!(shown, "a message of 14 bytes a 32-byte digest");
    }

    #[test]
    fn inv_9_each_scheme_signs_only_its_own_input() {
        let digest = Signable::Digest([1; 32]);
        let message = Signable::Message(b"m");
        for scheme in [Scheme::Ecdsa, Scheme::Secp256k1Evm] {
            assert!(check_input("k", scheme, scheme, &digest).is_ok());
            assert!(matches!(
                check_input("k", scheme, scheme, &message),
                Err(KeyError::Refused(_))
            ));
        }
        for scheme in [Scheme::Sr25519, Scheme::Ed25519] {
            assert!(check_input("k", scheme, scheme, &message).is_ok());
            assert!(matches!(
                check_input("k", scheme, scheme, &digest),
                Err(KeyError::Refused(_))
            ));
        }
        assert!(matches!(
            check_input("k", Scheme::Ecdsa, Scheme::Secp256k1Evm, &digest),
            Err(KeyError::Refused(_))
        ));
    }

    #[test]
    fn req_ky_1_vault_failures_are_transient_only_when_vault_did_not_answer() {
        use bezent_vault::Error;
        for e in [
            Error::Unreachable("timed out"),
            Error::Status {
                operation: "sign",
                status: 503,
            },
            Error::Status {
                operation: "sign",
                status: 429,
            },
        ] {
            assert!(vault_failure(e).is_transient());
        }
        for e in [
            Error::Status {
                operation: "sign",
                status: 403,
            },
            Error::Malformed("x"),
        ] {
            assert!(!vault_failure(e).is_transient());
        }
    }
}
