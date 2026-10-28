// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `bezent-signer health` (issue #61), through the binary: healthy only while the running signer's
//! liveness file is fresh and its socket is there; it loads no key and prints nothing configured.

use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use bezent_signer::config::Config;
use bezent_signer::health::{MAX_AGE, heartbeat};

fn dir(name: &str) -> PathBuf {
    // A short path: a Unix socket's is limited.
    let d = std::env::temp_dir().join(format!("bz-sh-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The published fixture's configuration, with its socket and liveness file in `dir`, and its key
/// file somewhere that does not exist: `health` must not need it.
fn write_config(dir: &Path) -> PathBuf {
    let mut json: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/signer.json")).unwrap();
    json["socket"] = dir.join("s.sock").to_str().unwrap().into();
    // Relative: it is the configuration directory's, as the socket and the key file are.
    json["health_file"] = "alive".into();
    json["keys"]["path"] = dir.join("no-such-keys.json").to_str().unwrap().into();
    let file = dir.join("signer.json");
    std::fs::write(&file, serde_json::to_vec(&json).unwrap()).unwrap();
    file
}

fn run(config: &Path) -> (bool, String, Duration) {
    let started = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_bezent-signer"))
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
fn healthy_needs_a_fresh_beat_and_the_socket() {
    let d = dir("both");
    let file = write_config(&d);
    let config = Config::load(&file).unwrap();
    assert_eq!(config.health_file, Some(d.join("alive")));

    let (ok, said, _) = run(&file);
    assert!(!ok, "{said}");
    assert_eq!(said.lines().count(), 1, "{said}");

    // A beat, and no socket: not healthy.
    heartbeat(&config).beat().unwrap();
    let (ok, said, _) = run(&file);
    assert!(!ok && said.contains("socket"), "{said}");

    // Both: healthy, with no key file anywhere.
    let _listener = UnixListener::bind(d.join("s.sock")).unwrap();
    let (ok, said, took) = run(&file);
    assert!(ok, "{said}");
    assert!(took < bezent_health::BUDGET, "{took:?}");
    assert_eq!(said.trim(), "bezent-signer: healthy");
}

#[test]
fn a_stale_beat_is_not_healthy() {
    let d = dir("stale");
    let file = write_config(&d);
    let config = Config::load(&file).unwrap();
    let _listener = UnixListener::bind(d.join("s.sock")).unwrap();
    let hb = heartbeat(&config);
    hb.beat().unwrap();
    assert!(run(&file).0);
    hb.beat_at(SystemTime::now() - MAX_AGE - Duration::from_secs(20))
        .unwrap();
    let (ok, said, took) = run(&file);
    assert!(!ok);
    // The unhealthy answer comes within the bound too.
    assert!(took < bezent_health::BUDGET, "{took:?}");
    assert!(said.contains("unhealthy"), "{said}");
}

#[test]
fn a_file_that_is_not_a_socket_is_not_a_serving_signer() {
    let d = dir("plain");
    let file = write_config(&d);
    let config = Config::load(&file).unwrap();
    heartbeat(&config).beat().unwrap();
    std::fs::write(d.join("s.sock"), b"").unwrap();
    assert!(!run(&file).0);
}

#[test]
fn an_unreadable_configuration_is_not_healthy_and_prints_nothing_of_it() {
    let d = dir("unreadable");
    let file = d.join("signer.json");
    std::fs::write(&file, br#"{"socket": "/run/hunter2.sock", "callers": 7}"#).unwrap();
    let (ok, said, _) = run(&file);
    assert!(!ok);
    assert!(!said.contains("hunter2"), "{said}");
    let (ok, said, took) = run(&d.join("missing.json"));
    assert!(!ok);
    assert!(took < bezent_health::BUDGET, "{took:?}");
    assert!(!said.contains("missing"), "{said}");
}
