// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The signer's configuration: a JSON file whose path is the process's only argument.
//!
//! It names where keys come from, never a key (`REQ-KY-1`): the file backend's key file is a path.
//! The database URL is read from a file too, so a dump of the configuration carries no secret. Or
//! the store is named by `store`: its host, and a login and password that are `SecretRef`s (a file
//! or a field in Vault), from which the URL is built in the process (issue #64).
//!
//! ```json
//! {
//!   "socket": "/run/bezent/signer.sock",
//!   "database_url_file": "/run/secrets/bezent-signer-db",
//!   "callers": [{ "name": "core", "uid": 1001, "token_sha256": ["<64 hex>"] }],
//!   "networks": [
//!     { "id": "kreivo-local", "environment": "test", "issuing": true, "contract_account": "0x…",
//!       "genesis": "0x…" },
//!     { "id": "polygon-amoy", "environment": "test", "chain_id": 80002,
//!       "cancel_max_fee_per_gas": "3000000000000",
//!       "delivery": { "roles": "0x…", "multisend": "0x…", "emitter": "0x…",
//!                     "role": "bezent.delivery" } }
//!   ],
//!   "keys": { "backend": "file", "path": "test-keys.json" },
//!   "bindings": [
//!     { "key": "kreivo-mint", "network": "kreivo-local", "purposes": ["mint"],
//!       "window_cap": "1000000000", "window_seconds": 86400 }
//!   ]
//! }
//! ```
//!
//! A binding gives one key the intents of one network and purposes (`REQ-KY-4`: keys per network
//! and role), and the cap on what it signs per window (`DEC-3`). Relative paths are taken from the
//! configuration file's directory.
//!
//! An EVM network declares its `chain_id`, and only an EVM key (`secp256k1-evm`) is bound to it; a
//! network without one takes only a Kreivo key (PR #39 second review MF1: a Polygon intent never
//! reaches the Kreivo key, nor a Kreivo intent the Polygon key). A network whose key signs
//! `cancel` also declares `cancel_max_fee_per_gas`, the most a cancel may pay per gas, in wei
//! (review SF2). An EVM key, by its id and by its public key, is bound to one network only (third
//! review SF-B), and every EVM intent's bytes must carry that network's chain id.
//!
//! The signer decodes every transaction it signs and compares it with the core's order (the audit
//! of PR #56, must-fix 2; `REQ-AR-3`, `INV-9`, issue #23), so it needs what the bytes must name: an
//! issuing network whose key signs `mint` or `refund-mint` declares its `genesis` and the on-chain
//! component's `contract_account`; an EVM network whose key signs `payout` declares the vault's
//! `delivery` contracts (the Roles modifier, MultiSendCallOnly, the reference emitter, and the
//! delivery role's name), as F-008 deployed them.
//!
//! The statement key (`REQ-PR-1`; PLAN `DEC-16`, 2026-10-14; issue #42) is named apart, by
//! `"statement_key": "<key id>"`: an ed25519 key that signs intents of purpose `statement` and
//! nothing else. It is bound to no network, and no binding may name it or the purpose `statement`.
//!
//! Any other ed25519 key is a native Kreivo delivery account (the public key is the account): it
//! may be bound to a network that is not an EVM network, and only when its custody never releases
//! it ([`KeyBackend::non_exportable`]: Vault Transit or KMS, `REQ-CU-1`; the test file's keys are
//! test keys). Its public key must not be the statement key's.

use bezent_vault::{StoreSource, VaultConfig};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::keys::{Environment, KeyBackend, Scheme};

/// The purpose of an intent that destroys a bridged asset on the issuing network. Such an intent
/// must destroy from the on-chain component's own account (`DEC-3`, `INV-4`).
pub const DESTROY: &str = "destroy";

/// The purpose of an EVM cancel (F-009 NOTES 12; `REQ-NX-4`).
pub const CANCEL: &str = "cancel";

/// The purpose of a payout on an origin network.
pub const PAYOUT: &str = "payout";

/// The purpose of a mint on the issuing network.
pub const MINT: &str = "mint";

/// The purpose of a refund mint on the issuing network.
pub const REFUND_MINT: &str = "refund-mint";

/// The purpose of a statement intent: the statement key signs it, and nothing else does
/// (`REQ-PR-1`; PLAN `DEC-16`, 2026-10-14; issue #42).
pub const STATEMENT: &str = "statement";

/// A configuration that could not be read or is refused. It never quotes the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "configuration refused: {}", self.0)
    }
}

impl std::error::Error for ConfigError {}

fn refuse<T>(why: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError(why.into()))
}

/// The signer's configuration, checked.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The Unix socket the signer listens on.
    pub socket: PathBuf,
    /// A file holding the database URL of a login role that is a member of `bezent_signer` only.
    #[serde(default)]
    pub database_url_file: Option<PathBuf>,
    /// The store as a host and a login and password from files or Vault, in place of
    /// `database_url_file` (exactly one of them serves). The login is a member of `bezent_signer`
    /// only.
    #[serde(default)]
    pub store: Option<StoreSource>,
    /// Where Vault is and how to log in to it, when any secret is read from it: the store's login
    /// or password, or `keys.vault`.
    #[serde(default)]
    pub vault: Option<VaultConfig>,
    /// Who may ask for signatures.
    pub callers: Vec<Caller>,
    /// The networks the signer signs for.
    pub networks: Vec<Network>,
    /// Where the keys come from.
    pub keys: KeySource,
    /// Which key signs which intents, and its cap.
    pub bindings: Vec<Binding>,
    /// The statement key: an ed25519 key that signs statement intents only (`REQ-PR-1`).
    #[serde(default)]
    pub statement_key: Option<String>,
    /// The liveness file the running signer rewrites and `bezent-signer health` reads (issue #61).
    /// Left out, `/tmp/bezent-signer.health`; a task whose root file system is read-only names a
    /// writable path here.
    #[serde(default)]
    pub health_file: Option<PathBuf>,
}

/// A caller of the signer: its peer user id and the SHA-256 of its tokens (two while one rotates).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Caller {
    /// A name for the log.
    pub name: String,
    /// The user id the caller's process runs as.
    pub uid: u32,
    /// Hex SHA-256 of each token the caller may present.
    pub token_sha256: Vec<String>,
}

/// A network the signer signs for.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Network {
    /// The network id the core records on intents.
    pub id: String,
    /// Test or production (`REQ-KY-6`).
    pub environment: Environment,
    /// Whether this is the issuing network.
    #[serde(default)]
    pub issuing: bool,
    /// On the issuing network: the on-chain component's account, hex. Destroys come only from it.
    #[serde(default)]
    pub contract_account: Option<String>,
    /// On an EVM network: its chain id. Its presence is what makes the network an EVM one.
    #[serde(default)]
    pub chain_id: Option<u64>,
    /// On an EVM network whose key signs `cancel`: the most a cancel pays per gas, in wei, a
    /// decimal string.
    #[serde(default)]
    pub cancel_max_fee_per_gas: Option<String>,
    /// On the issuing network whose key signs `mint` or `refund-mint`: its genesis hash, `0x` hex,
    /// which every transaction the signer signs for it must carry (audit must-fix 2).
    #[serde(default)]
    pub genesis: Option<String>,
    /// On an EVM network whose key signs `payout`: the vault's contracts a payout must name
    /// (audit must-fix 2).
    #[serde(default)]
    pub delivery: Option<DeliveryContracts>,
}

/// The vault's contracts on an EVM network, as F-008 deploys them: what a payout's bytes must name.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryContracts {
    /// The Roles modifier, the transaction's target, `0x` hex.
    pub roles: String,
    /// MultiSendCallOnly, `0x` hex.
    pub multisend: String,
    /// The reference emitter, `0x` hex.
    pub emitter: String,
    /// The delivery role's name (`bezent.delivery`).
    pub role: String,
}

/// Where the keys come from (`DEC-11`).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "backend", rename_all = "lowercase", deny_unknown_fields)]
pub enum KeySource {
    /// Test keys from a file.
    File {
        /// The key file.
        path: PathBuf,
    },
    /// Delivery keys in AWS KMS, the other keys (statement, pause) optionally from Vault
    /// (`DEC-11`, issue #5).
    Kms {
        /// The environment of every key here; each KMS key's `bezent:environment` tag and the
        /// Vault secret must match it, and so must the networks the keys are bound to.
        environment: Environment,
        /// The AWS region.
        region: String,
        /// A service URL override (a VPC endpoint).
        #[serde(default)]
        endpoint: Option<String>,
        /// The KMS keys.
        keys: Vec<crate::kms::KmsKey>,
        /// The non-delivery keys, from Vault.
        #[serde(default)]
        vault: Option<crate::vault::VaultSource>,
    },
    /// Delivery keys in Vault Transit: ed25519 keys that never leave Vault (the Kreivo delivery
    /// account), the other keys (statement, pause) optionally from Vault KV. The signer logs in
    /// to Vault under its task role (`vault` table).
    Transit {
        /// The environment of every key here; the Vault KV secret must match it, and so must
        /// the networks the keys are bound to.
        environment: Environment,
        /// The Transit engine's mount, for example `transit-bezent-signer`.
        mount: String,
        /// The Transit keys.
        keys: Vec<crate::transit::TransitKey>,
        /// The non-delivery keys, from Vault KV.
        #[serde(default)]
        vault: Option<crate::vault::VaultSource>,
    },
}

/// One key's intents and cap.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// The key id.
    pub key: String,
    /// The network id.
    pub network: String,
    /// The intent purposes this key signs on that network.
    pub purposes: Vec<String>,
    /// The most this key signs per window, in minor units, as a decimal string (`INV-11`).
    pub window_cap: String,
    /// The window's length.
    pub window_seconds: u64,
}

/// What the signer decides by: the networks and bindings, resolved.
#[derive(Debug, Clone)]
pub struct Policy {
    networks: BTreeMap<String, NetworkPolicy>,
    bindings: BTreeMap<(String, String), KeyPolicy>,
    statement_key: Option<String>,
}

/// A network, resolved.
#[derive(Debug, Clone)]
pub struct NetworkPolicy {
    /// Test or production.
    pub environment: Environment,
    /// Whether this is the issuing network.
    pub issuing: bool,
    /// The on-chain component's account, if this is the issuing network.
    pub contract_account: Option<Vec<u8>>,
    /// The chain id, if this is an EVM network.
    pub chain_id: Option<u64>,
    /// The most a cancel pays per gas, in wei, if this network's key signs cancels.
    pub cancel_max_fee_per_gas: Option<u128>,
    /// The issuing network's genesis hash, if declared.
    pub genesis: Option<[u8; 32]>,
    /// The vault's contracts, on an EVM network that pays out.
    pub delivery: Option<crate::delivery::EvmContracts>,
}

impl NetworkPolicy {
    /// Whether a key of `scheme` may sign this network's intents: an EVM key on an EVM network,
    /// a Kreivo key elsewhere (PR #39 second review MF1).
    pub fn takes(&self, scheme: Scheme) -> bool {
        (scheme == Scheme::Secp256k1Evm) == self.chain_id.is_some()
    }
}

/// A key's binding, resolved.
#[derive(Debug, Clone)]
pub struct KeyPolicy {
    /// The key id.
    pub key: String,
    /// The cap per window, in minor units.
    pub window_cap: u128,
    /// The window's length in seconds.
    pub window_seconds: u64,
}

impl Config {
    /// Reads the configuration at `path`, resolving relative paths against its directory.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let bytes = std::fs::read(path)
            .map_err(|e| ConfigError(format!("cannot read {}: {}", path.display(), e.kind())))?;
        let mut config = Self::parse(&bytes)?;
        let base = path.parent().unwrap_or(Path::new("."));
        let resolve = |p: &mut PathBuf| {
            if p.is_relative() {
                *p = base.join(&*p);
            }
        };
        resolve(&mut config.socket);
        if let Some(store) = config.store.as_mut() {
            store.rebase(base);
        }
        if let Some(p) = config.health_file.as_mut() {
            resolve(p);
        }
        if let Some(p) = config.database_url_file.as_mut() {
            resolve(p);
        }
        match &mut config.keys {
            KeySource::File { path } => resolve(path),
            KeySource::Kms { .. } | KeySource::Transit { .. } => {}
        }
        Ok(config)
    }

    /// Parses a configuration, and checks what needs no key: the Vault table and the store, and
    /// that every secret read from Vault has a table to log in with. Contacts nothing.
    pub fn parse(bytes: &[u8]) -> Result<Self, ConfigError> {
        let config: Self = serde_json::from_slice(bytes).map_err(|e| {
            ConfigError(format!(
                "not a valid configuration ({:?} error at line {}, column {})",
                e.classify(),
                e.line(),
                e.column()
            ))
        })?;
        config.check_secrets()?;
        Ok(config)
    }

    fn check_secrets(&self) -> Result<(), ConfigError> {
        let refuse_with = |e: bezent_vault::Error| ConfigError(e.to_string());
        if let Some(vault) = &self.vault {
            vault.check().map_err(refuse_with)?;
        }
        if let Some(store) = &self.store {
            store.check().map_err(refuse_with)?;
            if self.database_url_file.is_some() {
                return refuse("set one of database_url_file and store, not both");
            }
        }
        let from_vault = self
            .store
            .as_ref()
            .is_some_and(|s| s.login.is_vault() || s.password.is_vault())
            || matches!(
                &self.keys,
                KeySource::Kms { vault: Some(_), .. } | KeySource::Transit { .. }
            );
        if from_vault && self.vault.is_none() {
            return refuse("a secret is read from Vault but there is no vault table");
        }
        Ok(())
    }

    /// Checks the configuration against the key backend and resolves it. Refuses, at start, a
    /// binding of a key to a network of another environment (`REQ-KY-6`, `AC-I4.2`), a binding to
    /// an undeclared network or key, two keys for one network and purpose, a destroy on the
    /// issuing network without the on-chain component's account, and a caller without a token.
    pub fn policy(&self, backend: &impl KeyBackend) -> Result<Policy, ConfigError> {
        if self.callers.is_empty() {
            return refuse("no caller is declared");
        }
        for caller in &self.callers {
            if caller.token_sha256.is_empty() {
                return refuse(format!("caller {} has no token", caller.name));
            }
            for hash in &caller.token_sha256 {
                if hash.len() != 64 || hex::decode(hash).is_err() {
                    return refuse(format!(
                        "caller {}: a token_sha256 is not 64 hex digits",
                        caller.name
                    ));
                }
            }
        }

        let mut networks = BTreeMap::new();
        for n in &self.networks {
            let contract_account = match &n.contract_account {
                Some(hex_account) => Some(
                    hex::decode(hex_account.strip_prefix("0x").unwrap_or(hex_account))
                        .ok()
                        .filter(|a| !a.is_empty())
                        .ok_or_else(|| {
                            ConfigError(format!("network {}: contract_account is not hex", n.id))
                        })?,
                ),
                None => None,
            };
            if contract_account.is_some() && !n.issuing {
                return refuse(format!(
                    "network {}: only the issuing network has a contract account",
                    n.id
                ));
            }
            if n.issuing && n.chain_id.is_some() {
                return refuse(format!(
                    "network {}: the issuing network is not an EVM network; it has no chain_id",
                    n.id
                ));
            }
            let cancel_max_fee_per_gas = match &n.cancel_max_fee_per_gas {
                Some(fee) => Some(
                    parse_minor_units(fee)
                        .filter(|_| n.chain_id.is_some())
                        .ok_or_else(|| {
                            ConfigError(format!(
                                "network {}: cancel_max_fee_per_gas is a decimal count of wei, on an EVM network",
                                n.id
                            ))
                        })?,
                ),
                None => None,
            };
            let hex32 = |field: &str, text: &str| -> Result<Vec<u8>, ConfigError> {
                hex::decode(text.strip_prefix("0x").unwrap_or(text))
                    .ok()
                    .ok_or_else(|| ConfigError(format!("network {}: {field} is not hex", n.id)))
            };
            let genesis = match &n.genesis {
                Some(g) if n.issuing => Some(
                    <[u8; 32]>::try_from(hex32("genesis", g)?.as_slice()).map_err(|_| {
                        ConfigError(format!("network {}: genesis is not 32 bytes", n.id))
                    })?,
                ),
                Some(_) => {
                    return refuse(format!(
                        "network {}: only the issuing network declares a genesis here",
                        n.id
                    ));
                }
                None => None,
            };
            let delivery = match &n.delivery {
                Some(d) if n.chain_id.is_some() => {
                    let address = |field: &str, text: &str| -> Result<[u8; 20], ConfigError> {
                        <[u8; 20]>::try_from(hex32(field, text)?.as_slice()).map_err(|_| {
                            ConfigError(format!("network {}: {field} is not 20 bytes", n.id))
                        })
                    };
                    Some(crate::delivery::EvmContracts {
                        roles: address("delivery.roles", &d.roles)?,
                        multisend: address("delivery.multisend", &d.multisend)?,
                        emitter: address("delivery.emitter", &d.emitter)?,
                        role_key: crate::delivery::role_key(&d.role).ok_or_else(|| {
                            ConfigError(format!(
                                "network {}: delivery.role is 1 to 32 ASCII bytes",
                                n.id
                            ))
                        })?,
                    })
                }
                Some(_) => {
                    return refuse(format!(
                        "network {}: only an EVM network declares the vault's delivery contracts",
                        n.id
                    ));
                }
                None => None,
            };
            let resolved = NetworkPolicy {
                environment: n.environment,
                issuing: n.issuing,
                contract_account,
                chain_id: n.chain_id,
                cancel_max_fee_per_gas,
                genesis,
                delivery,
            };
            if n.id.is_empty() || networks.insert(n.id.clone(), resolved).is_some() {
                return refuse(format!("network id {:?} is empty or repeated", n.id));
            }
        }

        let mut bindings = BTreeMap::new();
        let mut keys_seen = BTreeSet::new();
        let mut evm_networks: BTreeMap<Vec<u8>, String> = BTreeMap::new();
        let mut evm_key_ids: BTreeMap<String, String> = BTreeMap::new();
        for b in &self.bindings {
            let Some(network) = networks.get(&b.network) else {
                return refuse(format!(
                    "key {} is bound to undeclared network {}",
                    b.key, b.network
                ));
            };
            let scheme = backend
                .scheme(&b.key)
                .map_err(|e| ConfigError(format!("binding of key {}: {e}", b.key)))?;
            if scheme == Scheme::Ed25519 {
                // The statement key signs nothing else (REQ-PR-1). Any other ed25519 key is a
                // native Kreivo delivery account, and only a custody that never releases the key
                // may hold one (REQ-CU-1); the test file's keys are test keys (REQ-KY-6).
                if self.statement_key.as_deref() == Some(b.key.as_str()) {
                    return refuse(format!(
                        "key {} is an ed25519 statement key, used for nothing else (REQ-PR-1)",
                        b.key
                    ));
                }
                if !backend.non_exportable(&b.key) && backend.environment() != Environment::Test {
                    return refuse(format!(
                        "key {} is an ed25519 delivery key held where the signer can read it; \
                         delivery keys live in a key-management service (REQ-CU-1)",
                        b.key
                    ));
                }
            }
            if !network.takes(scheme) {
                return refuse(format!(
                    "key {} is a {} key and network {} {} (INV-9, REQ-KY-4)",
                    b.key,
                    scheme.as_str(),
                    b.network,
                    if network.chain_id.is_some() {
                        "is an EVM network"
                    } else {
                        "is not an EVM network"
                    }
                ));
            }
            if backend.environment() != network.environment {
                return refuse(format!(
                    "key {} is a {} key and network {} is {} (REQ-KY-6, AC-I4.2)",
                    b.key,
                    backend.environment(),
                    b.network,
                    network.environment
                ));
            }
            let window_cap = parse_minor_units(&b.window_cap).ok_or_else(|| {
                ConfigError(format!(
                    "key {}: window_cap is not a decimal count of minor units",
                    b.key
                ))
            })?;
            if b.window_seconds == 0 {
                return refuse(format!("key {}: window_seconds is zero", b.key));
            }
            if b.purposes.is_empty() {
                return refuse(format!("key {}: no purpose", b.key));
            }
            if b.purposes.iter().any(|p| p == STATEMENT) {
                return refuse(format!(
                    "key {}: the purpose statement is the statement key's alone, bound to no network (REQ-PR-1)",
                    b.key
                ));
            }
            if !keys_seen.insert((b.key.clone(), b.network.clone())) {
                return refuse(format!("key {} is bound twice to {}", b.key, b.network));
            }
            // SF-B (third review): an EVM key, by its id and by its public key, is bound to one
            // network only, so no lane key signs for two chains (REQ-KY-4: keys per network).
            if scheme == Scheme::Secp256k1Evm {
                let public = backend
                    .public_key(&b.key)
                    .map_err(|e| ConfigError(format!("binding of key {}: {e}", b.key)))?;
                if let Some(other) = evm_networks.insert(public, b.network.clone())
                    && other != b.network
                {
                    return refuse(format!(
                        "key {} is an EVM key bound to {other} and to {}; an EVM key signs for \
                         one network only (REQ-KY-4, SF-B)",
                        b.key, b.network
                    ));
                }
                if let Some(other) = evm_key_ids.insert(b.key.clone(), b.network.clone())
                    && other != b.network
                {
                    return refuse(format!(
                        "key {} is an EVM key bound to {other} and to {} (REQ-KY-4, SF-B)",
                        b.key, b.network
                    ));
                }
            }
            for purpose in &b.purposes {
                if purpose == CANCEL && network.cancel_max_fee_per_gas.is_none() {
                    return refuse(format!(
                        "network {}: a cancel needs the network's chain_id and cancel_max_fee_per_gas (REQ-NX-4)",
                        b.network
                    ));
                }
                if purpose == DESTROY && network.issuing && network.contract_account.is_none() {
                    return refuse(format!(
                        "network {}: a destroy needs the on-chain component's account (DEC-3)",
                        b.network
                    ));
                }
                // Audit must-fix 2 (REQ-AR-3, INV-9): the signer decodes what it signs and
                // compares it with the core's order, so it knows what the bytes must name.
                if purpose == PAYOUT && network.delivery.is_none() {
                    return refuse(format!(
                        "network {}: a payout needs the vault's delivery contracts (REQ-AR-3)",
                        b.network
                    ));
                }
                if (purpose == MINT || purpose == REFUND_MINT)
                    && (network.genesis.is_none() || network.contract_account.is_none())
                {
                    return refuse(format!(
                        "network {}: a {purpose} needs the network's genesis and the on-chain \
                         component's account (REQ-AR-3)",
                        b.network
                    ));
                }
                let policy = KeyPolicy {
                    key: b.key.clone(),
                    window_cap,
                    window_seconds: b.window_seconds,
                };
                if bindings
                    .insert((b.network.clone(), purpose.clone()), policy)
                    .is_some()
                {
                    return refuse(format!(
                        "two keys sign {purpose} on {} (REQ-KY-4)",
                        b.network
                    ));
                }
            }
        }
        if let Some(key) = &self.statement_key {
            let scheme = backend
                .scheme(key)
                .map_err(|e| ConfigError(format!("statement key {key}: {e}")))?;
            if scheme != Scheme::Ed25519 {
                return refuse(format!(
                    "statement key {key} is a {} key; the statement key is ed25519 (REQ-PR-1)",
                    scheme.as_str()
                ));
            }
            // Used for nothing else, by its public key as well as its id: no delivery key is
            // the same key under another id (REQ-PR-1).
            let public = backend
                .public_key(key)
                .map_err(|e| ConfigError(format!("statement key {key}: {e}")))?;
            for b in &self.bindings {
                if backend.public_key(&b.key).is_ok_and(|p| p == public) {
                    return refuse(format!(
                        "statement key {key} is also key {}, bound to network {}; it signs \
                         statements only (REQ-PR-1)",
                        b.key, b.network
                    ));
                }
            }
            if self.bindings.iter().any(|b| &b.key == key) {
                return refuse(format!(
                    "statement key {key} is also bound to a network; it signs statements only (REQ-PR-1)"
                ));
            }
        }
        Ok(Policy {
            networks,
            bindings,
            statement_key: self.statement_key.clone(),
        })
    }
}

impl Policy {
    /// The network `id`, if declared.
    pub fn network(&self, id: &str) -> Option<&NetworkPolicy> {
        self.networks.get(id)
    }

    /// The key that signs `purpose` on `network`, if one is bound.
    pub fn binding(&self, network: &str, purpose: &str) -> Option<&KeyPolicy> {
        self.bindings.get(&(network.to_owned(), purpose.to_owned()))
    }

    /// The statement key, if one is configured (`REQ-PR-1`).
    pub fn statement_key(&self) -> Option<&str> {
        self.statement_key.as_deref()
    }
}

/// Holds the keys' environment to the store's networks and to the configuration's own, the way the
/// core does (`REQ-KY-6`, `AC-I4.2`, issue #25): a test custody never runs on a store that
/// declares a production network (`production_in_store` is [`bezent_store::environment::production_networks`]),
/// and a network's declared environment must agree with its id's (a test network is one of
/// [`bezent_store::environment::TEST_NETWORKS`]), so a label in the configuration alone never
/// makes a production network a test one.
pub fn confirm_store_environment(
    networks: &[Network],
    keys: Environment,
    production_in_store: &[String],
) -> Result<(), ConfigError> {
    if keys == Environment::Test && !production_in_store.is_empty() {
        return refuse(format!(
            "test keys refused: the store declares production networks ({}) (REQ-KY-6, AC-I4.2)",
            production_in_store.join(", ")
        ));
    }
    for n in networks {
        let is_test = bezent_store::environment::is_test_network(&n.id);
        if is_test != (n.environment == Environment::Test) {
            return refuse(format!(
                "network {} is declared {} but is {} by its id (REQ-KY-6, AC-I4.2)",
                n.id,
                n.environment,
                if is_test {
                    "a test network"
                } else {
                    "not a test network"
                }
            ));
        }
    }
    Ok(())
}

/// A decimal count of minor units (`INV-11`): digits only, at most `u128::MAX`.
pub fn parse_minor_units(text: &str) -> Option<u128> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod secret_tests {
    use super::*;

    /// The published fixture, with `edit` applied to it.
    fn parse(edit: impl FnOnce(&mut serde_json::Value)) -> Result<Config, ConfigError> {
        let mut json: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/signer.json")).unwrap();
        edit(&mut json);
        Config::parse(json.to_string().as_bytes())
    }

    const VAULT: &str = r#"{"address":"https://vault.example:8200","mount":"bezent","role":"bezent-signer-iam","sts_region":"us-east-2","server_id":"vault.example"}"#;

    fn store(login: &str) -> serde_json::Value {
        serde_json::from_str(&format!(
            r#"{{"host":"db.example","database":"bezent","login":{login},"password":{{"vault":{{"path":"secret/bezent/signer","field":"db_password"}}}}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn req_ky_1_the_store_is_a_host_with_a_login_and_password_from_files_or_vault() {
        let config = parse(|j| {
            j["store"] = store(r#"{"file":"login"}"#);
            j["vault"] = serde_json::from_str(VAULT).unwrap();
        })
        .unwrap();
        assert!(config.store.is_some() && config.vault.is_some());
        // Debug prints references, never values.
        assert!(!format!("{config:?}").contains("hunter2"));
    }

    #[test]
    fn req_ky_1_a_vault_secret_needs_the_vault_table_and_the_store_one_source() {
        assert!(parse(|j| j["store"] = store(r#"{"file":"login"}"#)).is_err());
        assert!(
            parse(|j| {
                j["store"] = store(r#"{"file":"login"}"#);
                j["vault"] = serde_json::from_str(VAULT).unwrap();
                j["database_url_file"] = "url".into();
            })
            .is_err()
        );
        assert!(
            parse(|j| {
                j["vault"] = serde_json::from_str(&VAULT.replace("https", "http")).unwrap();
            })
            .is_err(),
            "plain http off loopback"
        );
    }

    #[test]
    fn req_ky_1_keys_vault_logs_in_and_no_longer_takes_a_token_file() {
        let kms = |vault: serde_json::Value| {
            move |j: &mut serde_json::Value| {
                j["keys"] = serde_json::json!({
                    "backend": "kms", "environment": "test", "region": "us-east-2",
                    "keys": [], "vault": vault,
                });
            }
        };
        assert!(
            parse(kms(serde_json::json!({"path": "secret/bezent/signer"}))).is_err(),
            "keys.vault with no vault table"
        );
        assert!(
            parse(|j| {
                kms(serde_json::json!({"path": "secret/bezent/signer"}))(j);
                j["vault"] = serde_json::from_str(VAULT).unwrap();
            })
            .is_ok()
        );
        assert!(
            parse(kms(
                serde_json::json!({"path": "p", "address": "https://v.example", "token_file": "t"})
            ))
            .is_err()
        );
    }
}
