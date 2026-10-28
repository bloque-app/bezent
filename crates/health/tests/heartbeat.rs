// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! The heartbeat file: fresh, stale, absent, and removed at start (issue #61).

use std::time::{Duration, SystemTime};

use bezent_health::{Beat, Heartbeat, Unhealthy, max_age};

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("bezent-health-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn a_beat_is_healthy_until_it_is_older_than_the_limit() {
    let hb = Heartbeat::new(temp("fresh"));
    hb.beat().unwrap();
    assert_eq!(hb.check(Duration::from_secs(30)), Ok(()));
    let later = SystemTime::now() + Duration::from_secs(120);
    assert!(matches!(
        hb.check_at(Duration::from_secs(30), later),
        Err(Unhealthy::Stale {
            limit_seconds: 30,
            ..
        })
    ));
}

#[test]
fn the_age_is_the_content_not_the_modification_time() {
    let path = temp("content");
    let hb = Heartbeat::new(&path);
    hb.beat_at(SystemTime::now() - Duration::from_secs(600))
        .unwrap();
    // A fresh modification time does not make an old beat fresh.
    assert!(matches!(
        hb.check(Duration::from_secs(30)),
        Err(Unhealthy::Stale { .. })
    ));
    hb.beat_at(SystemTime::now() - Duration::from_secs(10))
        .unwrap();
    assert_eq!(hb.check(Duration::from_secs(30)), Ok(()));
    // Not a time: no heartbeat.
    std::fs::write(&path, "not a time").unwrap();
    assert_eq!(
        hb.check(Duration::from_secs(30)),
        Err(Unhealthy::NoHeartbeat)
    );
}

#[test]
fn a_beat_dated_ahead_is_fresh_within_the_skew_and_stale_beyond_it() {
    let hb = Heartbeat::new(temp("ahead"));
    hb.beat_at(SystemTime::now() + Duration::from_secs(2))
        .unwrap();
    assert_eq!(hb.check(Duration::from_secs(30)), Ok(()));
    hb.beat_at(SystemTime::now() + Duration::from_secs(120))
        .unwrap();
    assert_eq!(
        hb.check(Duration::from_secs(30)),
        Err(Unhealthy::FromTheFuture)
    );
}

#[test]
fn a_beat_leaves_no_temporary_file_and_replaces_the_last() {
    let path = temp("atomic");
    let hb = Heartbeat::new(&path);
    hb.beat().unwrap();
    hb.beat().unwrap();
    assert!(!path.with_file_name("atomic.tmp").exists());
    assert_eq!(hb.check(Duration::from_secs(30)), Ok(()));
}

#[test]
fn only_the_first_failure_and_the_recovery_are_reported() {
    let dir = temp("rate");
    let hb = Heartbeat::new(dir.join("inner").join("beat"));
    let failed = |first| Beat::Failed {
        kind: std::io::ErrorKind::NotFound,
        first,
    };
    assert_eq!(hb.try_beat(), failed(true));
    assert_eq!(hb.try_beat(), failed(false));
    assert_eq!(hb.try_beat(), failed(false));
    std::fs::create_dir_all(dir.join("inner")).unwrap();
    assert_eq!(hb.try_beat(), Beat::Recovered);
    assert_eq!(hb.try_beat(), Beat::Written);
}

#[test]
fn no_file_is_not_healthy() {
    let hb = Heartbeat::new(temp("absent"));
    hb.reset().unwrap();
    assert_eq!(
        hb.check(Duration::from_secs(30)),
        Err(Unhealthy::NoHeartbeat)
    );
}

#[test]
fn reset_removes_an_earlier_runs_beat() {
    let hb = Heartbeat::new(temp("earlier"));
    hb.beat().unwrap();
    hb.reset().unwrap();
    hb.reset().unwrap();
    assert_eq!(
        hb.check(Duration::from_secs(30)),
        Err(Unhealthy::NoHeartbeat)
    );
}

#[test]
fn a_beat_that_cannot_be_written_is_an_error_not_a_silence() {
    let hb = Heartbeat::new(temp("no-such-dir").join("inner").join("beat"));
    assert!(hb.beat().is_err());
}

#[test]
fn the_default_path_names_the_service_and_the_limit_is_three_intervals_and_a_pass() {
    assert_eq!(
        Heartbeat::default_path("core").to_str(),
        Some("/tmp/bezent-core.health")
    );
    assert_eq!(
        max_age(Duration::from_secs(10), Duration::from_secs(60)),
        Duration::from_secs(90)
    );
}
