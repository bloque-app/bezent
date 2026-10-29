// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The endpoint's configuration (`T-009-01`): one instance per EVM network (PLAN `DEC-8`).
//!
//! A TOML file. No secret is in it: the database URL, the providers' URLs (which may carry a
//! key, `F-014`) and the signer token are read from the files it names (`REQ-KY-1`: never from
//! the environment). Each can instead be a `SecretRef` (issue #64): a file (`{ file = "…" }`) or a
//! field in Vault (`{ vault = { path, field } }`), with the login's settings in `[vault_login]`
//! (the name `vault` is the Safe's address here). The store is then a `[store]` (host, login and
//! password as `SecretRef`s; the URL is built in the process). What the store declares (the reserve, the admitted tokens) is read from the
//! store and checked against this file at start-up ([`crate::network::Declarations`]).
//!
//! ```toml
//! network = "polygon"
//! chain_id = 137
//! pinned_block = 95400000
//! pinned_hash = "0x…"
//! vault = "0x…"            # the Safe: the reserve address of every asset on this network
//! roles = "0x…"            # its Roles modifier
//! emitter = "0x…"          # the reference emitter
//! multisend = "0x…"        # MultiSendCallOnly 1.4.1
//! delivery_role = "bezent.delivery"
//! start_block = 95400000
//! database_url_file = "/run/secrets/…"
//!
//! [[providers]]            # exactly two, independent (DEC-12)
//! name = "a"
//! url_file = "/run/secrets/…"
//!
//! [[convertibles]]         # REQ-CV-4: itemised, never reserve
//! token = "0x…"
//! pool = "0x…"
//!
//! [lane]                   # REQ-EO-4: one lane, the delivery key's nonce sequence
//! key = "polygon-payout"
//! public_key = "0x02…"
//!
//! [gas]
//! max_gas = 400000
//! max_fee_per_gas = "2000000000000"
//!
//! [signer]
//! socket = "/run/bezent-signer/signer.sock"
//! uid = 1001
//! token_file = "/run/secrets/…"
//! ```

use std::path::{Path, PathBuf};

use alloy::primitives::{Address, B256};
use bezent_vault::{Resolver, SecretRef, StoreSource, VaultConfig, Zeroizing};
use serde::Deserialize;

use crate::evm::{self, Contracts};

/// One provider.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    /// Its name, for logs and records.
    pub name: String,
    /// A file holding its URL (which may carry a key).
    #[serde(default)]
    pub url_file: Option<PathBuf>,
    /// Its URL, written here only when it carries no key (a local node, a test).
    #[serde(default)]
    pub url: Option<String>,
    /// Its URL as a `SecretRef`: a file, or a field in Vault.
    #[serde(default)]
    pub url_secret: Option<SecretRef>,
}

/// A convertible token admitted at the vault, with the pool its conversion uses (F-008,
/// "Pinned pools").
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Convertible {
    /// The token.
    pub token: String,
    /// The declared pool.
    pub pool: String,
}

/// The delivery key's lane.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LaneConfig {
    /// The signer's id for the key.
    pub key: String,
    /// Its secp256k1 public key, compressed, `0x` hex.
    pub public_key: String,
}

/// Bounds on what a payout may cost.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GasConfig {
    /// The highest gas limit a payout may carry.
    pub max_gas: u64,
    /// The highest `maxFeePerGas`, in wei, a decimal string.
    pub max_fee_per_gas: String,
    /// The margin over the estimate, in percent.
    #[serde(default = "default_margin")]
    pub margin_percent: u64,
}

fn default_margin() -> u64 {
    130
}

/// The signer link.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignerConfig {
    /// The signer's socket.
    pub socket: PathBuf,
    /// The signer's user id.
    pub uid: u32,
    /// A file holding this endpoint's token. Exactly one of `token_file` and `token`.
    #[serde(default)]
    pub token_file: Option<PathBuf>,
    /// The same token as a `SecretRef`: a file, or a field in Vault.
    #[serde(default)]
    pub token: Option<SecretRef>,
}

/// The configuration.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The network's id in the store.
    pub network: String,
    /// Its chain id (`AC-I4.1`).
    pub chain_id: u64,
    /// A block of its history, pinned by number and hash (`AC-I4.1`).
    pub pinned_block: u64,
    /// That block's hash.
    pub pinned_hash: String,
    /// The vault.
    pub vault: String,
    /// The vault's Roles modifier.
    pub roles: String,
    /// The reference emitter.
    pub emitter: String,
    /// MultiSendCallOnly.
    pub multisend: String,
    /// The delivery role's name.
    #[serde(default = "default_role")]
    pub delivery_role: String,
    /// The first block to read.
    pub start_block: u64,
    /// The most blocks one `eth_getLogs` range covers.
    #[serde(default = "default_range")]
    pub max_range: u64,
    /// How often the endpoint looks, in milliseconds.
    #[serde(default = "default_poll")]
    pub poll_ms: u64,
    /// A file holding the database URL (the endpoint's login). Exactly one of `database_url_file`
    /// and `[store]`.
    #[serde(default)]
    pub database_url_file: Option<PathBuf>,
    /// The store as a host and a login and password from files or Vault, in place of
    /// `database_url_file`; the login is the same.
    #[serde(default)]
    pub store: Option<StoreSource>,
    /// Where Vault is and how to log in to it, when any secret is read from it. Not `vault`: that
    /// is the Safe's address.
    #[serde(default)]
    pub vault_login: Option<VaultConfig>,
    /// The pool size.
    #[serde(default = "default_connections")]
    pub max_connections: u32,
    /// The two providers.
    pub providers: Vec<Provider>,
    /// The convertibles.
    #[serde(default)]
    pub convertibles: Vec<Convertible>,
    /// The lane.
    pub lane: LaneConfig,
    /// The gas bounds.
    pub gas: GasConfig,
    /// The signer link.
    pub signer: SignerConfig,
    /// The liveness file the running endpoint rewrites after each pass that reached the head of the
    /// network it observes and `bezent-evm-endpoint health` reads (issue #61). Left out,
    /// `/tmp/bezent-evm-endpoint.health`; a task whose root file system is read-only names a writable path here.
    #[serde(default)]
    pub health_file: Option<PathBuf>,
}

fn default_role() -> String {
    "bezent.delivery".into()
}
fn default_range() -> u64 {
    500
}
fn default_poll() -> u64 {
    2_000
}
fn default_connections() -> u32 {
    4
}

/// The configuration, checked: addresses parsed, the lane's address derived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    /// The contracts of the delivery call.
    pub contracts: Contracts,
    /// The vault.
    pub vault: Address,
    /// The pinned hash.
    pub pinned_hash: B256,
    /// The lane's public key.
    pub lane_public_key: Vec<u8>,
    /// The lane's address.
    pub lane_address: Address,
    /// The convertibles and their pools.
    pub convertibles: Vec<(Address, Address)>,
    /// The highest `maxFeePerGas`.
    pub max_fee_per_gas: u128,
}

fn address(field: &str, s: &str) -> Result<Address, String> {
    s.parse()
        .map_err(|e| format!("{field}: not an address ({e})"))
}

impl Config {
    /// Reads and checks `path`. Relative paths in it resolve against its directory.
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let mut c = Self::parse(&text)?;
        if let Some(dir) = path.parent() {
            let resolve = |p: &mut PathBuf| {
                if p.is_relative() {
                    *p = dir.join(&*p);
                }
            };
            if let Some(p) = c.database_url_file.as_mut() {
                resolve(p);
            }
            if let Some(p) = c.signer.token_file.as_mut() {
                resolve(p);
            }
            if let Some(store) = c.store.as_mut() {
                store.rebase(dir);
            }
            if let Some(token) = c.signer.token.as_mut() {
                token.rebase(dir);
            }
            if let Some(p) = c.health_file.as_mut() {
                resolve(p);
            }
            for p in &mut c.providers {
                if let Some(f) = &mut p.url_file {
                    resolve(f);
                }
                if let Some(secret) = p.url_secret.as_mut() {
                    secret.rebase(dir);
                }
            }
        }
        Ok(c)
    }

    /// Parses and checks a configuration.
    pub fn parse(text: &str) -> Result<Self, String> {
        let c: Self = toml::from_str(text).map_err(|e| format!("the configuration: {e}"))?;
        c.check()?;
        Ok(c)
    }

    /// The checks that need no network (`T-009-01`): two independent providers, every address
    /// well formed, the lane's key a secp256k1 key.
    pub fn check(&self) -> Result<Checked, String> {
        if self.network.is_empty() {
            return Err("network: empty".into());
        }
        // DEC-12: exactly two providers, independent: distinct names and sources.
        if self.providers.len() != 2 {
            return Err(format!(
                "DEC-12: two independent providers are needed; {} declared",
                self.providers.len()
            ));
        }
        for p in &self.providers {
            let sources = [
                p.url_file.is_some(),
                p.url.is_some(),
                p.url_secret.is_some(),
            ];
            if sources.iter().filter(|s| **s).count() != 1 {
                return Err(format!(
                    "provider {}: exactly one of url_file, url and url_secret",
                    p.name
                ));
            }
        }
        let (a, b) = (&self.providers[0], &self.providers[1]);
        if a.name == b.name
            || (a.url_file.is_some() && a.url_file == b.url_file)
            || (a.url.is_some() && a.url == b.url)
            || (a.url_secret.is_some() && a.url_secret == b.url_secret)
        {
            return Err(
                "DEC-12: the two providers must be independent: distinct names and URLs".into(),
            );
        }
        self.check_secrets()?;
        if self.max_range == 0 {
            return Err("max_range: at least 1".into());
        }
        if self.start_block < self.pinned_block {
            return Err("start_block: at or after the pinned block".into());
        }
        let pinned_hash: B256 = self
            .pinned_hash
            .parse()
            .map_err(|e| format!("pinned_hash: {e}"))?;
        let vault = address("vault", &self.vault)?;
        let contracts = Contracts {
            roles: address("roles", &self.roles)?,
            multisend: address("multisend", &self.multisend)?,
            emitter: address("emitter", &self.emitter)?,
            delivery_role: evm::role_key(&self.delivery_role)
                .ok_or("delivery_role: 1 to 32 ASCII bytes")?,
        };
        let lane_public_key = self
            .lane
            .public_key
            .strip_prefix("0x")
            .and_then(|h| hex::decode(h).ok())
            .ok_or("lane.public_key: 0x hex")?;
        let lane_address =
            evm::address_of(&lane_public_key).ok_or("lane.public_key: not a secp256k1 key")?;
        if [
            vault,
            contracts.roles,
            contracts.emitter,
            contracts.multisend,
        ]
        .contains(&lane_address)
        {
            return Err("lane: the delivery key is one of the vault's contracts".into());
        }
        let mut convertibles = Vec::new();
        for c in &self.convertibles {
            convertibles.push((
                address("convertibles.token", &c.token)?,
                address("convertibles.pool", &c.pool)?,
            ));
        }
        let max_fee_per_gas: u128 = self
            .gas
            .max_fee_per_gas
            .parse()
            .map_err(|_| "gas.max_fee_per_gas: a decimal number of wei")?;
        if self.gas.max_gas == 0 || self.gas.margin_percent < 100 {
            return Err("gas: max_gas above 0 and margin_percent at least 100".into());
        }
        Ok(Checked {
            contracts,
            vault,
            pinned_hash,
            lane_public_key,
            lane_address,
            convertibles,
            max_fee_per_gas,
        })
    }

    /// A provider's URL: written in the file, or read from its file or from Vault.
    pub async fn provider_url(p: &Provider, resolver: &Resolver) -> Result<String, String> {
        match (&p.url, &p.url_file, &p.url_secret) {
            (Some(u), _, _) => Ok(u.clone()),
            (None, Some(f), _) => std::fs::read_to_string(f)
                .map(|s| s.trim().to_owned())
                .map_err(|e| format!("provider {}: cannot read {}: {e}", p.name, f.display())),
            (None, None, Some(secret)) => resolver
                .resolve(secret)
                .await
                .map(|u| u.to_string())
                .map_err(|e| format!("provider {}: {e}", p.name)),
            (None, None, None) => Err(format!("provider {}: no URL", p.name)),
        }
    }

    /// The two providers' URLs, which must differ once read: two references to Vault can name one
    /// URL, and the checks of [`Config::check`] compare references, not values (`DEC-12`).
    pub async fn provider_urls(&self, resolver: &Resolver) -> Result<[String; 2], String> {
        let a = Self::provider_url(&self.providers[0], resolver).await?;
        let b = Self::provider_url(&self.providers[1], resolver).await?;
        if a == b {
            return Err("DEC-12: the two providers must be independent: distinct URLs".into());
        }
        Ok([a, b])
    }

    /// The checks of the secrets' sources, which touch no network: one source for the store and
    /// for the signer token, a valid `[vault_login]` table, and one wherever a secret comes from
    /// Vault.
    fn check_secrets(&self) -> Result<(), String> {
        if self.database_url_file.is_some() == self.store.is_some() {
            return Err("set exactly one of database_url_file and [store]".into());
        }
        if self.signer.token_file.is_some() == self.signer.token.is_some() {
            return Err("set exactly one of signer.token_file and signer.token".into());
        }
        if let Some(vault) = &self.vault_login {
            vault.check().map_err(|e| e.to_string())?;
        }
        if let Some(store) = &self.store {
            store.check().map_err(|e| e.to_string())?;
        }
        let from_vault = self
            .store
            .as_ref()
            .is_some_and(|s| s.login.is_vault() || s.password.is_vault())
            || self.signer.token.as_ref().is_some_and(SecretRef::is_vault)
            || self
                .providers
                .iter()
                .any(|p| p.url_secret.as_ref().is_some_and(SecretRef::is_vault));
        if from_vault && self.vault_login.is_none() {
            return Err("a secret is read from Vault but there is no [vault_login] table".into());
        }
        Ok(())
    }

    /// The resolver for this configuration's `[vault_login]` table. Contacts nothing until a
    /// secret is read from Vault.
    pub fn resolver(&self) -> Result<Resolver, String> {
        Resolver::new(self.vault_login.as_ref()).map_err(|e| e.to_string())
    }

    /// The store's URL: read from `database_url_file`, or built from `[store]`. Never printed.
    pub async fn store_url(&self, resolver: &Resolver) -> Result<Zeroizing<String>, String> {
        match (&self.database_url_file, &self.store) {
            (Some(file), _) => bezent_vault::read_file(file).map_err(|e| e.to_string()),
            (None, Some(store)) => resolver.store_url(store).await.map_err(|e| e.to_string()),
            (None, None) => Err("no store is configured".to_owned()),
        }
    }

    /// The endpoint's token for the signer link.
    pub async fn signer_token(&self, resolver: &Resolver) -> Result<Zeroizing<String>, String> {
        match (&self.signer.token_file, &self.signer.token) {
            (Some(file), _) => bezent_vault::read_file(file).map_err(|e| e.to_string()),
            (None, Some(token)) => resolver.resolve(token).await.map_err(|e| e.to_string()),
            (None, None) => Err("no signer token is configured".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../fixtures/endpoint.toml");

    #[test]
    fn t_009_01_the_fixture_checks() {
        let c = Config::parse(FIXTURE).unwrap();
        let checked = c.check().unwrap();
        assert_eq!(c.chain_id, 137);
        // Foundry's test account 4: a test key with no power anywhere (REQ-KY-6).
        assert_eq!(
            checked.lane_address,
            "0x15d34AAf54267DB7D7c367839AAf71A00a2C6A65"
                .parse::<Address>()
                .unwrap()
        );
        assert_eq!(
            checked.contracts.delivery_role,
            evm::role_key("bezent.delivery").unwrap()
        );
    }

    #[test]
    fn dec_12_one_provider_or_the_same_one_twice_is_refused() {
        let one = FIXTURE.replace(
            "[[providers]]\nname = \"b\"\nurl_file = \"/run/secrets/bezent-evm-polygon-rpc-b\"\n",
            "",
        );
        assert!(Config::parse(&one).unwrap_err().contains("DEC-12"));
        let twice = FIXTURE.replace(
            "/run/secrets/bezent-evm-polygon-rpc-b",
            "/run/secrets/bezent-evm-polygon-rpc-a",
        );
        assert!(Config::parse(&twice).unwrap_err().contains("DEC-12"));
    }

    #[test]
    fn t_009_01_a_bad_address_or_key_is_refused() {
        let bad = FIXTURE.replace("vault = \"0x", "vault = \"0xzz");
        assert!(Config::parse(&bad).unwrap_err().contains("vault"));
        let key = FIXTURE.replace("public_key = \"0x03", "public_key = \"0x07");
        assert!(Config::parse(&key).unwrap_err().contains("lane.public_key"));
    }

    const VAULT: &str = "\n[vault_login]\naddress = \"https://vault.example:8200\"\nmount = \"bezent\"\nrole = \"bezent-evm-endpoint-iam\"\nsts_region = \"us-east-2\"\nserver_id = \"vault.example\"\n";
    const STORE: &str = "\n[store]\nhost = \"db.example\"\ndatabase = \"bezent\"\nlogin = { vault = { path = \"secret/bezent/evm-endpoint\", field = \"db_login\" } }\npassword = { vault = { path = \"secret/bezent/evm-endpoint\", field = \"db_password\" } }\n";

    fn secret(field: &str) -> String {
        format!("{{ vault = {{ path = \"secret/bezent/evm-endpoint\", field = \"{field}\" }} }}")
    }

    /// The fixture with every secret taken from Vault.
    fn from_vault() -> String {
        FIXTURE
            .replace(
                "database_url_file = \"/run/secrets/bezent-evm-polygon-db\"\n",
                "",
            )
            .replace(
                "url_file = \"/run/secrets/bezent-evm-polygon-rpc-a\"",
                &format!("url_secret = {}", secret("rpc_a")),
            )
            .replace(
                "url_file = \"/run/secrets/bezent-evm-polygon-rpc-b\"",
                &format!("url_secret = {}", secret("rpc_b")),
            )
            .replace(
                "token_file = \"/run/secrets/bezent-evm-polygon-signer-token\"",
                &format!("token = {}", secret("signer_token")),
            )
            + STORE
            + VAULT
    }

    #[test]
    fn req_ky_1_the_store_providers_and_signer_token_can_come_from_vault() {
        let c = Config::parse(&from_vault()).unwrap();
        assert!(c.store.is_some() && c.vault_login.is_some());
        assert!(c.providers.iter().all(|p| p.url_secret.is_some()));
        c.check().unwrap();
    }

    #[test]
    fn req_ky_1_one_source_each_and_a_login_table_for_a_vault_secret() {
        let dec12 = |text: &str| Config::parse(text).unwrap_err();
        assert!(dec12(&from_vault().replace(VAULT, "")).contains("vault_login"));
        assert!(dec12(&(FIXTURE.to_owned() + STORE + VAULT)).contains("database_url_file"));
        assert!(
            dec12(&from_vault().replace("token = {", "token_file = \"t\"\ntoken = {"))
                .contains("signer.token")
        );
        let two_sources = from_vault().replacen("url_secret", "url = \"ws://x\"\nurl_secret", 1);
        assert!(dec12(&two_sources).contains("exactly one of url_file, url and url_secret"));
        // The same reference twice is the same provider.
        let same = from_vault().replace(&secret("rpc_b"), &secret("rpc_a"));
        assert!(dec12(&same).contains("independent"));
    }

    /// `DEC-12` on values: two references to Vault that name one URL are not two providers.
    #[tokio::test]
    async fn dec_12_two_references_that_resolve_to_one_url_are_refused_at_start() {
        let dir = std::env::temp_dir().join(format!("bze-evm-secrets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a", "b"] {
            std::fs::write(dir.join(name), "https://rpc.example/KEY\n").unwrap();
        }
        std::fs::write(dir.join("c"), "https://other.example/KEY\n").unwrap();
        let mut c = Config::parse(FIXTURE).unwrap();
        let resolver = c.resolver().unwrap();
        let file = |n: &str| Some(SecretRef::File(dir.join(n)));
        for (p, n) in c.providers.iter_mut().zip(["a", "b"]) {
            p.url_file = None;
            p.url_secret = file(n);
        }
        let err = c.provider_urls(&resolver).await.unwrap_err();
        assert!(err.contains("DEC-12") && !err.contains("KEY"), "{err}");
        c.providers[1].url_secret = file("c");
        assert_eq!(
            c.provider_urls(&resolver).await.unwrap(),
            ["https://rpc.example/KEY", "https://other.example/KEY"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
