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

/// The premise: the default setup colours (`with_ansi(true)` is what it does when `NO_COLOR` is not
/// set, which is how a container without it runs), and its colour breaks the `kind=` pattern. The
/// colour is asked for outright, so the test says the same under any environment, `NO_COLOR` set or not.
#[test]
fn the_default_setup_would_colour_the_field_names_and_break_the_filter() {
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
    // The process-wide subscriber first, so that this scoped one does not race the callsite cache.
    common::pagelog::install();
    let buffer = Buffer::default();
    let writer = buffer.clone();
    let default = tracing_subscriber::fmt()
        .with_ansi(true)
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
/// source (every `error!` call, whole, `tracing::error!` or imported: `quoted_kinds`), so a new page with a quoted kind fails here instead of silently never matching a
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
        for call in quoted_kinds(&src) {
            quoted.push(format!("{}: {}", f.display(), call));
        }
    }
    assert!(
        quoted.is_empty(),
        "a quoted `kind = \"…\"` renders kind=\"…\" and no `kind=…` filter matches it; \
         write `kind = %\"…\"`:\n{}",
        quoted.join("\n")
    );
}

/// The `error!(…)` calls of `src` (qualified or imported, a word boundary before `error!`) whose own
/// fields, at the top level of the call and outside any string, hold `kind = "…"`: a literal kind,
/// which renders quoted. Each call is read to its matching closing parenthesis, over any number of
/// lines; strings (plain and raw), characters and comments are skipped, so a message that quotes
/// `kind = "x"` is not a field. Returns the first line of each such call.
fn quoted_kinds(src: &str) -> Vec<String> {
    let b = src.as_bytes();
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(off) = src[from..].find("error!(") {
        let at = from + off;
        from = at + 7;
        if at > 0 && ident(b[at - 1]) {
            continue; // `debug_error!(`, `my_error!(`
        }
        let line = src[..at].rfind('\n').map_or(0, |n| n + 1);
        if src[line..at].contains("//") {
            continue; // a commented-out call
        }
        let (mut i, mut depth, mut bad) = (from, 1usize, false);
        while i < b.len() && depth > 0 {
            match b[i] {
                b'"' => i = skip_string(b, i),
                b'r' if matches!(b.get(i + 1), Some(b'"' | b'#'))
                    && (i == 0 || !ident(b[i - 1])) =>
                {
                    let hashes = b[i + 1..].iter().take_while(|&&c| c == b'#').count();
                    if b.get(i + 1 + hashes) == Some(&b'"') {
                        let close = format!("\"{}", "#".repeat(hashes));
                        i = src[i + 2 + hashes..]
                            .find(&close)
                            .map_or(b.len(), |e| i + 2 + hashes + e + close.len());
                    } else {
                        i += 1;
                    }
                }
                b'\'' => {
                    // A character literal ('(', '\'') or a lifetime.
                    if b.get(i + 1) == Some(&b'\\') {
                        i = src[i + 2..].find('\'').map_or(b.len(), |e| i + 2 + e + 1);
                    } else if b.get(i + 2) == Some(&b'\'') {
                        i += 3;
                    } else {
                        i += 1;
                    }
                }
                b'/' if b.get(i + 1) == Some(&b'/') => {
                    i = src[i..].find('\n').map_or(b.len(), |e| i + e);
                }
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    i = src[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
                }
                b'(' => {
                    depth += 1;
                    i += 1;
                }
                b')' => {
                    depth -= 1;
                    i += 1;
                }
                b'k' if depth == 1
                    && src[i..].starts_with("kind")
                    && !ident(b[i - 1])
                    && !ident(b.get(i + 4).copied().unwrap_or(b' ')) =>
                {
                    let mut j = i + 4;
                    while b.get(j).is_some_and(u8::is_ascii_whitespace) {
                        j += 1;
                    }
                    if b.get(j) == Some(&b'=') && b.get(j + 1) != Some(&b'=') {
                        j += 1;
                        while b.get(j).is_some_and(u8::is_ascii_whitespace) {
                            j += 1;
                        }
                        bad |= b.get(j) == Some(&b'"');
                    }
                    i += 4;
                }
                _ => i += 1,
            }
        }
        if bad {
            found.push(src[at..].lines().next().unwrap_or("").trim().to_owned());
        }
        from = i.max(from);
    }
    found
}

/// The end of the string literal that starts at `b[start] == b'"'`.
fn skip_string(b: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    b.len()
}

/// The scan reads whole calls: a literal kind on a later line, in a bare `error!`, or written without
/// spaces is found; a displayed kind, a message that quotes the text, a comment and another macro's
/// name are not. The old scan (the first two lines of `tracing::error!(`) found none of the first three.
#[test]
fn the_quoted_kind_scan_reads_whole_calls_and_bare_error_macros() {
    let flagged = |src: &str| quoted_kinds(src).len();
    // The old scan, kept to show what it missed.
    let old = |src: &str| {
        let mut n = 0;
        let mut rest = src;
        while let Some(at) = rest.find("tracing::error!(") {
            let head: String = rest[at..].lines().take(2).collect::<Vec<_>>().join(" ");
            n += usize::from(head.contains("kind = \""));
            rest = &rest[at + 16..];
        }
        n
    };
    let third_line = "tracing::error!(\n    transfer = %t,\n    kind = \"late\",\n    \"page\");";
    let bare = "use tracing::error;\nerror!(kind = \"late\", \"page\");";
    let tight = "error!(a = 1,\n  b = 2,\n  c = 3, kind=\"late\", \"page\")";
    for src in [third_line, bare, tight] {
        assert_eq!(flagged(src), 1, "{src}");
    }
    assert_eq!((old(third_line), old(bare), old(tight)), (0, 0, 0));
    for src in [
        "tracing::error!(kind = %\"late\", \"page\");",
        "tracing::error!(%detail, \"page: kind = \\\"x\\\" is quoted\");",
        "tracing::error!(a = f(\"kind = \\\"x\\\"\"), kind = %\"late\", \"p\");",
        "tracing::error!(a = ')', kind = %\"late\", \"p\");",
        "// error!(kind = \"late\", \"page\");",
        "my_error!(kind = \"late\");",
        "tracing::error!(total_kind = \"late\", \"page\");",
    ] {
        assert_eq!(flagged(src), 0, "{src}");
    }
    // A call that follows another is read separately.
    assert_eq!(
        flagged("error!(a = 1, \"x\");\nerror!(kind = \"late\", \"y\");"),
        1
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
