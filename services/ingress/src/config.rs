// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The ingress configuration, read from a TOML file named on the command line.
//!
//! The database URL may carry the service login's password, so the configuration never prints it:
//! its `Debug` leaves it out. No key lives here (`REQ-AR-3`, `REQ-KY-1`): the webhook key, which
//! moves no value, is read from the file `[webhooks] key_file` names, or from Vault (`key`).
//!
//! Secrets can stay out of the file and out of the image (issue #64): `[store]` names the host, and
//! the login and password as `SecretRef`s (a file, or a field in Vault with `[vault]`), and the URL
//! is built in the process. `database_url` and `key_file` read as before.

use bezent_vault::{Resolver, SecretRef, StoreSource, VaultConfig, Zeroizing};
use serde::Deserialize;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The ingress configuration.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The address the HTTP surface listens on.
    pub listen: SocketAddr,
    /// The store, as a login that is a member of `bezent_ingress` and of nothing else, checked at
    /// start-up (`F-001` NOTES, S1; #46).
    /// Exactly one of `database_url` and `[store]`.
    #[serde(default)]
    pub database_url: Option<String>,
    /// The store as a host and a login and password, in place of `database_url`.
    #[serde(default)]
    pub store: Option<StoreSource>,
    /// Where Vault is and how to log in to it, when any secret is read from it.
    #[serde(default)]
    pub vault: Option<VaultConfig>,
    /// The largest number of store connections.
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    /// How long a request waits for a store connection before it is refused with `ERR-Overloaded`
    /// (`REQ-LM-4`), in milliseconds.
    #[serde(default = "default_acquire_timeout_ms")]
    pub acquire_timeout_ms: u64,
    /// The largest number of requests taken in at once; one more is refused with `ERR-Overloaded`
    /// (`REQ-LM-4`).
    #[serde(default = "default_max_in_flight")]
    pub max_in_flight: usize,
    /// The largest number of log positions one read of the sequenced log covers (`REQ-ST-4`).
    #[serde(default = "default_log_page")]
    pub log_page: i64,
    /// Webhooks to requesters (`REQ-ST-5`); none without this section.
    #[serde(default)]
    pub webhooks: Option<WebhooksConfig>,
}

/// The `[webhooks]` section.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhooksConfig {
    /// The store as the dispatcher's own login: a member of `bezent_webhooks` and of nothing
    /// else, never the ingress's login (PR #55 review M2). Only that role opens the sealed secrets
    /// and URLs and writes deliveries. Never printed.
    /// Exactly one of `database_url` and `store`.
    #[serde(default)]
    pub database_url: Option<String>,
    /// The dispatcher's login as a host and a login and password, in place of `database_url`.
    #[serde(default)]
    pub store: Option<StoreSource>,
    /// A file holding the webhook key, 64 hexadecimal characters, which seals the requesters'
    /// signing secrets and callback URLs at rest. Losing it loses them: requesters then take a new
    /// secret and register again. Exactly one of `key_file` and `key`.
    #[serde(default)]
    pub key_file: Option<PathBuf>,
    /// The same key as a `SecretRef`: a file, or a field in Vault.
    #[serde(default)]
    pub key: Option<SecretRef>,
    /// The test harness's hosts (names or IP literals as a URL writes them): they may take plain
    /// `http` and resolve to loopback, private, unique-local or shared addresses, never to
    /// link-local or metadata ones (PR #55 review M1). Only on a test store: the ingress refuses to
    /// start with one on a store that declares a production network.
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    /// bezent's own service addresses, never posted to: IP addresses or networks
    /// (`10.0.4.0/24`), with or without a port (`127.0.0.1:8080`), or host names with or without a
    /// port (`core.internal`).
    #[serde(default)]
    pub deny: Vec<String>,
    /// Attempts before a delivery is dead-lettered.
    #[serde(default = "default_max_attempts")]
    pub max_attempts: i32,
    /// The first retry's delay, in milliseconds; each retry doubles it.
    #[serde(default = "default_backoff_base_ms")]
    pub backoff_base_ms: u64,
    /// The longest delay between attempts, in seconds.
    #[serde(default = "default_backoff_cap_seconds")]
    pub backoff_cap_seconds: u64,
    /// How long one attempt waits for an answer, in milliseconds.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

impl WebhooksConfig {
    /// The dispatcher's store URL: `database_url`, or built from `store`. Never printed.
    pub async fn store_url(&self, resolver: &Resolver) -> Result<Zeroizing<String>, String> {
        match (&self.database_url, &self.store) {
            (Some(url), _) => Ok(Zeroizing::new(url.clone())),
            (None, Some(store)) => resolver.store_url(store).await.map_err(|e| e.to_string()),
            (None, None) => Err("[webhooks]: no store is configured".to_owned()),
        }
    }

    /// The sealing key: from the file (which must be private to its owner), or from Vault.
    pub async fn sealing_key(
        &self,
        resolver: &Resolver,
    ) -> Result<crate::webhooks::SealingKey, String> {
        match (&self.key_file, &self.key) {
            (Some(file), _) => crate::webhooks::SealingKey::from_file(file),
            (None, Some(SecretRef::File(file))) => crate::webhooks::SealingKey::from_file(file),
            (None, Some(key)) => {
                let text = resolver.resolve(key).await.map_err(|e| e.to_string())?;
                crate::webhooks::SealingKey::from_hex(&text)
            }
            (None, None) => Err("[webhooks]: no key is configured".to_owned()),
        }
    }

    /// The dispatcher's settings, under `key`; an unreadable `deny` entry is an error.
    pub fn settings(
        &self,
        key: crate::webhooks::SealingKey,
    ) -> Result<crate::webhooks::Webhooks, String> {
        let mut w = crate::webhooks::Webhooks::new(key);
        w.allow_hosts = self.allow_hosts.clone();
        w.deny = self
            .deny
            .iter()
            .map(|d| d.parse().map_err(|e| format!("[webhooks] deny: {e}")))
            .collect::<Result<_, _>>()?;
        w.max_attempts = self.max_attempts.max(1);
        w.backoff_base = Duration::from_millis(self.backoff_base_ms);
        w.backoff_cap = Duration::from_secs(self.backoff_cap_seconds);
        w.timeout = Duration::from_millis(self.timeout_ms);
        Ok(w)
    }

    /// Refuses an allowlist on a store that declares `production` networks (PR #55 review M1):
    /// a configuration line alone never opens plain `http` or internal addresses in production
    /// ([`bezent_store::environment`]).
    pub fn confirm_environment(&self, production: &[String]) -> Result<(), String> {
        if !self.allow_hosts.is_empty() && !production.is_empty() {
            return Err(format!(
                "[webhooks] allow_hosts is for test stores only; this store declares {}",
                production.join(", ")
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for WebhooksConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebhooksConfig")
            .field(
                "database_url",
                &self.database_url.as_ref().map(|_| "<redacted>"),
            )
            .field("store", &self.store)
            .field("key_file", &self.key_file)
            .field("key", &self.key)
            .field("allow_hosts", &self.allow_hosts)
            .field("deny", &self.deny)
            .field("max_attempts", &self.max_attempts)
            .field("backoff_base_ms", &self.backoff_base_ms)
            .field("backoff_cap_seconds", &self.backoff_cap_seconds)
            .field("timeout_ms", &self.timeout_ms)
            .finish()
    }
}

fn default_max_attempts() -> i32 {
    12
}
fn default_backoff_base_ms() -> u64 {
    2000
}
fn default_backoff_cap_seconds() -> u64 {
    3600
}
fn default_timeout_ms() -> u64 {
    10_000
}

fn default_max_connections() -> u32 {
    16
}
fn default_acquire_timeout_ms() -> u64 {
    250
}
fn default_max_in_flight() -> usize {
    256
}
fn default_log_page() -> i64 {
    500
}

impl Config {
    /// Reads the configuration from a TOML file.
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let mut config: Config = toml::from_str(&text)
            .map_err(|e| format!("invalid configuration {}: {e}", path.display()))?;
        config
            .check_secrets(path.parent().unwrap_or(Path::new(".")))
            .map_err(|e| format!("invalid configuration {}: {e}", path.display()))?;
        Ok(config)
    }

    /// The checks of the secrets' sources, which touch no network: one source for each URL and
    /// key, a valid `[vault]` table, and one wherever a secret comes from Vault.
    fn check_secrets(&mut self, base: &Path) -> Result<(), String> {
        if self.database_url.is_some() == self.store.is_some() {
            return Err("set exactly one of database_url and [store]".to_owned());
        }
        let mut vault_refs = false;
        let mut stores = vec![("[store]", self.store.as_mut())];
        if let Some(w) = self.webhooks.as_mut() {
            if w.database_url.is_some() == w.store.is_some() {
                return Err("[webhooks]: set exactly one of database_url and store".to_owned());
            }
            if w.key_file.is_some() == w.key.is_some() {
                return Err("[webhooks]: set exactly one of key_file and key".to_owned());
            }
            if let Some(key) = w.key.as_mut() {
                key.rebase(base);
                vault_refs |= key.is_vault();
            }
            if let Some(p) = w.key_file.as_mut()
                && p.is_relative()
            {
                *p = base.join(&*p);
            }
            stores.push(("[webhooks] store", w.store.as_mut()));
        }
        for (name, store) in stores {
            if let Some(store) = store {
                store.rebase(base);
                store.check().map_err(|e| format!("{name}: {e}"))?;
                vault_refs |= store.login.is_vault() || store.password.is_vault();
            }
        }
        if let Some(vault) = &self.vault {
            vault.check().map_err(|e| e.to_string())?;
        }
        if vault_refs && self.vault.is_none() {
            return Err("a secret is read from Vault but there is no [vault] table".to_owned());
        }
        Ok(())
    }

    /// The resolver for this configuration's `[vault]` table. Contacts nothing until a secret is
    /// read from Vault.
    pub fn resolver(&self) -> Result<Resolver, String> {
        Resolver::new(self.vault.as_ref()).map_err(|e| e.to_string())
    }

    /// The store's URL: `database_url`, or built from `[store]`. Never printed.
    pub async fn store_url(&self, resolver: &Resolver) -> Result<Zeroizing<String>, String> {
        match (&self.database_url, &self.store) {
            (Some(url), _) => Ok(Zeroizing::new(url.clone())),
            (None, Some(store)) => resolver.store_url(store).await.map_err(|e| e.to_string()),
            (None, None) => Err("no store is configured".to_owned()),
        }
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("listen", &self.listen)
            .field(
                "database_url",
                &self.database_url.as_ref().map(|_| "<redacted>"),
            )
            .field("store", &self.store)
            .field("vault", &self.vault)
            .field("max_connections", &self.max_connections)
            .field("acquire_timeout_ms", &self.acquire_timeout_ms)
            .field("max_in_flight", &self.max_in_flight)
            .field("log_page", &self.log_page)
            .field("webhooks", &self.webhooks)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req_pn_1_configuration_never_prints_the_store_credentials() {
        let config: Config = toml::from_str(
            "listen = \"127.0.0.1:8080\"\ndatabase_url = \"postgres://svc:hunter2@db/bezent\"\n",
        )
        .unwrap();
        assert!(!format!("{config:?}").contains("hunter2"));
        assert_eq!(config.max_in_flight, 256);

        // Nor the dispatcher's own login (PR #55 review M2).
        let config: Config = toml::from_str(
            "listen = \"127.0.0.1:8080\"\ndatabase_url = \"postgres://svc:hunter2@db/bezent\"\n\
             [webhooks]\ndatabase_url = \"postgres://hooks:swordfish@db/bezent\"\n\
             key_file = \"/run/bezent/webhook.key\"\n",
        )
        .unwrap();
        let printed = format!("{config:?}");
        assert!(!printed.contains("hunter2") && !printed.contains("swordfish"));
    }

    #[test]
    fn m1_an_allowlist_only_on_a_test_store_and_the_deny_list_must_parse() {
        let config: Config = toml::from_str(
            "listen = \"127.0.0.1:8080\"\ndatabase_url = \"postgres://db/bezent\"\n\
             [webhooks]\ndatabase_url = \"postgres://db/bezent\"\nkey_file = \"/k\"\n\
             allow_hosts = [\"harness\"]\ndeny = [\"10.0.4.0/24\", \"core.internal:8080\"]\n",
        )
        .unwrap();
        let w = config.webhooks.unwrap();
        assert!(w.confirm_environment(&[]).is_ok());
        let err = w.confirm_environment(&["polygon".into()]).unwrap_err();
        assert!(err.contains("test stores only"), "{err}");
        let settings = w.settings(crate::webhooks::SealingKey::random()).unwrap();
        assert_eq!(settings.deny.len(), 2);
        let mut bad = w.clone();
        bad.deny = vec!["10.0.0.0/40".into()];
        assert!(bad.settings(crate::webhooks::SealingKey::random()).is_err());
        let mut none = w;
        none.allow_hosts.clear();
        assert!(none.confirm_environment(&["polygon".into()]).is_ok());
    }

    const VAULT: &str = "[vault]\naddress = \"https://vault.example:8200\"\nmount = \"bezent\"\nrole = \"bezent-ingress-iam\"\nsts_region = \"us-east-2\"\nserver_id = \"vault.example\"\n";

    fn store(path: &str) -> String {
        format!(
            "host = \"db.example\"\ndatabase = \"bezent\"\nlogin = {{ vault = {{ path = \"{path}\", field = \"db_login\" }} }}\npassword = {{ vault = {{ path = \"{path}\", field = \"db_password\" }} }}\n"
        )
    }

    /// Writes `text` as an ingress.toml in a directory of its own and reads it.
    fn read(tag: &str, text: &str) -> Result<Config, String> {
        let dir = std::env::temp_dir().join(format!("bzi-secrets-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ingress.toml");
        std::fs::write(&path, text).unwrap();
        let result = Config::from_file(&path);
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    #[test]
    fn req_ky_1_the_stores_and_the_webhook_key_can_come_from_vault() {
        let text = format!(
            "listen = \"127.0.0.1:8080\"\n[store]\n{}{VAULT}[webhooks]\nkey = {{ vault = {{ path = \"secret/bezent/ingress\", field = \"webhook_key\" }} }}\n[webhooks.store]\n{}",
            store("secret/bezent/ingress"),
            store("secret/bezent/ingress-webhooks")
        );
        let config = read("ok", &text).unwrap();
        let w = config.webhooks.as_ref().unwrap();
        assert!(config.database_url.is_none() && w.database_url.is_none());
        assert!(w.key.is_some() && w.key_file.is_none());
        let printed = format!("{config:?}");
        assert!(printed.contains("webhook_key") && !printed.contains("hunter2"));
    }

    #[test]
    fn req_ky_1_one_source_for_each_secret_and_a_vault_table_for_a_vault_secret() {
        let url = "listen = \"127.0.0.1:8080\"\ndatabase_url = \"postgres://svc@db/bezent\"\n";
        let hooks_url = "[webhooks]\ndatabase_url = \"postgres://hooks@db/bezent\"\n";
        assert!(read("old", &format!("{url}{hooks_url}key_file = \"k\"\n")).is_ok());
        assert!(read("nokey", &format!("{url}{hooks_url}")).is_err());
        assert!(
            read(
                "twokeys",
                &format!("{url}{hooks_url}key_file = \"k\"\nkey = {{ file = \"k\" }}\n")
            )
            .is_err()
        );
        assert!(read("nostore", "listen = \"127.0.0.1:8080\"\n").is_err());
        assert!(
            read(
                "both",
                &format!("{url}[store]\n{}{VAULT}", store("secret/x"))
            )
            .is_err()
        );
        assert!(
            read(
                "novault",
                &format!(
                    "listen = \"127.0.0.1:8080\"\n[store]\n{}",
                    store("secret/x")
                )
            )
            .is_err()
        );
        let hooks_both = format!(
            "[webhooks]\ndatabase_url = \"postgres://h@db/bezent\"\nkey_file = \"k\"\n[webhooks.store]\n{}",
            store("secret/y")
        );
        assert!(read("hooksboth", &format!("{url}{VAULT}{hooks_both}")).is_err());
    }
}
