<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-001 — tasks

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-001-01 | Workspace layout of `DEC-1`, pinned toolchain, SPDX headers, empty service binaries that start and stop | `DEC-1`, `DEC-20` | `cargo build --workspace` and `cargo clippy --workspace -- -D warnings` are green |
| T-001-02 | `crates/core` types: amount newtype (checked, integer only), asset and network ids, the three reference newtypes, coordinates | `INV-11`, `REQ-ID-3` | Unit tests: overflow is an error, not a wrap; a compile-fail test shows a request reference cannot be passed where a bridge reference is expected |
| T-001-03 | `sqlx` migrations for intake, transfers, transitions, observations, reserve book, intents, event log | `DEC-4`, §5.3, §5.4 | Migrations apply on an empty database and are reversible in tests only |
| T-001-04 | Triggers: append-only tables, write-once milestones, frozen terminal transfers, balanced postings, gapless log | `INV-5`, `INV-10`, `INV-17`, `REQ-TR-2` | One store test per refusal, each named after its invariant |
| T-001-05 | Restore drill test | `NFR-7` | Dump, restore into a fresh database, derived views identical |
| T-001-06 | CI: `.github/workflows/ci.yml` on `ubuntu-latest` with a Postgres service; build, fmt, clippy, tests | `DEC-1` | Green on the PR |
