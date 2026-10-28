// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `bezent-signer health [<config-path>]`: the container's health check (issue #61).
//!
//! The check cannot ask the signer over its socket. The link refuses a caller that runs as the
//! signer's own user (`REQ-CU-1`, `REQ-AR-3`), and the health command runs as exactly that user; a
//! caller with no token is also a refusal the signer logs. So the running signer says it is well:
//! once the store login is checked and the socket bound, [`pulse`] asks the store a trivial question
//! every [`INTERVAL`] over the signer's own pool, and rewrites its liveness file
//! ([`bezent_health::Heartbeat`]) when the store answers. The check reads that file's age, and that the
//! socket is still there. It loads no key, reads no secret, and opens no connection: it never touches
//! the key backend (`load` in `main` is not on its path).

use std::path::{Path, PathBuf};
use std::time::Duration;

use bezent_health::Heartbeat;
use sqlx::PgPool;

use crate::config::Config;
use crate::log::Log;

/// The configuration path the image's `CMD` names; `health` uses it when it is given none.
pub const DEFAULT_CONFIG: &str = "/etc/bezent-signer/signer.json";

/// How often the running signer asks the store and beats.
pub const INTERVAL: Duration = Duration::from_secs(10);

/// How long the store has to answer one question.
const QUESTION: Duration = Duration::from_secs(5);

/// The most the last beat may be old: three intervals and one question.
pub const MAX_AGE: Duration = Duration::from_secs(35);

/// The signer's liveness file.
pub fn heartbeat(config: &Config) -> Heartbeat {
    Heartbeat::configured(config.health_file.as_deref(), "signer")
}

/// Beats every [`INTERVAL`] while the store answers. Runs until it is dropped; a store that does
/// not answer, or a file that cannot be written, is logged and leaves the file to go stale.
pub async fn pulse(pool: PgPool, heartbeat: Heartbeat, log: Log) {
    let mut tick = tokio::time::interval(INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let asked = tokio::time::timeout(QUESTION, sqlx::query("SELECT 1").execute(&pool)).await;
        if !matches!(asked, Ok(Ok(_))) {
            log.line(format_args!("health: the store did not answer"));
            continue;
        }
        if let Err(e) = heartbeat.beat() {
            log.line(format_args!(
                "health: the liveness file could not be written ({})",
                e.kind()
            ));
        }
    }
}

/// Whether `path` is a socket (and not a link to something else).
fn is_socket(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_socket())
}

/// The check: reads the configuration the service reads (and nothing else: no key, no store), then
/// the liveness file and the socket. A configuration that cannot be read is reported with fixed text.
pub fn check(config: &Path) -> Result<(), String> {
    let config =
        Config::load(config).map_err(|_| bezent_health::CONFIGURATION_UNREADABLE.to_owned())?;
    heartbeat(&config)
        .check(MAX_AGE)
        .map_err(|e| e.to_string())?;
    if !is_socket(&config.socket) {
        return Err("the link socket is missing".to_owned());
    }
    Ok(())
}

/// `config` as `health` is given it, or the image's default.
pub fn config_path(arg: Option<&std::ffi::OsString>) -> PathBuf {
    arg.map_or_else(|| PathBuf::from(DEFAULT_CONFIG), PathBuf::from)
}
