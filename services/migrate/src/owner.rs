// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The migration owner's login from a configuration file (`--config`, issue #64): the store's host,
//! and the owner's login and password as `SecretRef`s (a file, or a field in Vault with `[vault]`),
//! from which the URL is built in the process. Nothing here prints a value.
//!
//! ```toml
//! [vault]
//! address = "https://vault.example:8200"
//! mount = "bezent"
//! role = "bezent-migrate-iam"
//! sts_region = "us-east-2"
//! server_id = "vault.example"
//!
//! [store]
//! host = "db.example"
//! database = "bezent"
//! login = { vault = { path = "secret/bezent/migrate", field = "db_login" } }
//! password = { vault = { path = "secret/bezent/migrate", field = "db_password" } }
//! sslrootcert = "/etc/bezent/rds-ca.pem"
//! ```

use std::path::Path;

use bezent_vault::{Resolver, StoreSource, VaultConfig, Zeroizing};
use serde::Deserialize;

/// The owner's configuration.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerConfig {
    /// Where Vault is and how to log in to it, when a secret is read from it.
    #[serde(default)]
    pub vault: Option<VaultConfig>,
    /// The store, with the owner's login and password.
    pub store: StoreSource,
}

impl OwnerConfig {
    /// Reads and checks the file at `path`. Relative paths in it are the file's directory's.
    /// Touches no network.
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {}", path.display(), e.kind()))?;
        let mut config: Self = toml::from_str(&text)
            .map_err(|e| format!("invalid configuration {}: {}", path.display(), e.message()))?;
        config.store.rebase(path.parent().unwrap_or(Path::new(".")));
        config.check()?;
        Ok(config)
    }

    fn check(&self) -> Result<(), String> {
        if let Some(vault) = &self.vault {
            vault.check().map_err(|e| e.to_string())?;
        }
        self.store.check().map_err(|e| e.to_string())?;
        if (self.store.login.is_vault() || self.store.password.is_vault()) && self.vault.is_none() {
            return Err("a secret is read from Vault but there is no [vault] table".to_owned());
        }
        Ok(())
    }

    /// The owner's database URL, read now.
    pub async fn url(&self) -> Result<Zeroizing<String>, String> {
        Resolver::new(self.vault.as_ref())
            .map_err(|e| e.to_string())?
            .store_url(&self.store)
            .await
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VAULT: &str = "[vault]\naddress = \"https://vault.example:8200\"\nmount = \"bezent\"\nrole = \"bezent-migrate-iam\"\nsts_region = \"us-east-2\"\nserver_id = \"vault.example\"\n";
    const STORE: &str = "[store]\nhost = \"db.example\"\ndatabase = \"bezent\"\nlogin = { vault = { path = \"secret/bezent/migrate\", field = \"db_login\" } }\npassword = { vault = { path = \"secret/bezent/migrate\", field = \"db_password\" } }\n";

    fn read(tag: &str, text: &str) -> Result<OwnerConfig, String> {
        let dir = std::env::temp_dir().join(format!("bzm-owner-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("migrate.toml");
        std::fs::write(&path, text).unwrap();
        let result = OwnerConfig::from_file(&path);
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    #[test]
    fn req_ky_1_the_owner_can_come_from_vault_and_needs_the_table_for_it() {
        assert!(read("ok", &format!("{VAULT}{STORE}")).is_ok());
        assert!(read("novault", STORE).unwrap_err().contains("[vault]"));
        assert!(read("nostore", VAULT).is_err());
        assert!(
            read(
                "http",
                &format!("{}{STORE}", VAULT.replace("https", "http"))
            )
            .is_err()
        );
        assert!(read("extra", &format!("{VAULT}{STORE}[other]\n")).is_err());
    }

    #[tokio::test]
    async fn req_ky_1_the_owners_url_is_built_from_files_in_the_process() {
        let dir = std::env::temp_dir().join(format!("bzm-owner-url-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("l"), "owner\n").unwrap();
        std::fs::write(dir.join("p"), "hunter2/x\n").unwrap();
        let path = dir.join("migrate.toml");
        std::fs::write(
            &path,
            "[store]\nhost = \"db.example\"\ndatabase = \"bezent\"\nlogin = { file = \"l\" }\npassword = { file = \"p\" }\nsslrootcert = \"ca.pem\"\n",
        )
        .unwrap();
        let config = OwnerConfig::from_file(&path).unwrap();
        let url = config.url().await.unwrap();
        assert!(bezent_store::tls::connect_options(&url).is_ok());
        assert!(url.starts_with(
            "postgres://owner:hunter2%2Fx@db.example:5432/bezent?sslmode=verify-full&sslrootcert="
        ));
        // The configuration prints references, never values.
        assert!(!format!("{config:?}").contains("hunter2"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
