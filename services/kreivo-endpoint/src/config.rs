// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The endpoint's configuration, read from a TOML file named on the command line.
//!
//! No key and no secret lives here (`REQ-AR-3`, `REQ-KY-1`): the store's URL and the signer link's
//! token are read from files the configuration names, and the delivery key is named by the signer's
//! key id and its public key only. `Debug` prints nothing secret.
//!
//! A secret can also be a `SecretRef` (issue #64): a file (`{ file = "…" }`) or a field in Vault
//! (`{ vault = { path, field } }`, which needs the `[vault]` table). The store is then a `[store]`
//! (host, and login and password as `SecretRef`s; the URL is built in the process), the signer
//! token is `signer.token`, and an archive node whose URL carries a key is `archive_rpc_secrets`.
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
//! contract_code_hash = "0x…"           # the code hash of the build deployed there (AC-I4.1); see below
//! start_block = 7000000                # the block the contract was instantiated in
//! database_url_file = "/run/secrets/bezent-kreivo-endpoint-db"
//! archive_rpc = ["wss://archive.example"] # history sources, tried in order (issue #49); see below
//! max_walk = 200000                    # on a first start, at least the blocks from start_block to the head
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
//!
//! The live Kreivo contract's code hash is
//! `0xe89943fbbed677f2e33fde16e4877c84bf339354d05c796accf97614e2fb24ad`: that is the value to configure.
//! It equals the hash of the public CI build (PR #40, run 39369032023, artifact `bezent-contract`).
//! A wrong value stops the endpoint at start, which is the intended failure.
//!
//! **History** (issue #49): the light client reads only what its peers still hold, and they prune
//! state after 256 blocks, so blocks more than `light_client_depth` behind the finalized head are
//! read from `archive_rpc`, archive nodes' WebSocket JSON-RPC, each answer proven against the light
//! client's finalized chain ([`crate::chain::archive`]). At least one source is required to run;
//! each must serve proven state at `start_block` and agree with the light client, or the endpoint
//! refuses to start. `max_walk` bounds how far one look walks back by parent hash: on a first start
//! it must cover the blocks from `start_block` to the finalized head, or the backlog is a gap (the walk
//! is kept in rungs, so an error late in it, or a restart, does not lose it). `archive_max_response_bytes`
//! caps one answer of a source (16 MiB by default).

use std::fmt;
use std::path::{Path, PathBuf};

use bezent_vault::{Resolver, SecretRef, StoreSource, VaultConfig, Zeroizing};
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
    ///
    /// Exactly one of `database_url_file` and `[store]`.
    #[serde(default)]
    pub database_url_file: Option<PathBuf>,
    /// The store as a host and a login and password from files or Vault, in place of
    /// `database_url_file`; the login is the same.
    #[serde(default)]
    pub store: Option<StoreSource>,
    /// Where Vault is and how to log in to it, when any secret is read from it.
    #[serde(default)]
    pub vault: Option<VaultConfig>,
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
    /// History sources (issue #49): archive nodes' WebSocket JSON-RPC URLs (`ws://` or `wss://`),
    /// tried in order, the one that answered last first. Nothing they serve is trusted: headers
    /// are checked by hash, state by proof, bodies by extrinsics root. Required to run, not for
    /// `--check`.
    #[serde(default)]
    pub archive_rpc: Vec<String>,
    /// History sources whose URL carries a key, so it is a `SecretRef` (a file, or a field in
    /// Vault) and not written here: tried after `archive_rpc`, and held to the same rules.
    #[serde(default)]
    pub archive_rpc_secrets: Vec<SecretRef>,
    /// Blocks at most this far behind the finalized head are read through the light client, older
    /// ones from `archive_rpc`. Below 256, the state its peers keep.
    #[serde(default = "default_light_client_depth")]
    pub light_client_depth: u64,
    /// How long one request to a history source may take, in milliseconds.
    #[serde(default = "default_archive_timeout_ms")]
    pub archive_timeout_ms: u64,
    /// The most bytes one answer of a history source may be (issue #49): a longer one is a failed
    /// request. 16 MiB by default; a block's events are far smaller.
    #[serde(default = "default_archive_max_response_bytes")]
    pub archive_max_response_bytes: u32,
    /// The delivery key's signing lane.
    pub lane: LaneConfig,
    /// The signer link.
    pub signer: SignerConfig,
    /// The liveness file the running endpoint rewrites after each pass that reached the head of the
    /// network it observes and `bezent-kreivo-endpoint health` reads (issue #61). Left out,
    /// `/tmp/bezent-kreivo-endpoint.health`; a task whose root file system is read-only names a writable path here.
    #[serde(default)]
    pub health_file: Option<PathBuf>,
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
    /// A file holding the endpoint's token for the link. Exactly one of `token_file` and `token`.
    #[serde(default)]
    pub token_file: Option<PathBuf>,
    /// The same token as a `SecretRef`: a file, or a field in Vault.
    #[serde(default)]
    pub token: Option<SecretRef>,
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
fn default_light_client_depth() -> u64 {
    128
}
fn default_archive_timeout_ms() -> u64 {
    30_000
}
fn default_archive_max_response_bytes() -> u32 {
    16 * 1024 * 1024
}

/// A history source's URL for logs: scheme, host and port only. Credentials (`user:pass@`), the
/// path (some providers put the API key there) and the query are left out.
pub fn source_name(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
            let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
            format!("{scheme}://{host}")
        }
        None => "an invalid URL".to_owned(),
    }
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
            config.database_url_file.as_mut(),
            config.signer.token_file.as_mut(),
            Some(&mut config.signer.socket),
        ]
        .into_iter()
        .flatten()
        {
            if p.is_relative() {
                *p = base.join(&*p);
            }
        }
        if let Some(p) = config.health_file.as_mut()
            && p.is_relative()
        {
            *p = base.join(&*p);
        }
        if let Some(store) = config.store.as_mut() {
            store.rebase(base);
        }
        if let Some(token) = config.signer.token.as_mut() {
            token.rebase(base);
        }
        for secret in &mut config.archive_rpc_secrets {
            secret.rebase(base);
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
        if !(1..256).contains(&self.light_client_depth) || self.archive_timeout_ms == 0 {
            return Err("light_client_depth must be 1 to 255, archive_timeout_ms positive".into());
        }
        if self.archive_max_response_bytes < 1024 {
            return Err("archive_max_response_bytes must be at least 1024".into());
        }
        if let Some(u) = self
            .archive_rpc
            .iter()
            .find(|u| !(u.starts_with("ws://") || u.starts_with("wss://")))
        {
            return Err(format!(
                "archive_rpc {} is not a ws:// or wss:// URL",
                source_name(u)
            ));
        }
        self.check_secrets()
    }

    /// The checks of the secrets' sources, which touch no network: one source for the store and
    /// for the signer token, a valid `[vault]` table, and one wherever a secret comes from Vault.
    fn check_secrets(&self) -> Result<(), String> {
        if self.database_url_file.is_some() == self.store.is_some() {
            return Err("set exactly one of database_url_file and [store]".into());
        }
        if self.signer.token_file.is_some() == self.signer.token.is_some() {
            return Err("set exactly one of signer.token_file and signer.token".into());
        }
        if let Some(vault) = &self.vault {
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
            || self.archive_rpc_secrets.iter().any(SecretRef::is_vault);
        if from_vault && self.vault.is_none() {
            return Err("a secret is read from Vault but there is no [vault] table".into());
        }
        Ok(())
    }

    /// The resolver for this configuration's `[vault]` table. Contacts nothing until a secret is
    /// read from Vault.
    pub fn resolver(&self) -> Result<Resolver, String> {
        Resolver::new(self.vault.as_ref()).map_err(|e| e.to_string())
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

    /// Every history source: `archive_rpc`, then `archive_rpc_secrets` as they resolve. Each must be
    /// a `ws://` or `wss://` URL; the error names the source by scheme and host only.
    pub async fn archive_sources(&self, resolver: &Resolver) -> Result<Vec<String>, String> {
        let mut sources = self.archive_rpc.clone();
        for secret in &self.archive_rpc_secrets {
            let url = resolver.resolve(secret).await.map_err(|e| e.to_string())?;
            if !(url.starts_with("ws://") || url.starts_with("wss://")) {
                return Err(format!(
                    "archive_rpc_secrets {} is not a ws:// or wss:// URL",
                    source_name(&url)
                ));
            }
            sources.push(url.to_string());
        }
        Ok(sources)
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
            .field("store", &self.store)
            .field("vault", &self.vault)
            .field("archive_rpc_secrets", &self.archive_rpc_secrets)
            .field(
                "archive_rpc",
                &self
                    .archive_rpc
                    .iter()
                    .map(|u| source_name(u))
                    .collect::<Vec<_>>(),
            )
            .field("light_client_depth", &self.light_client_depth)
            .field("lane", &self.lane)
            .field("signer", &self.signer)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Config {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/endpoint.toml");
        Config::from_file(&path).unwrap()
    }

    #[test]
    fn issue_49_history_sources_are_websocket_urls() {
        let mut c = sample();
        assert_eq!(c.archive_rpc, vec!["wss://rpc.kreivo.bloque.sh".to_owned()]);
        c.archive_rpc
            .push("https://user:secret@archive.example/?key=k".into());
        let err = c.check().unwrap_err();
        assert!(err.contains("https://archive.example"), "{err}");
        assert!(!err.contains("secret") && !err.contains("key=k"), "{err}");
        let debug = format!("{c:?}");
        assert!(
            !debug.contains("user:secret") && !debug.contains("key=k"),
            "{debug}"
        );
        c.archive_rpc.pop();
        c.light_client_depth = 256;
        assert!(c.check().is_err());
    }

    #[test]
    fn issue_49_a_source_is_named_by_its_host_alone() {
        for (url, name) in [
            ("wss://rpc.example", "wss://rpc.example"),
            ("wss://rpc.example:443/", "wss://rpc.example:443"),
            // A key in the path, in the userinfo, in the query, in a fragment, and with an `@`
            // after the path.
            ("wss://rpc.example/v2/KEY123", "wss://rpc.example"),
            (
                "wss://user:KEY123@rpc.example:9944/ws",
                "wss://rpc.example:9944",
            ),
            (
                "wss://rpc.example/ws?apikey=KEY123#KEY123",
                "wss://rpc.example",
            ),
            ("wss://rpc.example?apikey=KEY123", "wss://rpc.example"),
            ("wss://rpc.example/path/KEY@123", "wss://rpc.example"),
            ("not a url KEY123", "an invalid URL"),
        ] {
            let got = source_name(url);
            assert_eq!(got, name, "{url}");
            assert!(!got.contains("KEY123"), "{got}");
        }
    }

    #[test]
    fn issue_49_the_response_cap_is_16_mib_and_bounded_below() {
        let mut c = sample();
        assert_eq!(c.archive_max_response_bytes, 16 * 1024 * 1024);
        c.archive_max_response_bytes = 10;
        assert!(
            c.check()
                .unwrap_err()
                .contains("archive_max_response_bytes")
        );
    }

    const VAULT: &str = "\n[vault]\naddress = \"https://vault.example:8200\"\nmount = \"bezent\"\nrole = \"bezent-kreivo-endpoint-iam\"\nsts_region = \"us-east-2\"\nserver_id = \"vault.example\"\n";
    const STORE: &str = "\n[store]\nhost = \"db.example\"\ndatabase = \"bezent\"\nlogin = { vault = { path = \"secret/bezent/kreivo-endpoint\", field = \"db_login\" } }\npassword = { vault = { path = \"secret/bezent/kreivo-endpoint\", field = \"db_password\" } }\n";

    /// The fixture with `edit` applied to its text, parsed and checked.
    fn parse(edit: impl FnOnce(String) -> String) -> Result<Config, String> {
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/endpoint.toml"),
        )
        .unwrap();
        let config: Config = toml::from_str(&edit(text)).map_err(|e| e.to_string())?;
        config.check()?;
        Ok(config)
    }

    fn with_vault_sources(text: String) -> String {
        text.replace("database_url_file = \"/run/secrets/bezent-kreivo-endpoint-db\"\n", "")
            .replace(
                "token_file = \"/run/secrets/bezent-kreivo-endpoint-signer-token\"",
                "token = { vault = { path = \"secret/bezent/kreivo-endpoint\", field = \"signer_token\" } }",
            )
            + STORE
            + VAULT
    }

    #[test]
    fn req_ky_1_the_store_signer_token_and_history_sources_can_come_from_vault() {
        let c = parse(|t| {
            with_vault_sources(t).replace(
                "light_client_depth",
                "archive_rpc_secrets = [{ vault = { path = \"secret/bezent/kreivo-endpoint\", field = \"archive_rpc\" } }]\nlight_client_depth",
            )
        })
        .unwrap();
        assert!(c.database_url_file.is_none() && c.store.is_some());
        assert_eq!(c.archive_rpc_secrets.len(), 1);
        let debug = format!("{c:?}");
        assert!(debug.contains("archive_rpc") && !debug.contains("hunter2"));
    }

    #[test]
    fn req_ky_1_one_source_each_and_a_vault_table_for_a_vault_secret() {
        // The existing file forms are the fixture itself.
        assert!(parse(|t| t).is_ok());
        assert!(
            parse(|t| t.replace(
                "database_url_file = \"/run/secrets/bezent-kreivo-endpoint-db\"\n",
                ""
            ))
            .is_err()
        );
        assert!(parse(|t| t + STORE + VAULT).is_err(), "both store forms");
        assert!(
            parse(|t| with_vault_sources(t).replace(VAULT, "")).is_err(),
            "no [vault]"
        );
        assert!(
            parse(|t| with_vault_sources(t).replace("token = {", "token_file = \"t\"\ntoken = {"))
                .is_err(),
            "both token forms"
        );
    }

    #[tokio::test]
    async fn req_ky_1_history_sources_resolve_and_are_held_to_websocket_urls() {
        let dir = std::env::temp_dir().join(format!("bze-secrets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("good"), "wss://archive.example/ws/KEY123\n").unwrap();
        std::fs::write(dir.join("bad"), "https://user:KEY123@archive.example/\n").unwrap();
        let file = |name: &str| SecretRef::File(dir.join(name));
        let mut c = sample();
        let resolver = c.resolver().unwrap();
        c.archive_rpc_secrets = vec![file("good")];
        let sources = c.archive_sources(&resolver).await.unwrap();
        assert_eq!(
            sources,
            [
                "wss://rpc.kreivo.bloque.sh",
                "wss://archive.example/ws/KEY123"
            ]
        );
        c.archive_rpc_secrets = vec![file("bad")];
        let err = c.archive_sources(&resolver).await.unwrap_err();
        assert!(
            err.contains("https://archive.example") && !err.contains("KEY123"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
