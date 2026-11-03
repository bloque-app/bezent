// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Local stand-ins for the signing services, each with a real key: what KMS, Vault Transit and the
//! Vault signing plugin would do, and the ways each goes wrong (down, signing with another key, a
//! high `s`). No test calls AWS or Vault through them.

use std::sync::Mutex;

use bezent_signer::keys::KeyError;
use bezent_signer::kms::KmsApi;
use bezent_signer::transit::TransitApi;
use bezent_signer::vault_plugin::PluginApi;
use k256::PublicKey;
use k256::ecdsa::signature::hazmat::PrehashSigner;
use k256::ecdsa::{Signature, SigningKey};
use k256::pkcs8::EncodePublicKey;

/// A secp256k1 key from a label.
pub fn secp_key(n: u8) -> SigningKey {
    SigningKey::from_slice(&[n; 32]).unwrap()
}

/// `signature` with `s` moved to the high half, as a service that does not normalise may answer.
pub fn high_s(signature: Signature) -> Signature {
    let (r, s) = signature.split_scalars();
    Signature::from_scalars(r.to_bytes(), (-*s).to_bytes()).unwrap()
}

/// A KMS stand-in: one key, its tag, and optionally a flip of `s` to the high half (KMS does not
/// normalise), a signature by another key, or no answer.
pub struct LocalKms {
    pub key: SigningKey,
    pub other: SigningKey,
    pub tag: Option<String>,
    pub high_s: bool,
    pub wrong_signer: bool,
    pub down: Mutex<bool>,
}

impl LocalKms {
    pub fn new(n: u8, tag: Option<&str>) -> Self {
        Self {
            key: secp_key(n),
            other: secp_key(n + 100),
            tag: tag.map(str::to_owned),
            high_s: false,
            wrong_signer: false,
            down: Mutex::new(false),
        }
    }

    pub fn public(&self) -> PublicKey {
        PublicKey::from(self.key.verifying_key())
    }

    fn up(&self) -> Result<(), KeyError> {
        if *self.down.lock().unwrap() {
            return Err(KeyError::Unavailable("KMS did not answer".into()));
        }
        Ok(())
    }
}

impl KmsApi for LocalKms {
    async fn public_key_der(&self, _key: &str) -> Result<Vec<u8>, KeyError> {
        self.up()?;
        Ok(self
            .public()
            .to_public_key_der()
            .unwrap()
            .as_bytes()
            .to_vec())
    }

    async fn environment_tag(&self, _key: &str) -> Result<Option<String>, KeyError> {
        Ok(self.tag.clone())
    }

    async fn sign_digest(&self, _key: &str, digest: [u8; 32]) -> Result<Vec<u8>, KeyError> {
        self.up()?;
        let key = if self.wrong_signer {
            &self.other
        } else {
            &self.key
        };
        let signature: Signature = key.sign_prehash(&digest).unwrap();
        let signature = if self.high_s {
            high_s(signature)
        } else {
            signature
        };
        Ok(signature.to_der().as_bytes().to_vec())
    }
}

/// A Transit stand-in: one ed25519 key at version 3; `rogue` signs with another key than it
/// publishes; `down` fails.
pub struct LocalTransit {
    pub key: ed25519_dalek::SigningKey,
    pub rogue: Option<ed25519_dalek::SigningKey>,
    pub down: Mutex<bool>,
}

impl LocalTransit {
    pub fn new(key: ed25519_dalek::SigningKey) -> Self {
        Self {
            key,
            rogue: None,
            down: Mutex::new(false),
        }
    }

    fn up(&self) -> Result<(), KeyError> {
        if *self.down.lock().unwrap() {
            return Err(KeyError::Unavailable("Vault is unreachable".into()));
        }
        Ok(())
    }
}

impl TransitApi for LocalTransit {
    async fn public_key(&self, _key: &str) -> Result<(u32, [u8; 32]), KeyError> {
        self.up()?;
        Ok((3, self.key.verifying_key().to_bytes()))
    }

    async fn sign(&self, _key: &str, version: u32, message: &[u8]) -> Result<[u8; 64], KeyError> {
        use ed25519_dalek::Signer as _;
        self.up()?;
        assert_eq!(version, 3, "the version read at start");
        Ok(self
            .rogue
            .as_ref()
            .unwrap_or(&self.key)
            .sign(message)
            .to_bytes())
    }
}

/// A stand-in for the Vault signing plugin: signs any 32 bytes it is given as a hash, DER, no
/// recovery id, as `blockchain-signer-vault-plugin` does. `high_s` answers with a high `s` (the
/// plugin signs low-`s`; the signer must not rely on it), `rogue` signs with another key, `down`
/// does not answer. `asked` records every hash it was sent.
pub struct LocalPlugin {
    pub key: SigningKey,
    pub rogue: Option<SigningKey>,
    pub high_s: bool,
    pub down: Mutex<bool>,
    pub asked: Mutex<Vec<(String, [u8; 32])>>,
}

impl LocalPlugin {
    pub fn new(key: SigningKey) -> Self {
        Self {
            key,
            rogue: None,
            high_s: false,
            down: Mutex::new(false),
            asked: Mutex::default(),
        }
    }

    fn up(&self) -> Result<(), KeyError> {
        if *self.down.lock().unwrap() {
            return Err(KeyError::Unavailable("Vault is unreachable".into()));
        }
        Ok(())
    }
}

impl PluginApi for LocalPlugin {
    async fn sign_hash(&self, key: &str, hash: &[u8; 32]) -> Result<Vec<u8>, KeyError> {
        self.up()?;
        self.asked.lock().unwrap().push((key.to_owned(), *hash));
        let signing = self.rogue.as_ref().unwrap_or(&self.key);
        let signature: Signature = signing.sign_prehash(hash).unwrap();
        let signature = if self.high_s {
            high_s(signature)
        } else {
            signature
        };
        Ok(signature.to_der().as_bytes().to_vec())
    }

    async fn reachable(&self) -> Result<(), KeyError> {
        self.up()
    }
}
