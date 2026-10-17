// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Keys (`T-005-03`; `REQ-CU-1`, `REQ-KY-1`, `REQ-KY-6`; PLAN `DEC-11`).
//!
//! [`KeyBackend`] is what the signer asks of a key custody: a key's scheme and public key, and a
//! signature over a message. `DEC-11` puts delivery keys in AWS KMS and the others in Vault; both
//! are `M-1` backends behind this trait. The only `M-0` backend is [`FileBackend`]: test keys from
//! a file whose path the configuration names (never the environment, `REQ-KY-1`), tagged
//! `"environment": "test"`. It refuses a file with any other tag, so a key it holds is always a
//! test key, and the signer refuses to use one for a production network (`REQ-KY-6`, `AC-I4.2`).
//!
//! The schemes: `sr25519` (a Kreivo account; the `substrate` signing context), `ecdsa` (a Kreivo
//! ECDSA account: secp256k1 over the BLAKE2b-256 of the message, 65 bytes `r ‖ s ‖ v`) and
//! `secp256k1-evm` (secp256k1 over the Keccak-256 of the message, 65 bytes `r ‖ s ‖ v`, `v` the
//! recovery id 0 or 1), and `ed25519` (RFC 8032 over the message itself, 64 bytes): the statement key
//! of `REQ-PR-1` (`F-004`), which signs nothing else, so the policy refuses to bind it to a network. ECDSA signatures are deterministic (RFC 6979) and low-`s`; sr25519 ones are
//! randomised, which is why a replay returns the recorded signature rather than signing again
//! (`AC-D2.1`). The message is the network's signing payload as the core recorded it.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::path::Path;

use blake2::Blake2b;
use blake2::digest::consts::U32;
use k256::ecdsa::SigningKey;
use serde::Deserialize;
use sha3::{Digest, Keccak256};
use zeroize::Zeroizing;

/// A signature scheme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum Scheme {
    /// A Kreivo sr25519 account.
    #[serde(rename = "sr25519")]
    Sr25519,
    /// A Kreivo ECDSA account (secp256k1 over BLAKE2b-256).
    #[serde(rename = "ecdsa")]
    Ecdsa,
    /// An EVM account (secp256k1 over Keccak-256).
    #[serde(rename = "secp256k1-evm")]
    Secp256k1Evm,
    /// The statement key (`REQ-PR-1`): ed25519 over the message (RFC 8032).
    #[serde(rename = "ed25519")]
    Ed25519,
}

impl Scheme {
    /// The name the store records (`signatures.scheme`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sr25519 => "sr25519",
            Self::Ecdsa => "ecdsa",
            Self::Secp256k1Evm => "secp256k1-evm",
            Self::Ed25519 => "ed25519",
        }
    }
}

/// Whether a key, or a network, belongs to a test environment or to production (`REQ-KY-6`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    /// A test environment.
    Test,
    /// Production.
    Production,
}

impl fmt::Display for Environment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Test => "test",
            Self::Production => "production",
        })
    }
}

/// Why a key could not be loaded or used. It never carries key material (`REQ-KY-1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    /// The backend holds no key by this id.
    UnknownKey(String),
    /// The key source is refused, with the spec id that refuses it.
    Refused(String),
    /// The key source could not be read or parsed; the text names where, never what.
    Source(String),
    /// The backend failed to sign.
    Backend(String),
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKey(id) => write!(f, "no key {id}"),
            Self::Refused(why) => write!(f, "key source refused: {why}"),
            Self::Source(why) => write!(f, "key source unreadable: {why}"),
            Self::Backend(why) => write!(f, "key backend failed: {why}"),
        }
    }
}

impl std::error::Error for KeyError {}

/// A key custody (`DEC-11`): the file backend in `M-0`; AWS KMS and Vault in `M-1`.
pub trait KeyBackend: Send + Sync + 'static {
    /// The environment every key of this backend belongs to.
    fn environment(&self) -> Environment;

    /// The scheme of key `id`.
    fn scheme(&self, id: &str) -> Result<Scheme, KeyError>;

    /// The public key of key `id`: 32 bytes for sr25519, 33 (compressed) for secp256k1.
    fn public_key(&self, id: &str) -> Result<Vec<u8>, KeyError>;

    /// Signs `message` with key `id`.
    fn sign(
        &self,
        id: &str,
        message: &[u8],
    ) -> impl Future<Output = Result<Vec<u8>, KeyError>> + Send;
}

enum Secret {
    Sr25519(schnorrkel::Keypair),
    Secp256k1(SigningKey),
    Ed25519(ed25519_dalek::SigningKey),
}

struct FileKey {
    scheme: Scheme,
    public_key: Vec<u8>,
    secret: Secret,
}

/// Test keys from a file (`M-0` only). See the module documentation.
pub struct FileBackend {
    keys: BTreeMap<String, FileKey>,
    environment: Environment,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyFile {
    #[serde(default)]
    #[allow(dead_code)]
    comment: Option<String>,
    environment: String,
    keys: Vec<KeyEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyEntry {
    id: String,
    scheme: Scheme,
    /// The 32-byte secret seed, hex. Wiped on drop.
    seed: Zeroizing<String>,
}

impl FileBackend {
    /// Loads the key file at `path`. The file must say `"environment": "test"`.
    pub fn load(path: &Path) -> Result<Self, KeyError> {
        let bytes = Zeroizing::new(std::fs::read(path).map_err(|e| {
            KeyError::Source(format!("cannot read {}: {}", path.display(), e.kind()))
        })?);
        Self::parse(&bytes)
    }

    /// Parses a key file's contents.
    pub fn parse(bytes: &[u8]) -> Result<Self, KeyError> {
        // serde_json's messages can quote the input; only the position is reported.
        let file: KeyFile = serde_json::from_slice(bytes).map_err(|e| {
            KeyError::Source(format!(
                "the key file is not a valid key file ({:?} error at line {}, column {})",
                e.classify(),
                e.line(),
                e.column()
            ))
        })?;
        if file.environment != "test" {
            return Err(KeyError::Refused(
                "the file backend holds test keys only: the key file must say \
                 \"environment\": \"test\" (REQ-KY-6, DEC-11)"
                    .to_owned(),
            ));
        }
        let mut keys = BTreeMap::new();
        for entry in file.keys {
            let id = entry.id.clone();
            if id.is_empty() {
                return Err(KeyError::Source("a key has an empty id".to_owned()));
            }
            let key = FileKey::from_seed(&id, entry.scheme, &entry.seed)?;
            if keys.insert(id.clone(), key).is_some() {
                return Err(KeyError::Source(format!("key {id} appears twice")));
            }
        }
        Ok(Self {
            keys,
            environment: Environment::Test,
        })
    }

    /// Keys whose secrets another custody read: `(id, scheme, 32-byte seed in hex)`. The Vault
    /// loader ([`crate::vault`]) builds its backend this way, in the environment its secret declares.
    pub fn from_seeds(
        environment: Environment,
        entries: Vec<(String, Scheme, Zeroizing<String>)>,
    ) -> Result<Self, KeyError> {
        let mut keys = BTreeMap::new();
        for (id, scheme, seed) in entries {
            if id.is_empty() {
                return Err(KeyError::Source("a key has an empty id".to_owned()));
            }
            let key = FileKey::from_seed(&id, scheme, &seed)?;
            if keys.insert(id.clone(), key).is_some() {
                return Err(KeyError::Source(format!("key {id} appears twice")));
            }
        }
        Ok(Self { keys, environment })
    }

    /// The ids of the keys held.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.keys.keys().map(String::as_str)
    }

    fn key(&self, id: &str) -> Result<&FileKey, KeyError> {
        self.keys
            .get(id)
            .ok_or_else(|| KeyError::UnknownKey(id.to_owned()))
    }
}

impl FileKey {
    fn from_seed(id: &str, scheme: Scheme, seed_hex: &str) -> Result<Self, KeyError> {
        let digits = seed_hex.strip_prefix("0x").unwrap_or(seed_hex);
        let mut seed = Zeroizing::new([0u8; 32]);
        hex::decode_to_slice(digits, seed.as_mut_slice())
            .map_err(|_| KeyError::Source(format!("key {id}: the seed is not 32 bytes of hex")))?;
        let bad = || KeyError::Source(format!("key {id}: the seed is not a valid secret"));
        Ok(match scheme {
            Scheme::Sr25519 => {
                let mini =
                    schnorrkel::MiniSecretKey::from_bytes(seed.as_slice()).map_err(|_| bad())?;
                let pair = mini.expand_to_keypair(schnorrkel::ExpansionMode::Ed25519);
                Self {
                    scheme,
                    public_key: pair.public.to_bytes().to_vec(),
                    secret: Secret::Sr25519(pair),
                }
            }
            Scheme::Ed25519 => {
                let key = ed25519_dalek::SigningKey::from_bytes(&seed);
                Self {
                    scheme,
                    public_key: key.verifying_key().to_bytes().to_vec(),
                    secret: Secret::Ed25519(key),
                }
            }
            Scheme::Ecdsa | Scheme::Secp256k1Evm => {
                let key = SigningKey::from_slice(seed.as_slice()).map_err(|_| bad())?;
                Self {
                    scheme,
                    public_key: key
                        .verifying_key()
                        .to_encoded_point(true)
                        .as_bytes()
                        .to_vec(),
                    secret: Secret::Secp256k1(key),
                }
            }
        })
    }

    fn sign(&self, id: &str, message: &[u8]) -> Result<Vec<u8>, KeyError> {
        match (&self.secret, self.scheme) {
            (Secret::Sr25519(pair), Scheme::Sr25519) => Ok(pair
                .sign(schnorrkel::signing_context(b"substrate").bytes(message))
                .to_bytes()
                .to_vec()),
            (Secret::Secp256k1(key), Scheme::Ecdsa) => recoverable(id, key, &ecdsa_digest(message)),
            (Secret::Secp256k1(key), Scheme::Secp256k1Evm) => {
                recoverable(id, key, &evm_digest(message))
            }
            (Secret::Ed25519(key), Scheme::Ed25519) => {
                use ed25519_dalek::Signer as _;
                Ok(key.sign(message).to_bytes().to_vec())
            }
            _ => Err(KeyError::Backend(format!("key {id}: scheme mismatch"))),
        }
    }
}

/// The digest a Kreivo ECDSA account signs: Substrate pre-hashes a signing payload longer than 256
/// bytes with BLAKE2b-256, and the ECDSA scheme hashes what it is given with BLAKE2b-256 again.
pub fn ecdsa_digest(payload: &[u8]) -> [u8; 32] {
    let hash = |bytes: &[u8]| -> [u8; 32] { Blake2b::<U32>::digest(bytes).into() };
    if payload.len() > 256 {
        hash(&hash(payload))
    } else {
        hash(payload)
    }
}

/// The digest an EVM key signs: Keccak-256 of the message.
pub fn evm_digest(message: &[u8]) -> [u8; 32] {
    Keccak256::digest(message).into()
}

fn recoverable(id: &str, key: &SigningKey, digest: &[u8]) -> Result<Vec<u8>, KeyError> {
    let (signature, recovery) = key
        .sign_prehash_recoverable(digest)
        .map_err(|_| KeyError::Backend(format!("key {id}: signing failed")))?;
    let mut out = signature.to_bytes().to_vec();
    out.push(recovery.to_byte());
    Ok(out)
}

impl KeyBackend for FileBackend {
    fn environment(&self) -> Environment {
        self.environment
    }

    fn scheme(&self, id: &str) -> Result<Scheme, KeyError> {
        self.key(id).map(|k| k.scheme)
    }

    fn public_key(&self, id: &str) -> Result<Vec<u8>, KeyError> {
        self.key(id).map(|k| k.public_key.clone())
    }

    fn sign(
        &self,
        id: &str,
        message: &[u8],
    ) -> impl Future<Output = Result<Vec<u8>, KeyError>> + Send {
        std::future::ready(self.key(id).and_then(|k| k.sign(id, message)))
    }
}

/// Key ids, schemes and public keys only (`REQ-KY-1`).
impl fmt::Debug for FileBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut map = f.debug_map();
        for (id, key) in &self.keys {
            map.entry(
                id,
                &format_args!("{} 0x{}", key.scheme.as_str(), hex::encode(&key.public_key)),
            );
        }
        map.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};

    fn file(scheme: &str, seed: &str) -> String {
        format!(
            r#"{{"environment": "test", "keys": [{{"id": "k", "scheme": "{scheme}", "seed": "{seed}"}}]}}"#
        )
    }

    const SEED: &str = "0x0101010101010101010101010101010101010101010101010101010101010101";

    #[tokio::test]
    async fn req_cu_1_sr25519_signature_verifies() {
        let backend = FileBackend::parse(file("sr25519", SEED).as_bytes()).unwrap();
        let signature = backend.sign("k", b"payload").await.unwrap();
        let public = schnorrkel::PublicKey::from_bytes(&backend.public_key("k").unwrap()).unwrap();
        let signature = schnorrkel::Signature::from_bytes(&signature).unwrap();
        public
            .verify(
                schnorrkel::signing_context(b"substrate").bytes(b"payload"),
                &signature,
            )
            .expect("verifies");
    }

    #[tokio::test]
    async fn req_pr_1_ed25519_statement_key_signature_verifies() {
        let backend = FileBackend::parse(file("ed25519", SEED).as_bytes()).unwrap();
        assert_eq!(backend.scheme("k").unwrap(), Scheme::Ed25519);
        let signature = backend.sign("k", b"statement").await.unwrap();
        let public: [u8; 32] = backend.public_key("k").unwrap().try_into().unwrap();
        let public = ed25519_dalek::VerifyingKey::from_bytes(&public).unwrap();
        let signature = ed25519_dalek::Signature::from_slice(&signature).unwrap();
        public
            .verify_strict(b"statement", &signature)
            .expect("verifies");
    }

    async fn recovers(scheme: &str, digest: Vec<u8>) {
        let backend = FileBackend::parse(file(scheme, SEED).as_bytes()).unwrap();
        let signature = backend.sign("k", b"payload").await.unwrap();
        assert_eq!(signature.len(), 65);
        let sig = Signature::from_slice(&signature[..64]).unwrap();
        assert!(sig.normalize_s().is_none(), "low s");
        let recovered = VerifyingKey::recover_from_prehash(
            &digest,
            &sig,
            RecoveryId::from_byte(signature[64]).unwrap(),
        )
        .unwrap();
        assert_eq!(
            recovered.to_encoded_point(true).as_bytes(),
            backend.public_key("k").unwrap().as_slice()
        );
        // Deterministic (RFC 6979).
        assert_eq!(backend.sign("k", b"payload").await.unwrap(), signature);
    }

    #[tokio::test]
    async fn req_cu_1_kreivo_ecdsa_signature_recovers_the_key() {
        recovers("ecdsa", Blake2b::<U32>::digest(b"payload").to_vec()).await;
    }

    #[tokio::test]
    async fn req_cu_1_evm_signature_recovers_the_key() {
        recovers("secp256k1-evm", Keccak256::digest(b"payload").to_vec()).await;
    }

    #[test]
    fn req_ky_6_file_backend_refuses_a_production_key_file() {
        let production = file("sr25519", SEED).replace("\"test\"", "\"production\"");
        assert!(matches!(
            FileBackend::parse(production.as_bytes()),
            Err(KeyError::Refused(_))
        ));
        assert_eq!(
            FileBackend::parse(file("sr25519", SEED).as_bytes())
                .unwrap()
                .environment(),
            Environment::Test
        );
    }
}
