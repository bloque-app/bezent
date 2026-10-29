// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The Vault signing-plugin engine, for the Polygon delivery key (`secp256k1-evm`; the plan's
//! ruling `d-delivery-keys`; `REQ-CU-1`, `REQ-KY-1`, `REQ-KY-3`, `REQ-KY-4`, `REQ-KY-6`, `INV-9`).
//!
//! The key is created inside a Vault secrets engine, `blockchain-signer-vault-plugin` (pinned to a
//! reviewed commit and built by the operator; `deploy/signer-sidecar.md`), which signs on request
//! and has no path that returns it. The plugin signs any bytes it is given as a hash and returns a
//! DER signature without a recovery id, so the signer does the rest itself:
//!
//! 1. It computes the Keccak-256 of the signing payload ([`crate::engine::prepare`]). Only a
//!    32-byte digest reaches the plugin: a message is refused ([`crate::engine::check_input`]), and
//!    the client refuses any hash that is not 32 bytes before it sends anything
//!    ([`bezent_vault::Session::plugin_sign_hash`]).
//! 2. It sends only the hash (`<mount>/keys/<key>/sign/hash`).
//! 3. It turns the DER into `r ‖ s` with `s` made low, and finds `v` by recovering, for each
//!    candidate, the address the configuration declares for the key. If neither recovers it, the
//!    plugin signed with another key, and the signature is refused.
//! 4. It verifies `r ‖ s` against the key's public key before it returns `r ‖ s ‖ v` (`INV-9`).
//!
//! The configuration declares the key's address and its compressed public key, and the signer
//! checks offline, at start, that the one is the other's (`REQ-KY-3`); in production a published
//! test fixture's key is refused (`REQ-KY-6`). It reads nothing from Vault to learn the key: the
//! role it logs in as for this engine may `update` `<mount>/keys/<key>/sign/hash` and nothing else,
//! not even read the key, and it is a role of its own (`role`), apart from the role that reads the
//! store's login from Vault KV. A key that is not the declared one is found at the first
//! signature, which is refused, so nothing it signed is ever released or recorded.
//!
//! The signer logs in for each signature ([`bezent_vault::Resolver::session`] reuses a login for a
//! few seconds at most). A Vault that does not answer is [`KeyError::Unavailable`] and the intent
//! is retried later; a wrong answer or a refusal is [`KeyError::Backend`]. No hash, signature or
//! token is logged or put in an error. The engine's health is a login under its role: it signs
//! nothing (a signature with no recorded intent would be one `AC-I1.2` treats as a compromise).

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::sync::Arc;

use bezent_vault::{Resolver, VaultConfig};
use k256::PublicKey;
use k256::ecdsa::VerifyingKey;
use k256::elliptic_curve::sec1::ToEncodedPoint;
use serde::Deserialize;

use crate::engine::{
    EngineKind, Health, Signable, SigningEngine, check_input, recoverable_from_der, vault_failure,
};
use crate::keys::{Environment, KeyError, Scheme};
use crate::kms::evm_address;

/// The plugin calls the engine needs. [`VaultPlugin`] is the real one; tests implement it with a
/// local key.
pub trait PluginApi: Send + Sync + 'static {
    /// `sign/hash`: key `key` signs `hash`; the DER signature.
    fn sign_hash(
        &self,
        key: &str,
        hash: &[u8; 32],
    ) -> impl Future<Output = Result<Vec<u8>, KeyError>> + Send;

    /// Whether the plugin can be reached as the engine's role, without signing.
    fn reachable(&self) -> impl Future<Output = Result<(), KeyError>> + Send;
}

/// One plugin key the configuration names.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginKey {
    /// The signer's id for the key (what bindings name).
    pub id: String,
    /// The key's id in the plugin (`<mount>/keys/<key>`).
    pub key: String,
    /// The key's 20-byte EVM address, `0x` hex.
    pub address: String,
    /// The key's 33-byte compressed public key, `0x` hex, as the plugin reported it when the key
    /// was created. It must be the address's key.
    pub public_key: String,
}

/// A signing plugin of a Vault server, reached as the engine's own role.
pub struct VaultPlugin {
    resolver: Arc<Resolver>,
    mount: String,
}

impl VaultPlugin {
    /// The plugin at `mount`, through `resolver`.
    pub fn new(resolver: Arc<Resolver>, mount: &str) -> Self {
        Self {
            resolver,
            mount: mount.to_owned(),
        }
    }

    /// The plugin at `mount` on the Vault of `vault`, logged in to as `role` (the engine's own
    /// role, not the `vault` table's). Contacts nothing.
    pub fn with_role(vault: &VaultConfig, role: &str, mount: &str) -> Result<Self, KeyError> {
        let config = VaultConfig {
            role: role.to_owned(),
            ..vault.clone()
        };
        let resolver = Resolver::new(Some(&config)).map_err(|e| KeyError::Source(e.to_string()))?;
        Ok(Self::new(Arc::new(resolver), mount))
    }
}

impl PluginApi for VaultPlugin {
    async fn sign_hash(&self, key: &str, hash: &[u8; 32]) -> Result<Vec<u8>, KeyError> {
        let session = self.resolver.session().await.map_err(vault_failure)?;
        session
            .plugin_sign_hash(&self.mount, key, hash)
            .await
            .map_err(vault_failure)
    }

    async fn reachable(&self) -> Result<(), KeyError> {
        self.resolver
            .session()
            .await
            .map(drop)
            .map_err(vault_failure)
    }
}

struct Held {
    key: String,
    public_key: Vec<u8>,
    verifying: VerifyingKey,
    address: [u8; 20],
}

/// EVM delivery keys held in a Vault signing plugin.
pub struct PluginBackend<A: PluginApi> {
    api: A,
    environment: Environment,
    keys: BTreeMap<String, Held>,
}

fn refused(why: String) -> KeyError {
    KeyError::Refused(why)
}

fn parse_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    hex::decode(text.strip_prefix("0x").unwrap_or(text))
        .ok()?
        .try_into()
        .ok()
}

impl<A: PluginApi> PluginBackend<A> {
    /// Checks every key (module documentation). Offline: nothing is asked of Vault. `environment`
    /// is the environment the configuration declares for this custody.
    pub fn new(api: A, environment: Environment, keys: &[PluginKey]) -> Result<Self, KeyError> {
        let fixture = crate::kms::fixture_public_keys();
        let mut held = BTreeMap::new();
        for k in keys {
            let id = &k.id;
            if id.is_empty() || held.contains_key(id) {
                return Err(KeyError::Source(format!(
                    "key id {id:?} is empty or repeated"
                )));
            }
            if k.key.is_empty() || k.key.contains('/') {
                return Err(KeyError::Source(format!(
                    "key {id}: the plugin key is not a plain name"
                )));
            }
            let address = parse_hex::<20>(&k.address).ok_or_else(|| {
                refused(format!(
                    "key {id}: an EVM key declares its 20-byte address, 0x hex"
                ))
            })?;
            let public = parse_hex::<33>(&k.public_key)
                .and_then(|bytes| PublicKey::from_sec1_bytes(&bytes).ok())
                .ok_or_else(|| {
                    refused(format!(
                        "key {id}: public_key is not a 33-byte compressed secp256k1 key, 0x hex"
                    ))
                })?;
            if evm_address(&public) != address {
                return Err(refused(format!(
                    "key {id}: the declared public key is not the configured address's (REQ-KY-3)"
                )));
            }
            let compressed = public.to_encoded_point(true).as_bytes().to_vec();
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
                    public_key: compressed,
                    verifying: VerifyingKey::from(&public),
                    address,
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

    /// The engine's API, for tests that inspect what it was asked.
    pub fn api(&self) -> &A {
        &self.api
    }
}

impl<A: PluginApi> SigningEngine for PluginBackend<A> {
    fn kind(&self) -> EngineKind {
        EngineKind::SigningService
    }

    fn name(&self) -> &'static str {
        "Vault signing plugin"
    }

    fn environment(&self) -> Environment {
        self.environment
    }

    fn key_ids(&self) -> Vec<String> {
        self.keys.keys().cloned().collect()
    }

    fn scheme(&self, id: &str) -> Result<Scheme, KeyError> {
        self.held(id).map(|_| Scheme::Secp256k1Evm)
    }

    fn public_key(&self, id: &str) -> Result<Vec<u8>, KeyError> {
        self.held(id).map(|k| k.public_key.clone())
    }

    fn address(&self, id: &str) -> Result<Vec<u8>, KeyError> {
        self.held(id).map(|k| k.address.to_vec())
    }

    fn sign_prepared(
        &self,
        id: &str,
        scheme: Scheme,
        input: Signable<'_>,
    ) -> impl Future<Output = Result<Vec<u8>, KeyError>> + Send {
        let held = self.held(id).and_then(|k| {
            check_input(id, Scheme::Secp256k1Evm, scheme, &input)?;
            match input {
                Signable::Digest(digest) => Ok((k, digest)),
                Signable::Message(_) => unreachable!("check_input refuses a message"),
            }
        });
        async move {
            let (k, digest) = held?;
            let der = self.api.sign_hash(&k.key, &digest).await?;
            // v by recovery against the declared address; then verified against the public key.
            recoverable_from_der(id, "the plugin", &der, &digest, &k.verifying, |found| {
                evm_address(&PublicKey::from(found)) == k.address
            })
        }
    }

    async fn health(&self) -> Health {
        match self.api.reachable().await {
            Ok(()) => Health::ready(),
            Err(e) => Health::not_ready(e.to_string()),
        }
    }
}

/// Key ids, schemes, addresses and public keys only (`REQ-KY-1`).
impl<A: PluginApi> fmt::Debug for PluginBackend<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut map = f.debug_map();
        for (id, key) in &self.keys {
            map.entry(
                id,
                &format_args!(
                    "vault-plugin secp256k1-evm 0x{} 0x{}",
                    hex::encode(key.address),
                    hex::encode(&key.public_key)
                ),
            );
        }
        map.finish()
    }
}
