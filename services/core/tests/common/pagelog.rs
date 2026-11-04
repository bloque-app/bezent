// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-014 · what a page looks like in the log, captured from the real emission and compared with
//! what `deploy/runbooks/alerts.md` and the runbooks say it looks like.
//!
//! The subscriber is the services' own (`bezent_logging::subscriber`, the one `main.rs` starts),
//! writing to a buffer, so a line here is the line the container's log would hold. A page is an
//! ERROR line a metric filter matches on the pattern `alerts.md` states; the runbook of the kind
//! shows the shape of the line, and the shape is applied to the captured line, so a renamed kind,
//! a reordered field or a reworded runbook fails a test instead of drifting.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tracing::subscriber::DefaultGuard;

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

/// The services' log, captured for the current thread until this is dropped. The tests run on a
/// current-thread runtime, so every event of the test lands here.
pub struct PageLog {
    buffer: Buffer,
    _guard: DefaultGuard,
}

impl PageLog {
    pub fn start() -> Self {
        let buffer = Buffer::default();
        let writer = buffer.clone();
        let guard =
            tracing::subscriber::set_default(bezent_logging::subscriber(move || writer.clone()));
        Self {
            buffer,
            _guard: guard,
        }
    }

    /// Everything logged so far.
    pub fn text(&self) -> String {
        String::from_utf8(self.buffer.0.lock().unwrap().clone()).unwrap()
    }
}

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The metric filter's pattern for `kind`, as `alerts.md` states it (the string between the
/// quotes after "The filter for a kind is the string"), with the kind put in.
pub fn filter_pattern(kind: &str) -> String {
    let alerts = std::fs::read_to_string(repo().join("deploy/runbooks/alerts.md")).unwrap();
    let marker = "The filter for a kind is the string `\"";
    let at = alerts
        .find(marker)
        .expect("alerts.md no longer states the filter pattern");
    let rest = &alerts[at + marker.len()..];
    let template = &rest[..rest.find("\"`").expect("the pattern is not closed")];
    assert!(template.contains("<kind>"), "{template}");
    template.replace("<kind>", kind)
}

/// The lines of `log` that the filter for `kind` matches: an ERROR line holding the pattern, the
/// kind ending where the pattern ends (so `kind=a-b` is not a match for `kind=a`).
pub fn page_lines(log: &PageLog, kind: &str) -> Vec<String> {
    let pattern = filter_pattern(kind);
    log.text()
        .lines()
        .filter(|l| l.contains(" ERROR "))
        .filter(|l| {
            l.match_indices(&pattern).any(|(at, _)| {
                l[at + pattern.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| c.is_whitespace())
            })
        })
        .map(str::to_string)
        .collect()
}

/// The shapes a runbook for `kind` quotes of its page line: the code spans of the files under
/// `pages/` that list `kind` among their `pages:`, which hold `kind=` and start with `page` (the
/// line's message, anchored to it) or with `kind=` (a field, anywhere in the line), and whose own
/// `kind=` could be this kind (`<kind>`, the kind itself, or a prefix ending in `...`).
pub fn documented_shapes(kind: &str) -> Vec<String> {
    let mut shapes = Vec::new();
    let dir = repo().join("deploy/runbooks/pages");
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let text = std::fs::read_to_string(entry.path()).unwrap();
        let lists = text
            .lines()
            .find(|l| l.starts_with("pages:"))
            .is_some_and(|l| {
                l.trim_start_matches("pages:")
                    .trim()
                    .trim_matches(['[', ']'])
                    .split(',')
                    .any(|k| k.trim() == kind)
            });
        if !lists {
            continue;
        }
        for (i, span) in text.split('`').enumerate() {
            if i % 2 == 0 || !(span.starts_with("page") || span.starts_with("kind=")) {
                continue;
            }
            let span = span.replace("<kind>", kind);
            let Some(at) = span.find("kind=") else {
                continue;
            };
            let token = span[at + 5..].split_whitespace().next().unwrap_or("");
            let compatible = match token.strip_suffix("...") {
                Some(prefix) => kind.starts_with(prefix),
                None => token == kind,
            };
            if compatible {
                shapes.push(span);
            }
        }
    }
    shapes
}

/// Whether `line` has the shape a runbook quotes: its literal pieces in order, `...` for anything
/// and `<name>` for something non-empty. A shape that starts with `page` is anchored to the
/// line's message (the text after `ERROR `).
fn matches_shape(shape: &str, line: &str) -> bool {
    let anchored = shape.starts_with("page");
    let text = if anchored {
        line.split_once(" ERROR ").map_or(line, |(_, rest)| rest)
    } else {
        line
    };
    // Split the shape into literals and wildcards.
    let mut pieces: Vec<(String, bool)> = Vec::new(); // (literal, wildcard-follows-requires-content)
    let mut rest = shape;
    let mut literal = String::new();
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("...") {
            pieces.push((std::mem::take(&mut literal), false));
            rest = r;
        } else if rest.starts_with('<') && rest.contains('>') {
            pieces.push((std::mem::take(&mut literal), true));
            rest = &rest[rest.find('>').unwrap() + 1..];
        } else {
            let c = rest.chars().next().unwrap();
            literal.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    let tail = literal;
    let mut pos = 0;
    let mut first = true;
    let mut needs_content = false;
    for (lit, content) in pieces {
        let Some(found) = text[pos..].find(&lit) else {
            return false;
        };
        if first && anchored && found != 0 {
            return false;
        }
        if needs_content && found == 0 && !lit.is_empty() {
            return false;
        }
        pos += found + lit.len();
        first = false;
        needs_content = content;
    }
    let Some(found) = text[pos..].find(&tail) else {
        return false;
    };
    !(needs_content && found == 0 && !tail.is_empty())
}

/// `kind` was paged `times` times in `log`, each page a line `alerts.md`'s filter matches, with no
/// colour escape in it, and one of the shapes the runbooks quote for the kind holding for it.
pub fn assert_page(log: &PageLog, kind: &str, times: usize) {
    let lines = page_lines(log, kind);
    assert_eq!(
        lines.len(),
        times,
        "the filter `{}` should match {times} ERROR line(s) of the log; the log:\n{}",
        filter_pattern(kind),
        log.text()
    );
    let shapes = documented_shapes(kind);
    assert!(
        !shapes.is_empty(),
        "no runbook under deploy/runbooks/pages/ lists `{kind}` and quotes the shape of its page line"
    );
    for line in &lines {
        assert!(
            !line.contains('\u{1b}'),
            "a colour escape in a page: {line:?}"
        );
        assert!(
            shapes.iter().any(|shape| matches_shape(shape, line)),
            "the runbooks quote the page of `{kind}` as {shapes:?}; this line has none of those shapes:\n{line}"
        );
    }
}
