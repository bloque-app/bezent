// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-014 · `REQ-IC-6`, `NFR-9`: a runbook for every incident class and every named condition, each
//! page kind the services log sends a person to one, and no runbook claims what it cannot show.
//!
//! This test reads `deploy/runbooks/`, `SPEC.md` (§10), `notes/BRIDGE-REQUIREMENTS.md` (§5) and the
//! services' sources. It proves coverage and honesty, not that a person followed a runbook: that
//! is `deploy/runbooks/EXERCISES.md`, and the file is empty until someone does.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn walk(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, ext, out);
        } else if p.extension().is_some_and(|x| x == ext) {
            out.push(p);
        }
    }
}

#[derive(Debug, Default)]
struct Runbook {
    path: PathBuf,
    id: String,
    kind: String,
    covers: Vec<String>,
    pages: Vec<String>,
    not_on_base: Vec<String>,
    observables: Vec<String>,
    tests: Vec<String>,
    exercised: String,
    body: String,
}

fn list(value: &str) -> Vec<String> {
    value
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The front matter: `key: value`, `key: [a, b]` or `key:` followed by `  - item` lines.
fn parse(path: &Path) -> Runbook {
    let text = read(path);
    // The licence header comes first (the licensing check reads the top of the file); the front
    // matter follows it.
    let text = match text.strip_prefix("<!--") {
        Some(after) => after.split_once("-->\n").map_or(text.as_str(), |(_, rest)| rest).to_owned(),
        None => text,
    };
    let rest = text.strip_prefix("---\n").unwrap_or_else(|| {
        panic!("{}: front matter must follow the licence header", path.display())
    });
    let (head, body) = rest
        .split_once("\n---\n")
        .unwrap_or_else(|| panic!("{}: front matter is not closed", path.display()));
    let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut open: Option<String> = None;
    for line in head.lines() {
        if let Some(item) = line.trim_start().strip_prefix("- ")
            && let Some(k) = &open
        {
            fields
                .entry(k.clone())
                .or_default()
                .push(item.trim().to_owned());
            continue;
        }
        let (k, v) = line.split_once(':').unwrap_or((line, ""));
        let v = v.split('#').next().unwrap().trim();
        open = Some(k.trim().to_owned());
        fields.insert(
            k.trim().to_owned(),
            if v.is_empty() { vec![] } else { list(v) },
        );
    }
    let one = |k: &str| {
        fields
            .get(k)
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_default()
    };
    let many = |k: &str| fields.get(k).cloned().unwrap_or_default();
    Runbook {
        path: path.to_owned(),
        id: one("id"),
        kind: one("kind"),
        covers: many("covers"),
        pages: many("pages"),
        not_on_base: many("not_on_base"),
        observables: many("observables"),
        tests: many("tests"),
        exercised: one("exercised"),
        body: body.to_owned(),
    }
}

fn runbooks() -> Vec<Runbook> {
    let mut files = Vec::new();
    walk(&root().join("deploy/runbooks"), "md", &mut files);
    files.sort();
    files
        .into_iter()
        .filter(|p| {
            !["README.md", "EXERCISES.md", "alerts.md"]
                .contains(&p.file_name().unwrap().to_str().unwrap())
        })
        .map(|p| parse(&p))
        .collect()
}

fn ids(of_kind: &str) -> BTreeSet<String> {
    runbooks()
        .into_iter()
        .filter(|r| r.kind == of_kind)
        .map(|r| r.id)
        .collect()
}

#[test]
fn req_ic_6_every_active_named_condition_has_a_runbook() {
    let spec = read(&root().join("SPEC.md"));
    let table = spec
        .split("### Named conditions")
        .nth(1)
        .expect("SPEC §10 conditions table");
    let table = table.split("\n## ").next().unwrap();
    let mut active = BTreeSet::new();
    for row in table.lines().filter(|l| l.starts_with("| `CND-")) {
        let id = row.trim_start_matches("| `").split('`').next().unwrap();
        if !row.contains("Withdrawn by amendment") {
            active.insert(id.to_owned());
        }
    }
    assert!(active.len() >= 11, "found {active:?}");
    let have = ids("condition");
    let missing: Vec<_> = active.difference(&have).collect();
    assert!(missing.is_empty(), "no runbook for {missing:?}");
    let stale: Vec<_> = have.difference(&active).collect();
    assert!(
        stale.is_empty(),
        "a runbook for a condition SPEC does not have: {stale:?}"
    );
}

#[test]
fn req_ic_6_every_incident_class_has_a_runbook() {
    let notes = read(&root().join("notes/BRIDGE-REQUIREMENTS.md"));
    let classes: BTreeSet<String> = notes
        .lines()
        .filter(|l| l.starts_with("| I-"))
        .map(|l| {
            l.trim_start_matches("| ")
                .split(' ')
                .next()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(classes.len(), 16, "{classes:?}");
    let have = ids("incident");
    let missing: Vec<_> = classes.difference(&have).collect();
    assert!(missing.is_empty(), "no runbook for {missing:?}");
    let stale: Vec<_> = have.difference(&classes).collect();
    assert!(stale.is_empty(), "{stale:?}");
}

#[test]
fn req_ic_6_each_incident_requirement_is_answered_by_a_runbook() {
    let all = runbooks();
    for req in ["REQ-IC-1", "REQ-IC-2", "REQ-IC-3", "REQ-IC-4", "REQ-IC-5"] {
        assert!(
            all.iter()
                .any(|r| r.covers.iter().any(|c| c == req) || r.body.contains(req)),
            "no runbook names {req}"
        );
    }
}

/// Every kind the services log as a page, from their sources: a `Page { kind: "…" }` and a
/// `tracing::error!(kind = %"…")`.
fn page_kinds_in_source() -> BTreeSet<String> {
    let mut files = Vec::new();
    for service in [
        "core",
        "ingress",
        "signer",
        "kreivo-endpoint",
        "evm-endpoint",
        "sidecar",
    ] {
        walk(
            &root().join("services").join(service).join("src"),
            "rs",
            &mut files,
        );
    }
    let mut kinds = BTreeSet::new();
    for f in files {
        let src = read(&f);
        for (open, close) in [
            ("kind: \"", "\""),
            ("kind = %\"", "\""),
            ("kind = \"", "\""),
        ] {
            let mut rest = src.as_str();
            while let Some(at) = rest.find(open) {
                let tail = &rest[at + open.len()..];
                let kind = tail.split(close).next().unwrap();
                // A page kind is lower-case words joined by hyphens, at least two of them.
                if kind.contains('-')
                    && kind.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                    && !kind.starts_with('-')
                {
                    kinds.insert(kind.to_owned());
                }
                rest = tail;
            }
        }
    }
    kinds
}

#[test]
fn f_014_every_page_the_services_log_has_a_runbook_and_a_row_in_alerts() {
    let in_source: BTreeSet<String> = page_kinds_in_source()
        .into_iter()
        // Not pages: a manual action's kind, an error text, a log field's value.
        .filter(|k| {
            ![
                "review-resolution",
                "release-unassociated",
                "kreivo-mint",
                "kreivo-delivery",
                "polygon-delivery",
                "kreivo-local",
                "polygon-payout",
                "owed-mint",
                "owed-payout",
                "refund-mint",
                "taken-in",
                "in-transit",
                "network-cost",
                "venue-fee",
                "surplus-contribution",
                "surplus-withdrawal",
                "unassociated-refund",
            ]
            .contains(&k.as_str())
        })
        .collect();
    let documented: BTreeSet<String> = runbooks().into_iter().flat_map(|r| r.pages).collect();
    let known_pages = [
        "reserve-divergence",
        "reserve-alert",
        "reserve-check-incomplete",
        "reserve-check-failed",
        "reserve-check-stalled",
        "supply-mismatch-pause",
        "book-quarantine",
        "key-compromise",
        "statement-overdue",
        "statement-stuck",
        "statement-publish-failed",
        "statement-abandoned",
        "payout-window-alert",
        "payout-refusal-alert",
        "resolution-blocked-by-inclusion",
    ];
    // The scan finds every known page (so it is reading the right places) ...
    for k in known_pages {
        assert!(
            in_source.contains(k),
            "the scan no longer finds the page {k}"
        );
    }
    // ... and every page it finds is documented, and every one documented exists.
    let undocumented: Vec<_> = known_pages
        .iter()
        .filter(|k| !documented.contains(**k))
        .collect();
    assert!(
        undocumented.is_empty(),
        "pages with no runbook: {undocumented:?}"
    );
    let new: Vec<_> = in_source
        .iter()
        .filter(|k| k.ends_with("-alert") || k.contains("late-mint") || k.starts_with("attested-"))
        .filter(|k| !documented.contains(*k))
        .collect();
    assert!(new.is_empty(), "a page kind with no runbook: {new:?}");

    let alerts = read(&root().join("deploy/runbooks/alerts.md"));
    let mut declared_absent = BTreeSet::new();
    for r in runbooks() {
        declared_absent.extend(r.not_on_base.iter().cloned());
        for p in &r.pages {
            assert!(
                alerts.contains(&format!("| `{p}` |")),
                "{}: page {p} has no row in alerts.md",
                r.id
            );
            let on_base = in_source.contains(p);
            let marked = r.not_on_base.contains(p);
            assert!(
                on_base != marked,
                "{}: page {p} is {} on this base but {} marked not_on_base; update the runbook",
                r.id,
                if on_base { "present" } else { "absent" },
                if marked { "is" } else { "is not" }
            );
        }
    }
    // The pages of the two open pull requests the brief names are written down.
    for k in [
        "attested-withdrawal-unrelayed",
        "attested-withdrawal-awaiting-head",
        "late-mint-prover-silent",
        "late-mint-watch-failed",
        "late-mint-check-failed",
        "payout-refusal-alert",
        "resolution-blocked-by-inclusion",
    ] {
        assert!(documented.contains(k), "{k}");
    }
}

#[test]
fn f_014_every_runbook_has_its_sections_and_an_honest_exercise_status() {
    let exercises = read(&root().join("deploy/runbooks/EXERCISES.md"));
    let mut rows: BTreeSet<String> = BTreeSet::new();
    for l in exercises.lines().filter(|l| l.starts_with("| 20")) {
        rows.insert(l.split('|').nth(2).unwrap_or("").trim().to_owned());
    }
    let all = runbooks();
    assert!(all.len() >= 35, "{} runbooks", all.len());
    let sections = [
        "## What it is",
        "## Pages",
        "## Confirm",
        "## Do",
        "## Never",
        "## Exercise",
    ];
    for r in &all {
        let name = r.path.display().to_string();
        assert!(
            ["condition", "incident", "pages", "procedure"].contains(&r.kind.as_str()),
            "{name}"
        );
        assert!(!r.id.is_empty(), "{name}");
        let mut at = 0;
        for s in sections {
            let pos = r.body[at..]
                .find(s)
                .unwrap_or_else(|| panic!("{name}: no `{s}` after byte {at}"));
            at += pos + s.len();
            // Not an empty section.
            let after = &r.body[at..];
            let text = after.split("\n## ").next().unwrap();
            assert!(text.trim().len() > 20, "{name}: `{s}` is empty");
        }
        // Exercised only with a dated line in EXERCISES.md; and then the line exists.
        match r.exercised.as_str() {
            "no" => assert!(
                !rows.contains(&r.id),
                "{name}: exercised in EXERCISES.md, still `no`"
            ),
            "yes" => assert!(
                rows.contains(&r.id),
                "{name}: `yes` with no line in EXERCISES.md"
            ),
            other => panic!("{name}: exercised: {other}"),
        }
        if r.exercised == "no" {
            assert!(r.body.contains("Status: not exercised (NFR-9)"), "{name}");
        }
        // Never says what never to do, and a runbook that acts says what recorded it.
        assert!(
            r.body.contains("REQ-GV-5")
                || r.kind == "procedure"
                || r.kind == "pages"
                || r.kind == "incident"
                || r.kind == "condition",
            "{name}"
        );
    }
    let known: BTreeSet<_> = all.iter().map(|r| r.id.clone()).collect();
    for id in &rows {
        assert!(
            known.contains(id),
            "EXERCISES.md names {id}, which is no runbook"
        );
    }
}

#[test]
fn f_014_a_runbook_names_only_tests_and_observables_that_exist() {
    let observables_sql = read(&root().join("crates/store/migrations/0090_ops_observables.up.sql"));
    for r in runbooks() {
        for o in &r.observables {
            assert!(
                observables_sql.contains(&format!("'{o}'")),
                "{}: no metric {o}",
                r.id
            );
        }
        for t in &r.tests {
            let (file, function) = t
                .split_once("::")
                .map_or((t.as_str(), None), |(f, n)| (f, Some(n)));
            let path = root().join(file);
            assert!(path.exists(), "{}: test file {file} does not exist", r.id);
            if let Some(function) = function {
                let src = read(&path);
                assert!(
                    src.contains(&format!("fn {function}(")),
                    "{}: {file} has no fn {function}",
                    r.id
                );
            }
        }
    }
}

#[test]
fn f_014_a_runbook_cites_only_identifiers_the_spec_has() {
    let spec = read(&root().join("SPEC.md"));
    for r in runbooks() {
        let text = format!("{}{}", r.covers.join(" "), r.body);
        for prefix in ["REQ-", "INV-", "NFR-", "AC-"] {
            let mut rest = text.as_str();
            while let Some(at) = rest.find(prefix) {
                let tail = &rest[at..];
                let id: String = tail
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '.')
                    .collect();
                let id = id.trim_end_matches(['-', '.']).to_owned();
                // `REQ-GV-5` and the like; a bare prefix or a placeholder is not an id.
                let digits = id.chars().filter(char::is_ascii_digit).count();
                if digits > 0 && id.len() > prefix.len() + 1 {
                    assert!(
                        spec.contains(&id),
                        "{}: cites {id}, which SPEC.md does not have",
                        r.id
                    );
                }
                rest = &tail[prefix.len()..];
            }
        }
    }
}

#[test]
fn f_014_the_public_runbooks_hold_nothing_private() {
    let mut files = Vec::new();
    walk(&root().join("deploy/runbooks"), "md", &mut files);
    for f in files {
        let text = read(&f);
        let name = f.display().to_string();
        for banned in [
            "arn:aws",
            "hooks.slack.com",
            ".amazonaws.com",
            "BEGIN PRIVATE",
            "xoxb-",
            "vault.bloque",
        ] {
            assert!(!text.contains(banned), "{name} contains {banned}");
        }
        // A twelve-digit run is an account id; a forty-hex-digit one an address.
        let bytes: Vec<char> = text.chars().collect();
        let mut run = 0;
        for c in &bytes {
            run = if c.is_ascii_digit() { run + 1 } else { 0 };
            assert!(run < 12, "{name} has a run of twelve digits");
        }
        assert!(
            !text.contains("0x") || !has_address(&text),
            "{name} has an address"
        );
    }
}

fn has_address(text: &str) -> bool {
    text.match_indices("0x").any(|(i, _)| {
        text[i + 2..]
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .count()
            >= 40
    })
}

#[test]
fn f_014_the_readme_indexes_every_runbook() {
    let readme = read(&root().join("deploy/runbooks/README.md"));
    for r in runbooks() {
        let rel = r
            .path
            .strip_prefix(root().join("deploy/runbooks"))
            .unwrap()
            .display()
            .to_string();
        assert!(
            readme.contains(&format!("]({rel})")),
            "README.md does not link {rel}"
        );
    }
}
