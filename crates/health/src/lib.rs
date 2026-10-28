// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! # bezent-health
//!
//! What a service's `health` subcommand reads (issue #61). The images are distroless: there is no
//! shell and no `curl`, so an ECS container health check runs the service binary itself,
//! `bezent-<service> health`, which exits 0 only when the running instance in the same container is
//! healthy.
//!
//! The running instance decides that, because only it knows. After each pass of its main loop that
//! met the service's health condition (the store reached and the login checked; for an endpoint, also
//! caught up with the network it observes), it calls [`Heartbeat::beat`], which rewrites a small file.
//! The `health` command opens no connection, loads no key and reads no secret: it only looks at the
//! file's age ([`Heartbeat::check`]). A service that stopped, hung, lost its store or fell behind its
//! network stops beating, and the file grows stale.
//!
//! The file holds the time of the beat in seconds, and nothing else. Its age is read from the file's
//! modification time, so a half-written file is still fresh or stale, never misread. The service
//! removes the file at start ([`Heartbeat::reset`]), so a file left on a volume by an earlier run
//! never passes.
//!
//! Every message this crate produces is fixed text: a path, a URL or a configuration value never
//! reaches the line `health` prints.

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The longest a `health` command may take (the ECS container health check's own bound is set in the
/// task definition; this is the one the commands keep to).
pub const BUDGET: Duration = Duration::from_secs(5);

/// Why an instance is not healthy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unhealthy {
    /// The service has not beaten yet, or has stopped and removed the file, or the file cannot be read.
    NoHeartbeat,
    /// The last beat is older than the service's own cadence allows.
    Stale {
        /// The beat's age, in whole seconds.
        age_seconds: u64,
        /// The most the service allows, in whole seconds.
        limit_seconds: u64,
    },
}

impl std::fmt::Display for Unhealthy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHeartbeat => f.write_str("no heartbeat"),
            Self::Stale {
                age_seconds,
                limit_seconds,
            } => write!(
                f,
                "last heartbeat {age_seconds} s ago, more than the {limit_seconds} s allowed"
            ),
        }
    }
}

/// A liveness file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heartbeat {
    path: PathBuf,
}

impl Heartbeat {
    /// A heartbeat at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The path a service uses when its configuration names none: `/tmp/bezent-<service>.health`.
    /// A task that mounts its root file system read-only names a writable path in the configuration.
    pub fn default_path(service: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/bezent-{service}.health"))
    }

    /// The heartbeat at the configured path, or at [`Heartbeat::default_path`].
    pub fn configured(path: Option<&Path>, service: &str) -> Self {
        Self::new(path.map_or_else(|| Self::default_path(service), Path::to_path_buf))
    }

    /// Removes an earlier run's file. Call it at start, before the first beat. A file that is not
    /// there is not an error.
    pub fn reset(&self) -> io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Records that the instance is healthy now. A failure is the caller's to log: a service that
    /// cannot write its heartbeat will be reported unhealthy, which is true of it.
    pub fn beat(&self) -> io::Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        std::fs::write(&self.path, format!("{now}\n"))
    }

    /// Whether the last beat is no older than `max_age`.
    pub fn check(&self, max_age: Duration) -> Result<(), Unhealthy> {
        self.check_at(max_age, SystemTime::now())
    }

    /// [`Heartbeat::check`] at a given time.
    pub fn check_at(&self, max_age: Duration, now: SystemTime) -> Result<(), Unhealthy> {
        let modified = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .map_err(|_| Unhealthy::NoHeartbeat)?;
        // A beat in the future (the clock stepped back) is as fresh as it can be.
        let age = now.duration_since(modified).unwrap_or_default();
        if age > max_age {
            return Err(Unhealthy::Stale {
                age_seconds: age.as_secs(),
                limit_seconds: max_age.as_secs(),
            });
        }
        Ok(())
    }
}

/// How long a pass of a polling service may take between beats, from its poll interval: three
/// intervals, and `pass` for one pass's own work (a catch-up, a statement, a slow provider).
pub fn max_age(interval: Duration, pass: Duration) -> Duration {
    interval.saturating_mul(3).saturating_add(pass)
}

/// Prints the one line of a `health` command and picks its exit status: 0 when healthy, 1 when not.
/// The line is `<name>: healthy` or `<name>: unhealthy: <reason>`. `reason` must be fixed text.
pub fn report(name: &str, result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => {
            println!("{name}: healthy");
            ExitCode::SUCCESS
        }
        Err(reason) => {
            println!("{name}: unhealthy: {reason}");
            ExitCode::FAILURE
        }
    }
}

/// The line printed when the configuration cannot be read. Fixed: a parse error can quote the line
/// that failed, and a line of a configuration may be a secret.
pub const CONFIGURATION_UNREADABLE: &str = "configuration unreadable";
