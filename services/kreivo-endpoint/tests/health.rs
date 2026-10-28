// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `bezent-kreivo-endpoint health` (issue #61), through the binary: healthy only while the running endpoint's
//! liveness file is fresh, bounded, and nothing configured on the one line it prints. No database,
//! no provider, no network: the check opens none.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use bezent_kreivo_endpoint::config::Config;
use bezent_kreivo_endpoint::health::{heartbeat, max_age};

const FIXTURE: &str = include_str!("../fixtures/endpoint.toml");

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "bezent-kreivo-health-{}-{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The published fixture's configuration, with a liveness file of its own prepended (top-level
/// keys come before the tables).
fn write_config(dir: &Path) -> PathBuf {
    let file = dir.join("endpoint.toml");
    std::fs::write(
        &file,
        format!(
            "health_file = \"{}\"\n{FIXTURE}",
            dir.join("alive").display()
        ),
    )
    .unwrap();
    file
}

fn run(config: &Path) -> (bool, String, Duration) {
    let started = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_bezent-kreivo-endpoint"))
        .arg("health")
        .arg(config)
        .output()
        .unwrap();
    let took = started.elapsed();
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), said, took)
}

#[test]
fn no_heartbeat_is_not_healthy_a_fresh_one_is() {
    let d = dir("fresh");
    let file = write_config(&d);
    let config = Config::from_file(&file).unwrap();

    let (ok, said, _) = run(&file);
    assert!(!ok, "{said}");
    assert_eq!(said.lines().count(), 1, "{said}");

    heartbeat(&config).beat().unwrap();
    let (ok, said, _) = run(&file);
    assert!(ok, "{said}");
    assert_eq!(said.trim(), "bezent-kreivo-endpoint: healthy");
}

#[test]
fn a_stale_beat_is_not_healthy() {
    let d = dir("stale");
    let file = write_config(&d);
    let config = Config::from_file(&file).unwrap();
    heartbeat(&config).beat().unwrap();
    let beat = std::fs::File::options()
        .write(true)
        .open(d.join("alive"))
        .unwrap();
    // Well inside the limit (a slow machine must not tip it), then past it.
    beat.set_modified(SystemTime::now() - max_age(&config) / 2)
        .unwrap();
    assert!(run(&file).0);
    beat.set_modified(SystemTime::now() - max_age(&config) - Duration::from_secs(20))
        .unwrap();
    let (ok, said, _) = run(&file);
    assert!(!ok);
    assert!(said.contains("unhealthy"), "{said}");
}

#[test]
fn the_limit_covers_three_polls_and_a_pass() {
    let d = dir("limit");
    let config = Config::from_file(&write_config(&d)).unwrap();
    let limit = max_age(&config);
    assert!(limit >= Duration::from_millis(config.poll_ms) * 3 + Duration::from_secs(60));
}

#[test]
fn an_unreadable_configuration_is_not_healthy_and_prints_nothing_of_it() {
    let d = dir("unreadable");
    let file = d.join("endpoint.toml");
    std::fs::write(&file, "network = \"hunter2\"\nnot_a_field = 1\n").unwrap();
    let (ok, said, _) = run(&file);
    assert!(!ok);
    assert!(
        !said.contains("hunter2") && !said.contains("not_a_field"),
        "{said}"
    );
    let (ok, said, _) = run(&d.join("missing.toml"));
    assert!(!ok);
    assert!(!said.contains("missing"), "{said}");
}
