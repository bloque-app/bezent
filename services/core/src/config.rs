// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The core's configuration, read from a TOML file named on the command line.
//!
//! The database URL may carry the core login's password, so the configuration never prints it. The
//! password can also stay out of the file: `[store]` names the host, and the login and password as
//! `SecretRef`s (a file or a field in Vault, issue #64), and the URL is built in the process.
//! No key lives here, and the core holds none (`REQ-AR-3`, `REQ-KY-1`).

use bezent_vault::{Resolver, SecretRef, StoreSource, VaultConfig, Zeroizing};
use serde::Deserialize;
use std::fmt;
use std::path::{Path, PathBuf};

/// The core configuration.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The store, as the core's login: a member of `bezent_core` and of nothing else, checked at
    /// start-up (F-001 NOTES S1, F-005 review S5, #46). One pool, one login (issue #27).
    /// Exactly one of `database_url` and `[store]`.
    #[serde(default)]
    pub database_url: Option<String>,
    /// The store as a host and a login and password from files or Vault, in place of
    /// `database_url`. The login is the core's, as above.
    #[serde(default)]
    pub store: Option<StoreSource>,
    /// Where Vault is and how to log in to it, when any secret is read from it.
    #[serde(default)]
    pub vault: Option<VaultConfig>,
    /// The largest number of store connections.
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    /// Seconds between cycles.
    #[serde(default = "default_cycle_seconds")]
    pub cycle_seconds: u64,
    /// Seconds after a failed dry run before a new order is recorded.
    #[serde(default = "default_retry_seconds")]
    pub retry_preparation_seconds: i64,
    /// The deadline, in seconds, of a transfer whose route declares none (a refund).
    #[serde(default = "default_deadline_seconds")]
    pub default_deadline_seconds: i64,
    /// Seconds a transfer may hold `CND-PayoutWindow`, on the destination network's clock, before
    /// a human is paged, once and then daily (PR #52 review S-3). Two daily allowance periods by
    /// default.
    #[serde(default = "default_payout_window_alert_seconds")]
    pub payout_window_alert_seconds: i64,
    /// The deployment's environment: `test` or `production`. Left out, it is production: only an
    /// explicit `test` allows the test-only settings below (PR #50 review M2), and only on a store
    /// whose every network is a test network: the core confirms it against the store at start and
    /// every cycle ([`Config::confirm_environment`]; PR #50 re-check M-1).
    #[serde(default)]
    pub environment: Environment,
    /// The daily statement (`REQ-PR-1`: "MUST publish, at least daily"; PLAN `DEC-16`; issue #42).
    /// Required. Only a test deployment may leave the statement out, and only by saying so:
    /// `statement = "disabled-test-only"` with `environment = "test"` (PR #50 review M2).
    pub statement: Option<Statement>,
    /// The liveness file the running core rewrites after each healthy cycle and `bezent-core
    /// health` reads (issue #61). Left out, `/tmp/bezent-core.health`; a task whose root file
    /// system is read-only names a writable path here.
    #[serde(default)]
    pub health_file: Option<PathBuf>,
}

/// The deployment's environment (PR #50 review M2).
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    /// A test deployment: test-only settings are allowed.
    Test,
    /// Production, the default.
    #[default]
    Production,
}

/// The one value that leaves the statement out, in a test deployment only.
pub const STATEMENT_DISABLED: &str = "disabled-test-only";

/// The public half of the published test fixture's statement key
/// (`services/signer/fixtures/test-keys.json`, id `statement`): its private half is public, so
/// outside a test deployment it is refused (PR #50 review, notes).
pub const TEST_STATEMENT_KEY: &str =
    "0x66d5b675e8fddf84c70c76d1815d6a5d531bbc96de157b591489be0cdbe5f615";

/// The `[statement]` setting: where statements are signed and kept, or, in a test deployment
/// only, the explicit [`STATEMENT_DISABLED`].
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
// One value, read once at start: boxing the table would only change how the tests spell it.
#[allow(clippy::large_enum_variant)]
pub enum Statement {
    /// `statement = "disabled-test-only"`.
    Disabled(String),
    /// The `[statement]` table.
    Configured(StatementConfig),
}

impl Statement {
    /// The statement's configuration, unless it is disabled.
    pub fn configured(&self) -> Option<&StatementConfig> {
        match self {
            Self::Configured(c) => Some(c),
            Self::Disabled(_) => None,
        }
    }
}

/// Where the daily statement is signed and kept (`REQ-PR-1`, `REQ-PR-2`; PLAN `DEC-16`).
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatementConfig {
    /// The signer's socket: the statement key lives only behind the signer (`REQ-AR-3`).
    pub signer_socket: PathBuf,
    /// The user id the signer runs as; the core refuses a socket served by anyone else.
    pub signer_uid: u32,
    /// A file holding the core's token for the signer link (never the environment, `REQ-KY-1`).
    pub signer_token_file: Option<PathBuf>,
    /// The same token as a `SecretRef` (a file, or a field in Vault), in place of
    /// `signer_token_file`.
    #[serde(default)]
    pub signer_token: Option<SecretRef>,
    /// The statement key's ed25519 public key, `0x` and 64 hex digits: what every signature the
    /// signer returns is verified with before anything is published.
    pub public_key: String,
    /// The append-only directory statements are written to: the file sink, for tests. Refused
    /// outside `environment = "test"`; exactly one of `directory` and `s3` is set (issue #58).
    pub directory: Option<PathBuf>,
    /// The object-locked bucket statements are written to, by the core itself (`DEC-16`; issue
    /// #58). Written `[statement.s3]`.
    pub s3: Option<S3Config>,
}

/// How an object-locked bucket retains what is written to it.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub enum ObjectLockMode {
    /// Retention that only an identity with `s3:BypassGovernanceRetention` can shorten.
    #[serde(rename = "GOVERNANCE")]
    Governance,
    /// Retention that no one can shorten or remove until it expires.
    #[serde(rename = "COMPLIANCE")]
    Compliance,
    /// No retention header is sent: the bucket's own default applies, if any. Test only.
    #[serde(rename = "off")]
    Off,
}

/// The default retention of a statement object, in days.
pub const DEFAULT_RETENTION_DAYS: u32 = 365;

/// The most days an object may be retained for (100 years): a typo guard.
pub const MAX_RETENTION_DAYS: u32 = 36_500;

/// The `[statement.s3]` setting: the bucket the core writes the daily statements to (`DEC-16`;
/// issue #58). Credentials are not here: they come from the task role (`REQ-KY-1`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3Config {
    /// The bucket, created with versioning and Object Lock enabled.
    pub bucket: String,
    /// A key prefix inside the bucket, without leading or trailing slash; may be empty.
    #[serde(default)]
    pub prefix: String,
    /// The bucket's region.
    pub region: String,
    /// A KMS key (id, ARN or alias) for SSE-KMS; left out, the bucket's default encryption
    /// applies. A statement is public: anonymous readers cannot read SSE-KMS objects.
    pub kms_key_id: Option<String>,
    /// Retention mode; `off` only in a test deployment. Required: which mode production runs is
    /// the owner's to decide (issue #58), so there is no default.
    pub object_lock_mode: ObjectLockMode,
    /// Days each object is retained for; left out, [`DEFAULT_RETENTION_DAYS`].
    pub retention_days: Option<u32>,
}

impl S3Config {
    /// The retention in days: the setting, or the default.
    pub fn retention_days(&self) -> u32 {
        self.retention_days.unwrap_or(DEFAULT_RETENTION_DAYS)
    }

    /// Why this setting is refused, if it is (`test` is whether `environment = "test"`).
    pub fn validate(&self, test: bool) -> Result<(), String> {
        let b = &self.bucket;
        let ok = (3..=63).contains(&b.len())
            && b.bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'.')
            && b.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
            && b.ends_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit());
        if !ok {
            return Err("statement.s3.bucket is not a valid bucket name".into());
        }
        if !self.prefix.is_empty()
            && self.prefix.split('/').any(|p| {
                p.is_empty()
                    || p == "."
                    || p == ".."
                    || !p
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-_.=".contains(&c))
            })
        {
            return Err("statement.s3.prefix must be plain path segments (letters, digits, - _ . =) without a leading, trailing or doubled slash".into());
        }
        if self.region.is_empty()
            || !self
                .region
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            return Err("statement.s3.region must be a region name such as us-east-2".into());
        }
        if self.kms_key_id.as_deref().is_some_and(str::is_empty) {
            return Err("statement.s3.kms_key_id is empty; leave it out for the bucket's default encryption".into());
        }
        match self.object_lock_mode {
            ObjectLockMode::Off if !test => {
                return Err("statement.s3.object_lock_mode = \"off\" is refused outside environment = \"test\": statements are kept under object lock (REQ-PR-2, DEC-16)".into());
            }
            ObjectLockMode::Off => {}
            mode => {
                let days = self.retention_days();
                if days == 0 || days > MAX_RETENTION_DAYS {
                    return Err(format!(
                        "statement.s3.retention_days must be 1 to {MAX_RETENTION_DAYS} with object_lock_mode = {mode:?}; got {days} (a lock without a retention keeps nothing, REQ-PR-2)"
                    ));
                }
            }
        }
        Ok(())
    }
}

impl StatementConfig {
    /// The core's token for the signer link: from `signer_token_file` or `signer_token`.
    pub async fn token(&self, resolver: &Resolver) -> Result<Zeroizing<String>, String> {
        match (&self.signer_token_file, &self.signer_token) {
            (Some(file), _) => bezent_vault::read_file(file),
            (None, Some(token)) => resolver.resolve(token).await,
            (None, None) => return Err("no signer token is configured".to_owned()),
        }
        .map_err(|e| e.to_string())
    }

    /// The public key's bytes, if it is 32 bytes of hex.
    pub fn public_key_bytes(&self) -> Option<Vec<u8>> {
        let digits = self.public_key.strip_prefix("0x")?;
        hex::decode(digits).ok().filter(|k| k.len() == 32)
    }
}

impl fmt::Debug for StatementConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StatementConfig")
            .field("signer_socket", &self.signer_socket)
            .field("signer_uid", &self.signer_uid)
            .field("signer_token_file", &self.signer_token_file)
            .field("signer_token", &self.signer_token)
            .field("public_key", &self.public_key)
            .field("directory", &self.directory)
            .field("s3", &self.s3)
            .finish()
    }
}

fn default_max_connections() -> u32 {
    4
}
fn default_cycle_seconds() -> u64 {
    5
}
fn default_retry_seconds() -> i64 {
    60
}
fn default_deadline_seconds() -> i64 {
    86_400
}
fn default_payout_window_alert_seconds() -> i64 {
    2 * 86_400
}

impl Config {
    /// Reads the configuration from a TOML file.
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let mut config: Config = toml::from_str(&text)
            .map_err(|e| format!("invalid configuration {}: {e}", path.display()))?;
        let test = config.environment == Environment::Test;
        // REQ-PR-1 (PR #50 review M2): a deployment publishes statements; only a test one may
        // say it does not.
        let statement = match config.statement.as_mut() {
            None => {
                return Err(format!(
                    "invalid configuration {}: [statement] is required (REQ-PR-1: a statement at least daily); only a test deployment may set statement = \"{STATEMENT_DISABLED}\" with environment = \"test\"",
                    path.display()
                ));
            }
            Some(Statement::Disabled(v)) if v != STATEMENT_DISABLED => {
                return Err(format!(
                    "invalid configuration {}: statement must be the [statement] table, or \"{STATEMENT_DISABLED}\" in a test deployment",
                    path.display()
                ));
            }
            Some(Statement::Disabled(_)) if !test => {
                return Err(format!(
                    "invalid configuration {}: statement = \"{STATEMENT_DISABLED}\" is refused outside environment = \"test\" (REQ-PR-1)",
                    path.display()
                ));
            }
            Some(Statement::Disabled(_)) => None,
            Some(Statement::Configured(statement)) => Some(statement),
        };
        if let Some(statement) = statement {
            if statement.public_key_bytes().is_none() {
                return Err(format!(
                    "invalid configuration {}: statement.public_key must be 0x and 64 hex digits, the statement key's ed25519 public key",
                    path.display()
                ));
            }
            if !test
                && statement
                    .public_key
                    .eq_ignore_ascii_case(TEST_STATEMENT_KEY)
            {
                return Err(format!(
                    "invalid configuration {}: statement.public_key is the published test fixture's statement key, whose private half is public; refused outside environment = \"test\" (REQ-PR-1, REQ-KY-6)",
                    path.display()
                ));
            }
            match (&statement.directory, &statement.s3) {
                (Some(_), Some(_)) | (None, None) => {
                    return Err(format!(
                        "invalid configuration {}: set exactly one of statement.directory (tests) and [statement.s3] (REQ-PR-2, DEC-16)",
                        path.display()
                    ));
                }
                (Some(_), None) if !test => {
                    return Err(format!(
                        "invalid configuration {}: statement.directory is refused outside environment = \"test\": production statements go to the object-locked bucket, [statement.s3] (REQ-PR-2, DEC-16)",
                        path.display()
                    ));
                }
                (None, Some(s3)) => s3
                    .validate(test)
                    .map_err(|e| format!("invalid configuration {}: {e}", path.display()))?,
                (Some(_), None) => {}
            }
            let base = path.parent().unwrap_or(Path::new("."));
            for p in [&mut statement.signer_socket]
                .into_iter()
                .chain(statement.signer_token_file.as_mut())
                .chain(statement.directory.as_mut())
            {
                if p.is_relative() {
                    *p = base.join(&*p);
                }
            }
            if let Some(token) = statement.signer_token.as_mut() {
                token.rebase(base);
            }
            if statement.signer_token_file.is_some() == statement.signer_token.is_some() {
                return Err(format!(
                    "invalid configuration {}: set exactly one of statement.signer_token_file and statement.signer_token",
                    path.display()
                ));
            }
        }
        config
            .check_secrets(path)
            .map_err(|e| format!("invalid configuration {}: {e}", path.display()))?;
        // A relative liveness file is the configuration directory's, as the other paths are.
        if let Some(p) = config.health_file.as_mut()
            && p.is_relative()
        {
            *p = path.parent().unwrap_or(Path::new(".")).join(&*p);
        }
        if config.cycle_seconds == 0
            || config.retry_preparation_seconds < 0
            || config.default_deadline_seconds <= 0
            || config.payout_window_alert_seconds <= 0
        {
            return Err(format!(
                "invalid configuration {}: cycle_seconds, default_deadline_seconds and payout_window_alert_seconds must be positive, retry_preparation_seconds not negative",
                path.display()
            ));
        }
        Ok(config)
    }

    /// The checks of the secrets' sources, which touch no network: exactly one of `database_url`
    /// and `[store]`, a valid `[vault]` table, and one wherever a secret comes from Vault.
    fn check_secrets(&mut self, path: &Path) -> Result<(), String> {
        let base = path.parent().unwrap_or(Path::new("."));
        if self.database_url.is_some() == self.store.is_some() {
            return Err("set exactly one of database_url and [store]".to_owned());
        }
        if let Some(vault) = &self.vault {
            vault.check().map_err(|e| e.to_string())?;
        }
        if let Some(store) = self.store.as_mut() {
            store.rebase(base);
            store.check().map_err(|e| e.to_string())?;
        }
        let from_vault = self
            .store
            .as_ref()
            .is_some_and(|s| s.login.is_vault() || s.password.is_vault())
            || self
                .statement
                .as_ref()
                .and_then(Statement::configured)
                .and_then(|s| s.signer_token.as_ref())
                .is_some_and(SecretRef::is_vault);
        if from_vault && self.vault.is_none() {
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

    /// Whether this configuration relies on the store being a test one: `environment = "test"`
    /// (and what only it allows), or the published test statement key.
    pub fn needs_test_store(&self) -> bool {
        self.environment == Environment::Test
            || self
                .statement
                .as_ref()
                .and_then(Statement::configured)
                .is_some_and(|s| s.public_key.eq_ignore_ascii_case(TEST_STATEMENT_KEY))
    }

    /// Confirms the configuration against the store it runs on (PR #50 re-check M-1; `REQ-KY-6`,
    /// `REQ-PR-1`). A line of the file alone never makes a deployment a test one: the store is a
    /// test store only when every network it declares is a test network
    /// ([`bezent_store::environment`], the sidecar's rule), and `production` names those it
    /// declares that are not. `environment = "test"` (and so a disabled statement) is refused on a
    /// store with any production network, and so is the published test statement key, whatever
    /// the file says.
    pub fn confirm_environment(&self, production: &[String]) -> Result<(), String> {
        if production.is_empty() {
            return Ok(());
        }
        let networks = production.join(", ");
        if self.environment == Environment::Test {
            return Err(format!(
                "environment = \"test\" refused: the store declares production networks ({networks}); a test deployment runs on test networks only (REQ-KY-6, REQ-PR-1)"
            ));
        }
        if self.needs_test_store() {
            return Err(format!(
                "statement.public_key is the published test fixture's statement key, whose private half is public; refused on a store that declares production networks ({networks}) (REQ-PR-1, REQ-KY-6)"
            ));
        }
        Ok(())
    }

    /// The engine's settings.
    pub fn settings(&self) -> crate::Settings {
        crate::Settings {
            retry_preparation_after: chrono::Duration::seconds(self.retry_preparation_seconds),
            default_deadline: chrono::Duration::seconds(self.default_deadline_seconds),
            payout_window_alert_after: chrono::Duration::seconds(self.payout_window_alert_seconds),
        }
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field(
                "database_url",
                &self.database_url.as_ref().map(|_| "<redacted>"),
            )
            .field("store", &self.store)
            .field("vault", &self.vault)
            .field("max_connections", &self.max_connections)
            .field("cycle_seconds", &self.cycle_seconds)
            .field("retry_preparation_seconds", &self.retry_preparation_seconds)
            .field("default_deadline_seconds", &self.default_deadline_seconds)
            .field(
                "payout_window_alert_seconds",
                &self.payout_window_alert_seconds,
            )
            .field("environment", &self.environment)
            .field("statement", &self.statement)
            .field("health_file", &self.health_file)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req_ky_1_configuration_never_prints_the_store_credentials() {
        let config: Config =
            toml::from_str("database_url = \"postgres://core:hunter2@db/bezent\"\n").unwrap();
        assert!(!format!("{config:?}").contains("hunter2"));
        assert_eq!(config.cycle_seconds, 5);
        assert_eq!(config.environment, Environment::Production);
    }

    /// PR #50 review M2: a deployment whose configuration drops `[statement]` does not run for
    /// weeks publishing nothing: outside a test deployment the core refuses to start without it.
    /// A test deployment may leave it out only by saying so (and `main` logs it loudly).
    #[test]
    fn req_pr_1_the_core_refuses_to_start_without_a_statement_outside_test() {
        let dir = std::env::temp_dir().join(format!("bzc-config-m2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("core.toml");
        let load = |text: &str| {
            std::fs::write(
                &path,
                format!("database_url = \"postgres://core@db/bezent\"\n{text}"),
            )
            .unwrap();
            Config::from_file(&path)
        };
        let table = |key: &str| {
            format!(
                "[statement]\nsigner_socket = \"s.sock\"\nsigner_uid = 1\n\
                 signer_token_file = \"t\"\npublic_key = \"{key}\"\n\
                 [statement.s3]\nbucket = \"bezent-statements\"\nregion = \"us-east-2\"\n\
                 object_lock_mode = \"GOVERNANCE\"\n"
            )
        };
        let production = ["", "environment = \"production\"\n"];
        // No [statement]: refused, in production (the default) and in test alike.
        for env in production.iter().chain(&["environment = \"test\"\n"]) {
            let err = load(env).unwrap_err();
            assert!(err.contains("[statement] is required"), "{env}: {err}");
        }
        // The explicit test-only switch: refused outside test, accepted in test.
        for env in production {
            let err = load(&format!("{env}statement = \"disabled-test-only\"\n")).unwrap_err();
            assert!(err.contains("refused outside"), "{err}");
        }
        let test = load("environment = \"test\"\nstatement = \"disabled-test-only\"\n").unwrap();
        assert!(test.statement.unwrap().configured().is_none());
        // Any other value is no switch.
        assert!(load("environment = \"test\"\nstatement = \"off\"\n").is_err());
        // A statement key is accepted anywhere; the published test key only in test.
        let real = format!("0x{}", "ab".repeat(32));
        let ok = load(&table(&real)).unwrap();
        assert!(ok.statement.unwrap().configured().is_some());
        let err = load(&table(TEST_STATEMENT_KEY)).unwrap_err();
        assert!(err.contains("published test fixture"), "{err}");
        assert!(
            load(&format!(
                "environment = \"test\"\n{}",
                table(TEST_STATEMENT_KEY)
            ))
            .is_ok()
        );
        std::fs::remove_dir_all(dir).ok();
    }

    /// PR #50 re-check M-1: test mode does not come from the file alone. `environment = "test"`,
    /// a disabled statement and the published test statement key are each refused on a store
    /// that declares any production network, whatever the file says; a store of test networks
    /// only (or none yet) takes them.
    #[test]
    fn m_1_req_ky_6_test_mode_is_confirmed_by_the_stores_networks_not_the_file() {
        let parse = |text: &str| -> Config {
            toml::from_str(&format!(
                "database_url = \"postgres://core@db/bezent\"\n{text}"
            ))
            .unwrap()
        };
        let table = |key: &str| {
            format!(
                "[statement]\nsigner_socket = \"s.sock\"\nsigner_uid = 1\n\
                 signer_token_file = \"t\"\npublic_key = \"{key}\"\n\
                 [statement.s3]\nbucket = \"bezent-statements\"\nregion = \"us-east-2\"\n\
                 object_lock_mode = \"GOVERNANCE\"\n"
            )
        };
        let test_disabled = parse("environment = \"test\"\nstatement = \"disabled-test-only\"\n");
        let test_key = parse(&format!(
            "environment = \"test\"\n{}",
            table(TEST_STATEMENT_KEY)
        ));
        let test_real_key = parse(&format!(
            "environment = \"test\"\n{}",
            table(&format!("0x{}", "cd".repeat(32)))
        ));
        let production = parse(&table(&format!("0x{}", "ab".repeat(32))));
        // A test key under a production line still needs a test store (built here directly: the
        // file check refuses it before, this is the second net).
        let mut production_test_key = production.clone();
        production_test_key.statement = Some(Statement::Configured(StatementConfig {
            public_key: TEST_STATEMENT_KEY.into(),
            ..production
                .statement
                .as_ref()
                .unwrap()
                .configured()
                .unwrap()
                .clone()
        }));

        // A store of test networks only, or of none yet: every configuration runs.
        for config in [
            &test_disabled,
            &test_key,
            &test_real_key,
            &production,
            &production_test_key,
        ] {
            assert!(config.confirm_environment(&[]).is_ok(), "{config:?}");
        }
        // A store with one production network: everything test-only is refused.
        let mixed = vec!["polygon".to_owned()];
        for config in [&test_disabled, &test_key, &test_real_key] {
            let err = config.confirm_environment(&mixed).unwrap_err();
            assert!(err.contains("environment = \"test\" refused"), "{err}");
            assert!(err.contains("polygon"), "{err}");
        }
        let err = production_test_key.confirm_environment(&mixed).unwrap_err();
        assert!(err.contains("published test fixture"), "{err}");
        assert!(production_test_key.needs_test_store());
        // A production configuration with its own key runs on a production store.
        assert!(production.confirm_environment(&mixed).is_ok());
        assert!(!production.needs_test_store());
        // Which networks are test networks is the store crate's rule, shared with the sidecar.
        assert!(bezent_store::environment::is_test_network("polygon-amoy"));
        assert!(!bezent_store::environment::is_test_network("polygon"));
    }

    #[test]
    fn req_pr_1_the_statement_key_is_named_by_its_public_key_only() {
        let dir = std::env::temp_dir().join(format!("bzc-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("core.toml");
        let write = |key: &str| {
            std::fs::write(
                &path,
                format!(
                    "database_url = \"postgres://core@db/bezent\"\n\
                     environment = \"test\"\n\
                     [statement]\n\
                     signer_socket = \"signer.sock\"\n\
                     signer_uid = 1001\n\
                     signer_token_file = \"core-token\"\n\
                     public_key = \"{key}\"\n\
                     directory = \"statements\"\n"
                ),
            )
            .unwrap();
        };
        write(&format!("0x{}", "ab".repeat(32)));
        let config = Config::from_file(&path).unwrap();
        let s = config.statement.unwrap().configured().unwrap().clone();
        assert_eq!(s.public_key_bytes().unwrap(), vec![0xab; 32]);
        // Relative paths are the configuration's directory's.
        assert_eq!(s.directory, Some(dir.join("statements")));
        // A key of another length, or not hex, is refused: there is no secret to name here.
        for bad in ["0xabcd", "ab".repeat(32).as_str(), "0xzz"] {
            write(bad);
            assert!(Config::from_file(&path).is_err(), "{bad}");
        }
        std::fs::remove_dir_all(dir).ok();
    }

    /// Issue #58, `REQ-PR-2`: where statements go. Production takes the bucket only, with a
    /// retention; the directory sink, a lock mode of `off` and a missing sink are refused.
    #[test]
    fn req_pr_2_statements_go_to_the_object_locked_bucket_in_production() {
        let dir = std::env::temp_dir().join(format!("bzc-config-s3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("core.toml");
        let key = format!("0x{}", "ab".repeat(32));
        let load = |env: &str, sink: &str| {
            std::fs::write(
                &path,
                format!(
                    "database_url = \"postgres://core@db/bezent\"\n{env}\
                     [statement]\nsigner_socket = \"s.sock\"\nsigner_uid = 1\n\
                     signer_token_file = \"t\"\npublic_key = \"{key}\"\n{sink}"
                ),
            )
            .unwrap();
            Config::from_file(&path)
        };
        let s3 = |extra: &str| {
            let mode = if extra.contains("object_lock_mode") {
                ""
            } else {
                "object_lock_mode = \"GOVERNANCE\"\n"
            };
            format!(
                "[statement.s3]\nbucket = \"bezent-statements\"\nprefix = \"statements\"\n\
                 region = \"us-east-2\"\n{mode}{extra}"
            )
        };
        // The mode has no default: leaving it out is refused.
        let bare = "[statement.s3]\nbucket = \"bezent-statements\"\nregion = \"us-east-2\"\n";
        assert!(load("", bare).is_err());
        let err =
            toml::from_str::<S3Config>("bucket = \"bezent-statements\"\nregion = \"us-east-2\"\n")
                .unwrap_err()
                .to_string();
        assert!(err.contains("object_lock_mode"), "{err}");
        // The retention defaults to 365 days; COMPLIANCE with a retention is accepted.
        let ok = load("", &s3("")).unwrap();
        let cfg = ok
            .statement
            .unwrap()
            .configured()
            .unwrap()
            .s3
            .clone()
            .unwrap();
        assert_eq!(cfg.object_lock_mode, ObjectLockMode::Governance);
        assert_eq!(cfg.retention_days(), 365);
        assert!(
            load(
                "",
                &s3("object_lock_mode = \"COMPLIANCE\"\nretention_days = 400\n")
            )
            .is_ok()
        );
        // COMPLIANCE or GOVERNANCE without a retention keeps nothing: refused.
        for mode in ["COMPLIANCE", "GOVERNANCE"] {
            let err = load(
                "",
                &s3(&format!(
                    "object_lock_mode = \"{mode}\"\nretention_days = 0\n"
                )),
            )
            .unwrap_err();
            assert!(err.contains("retention_days"), "{err}");
        }
        // No lock outside a test deployment; allowed in one.
        let err = load("", &s3("object_lock_mode = \"off\"\n")).unwrap_err();
        assert!(err.contains("refused outside"), "{err}");
        assert!(
            load(
                "environment = \"test\"\n",
                &s3("object_lock_mode = \"off\"\n")
            )
            .is_ok()
        );
        // The directory sink is for tests; exactly one sink is set.
        let err = load("", "directory = \"d\"\n").unwrap_err();
        assert!(err.contains("statement.directory is refused"), "{err}");
        assert!(load("environment = \"test\"\n", "directory = \"d\"\n").is_ok());
        let err = load("environment = \"test\"\n", "").unwrap_err();
        assert!(err.contains("exactly one"), "{err}");
        let both = format!("directory = \"d\"\n{}", s3(""));
        let err = load("environment = \"test\"\n", &both).unwrap_err();
        assert!(err.contains("exactly one"), "{err}");
        // A malformed bucket, prefix or region is refused; so is an unknown key.
        for bad in [
            "bucket = \"BAD_Bucket\"",
            "prefix = \"/lead\"",
            "prefix = \"a//b\"",
            "prefix = \"../x\"",
            "region = \"\"",
            "kms_key_id = \"\"",
            "access_key_id = \"AKIA\"",
        ] {
            let sink = format!(
                "[statement.s3]\nbucket = \"bezent-statements\"\nregion = \"us-east-2\"\n\
                 object_lock_mode = \"GOVERNANCE\"\n{bad}\n"
            );
            // A repeated key is a TOML error, which is also a refusal.
            assert!(load("", &sink).is_err(), "{bad}");
        }
        std::fs::remove_dir_all(dir).ok();
    }

    const VAULT: &str = "[vault]\naddress = \"https://vault.example:8200\"\nmount = \"bezent\"\nrole = \"bezent-core-iam\"\nsts_region = \"us-east-2\"\nserver_id = \"vault.example\"\n";
    const STORE: &str = "[store]\nhost = \"db.example\"\ndatabase = \"bezent\"\nlogin = { vault = { path = \"secret/bezent/core\", field = \"db_login\" } }\npassword = { vault = { path = \"secret/bezent/core\", field = \"db_password\" } }\n";
    const STATEMENT: &str = "[statement]\nsigner_socket = \"s\"\nsigner_uid = 1000\n{token}\npublic_key = \"0x66d5b675e8fddf84c70c76d1815d6a5d531bbc96de157b591489be0cdbe5f615\"\ndirectory = \"d\"\n";

    /// Writes `text` as a core.toml in a directory of its own and reads it.
    fn read(tag: &str, text: &str) -> Result<Config, String> {
        let dir = std::env::temp_dir().join(format!("bzc-secrets-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("core.toml");
        std::fs::write(&path, text).unwrap();
        let result = Config::from_file(&path);
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    fn statement(token: &str) -> String {
        STATEMENT.replace("{token}", token)
    }

    #[test]
    fn req_ky_1_the_store_and_the_signer_token_can_come_from_vault() {
        let text = format!(
            "environment = \"test\"\n{STORE}{VAULT}{}",
            statement(
                "signer_token = { vault = { path = \"secret/bezent/core\", field = \"signer_token\" } }"
            )
        );
        let config = read("ok", &text).unwrap();
        assert!(config.database_url.is_none() && config.store.is_some());
        let printed = format!("{config:?}");
        assert!(printed.contains("db_password"), "references are printed");
        assert!(!printed.contains("hunter2"));
    }

    #[test]
    fn req_ky_1_one_store_source_one_token_source_and_a_vault_table_for_a_vault_secret() {
        let token_file = statement("signer_token_file = \"t\"");
        let url = "database_url = \"postgres://core@db/bezent\"\n";
        // Both store forms, or neither.
        assert!(
            read(
                "both",
                &format!("environment = \"test\"\n{url}{STORE}{VAULT}{token_file}")
            )
            .is_err()
        );
        assert!(read("neither", &format!("environment = \"test\"\n{token_file}")).is_err());
        // A vault reference with no [vault] table.
        assert!(
            read(
                "novault",
                &format!("environment = \"test\"\n{STORE}{token_file}")
            )
            .is_err()
        );
        // Both token forms, or neither.
        let both = statement("signer_token_file = \"t\"\nsigner_token = { file = \"t\" }");
        assert!(read("tokens", &format!("environment = \"test\"\n{url}{both}")).is_err());
        assert!(
            read(
                "notoken",
                &format!("environment = \"test\"\n{url}{}", statement(""))
            )
            .is_err()
        );
        // The existing forms still read.
        assert!(read("old", &format!("environment = \"test\"\n{url}{token_file}")).is_ok());
        // A file reference needs no [vault] table.
        let file_store = "[store]\nhost = \"db.example\"\ndatabase = \"bezent\"\nlogin = { file = \"l\" }\npassword = { file = \"p\" }\n";
        assert!(
            read(
                "files",
                &format!("environment = \"test\"\n{file_store}{token_file}")
            )
            .is_ok()
        );
    }

    #[tokio::test]
    async fn req_ky_1_the_store_url_is_built_from_files_in_the_process() {
        let dir = std::env::temp_dir().join(format!("bzc-secrets-url-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("l"), "bezent_core\n").unwrap();
        std::fs::write(dir.join("p"), "p@ss/word\n").unwrap();
        let text = "environment = \"test\"\n[store]\nhost = \"db.example\"\ndatabase = \"bezent\"\nlogin = { file = \"l\" }\npassword = { file = \"p\" }\nsslrootcert = \"ca.pem\"\n[statement]\nsigner_socket = \"s\"\nsigner_uid = 1\nsigner_token = { file = \"t\" }\npublic_key = \"0x66d5b675e8fddf84c70c76d1815d6a5d531bbc96de157b591489be0cdbe5f615\"\ndirectory = \"d\"\n";
        let path = dir.join("core.toml");
        std::fs::write(&path, text).unwrap();
        std::fs::write(dir.join("t"), "tok\n").unwrap();
        let config = Config::from_file(&path).unwrap();
        let resolver = config.resolver().unwrap();
        let url = config.store_url(&resolver).await.unwrap();
        // The URL parses as the store layer reads it, and keeps verify-full and the root certificate.
        assert!(bezent_store::tls::connect_options(&url).is_ok());
        assert!(url.contains("sslmode=verify-full") && url.contains("ca.pem"));
        assert!(url.starts_with("postgres://bezent_core:p%40ss%2Fword@db.example:5432/bezent?"));
        let statement = config
            .statement
            .as_ref()
            .and_then(Statement::configured)
            .unwrap();
        assert_eq!(statement.token(&resolver).await.unwrap().as_str(), "tok");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
