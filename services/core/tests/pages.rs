// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-014 · alerts. A page is a log line a metric filter can match: `kind=<kind>`, unquoted, on an
//! ERROR event. Two emitters wrote it differently until now: the reserve check's `LogPager`
//! (`kind = %page.kind`, rendered `kind=reserve-divergence`) and the engine's literals
//! (`kind = "payout-refusal-alert"`, rendered `kind="payout-refusal-alert"`, which a filter on
//! `kind=payout-refusal-alert` never matched). `deploy/runbooks/alerts.md` gives the patterns.

mod common;

use bezent_core_service::reserve::check::{LogPager, Page, Pager};
use common::pagelog::{PageLog, assert_page, documented_shapes, filter_pattern, page_lines};

fn page(kind: &str) -> Page {
    Page {
        kind: kind.into(),
        asset: "bUSDC".into(),
        detail: serde_json::json!({"x": 1}),
    }
}

/// The reserve check's pager is also the statements' and the key-compromise path of a supply
/// mismatch: one emission, and the lines the runbooks quote for each.
#[test]
fn a_pager_page_matches_the_filter_and_the_shape_each_runbook_quotes() {
    let log = PageLog::start();
    for kind in [
        "reserve-divergence",
        "supply-mismatch-pause",
        "reserve-alert",
        "book-quarantine",
        "reserve-check-stalled",
        "reserve-check-incomplete",
        "reserve-check-failed",
        "key-compromise",
        "statement-overdue",
        "statement-stuck",
        "statement-abandoned",
        "statement-publish-failed",
    ] {
        LogPager.page(&page(kind));
        assert_page(&log, kind, 1);
    }
}

/// What the check says, tested against a renamed kind: the filter of the old name no longer
/// matches the new one, and a kind that merely begins with another is not that other.
#[test]
fn a_renamed_kind_is_not_matched_by_the_old_filter() {
    let log = PageLog::start();
    LogPager.page(&page("reserve-divergence-v2"));
    assert!(page_lines(&log, "reserve-divergence").is_empty());
    assert_eq!(page_lines(&log, "reserve-divergence-v2").len(), 1);
    let renamed = std::panic::catch_unwind(|| {
        let log = PageLog::start();
        LogPager.page(&page("reserve-diverged"));
        assert_page(&log, "reserve-divergence", 1);
    });
    assert!(renamed.is_err(), "a renamed kind passed assert_page");
}

/// The page lines are the filter's, and the filter is `alerts.md`'s: the pattern is read from it.
#[test]
fn the_filter_is_the_one_alerts_md_states() {
    assert_eq!(filter_pattern("reserve-alert"), "kind=reserve-alert");
    assert!(!documented_shapes("reserve-alert").is_empty());
    assert!(documented_shapes("not-a-page").is_empty());
}

/// `alerts.md` says the logs are plain text. Nothing sets `NO_COLOR` in this repository, and
/// `tracing-subscriber` colours its output unless that variable is set, terminal or not, so the
/// services' own setup (`bezent-logging`) turns colour off itself. This is the line it writes,
/// with the environment as the test finds it.
#[test]
fn the_services_log_is_plain_text_whatever_the_environment_says() {
    let log = PageLog::start();
    LogPager.page(&page("reserve-divergence"));
    tracing::warn!(answer = %"x", "not a page");
    let text = log.text();
    assert!(!text.contains('\u{1b}'), "{text:?}");
    assert!(
        text.contains("page kind=reserve-divergence asset=bUSDC detail="),
        "{text}"
    );
}

/// The premise: the default setup colours when `NO_COLOR` is not set (which is how a container
/// without it runs), and its colour breaks the `kind=` pattern. Skipped where the variable is set.
#[test]
fn the_default_setup_would_colour_the_field_names_and_break_the_filter() {
    if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        return;
    }
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);
    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buffer = Buffer::default();
    let writer = buffer.clone();
    let default = tracing_subscriber::fmt()
        .with_target(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(default, || {
        tracing::error!(kind = %"reserve-divergence", "page");
    });
    let text = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    assert!(text.contains('\u{1b}'), "no colour was written: {text:?}");
    assert!(!text.contains("kind=reserve-divergence"), "{text:?}");
    // The exact bytes `alerts.md` quotes: italic name, dim equals sign.
    assert!(
        text.contains("\u{1b}[3mkind\u{1b}[0m\u{1b}[2m=\u{1b}[0mreserve-divergence"),
        "{text:?}"
    );
}

/// A literal kind renders quoted unless it is recorded with `%`. This reads every service's
/// source, so a new page with a quoted kind fails here instead of silently never matching a
/// filter. It checks the spelling only; what each page actually writes is captured from the real
/// emission by `assert_page` in the tests that raise the engine's and the late-mint check's pages
/// (`cancel.rs`, `late_mint.rs`), `signals.rs` and the first test here.
#[test]
fn every_page_in_every_service_records_its_kind_with_display() {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for service in [
        "core",
        "ingress",
        "signer",
        "kreivo-endpoint",
        "evm-endpoint",
        "sidecar",
    ] {
        walk(&root.join(service).join("src"), &mut files);
    }
    assert!(files.len() > 20, "found {} source files", files.len());
    let mut quoted = Vec::new();
    for f in files {
        let src = std::fs::read_to_string(&f).unwrap();
        let mut rest = src.as_str();
        while let Some(at) = rest.find("tracing::error!(") {
            let call = &rest[at..];
            // The macro's first line and the next: its fields come before the message.
            let head: String = call.lines().take(2).collect::<Vec<_>>().join(" ");
            if head.contains("kind = \"") {
                quoted.push(format!("{}: {}", f.display(), head.trim()));
            }
            rest = &rest[at + 16..];
        }
    }
    assert!(
        quoted.is_empty(),
        "a quoted `kind = \"…\"` renders kind=\"…\" and no `kind=…` filter matches it; \
         write `kind = %\"…\"`:\n{}",
        quoted.join("\n")
    );
}

/// The premise of the rule above, shown once: the same event with a literal and with `%`.
#[test]
fn a_literal_kind_renders_quoted_and_a_displayed_one_does_not() {
    let log = PageLog::start();
    tracing::error!(kind = "payout-refusal-alert", "page: x");
    let quoted = log.text();
    assert!(quoted.contains("kind=\"payout-refusal-alert\""), "{quoted}");
    assert!(page_lines(&log, "payout-refusal-alert").is_empty());
    tracing::error!(kind = %"payout-refusal-alert", "page: x");
    assert_eq!(page_lines(&log, "payout-refusal-alert").len(), 1);
}
