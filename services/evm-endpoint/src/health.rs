// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `bezent-evm-endpoint health [<config.toml>]`: the container's health check (issue #61).
//!
//! The EVM endpoint serves no port, so the running endpoint says it is well: at the end of each pass
//! of its loop that reached the head of the network it observes (both providers agree on a final point (the follower reached `CaughtUp`, `DEC-12`)) and
//! met the store throughout, it rewrites its liveness file ([`bezent_health::Heartbeat`]). A pass
//! that read a block but is still behind the head, one that found a gap or no final point, and one
//! that failed on the store do not beat. The check reads that file's age, with the cadence of the same
//! configuration. It opens no store connection, no provider connection and no signer link, and reads
//! no secret file.
//!
//! A first start can take longer than a container health check's start period (a backlog is walked
//! before the head is reached): the deployment sets the start period to cover it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bezent_health::Heartbeat;

use crate::config::Config;

/// The configuration path the image's `CMD` names; `health` uses it when it is given none.
pub const DEFAULT_CONFIG: &str = "/etc/bezent/evm-endpoint.toml";

/// What one pass may take beyond its poll interval: a catch-up, a slow provider or history source
/// (their own timeouts are 30 s by default), the signer and the proofs.
pub const PASS_ALLOWANCE: Duration = Duration::from_secs(120);

/// The endpoint's liveness file.
pub fn heartbeat(config: &Config) -> Heartbeat {
    Heartbeat::configured(config.health_file.as_deref(), "evm-endpoint")
}

/// The most the last beat may be old: three poll intervals and one pass's allowance.
pub fn max_age(config: &Config) -> Duration {
    bezent_health::max_age(Duration::from_millis(config.poll_ms), PASS_ALLOWANCE)
}

/// The check: reads the configuration the service reads, and the liveness file it names. A
/// configuration that cannot be read is reported with fixed text.
pub fn check(config: &Path) -> Result<(), String> {
    let config = Config::from_file(config)
        .map_err(|_| bezent_health::CONFIGURATION_UNREADABLE.to_owned())?;
    heartbeat(&config)
        .check(max_age(&config))
        .map_err(|e| e.to_string())
}

/// `config` as `health` is given it, or the image's default.
pub fn config_path(arg: Option<&String>) -> PathBuf {
    arg.map_or_else(|| PathBuf::from(DEFAULT_CONFIG), PathBuf::from)
}
