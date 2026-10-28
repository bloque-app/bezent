// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The endpoint's configuration (`T-009-01`): one instance per EVM network (PLAN `DEC-8`).
//!
//! A TOML file. No secret is in it: the database URL, the providers' URLs (which may carry a
//! key, `F-014`) and the signer token are read from the files it names (`REQ-KY-1`: never from
//! the environment). What the store declares (the reserve, the admitted tokens) is read from the
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
    /// A file holding this endpoint's token.
    pub token_file: PathBuf,
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
    /// A file holding the database URL (the endpoint's login).
    pub database_url_file: PathBuf,
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
            resolve(&mut c.database_url_file);
            resolve(&mut c.signer.token_file);
            if let Some(p) = c.health_file.as_mut() {
                resolve(p);
            }
            for p in &mut c.providers {
                if let Some(f) = &mut p.url_file {
                    resolve(f);
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
            match (&p.url_file, &p.url) {
                (Some(_), None) | (None, Some(_)) => {}
                _ => {
                    return Err(format!(
                        "provider {}: exactly one of url_file and url",
                        p.name
                    ));
                }
            }
        }
        let (a, b) = (&self.providers[0], &self.providers[1]);
        if a.name == b.name
            || (a.url_file.is_some() && a.url_file == b.url_file)
            || (a.url.is_some() && a.url == b.url)
        {
            return Err(
                "DEC-12: the two providers must be independent: distinct names and URLs".into(),
            );
        }
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

    /// A provider's URL, read from its file if it has one.
    pub fn provider_url(p: &Provider) -> Result<String, String> {
        match (&p.url, &p.url_file) {
            (Some(u), _) => Ok(u.clone()),
            (None, Some(f)) => std::fs::read_to_string(f)
                .map(|s| s.trim().to_owned())
                .map_err(|e| format!("provider {}: cannot read {}: {e}", p.name, f.display())),
            (None, None) => Err(format!("provider {}: no URL", p.name)),
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
}
