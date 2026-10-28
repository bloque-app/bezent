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
use std::sync::Arc;
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

/// How often the first start's walk to `start_block` is looked at.
pub const WALK_POLL: Duration = Duration::from_secs(5);

/// Whether the walk moved: the lowest anchor stored is lower than the lowest seen before. The first
/// reading only sets the mark.
pub fn walk_moved(lowest: &mut Option<i64>, now: Option<i64>) -> bool {
    let Some(now) = now else { return false };
    match lowest {
        Some(before) if now >= *before => false,
        Some(before) => {
            *before = now;
            true
        }
        None => {
            *lowest = Some(now);
            false
        }
    }
}

/// Beats while the first start's walk to `start_block` moves (issue #61). The walk keeps an anchor
/// every few blocks as it goes (`observation_walk_anchors`), so the lowest one stored going down is
/// progress, and a query that answers shows the store is up. Runs until it is dropped or aborted;
/// a walk that stands still, or a store that does not answer, leaves the file to go stale.
pub async fn walk_progress(pool: sqlx::PgPool, network: String, heartbeat: Arc<Heartbeat>) {
    let mut lowest = None;
    let mut tick = tokio::time::interval(WALK_POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let now: Result<Option<i64>, _> = sqlx::query_scalar(
            "SELECT min(block_number) FROM observation_walk_anchors WHERE network = $1",
        )
        .bind(&network)
        .fetch_one(&pool)
        .await;
        if let Ok(now) = now
            && walk_moved(&mut lowest, now)
        {
            beat(&heartbeat);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::walk_moved;

    #[test]
    fn a_walk_that_goes_down_is_progress_and_one_that_stands_still_is_not() {
        let mut lowest = None;
        assert!(!walk_moved(&mut lowest, None));
        assert!(!walk_moved(&mut lowest, Some(900)));
        assert!(walk_moved(&mut lowest, Some(800)));
        assert!(!walk_moved(&mut lowest, Some(800)));
        assert!(!walk_moved(&mut lowest, Some(850)));
        assert!(walk_moved(&mut lowest, Some(700)));
    }
}
