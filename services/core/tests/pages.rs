// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-014 · alerts. A page is a log line a metric filter can match: `kind=<kind>`, unquoted, on an
//! ERROR event. Two emitters wrote it differently until now: the reserve check's `LogPager`
//! (`kind = %page.kind`, rendered `kind=reserve-divergence`) and the engine's literals
//! (`kind = "payout-refusal-alert"`, rendered `kind="payout-refusal-alert"`, which a filter on
//! `kind=payout-refusal-alert` never matched). `deploy/runbooks/alerts.md` gives the patterns.

use std::io::Write;
use std::sync::{Arc, Mutex};

use bezent_core_service::reserve::check::{LogPager, Page, Pager};

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

/// What the service's own subscriber writes for `emit`, as the container's log would hold it
/// (`NO_COLOR=1`, no target: the settings of `main.rs`).
fn captured(emit: impl FnOnce()) -> String {
    let buffer = Buffer::default();
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_target(false)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, emit);
    let out = buffer.0.lock().unwrap().clone();
    String::from_utf8(out).unwrap()
}

#[test]
fn a_reserve_check_page_renders_kind_unquoted() {
    let line = captured(|| {
        LogPager.page(&Page {
            kind: "reserve-divergence".into(),
            asset: "bUSDC".into(),
            detail: serde_json::json!({"x": 1}),
        })
    });
    assert!(line.contains("ERROR"), "{line}");
    assert!(line.contains("page kind=reserve-divergence "), "{line}");
}

/// The engine's pages are written with a literal kind; a literal renders quoted unless it is
/// recorded with `%`. This reads every service's source, so a new page with a quoted kind fails
/// here instead of silently never matching a filter.
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
    let quoted = captured(|| tracing::error!(kind = "payout-refusal-alert", "page: x"));
    assert!(quoted.contains("kind=\"payout-refusal-alert\""), "{quoted}");
    assert!(!quoted.contains("kind=payout-refusal-alert"), "{quoted}");
    let shown = captured(|| tracing::error!(kind = %"payout-refusal-alert", "page: x"));
    assert!(shown.contains("kind=payout-refusal-alert"), "{shown}");
}
