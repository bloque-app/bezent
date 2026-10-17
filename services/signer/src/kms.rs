// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The AWS KMS key backend (`T-005-03`, issue #5; `REQ-CU-1`, `REQ-KY-1`, `REQ-KY-3`, `REQ-KY-4`,
//! `REQ-KY-6`; PLAN `DEC-11`).
//!
//! Delivery keys (mint, payout, refund) live inside AWS KMS as `ECC_SECG_P256K1` keys and never
//! leave it. The signer hashes the message itself, as [`crate::keys::FileBackend`] does
//! ([`ecdsa_digest`] for a Kreivo ECDSA account, [`evm_digest`] for an EVM key), asks KMS to
//! `Sign` that digest (`MessageType = DIGEST`, `ECDSA_SHA_256`), turns the DER answer into
//! `r ‖ s ‖ v` with a low `s` (the `v` found by recovering the expected public key) and checks
//! the result before it returns it.
//!
//! At start [`KmsBackend::connect`] fetches every configured key's public key once
//! (`GetPublicKey`), requires the key spec and usage, compares the public key with the address
//! (EVM) or account (Kreivo ECDSA) the configuration declares, and reads the key's
//! `bezent:environment` tag: it must equal the environment the configuration declares. A key
//! with no tag, a key of another environment, or a production configuration that names a key
//! whose public key is a published test fixture's is refused (`REQ-KY-6`, `AC-I4.2`, issue #25).
//! The signer then checks that environment against the networks the store declares, as the core
//! does ([`crate::config::confirm_store_environment`]).
//!
//! The caller's IAM policy is the operator's to define. Network access is through [`AwsKms`], which uses the default
//! AWS credential chain (the task role), never a key in the configuration.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;

use blake2::Blake2b;
use blake2::digest::consts::U32;
use k256::PublicKey;
use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
use k256::elliptic_curve::sec1::ToEncodedPoint;
use k256::pkcs8::DecodePublicKey;
use serde::Deserialize;
use sha3::{Digest, Keccak256};

use crate::keys::{Environment, KeyBackend, KeyError, Scheme, ecdsa_digest, evm_digest};

/// The tag every bezent KMS key carries, `test` or `production`.
pub const ENVIRONMENT_TAG: &str = "bezent:environment";

/// The KMS calls the backend needs. [`AwsKms`] is the real one; tests implement it with a local
/// key, so no test calls AWS.
pub trait KmsApi: Send + Sync + 'static {
    /// `GetPublicKey`: the key's SubjectPublicKeyInfo DER, if its spec is `ECC_SECG_P256K1` and
    /// its usage `SIGN_VERIFY`.
    fn public_key_der(&self, key: &str) -> impl Future<Output = Result<Vec<u8>, KeyError>> + Send;

    /// `ListResourceTags`: the value of the `bezent:environment` tag, if the key has one.
    fn environment_tag(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Option<String>, KeyError>> + Send;

    /// `Sign` with `MessageType = DIGEST`, `ECDSA_SHA_256`: the DER signature over `digest`.
    fn sign_digest(
        &self,
        key: &str,
        digest: [u8; 32],
    ) -> impl Future<Output = Result<Vec<u8>, KeyError>> + Send;
}

/// One KMS key the configuration names.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KmsKey {
    /// The signer's id for the key (what bindings name).
    pub id: String,
    /// The KMS key id or ARN. Use the ARN: the IAM policy lists ARNs.
    pub key: String,
    /// `secp256k1-evm` or `ecdsa`.
    pub scheme: Scheme,
    /// For `secp256k1-evm`: the 20-byte address, `0x` hex.
    #[serde(default)]
    pub address: Option<String>,
    /// For `ecdsa`: the 32-byte Kreivo account id (BLAKE2b-256 of the compressed public key),
    /// `0x` hex.
    #[serde(default)]
    pub account: Option<String>,
}

struct Held {
    key: String,
    scheme: Scheme,
    public_key: Vec<u8>,
    verifying: VerifyingKey,
}

/// Delivery keys held in AWS KMS.
pub struct KmsBackend<A: KmsApi> {
    api: A,
    environment: Environment,
    keys: BTreeMap<String, Held>,
}

fn refused(why: String) -> KeyError {
    KeyError::Refused(why)
}

/// The EVM address of a secp256k1 public key.
pub fn evm_address(public: &PublicKey) -> [u8; 20] {
    let point = public.to_encoded_point(false);
    let hash = Keccak256::digest(&point.as_bytes()[1..]);
    hash[12..].try_into().expect("20 bytes")
}

/// The Kreivo account id of an ECDSA public key: BLAKE2b-256 of the compressed key.
pub fn kreivo_account(public: &PublicKey) -> [u8; 32] {
    Blake2b::<U32>::digest(public.to_encoded_point(true).as_bytes()).into()
}

fn parse_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    hex::decode(text.strip_prefix("0x").unwrap_or(text))
        .ok()?
        .try_into()
        .ok()
}

/// The compressed public keys of the published test fixture: a production configuration must not
/// name a key whose public key is one of these (`REQ-KY-6`).
///
/// Fails closed: if the embedded fixture cannot be read, this panics at start-up rather than
/// returning an empty list that would let the production refusal pass everything.
pub fn fixture_public_keys() -> Vec<Vec<u8>> {
    fixture_public_keys_of(include_bytes!("../fixtures/test-keys.json"))
        .expect("the embedded test fixture must parse: refusing to run without the fixture list")
}

fn fixture_public_keys_of(bytes: &[u8]) -> Result<Vec<Vec<u8>>, KeyError> {
    let f = crate::keys::FileBackend::parse(bytes)?;
    let keys = f
        .ids()
        .map(|id| f.public_key(id))
        .collect::<Result<Vec<_>, _>>()?;
    if keys.is_empty() {
        return Err(KeyError::Source(
            "the test fixture holds no keys".to_owned(),
        ));
    }
    Ok(keys)
}

impl<A: KmsApi> KmsBackend<A> {
    /// Fetches and checks every key (see the module documentation). `environment` is the
    /// environment the configuration declares for this custody.
    pub async fn connect(
        api: A,
        environment: Environment,
        keys: &[KmsKey],
    ) -> Result<Self, KeyError> {
        let fixture = fixture_public_keys();
        let mut held = BTreeMap::new();
        for k in keys {
            let id = &k.id;
            if id.is_empty() || held.contains_key(id) {
                return Err(KeyError::Source(format!(
                    "key id {id:?} is empty or repeated"
                )));
            }
            if !matches!(k.scheme, Scheme::Ecdsa | Scheme::Secp256k1Evm) {
                return Err(refused(format!(
                    "key {id}: KMS holds secp256k1 keys only (DEC-11): scheme ecdsa or secp256k1-evm"
                )));
            }
            let tagged = match api.environment_tag(&k.key).await?.as_deref() {
                Some("test") => Environment::Test,
                Some("production") => Environment::Production,
                Some(_) => {
                    return Err(refused(format!(
                        "key {id}: the {ENVIRONMENT_TAG} tag is neither test nor production (REQ-KY-6)"
                    )));
                }
                None => {
                    return Err(refused(format!(
                        "key {id}: the KMS key has no {ENVIRONMENT_TAG} tag (REQ-KY-6, AC-I4.2)"
                    )));
                }
            };
            if tagged != environment {
                return Err(refused(format!(
                    "key {id}: the KMS key is tagged {tagged} and the configuration is {environment} \
                     (REQ-KY-6, AC-I4.2)"
                )));
            }
            let der = api.public_key_der(&k.key).await?;
            let public = PublicKey::from_public_key_der(&der).map_err(|_| {
                KeyError::Source(format!("key {id}: KMS returned an unreadable public key"))
            })?;
            let compressed = public.to_encoded_point(true).as_bytes().to_vec();
            if k.scheme == Scheme::Secp256k1Evm {
                let declared = k
                    .address
                    .as_deref()
                    .and_then(parse_hex::<20>)
                    .filter(|_| k.account.is_none())
                    .ok_or_else(|| {
                        refused(format!(
                            "key {id}: an EVM key declares its 20-byte address only"
                        ))
                    })?;
                if declared != evm_address(&public) {
                    return Err(refused(format!(
                        "key {id}: the KMS public key is not the configured address (REQ-KY-3)"
                    )));
                }
            } else {
                let declared = k
                    .account
                    .as_deref()
                    .and_then(parse_hex::<32>)
                    .filter(|_| k.address.is_none())
                    .ok_or_else(|| {
                        refused(format!(
                            "key {id}: a Kreivo ECDSA key declares its 32-byte account only"
                        ))
                    })?;
                if declared != kreivo_account(&public) {
                    return Err(refused(format!(
                        "key {id}: the KMS public key is not the configured account (REQ-KY-3)"
                    )));
                }
            }
            if environment == Environment::Production && fixture.contains(&compressed) {
                return Err(refused(format!(
                    "key {id}: its public key is a published test fixture's; refused in production \
                     (REQ-KY-6, AC-I4.2)"
                )));
            }
            held.insert(
                id.clone(),
                Held {
                    key: k.key.clone(),
                    scheme: k.scheme,
                    public_key: compressed,
                    verifying: VerifyingKey::from(&public),
                },
            );
        }
        Ok(Self {
            api,
            environment,
            keys: held,
        })
    }

    /// The ids of the keys held.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.keys.keys().map(String::as_str)
    }

    fn held(&self, id: &str) -> Result<&Held, KeyError> {
        self.keys
            .get(id)
            .ok_or_else(|| KeyError::UnknownKey(id.to_owned()))
    }
}

/// DER → `r ‖ s ‖ v`, low `s`, `v` found by recovering `expected` from `digest`.
pub fn der_to_recoverable(
    id: &str,
    der: &[u8],
    digest: &[u8; 32],
    expected: &VerifyingKey,
) -> Result<Vec<u8>, KeyError> {
    let signature = Signature::from_der(der)
        .map_err(|_| KeyError::Backend(format!("key {id}: KMS returned an unusable signature")))?;
    let signature = signature.normalize_s().unwrap_or(signature);
    for v in 0..=1u8 {
        let Some(recovery) = RecoveryId::from_byte(v) else {
            continue;
        };
        if let Ok(found) = VerifyingKey::recover_from_prehash(digest, &signature, recovery)
            && &found == expected
        {
            let mut out = signature.to_bytes().to_vec();
            out.push(v);
            return Ok(out);
        }
    }
    // KMS signed with another key than the one the signer checked at start.
    Err(KeyError::Backend(format!(
        "key {id}: the signature does not recover the key's public key"
    )))
}

impl<A: KmsApi> KeyBackend for KmsBackend<A> {
    fn environment(&self) -> Environment {
        self.environment
    }

    fn scheme(&self, id: &str) -> Result<Scheme, KeyError> {
        self.held(id).map(|k| k.scheme)
    }

    fn public_key(&self, id: &str) -> Result<Vec<u8>, KeyError> {
        self.held(id).map(|k| k.public_key.clone())
    }

    fn sign(
        &self,
        id: &str,
        message: &[u8],
    ) -> impl Future<Output = Result<Vec<u8>, KeyError>> + Send {
        let held = self.held(id).map(|k| {
            let digest = match k.scheme {
                Scheme::Ecdsa => ecdsa_digest(message),
                _ => evm_digest(message),
            };
            (k, digest)
        });
        async move {
            let (k, digest) = held?;
            let der = self.api.sign_digest(&k.key, digest).await?;
            der_to_recoverable(id, &der, &digest, &k.verifying)
        }
    }
}

/// Key ids, schemes and public keys only (`REQ-KY-1`).
impl<A: KmsApi> fmt::Debug for KmsBackend<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut map = f.debug_map();
        for (id, key) in &self.keys {
            map.entry(
                id,
                &format_args!(
                    "kms {} 0x{}",
                    key.scheme.as_str(),
                    hex::encode(&key.public_key)
                ),
            );
        }
        map.finish()
    }
}

/// The real KMS, through the AWS SDK and its default credential chain.
pub struct AwsKms {
    client: aws_sdk_kms::Client,
}

impl AwsKms {
    /// A client for `region`; `endpoint` overrides the service URL (a VPC endpoint).
    pub async fn new(region: &str, endpoint: Option<&str>) -> Self {
        let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(region.to_owned()));
        if let Some(url) = endpoint {
            loader = loader.endpoint_url(url);
        }
        Self {
            client: aws_sdk_kms::Client::new(&loader.load().await),
        }
    }
}

fn failed(what: &str, e: impl fmt::Display) -> KeyError {
    KeyError::Backend(format!("KMS {what} failed: {e}"))
}

impl KmsApi for AwsKms {
    async fn public_key_der(&self, key: &str) -> Result<Vec<u8>, KeyError> {
        use aws_sdk_kms::error::DisplayErrorContext;
        use aws_sdk_kms::types::{KeySpec, KeyUsageType};
        let out = self
            .client
            .get_public_key()
            .key_id(key)
            .send()
            .await
            .map_err(|e| failed("GetPublicKey", DisplayErrorContext(e)))?;
        if out.key_spec() != Some(&KeySpec::EccSecgP256K1)
            || out.key_usage() != Some(&KeyUsageType::SignVerify)
        {
            return Err(refused(format!(
                "KMS key {key} is not an ECC_SECG_P256K1 SIGN_VERIFY key (DEC-11)"
            )));
        }
        out.public_key()
            .map(|b| b.as_ref().to_vec())
            .ok_or_else(|| failed("GetPublicKey", "no public key"))
    }

    async fn environment_tag(&self, key: &str) -> Result<Option<String>, KeyError> {
        use aws_sdk_kms::error::DisplayErrorContext;
        let out = self
            .client
            .list_resource_tags()
            .key_id(key)
            .send()
            .await
            .map_err(|e| failed("ListResourceTags", DisplayErrorContext(e)))?;
        Ok(out
            .tags()
            .iter()
            .find(|t| t.tag_key() == ENVIRONMENT_TAG)
            .map(|t| t.tag_value().to_owned()))
    }

    async fn sign_digest(&self, key: &str, digest: [u8; 32]) -> Result<Vec<u8>, KeyError> {
        use aws_sdk_kms::error::DisplayErrorContext;
        use aws_sdk_kms::primitives::Blob;
        use aws_sdk_kms::types::{MessageType, SigningAlgorithmSpec};
        let out = self
            .client
            .sign()
            .key_id(key)
            .message(Blob::new(digest.to_vec()))
            .message_type(MessageType::Digest)
            .signing_algorithm(SigningAlgorithmSpec::EcdsaSha256)
            .send()
            .await
            .map_err(|e| failed("Sign", DisplayErrorContext(e)))?;
        out.signature()
            .map(|b| b.as_ref().to_vec())
            .ok_or_else(|| failed("Sign", "no signature"))
    }
}
