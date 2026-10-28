// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `bezent-core health [<config-path>]`: the container's health check (issue #61).
//!
//! The core serves no port, so the running core says it is well: after each cycle that read and
//! wrote the store (the cycle, and the reserve check's listing of the assets), it rewrites its
//! liveness file ([`bezent_health::Heartbeat`]). The check reads that file's age, with the cadence
//! of the same configuration. It opens no store connection and no signer link of its own.
//!
//! What a beat means: the store answers, the login was checked at start (a core whose login is
//! wrong never gets as far as a cycle), and, for a test deployment, the store still holds only test
//! networks. It does not mean the signer link is up. The link is used when a statement falls due,
//! once a day, and the core already pages an overdue statement (`REQ-PR-1`); restarting the core
//! cannot mend a signer that is down, so a signer outage is not allowed to stop the core's cycle
//! from being healthy.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bezent_health::Heartbeat;

use crate::Config;

/// The configuration path the image's `CMD` names; `health` uses it when it is given none.
pub const DEFAULT_CONFIG: &str = "/etc/bezent/core.toml";

/// The most one cycle's own work may add to its interval: the statement budget (60 s, in `main`)
/// and a margin for the cycle and the reserve check before it.
pub const PASS_ALLOWANCE: Duration = Duration::from_secs(120);

/// The core's liveness file.
pub fn heartbeat(config: &Config) -> Heartbeat {
    Heartbeat::configured(config.health_file.as_deref(), "core")
}

/// The most the last beat may be old: three cycles and one pass's allowance.
pub fn max_age(config: &Config) -> Duration {
    bezent_health::max_age(Duration::from_secs(config.cycle_seconds), PASS_ALLOWANCE)
}

/// The check: reads the configuration the service reads, and the liveness file it names. A
/// configuration that cannot be read is reported with fixed text (its parser's message can quote a
/// line of the file, and the file holds the store's URL).
pub fn check(config: &Path) -> Result<(), String> {
    let config = Config::from_file(config)
        .map_err(|_| bezent_health::CONFIGURATION_UNREADABLE.to_owned())?;
    heartbeat(&config)
        .check(max_age(&config))
        .map_err(|e| e.to_string())
}

/// `config` as `health` is given it, or the image's default.
pub fn config_path(arg: Option<&std::ffi::OsString>) -> PathBuf {
    arg.map_or_else(|| PathBuf::from(DEFAULT_CONFIG), PathBuf::from)
}
