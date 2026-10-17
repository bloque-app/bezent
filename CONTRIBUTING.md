<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Contributing to bezent

Thank you for your interest. bezent holds reserves and mints against them, so the bar for a change is that it can be
traced to a requirement, reviewed, and tested at the layer the requirement names. This guide says how to propose a
change at each level and what a pull request needs.

By taking part you agree to follow the `CODE_OF_CONDUCT.md`. Report security issues privately, as `SECURITY.md`
says, never in a public issue.

bezent is in progress and is not in production. Expect the plan and the code to move.

## How changes flow

Work is spec-driven. A change enters at the highest level it affects, and flows down:

    SPEC.md            what bezent does and why (no technology or network named)
      -> amendments/   a reviewed change to SPEC.md
      -> PLAN.md       how it is built: decisions (DEC-*), features (F-*), milestones (M-*)
        -> features/F-NNN-*/plan.md, tasks.md
          -> issue -> pull request -> code and tests

If code, plan and spec disagree, the spec wins. Behaviour that the spec does not describe is not built: a gap found
while coding becomes an amendment first. `CLAUDE.md` lists the vocabulary and the guardrails every change must
respect. It is written for coding agents, and it is the shortest complete statement of the project's rules.

## Which kind of change is yours?

| You want to… | Start with | Who decides |
|---|---|---|
| Report a bug: the code does not do what `SPEC.md` says | A **bug report** issue | Maintainers |
| Fix a bug, add a test, improve docs or tooling | A **pull request**, citing the issue | Maintainers |
| Change *how* something is built (a dependency, a data structure, a decision `DEC-*`) | A **plan change** issue, then a PR to `PLAN.md` or the feature's `plan.md` | Maintainers |
| Change *what* bezent does (a requirement, an invariant, an acceptance criterion) | An **amendment proposal** issue, then a PR adding `amendments/NNNN-*.md` | The spec owner (`GOVERNANCE.md`) |
| Answer an open question (`OQ-*` in `SPEC.md` §14) | An **amendment proposal** that closes it | The spec owner |

### Amendments

An amendment is one file, `amendments/NNNN-short-title.md`, numbered after the last one. It states the problem with
evidence, the exact text it changes in `SPEC.md` (before and after), the identifiers it adds, changes or withdraws
(identifiers are never renumbered or reused), what it costs, and its status: `Proposed`, then `Ruled` (with the date)
or `Declined`. Discussion happens on the pull request. When the spec owner rules it, the amendment is applied to
`SPEC.md` in the same PR and the version is bumped.

### Features and tasks

A feature lives in `features/F-NNN-name/`: `plan.md`, `tasks.md` (tasks `T-NNN-XX`, each tracing to a `REQ-*`, `INV-*`
or `AC-*`) and, where useful, `NOTES.md`. A task that traces to nothing in the spec is invalid.

## Pull requests

Before you open one:

    cargo fmt --all
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace             # the store tests need a PostgreSQL (DATABASE_URL)
    tools/oss/spdx.py --check          # licence headers (tools/oss/spdx.py --fix adds them)
    reuse lint

The contract (`contracts/bezent`), the EVM contracts (`contracts/evm`) and the test bed (`e2e/`) are workspaces of
their own; `.github/workflows/` shows how each is built and tested.

A pull request:

- does one thing, and cites the task (`T-NNN-XX`) and the requirement it serves (`REQ-*`, `INV-*`, `AC-*`) in its
  title or description, and its issue (`Closes #N` or `Part of #N`);
- names its tests after the `AC-*`, `INV-*`, `REQ-*` or `ERR-*` they verify, at the layer `SPEC.md` §9 names;
- adds no `unsafe` code and no floating point anywhere near an amount (`INV-11`);
- puts an SPDX header on every new file (`docs/oss/LICENSING.md`): services, the store and the ink! contract are
  AGPL-3.0-or-later, the other code Apache-2.0 OR MIT, documents CC-BY-4.0;
- is signed off (below).

A maintainer reviews it. `CODEOWNERS` routes reviews: changes to `SPEC.md` and `amendments/` need the spec owner's
approval. Workflows that need secrets do not run them for pull requests from forks; the tests fall back to public
endpoints.

## Sign-off: the Developer Certificate of Origin

Every commit must carry a `Signed-off-by:` line with your real name and email, which certifies the
[Developer Certificate of Origin 1.1](https://developercertificate.org/): you wrote the change, or have the right to
submit it under the project's licences. `git commit -s` adds it. Contributions are accepted under the licence of the
files they change (inbound = outbound). There is no separate contributor licence agreement (`GOVERNANCE.md`).

The `DCO` check (`.github/workflows/dco.yml`) fails a pull request with a commit whose `Signed-off-by:` does not name
that commit's author. To fix one, amend it with `git commit --amend -s`, or sign off a whole branch with
`git rebase --signoff <base>`, and push again. Do not override your git identity or signing configuration.

## Questions

Open a discussion, or an issue labelled `question`. Please do not email maintainers privately about anything except
security.
