// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The endpoint's configuration, read from a TOML file named on the command line.
//!
//! No key and no secret lives here (`REQ-AR-3`, `REQ-KY-1`): the store's URL and the signer link's
//! token are read from files the configuration names, and the delivery key is named by the signer's
//! key id and its public key only. `Debug` prints nothing secret.
//!
//! ```toml
//! network = "kreivo"
//! genesis = "0xc710…5b01"              # the declared network's genesis hash (AC-I4.1)
//! chain_spec = "chain-specs/kreivo.json"
//! relay_chain_spec = "/etc/bezent/ksmcc3.json"
//! relay_genesis = "0xb0a8…dafe"        # the relay chain's genesis hash, pinned with Kreivo's (AC-I4.1)
//! relay_checkpoint = "0x…"             # BLAKE2b-256 of the relay spec's checkpoint, if it has one
//! para_id = 2281                       # Kreivo's parachain id, as its chain spec must say
//! contract = "0x…"                     # the bezent contract's account
//! contract_code_hash = "0x…"           # the code hash of the build deployed there (AC-I4.1)
//! start_block = 7000000                # the block the contract was instantiated in
//! database_url_file = "/run/secrets/bezent-kreivo-endpoint-db"
//!
//! [lane]                               # the delivery key's signing lane (REQ-EO-4)
//! key = "kreivo-mint"
//! scheme = "sr25519"
//! public_key = "0x…"
//!
//! [signer]
//! socket = "/run/bezent-signer/signer.sock"
//! uid = 1001
//! token_file = "/run/secrets/bezent-kreivo-endpoint-signer-token"
//! ```

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::kreivo::{AccountId32, Scheme};
use crate::network::RelayPin;

/// The endpoint's configuration.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The network id, as the store declares it.
    pub network: String,
    /// The declared network's genesis hash, `0x` hex (`AC-I4.1`).
    pub genesis: String,
    /// The pinned chain spec (a light chain spec: its genesis is its state root's).
    pub chain_spec: PathBuf,
    /// The relay chain's spec, for a parachain.
    #[serde(default)]
    pub relay_chain_spec: Option<PathBuf>,
    /// The relay chain's genesis hash, `0x` hex, pinned with the network's (`AC-I4.1`): required
    /// with `relay_chain_spec`.
    #[serde(default)]
    pub relay_genesis: Option<String>,
    /// The relay chain spec's checkpoint (`lightSyncState`, `checkpoint`), pinned as the BLAKE2b-256
    /// of its canonical JSON, `0x` hex (`AC-I4.1`; review S-c): smoldot trusts the checkpoint's
    /// finality, so it is pinned with the genesis. Required when the relay spec has one, refused
    /// when it has none (the genesis is then the trust root).
    #[serde(default)]
    pub relay_checkpoint: Option<String>,
    /// The parachain id the chain spec must name (`AC-I4.1`; review S-d): smoldot follows the
    /// relay chain's heads of this id. Required with `relay_chain_spec`.
    #[serde(default)]
    pub para_id: Option<u32>,
    /// The bezent contract's account, `0x` hex.
    pub contract: String,
    /// The code hash of the contract build deployed at `contract`, `0x` hex (issue #24;
    /// `AC-I4.1`): the endpoint reads it from the chain at start-up and refuses to start on any
    /// other, or on none. Required, like the genesis: a deployment pins the build it was
    /// audited against (the contract job of CI prints it), and an upgraded contract is a change
    /// of configuration.
    pub contract_code_hash: String,
    /// The block the contract was instantiated in: observation starts there.
    pub start_block: u64,
    /// The contract's first event sequence number (zero at instantiation).
    #[serde(default)]
    pub first_seq: u64,
    /// A file holding the store's URL, for a login that is a member of `bezent_kreivo_endpoint`
    /// only (which inherits `bezent_endpoint`, itself a member of `bezent_service`): only it may
    /// record proofs (review S1), and it records the issuing network's observations and nothing
    /// else (migration 0034, issue #30).
    pub database_url_file: PathBuf,
    /// The largest number of store connections.
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    /// How often the endpoint looks at the finalized head, in milliseconds.
    #[serde(default = "default_poll_ms")]
    pub poll_ms: u64,
    /// The most blocks one read covers.
    #[serde(default = "default_max_range")]
    pub max_range: u64,
    /// The most blocks the endpoint walks back by parent hash in one look.
    #[serde(default = "default_max_walk")]
    pub max_walk: u64,
    /// The delivery key's signing lane.
    pub lane: LaneConfig,
    /// The signer link.
    pub signer: SignerConfig,
}

/// The delivery key's signing lane (`REQ-EO-4`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneConfig {
    /// The signer's id for the key.
    pub key: String,
    /// Its scheme: `sr25519` or `ecdsa`.
    pub scheme: String,
    /// Its public key, `0x` hex.
    pub public_key: String,
}

/// The signer link (`F-005`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignerConfig {
    /// The signer's socket.
    pub socket: PathBuf,
    /// The user id the signer runs as.
    pub uid: u32,
    /// A file holding the endpoint's token for the link.
    pub token_file: PathBuf,
}

fn default_max_connections() -> u32 {
    4
}
fn default_poll_ms() -> u64 {
    6_000
}
fn default_max_range() -> u64 {
    256
}
fn default_max_walk() -> u64 {
    4_096
}

/// `0x` hex of an exact length.
pub fn hex_bytes<const N: usize>(s: &str, what: &str) -> Result<[u8; N], String> {
    let digits = s
        .strip_prefix("0x")
        .ok_or_else(|| format!("{what} is not 0x hex"))?;
    hex::decode(digits)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| format!("{what} is not {N} bytes of hex"))
}

impl Config {
    /// Reads the configuration from a TOML file. Relative paths in it are relative to the file.
    pub fn from_file(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let mut config: Config = toml::from_str(&text)
            .map_err(|e| format!("invalid configuration {}: {e}", path.display()))?;
        let base = path.parent().unwrap_or(Path::new("."));
        for p in [
            Some(&mut config.chain_spec),
            config.relay_chain_spec.as_mut(),
            Some(&mut config.database_url_file),
            Some(&mut config.signer.token_file),
            Some(&mut config.signer.socket),
        ]
        .into_iter()
        .flatten()
        {
            if p.is_relative() {
                *p = base.join(&*p);
            }
        }
        config.check()?;
        Ok(config)
    }

    /// Refuses a configuration with a malformed field.
    pub fn check(&self) -> Result<(), String> {
        if self.network.is_empty() {
            return Err("network is empty".into());
        }
        self.declared_genesis()?;
        self.declared_relay()?;
        self.contract_account()?;
        self.declared_code_hash()?;
        self.lane_account()?;
        if self.max_range == 0 || self.max_walk == 0 {
            return Err("max_range and max_walk must be positive".into());
        }
        Ok(())
    }

    /// The declared genesis hash.
    pub fn declared_genesis(&self) -> Result<[u8; 32], String> {
        hex_bytes(&self.genesis, "genesis")
    }

    /// What the configuration pins of the relay chain: its genesis hash and `para_id`, required
    /// with a relay chain spec, and its checkpoint, if any. Each is refused without one.
    pub fn declared_relay(&self) -> Result<RelayPin, String> {
        let checkpoint = self
            .relay_checkpoint
            .as_deref()
            .map(|c| hex_bytes(c, "relay_checkpoint"))
            .transpose()?;
        match (&self.relay_chain_spec, &self.relay_genesis, self.para_id) {
            (None, None, None) if checkpoint.is_none() => Ok(RelayPin::default()),
            (Some(_), Some(g), Some(para_id)) => Ok(RelayPin {
                genesis: Some(hex_bytes(g, "relay_genesis")?),
                checkpoint,
                para_id: Some(para_id),
            }),
            (Some(_), None, _) => Err("AC-I4.1: relay_chain_spec needs relay_genesis".into()),
            (Some(_), _, None) => Err("AC-I4.1: relay_chain_spec needs para_id".into()),
            (None, ..) => Err(
                "relay_genesis, relay_checkpoint and para_id are declared without relay_chain_spec"
                    .into(),
            ),
        }
    }

    /// The contract's account.
    pub fn contract_account(&self) -> Result<AccountId32, String> {
        hex_bytes(&self.contract, "contract")
    }

    /// The declared code hash of the contract.
    pub fn declared_code_hash(&self) -> Result<[u8; 32], String> {
        hex_bytes(&self.contract_code_hash, "contract_code_hash")
    }

    /// The lane's scheme, public key and account.
    pub fn lane_account(&self) -> Result<(Scheme, Vec<u8>, AccountId32), String> {
        let scheme = Scheme::parse(&self.lane.scheme)
            .ok_or_else(|| format!("lane.scheme {} is not sr25519 or ecdsa", self.lane.scheme))?;
        let public = self
            .lane
            .public_key
            .strip_prefix("0x")
            .and_then(|d| hex::decode(d).ok())
            .ok_or("lane.public_key is not 0x hex")?;
        let account = scheme
            .account(&public)
            .ok_or("lane.public_key does not have its scheme's length")?;
        Ok((scheme, public, account))
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("network", &self.network)
            .field("genesis", &self.genesis)
            .field("chain_spec", &self.chain_spec)
            .field("relay_chain_spec", &self.relay_chain_spec)
            .field("relay_genesis", &self.relay_genesis)
            .field("relay_checkpoint", &self.relay_checkpoint)
            .field("para_id", &self.para_id)
            .field("contract", &self.contract)
            .field("contract_code_hash", &self.contract_code_hash)
            .field("start_block", &self.start_block)
            .field("first_seq", &self.first_seq)
            .field("database_url_file", &self.database_url_file)
            .field("lane", &self.lane)
            .field("signer", &self.signer)
            .finish_non_exhaustive()
    }
}
