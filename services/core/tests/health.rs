// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `bezent-core health` (issue #61), through the binary: healthy only while the running core's
//! liveness file is fresh, and nothing secret on the one line it prints.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use bezent_core_service::Config;
use bezent_core_service::health::{heartbeat, max_age};

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bezent-core-health-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_config(dir: &Path, cycle_seconds: u64) -> PathBuf {
    let file = dir.join("core.toml");
    std::fs::write(
        &file,
        format!(
            "database_url = \"postgres://core:hunter2@db/bezent\"\n\
             cycle_seconds = {cycle_seconds}\n\
             environment = \"test\"\n\
             statement = \"disabled-test-only\"\n\
             health_file = \"alive\"\n"
        ),
    )
    .unwrap();
    file
}

fn run(config: &Path) -> (bool, String, Duration) {
    let started = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_bezent-core"))
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
    let file = write_config(&d, 5);
    let config = Config::from_file(&file).unwrap();
    assert_eq!(config.health_file, Some(d.join("alive")));

    let (ok, said, _) = run(&file);
    assert!(!ok, "{said}");
    assert_eq!(said.lines().count(), 1, "{said}");

    heartbeat(&config).beat().unwrap();
    let (ok, said, took) = run(&file);
    assert!(ok, "{said}");
    assert!(took < bezent_health::BUDGET, "{took:?}");
    assert_eq!(said.trim(), "bezent-core: healthy");
}

#[test]
fn a_heartbeat_older_than_the_cycle_allows_is_not_healthy() {
    let d = dir("stale");
    let file = write_config(&d, 5);
    let config = Config::from_file(&file).unwrap();
    let hb = heartbeat(&config);
    // Well inside the limit (a slow machine must not tip it), then past it.
    hb.beat_at(SystemTime::now() - max_age(&config) / 2)
        .unwrap();
    assert!(run(&file).0);
    hb.beat_at(SystemTime::now() - max_age(&config) - Duration::from_secs(20))
        .unwrap();
    let (ok, said, took) = run(&file);
    assert!(!ok);
    // The unhealthy answer comes within the bound too.
    assert!(took < bezent_health::BUDGET, "{took:?}");
    assert!(said.contains("unhealthy"), "{said}");
}

#[test]
fn the_limit_follows_the_configured_cycle() {
    let d = dir("cycle");
    let slow = Config::from_file(&write_config(&d, 60)).unwrap();
    let quick = Config::from_file(&write_config(&d, 5)).unwrap();
    assert!(max_age(&slow) > max_age(&quick));
}

#[test]
fn an_unreadable_configuration_is_not_healthy_and_prints_nothing_of_it() {
    let d = dir("unreadable");
    let file = d.join("core.toml");
    std::fs::write(
        &file,
        "database_url = \"postgres://core:hunter2@db/bezent\"\ncycle_seconds = \"x\"\n",
    )
    .unwrap();
    let (ok, said, _) = run(&file);
    assert!(!ok);
    assert!(
        !said.contains("hunter2") && !said.contains("postgres"),
        "{said}"
    );
    let (ok, said, took) = run(&d.join("missing.toml"));
    assert!(!ok);
    assert!(took < bezent_health::BUDGET, "{took:?}");
    assert!(!said.contains("missing"), "{said}");
}

/// The wiring, which no test of `health` alone can show: a real core, on a real store, beats after
/// its first cycle, and `bezent-core health` then says so (issue #61). If the beat sat in the wrong
/// branch every task would be replaced in a loop.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn a_running_core_beats_and_health_says_so(admin: sqlx::PgPool) {
    let name = "core_health_test_login";
    let password = "test-only-core-health-login";
    bezent_store::login::ensure_login(&admin, name, bezent_store::CORE_ROLE, password)
        .await
        .unwrap();
    let base = admin.connect_options();
    let url = format!(
        "postgres://{name}:{password}@{}:{}/{}",
        base.get_host(),
        base.get_port(),
        base.get_database().unwrap()
    );

    let d = dir("running");
    let file = d.join("core.toml");
    std::fs::write(
        &file,
        format!(
            "database_url = \"{url}\"\ncycle_seconds = 1\nenvironment = \"test\"\n\
             statement = \"disabled-test-only\"\nhealth_file = \"{}\"\n",
            d.join("alive").display()
        ),
    )
    .unwrap();
    let mut core = Command::new(env!("CARGO_BIN_EXE_bezent-core"))
        .arg(&file)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    let started = Instant::now();
    let mut healthy = false;
    while started.elapsed() < Duration::from_secs(30) {
        if run(&file).0 {
            healthy = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let _ = core.kill();
    let _ = core.wait();
    assert!(healthy, "the running core never beat");
}
