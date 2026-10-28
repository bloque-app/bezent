// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `bezent-kreivo-endpoint health [<config.toml>]`: the container's health check (issue #61).
//!
//! The Kreivo endpoint serves no port, so the running endpoint says it is well: at the end of each pass
//! of its loop that reached the head of the network it observes (the light client follows the declared network and the follower reached the finalized head (`CaughtUp`)) and
//! met the store throughout, it rewrites its liveness file ([`bezent_health::Heartbeat`]). A pass
//! that is stuck behind the head, one that found a gap or no final point, and one
//! that failed on the store do not beat. The check reads that file's age, with the cadence of the same
//! configuration. It opens no store connection, no provider connection and no signer link, and reads
//! no secret file.
//!
//! A walk that moves is healthy too: each step that read and recorded blocks beats, so a first start
//! that walks a long backlog stays healthy while the anchor advances, and a stuck walk (a gap, no
//! head) stops beating and goes stale. The light client's first sync beats while it advances
//! (`chain::light`), for the same reason.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bezent_health::{Beat, Heartbeat};

use crate::config::Config;

/// The configuration path the image's `CMD` names; `health` uses it when it is given none.
pub const DEFAULT_CONFIG: &str = "/etc/bezent/kreivo-endpoint.toml";

/// What one pass may take beyond its poll interval: a catch-up, a slow provider or history source
/// (their own timeouts are 30 s by default), the signer and the proofs.
pub const PASS_ALLOWANCE: Duration = Duration::from_secs(120);

/// The endpoint's liveness file.
pub fn heartbeat(config: &Config) -> Heartbeat {
    Heartbeat::configured(config.health_file.as_deref(), "kreivo-endpoint")
}

/// Beats, and logs what is worth a line: the first failure to write the file, and the recovery.
pub fn beat(heartbeat: &Heartbeat) {
    match heartbeat.try_beat() {
        Beat::Failed { kind, first: true } => {
            tracing::error!(error = %kind, "the liveness file could not be written; `health` will report this endpoint unhealthy")
        }
        Beat::Recovered => tracing::info!("the liveness file is written again"),
        _ => {}
    }
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
