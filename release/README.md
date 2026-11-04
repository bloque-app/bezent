<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Release gate

`tools/ci/release_gate.py` (workflow `release-gate.yml`, on a `v*` tag or by hand) decides whether a commit may carry
value (`SPEC.md` section 15, `PLAN.md` `DEC-19`, `M-4`). It fails closed.

| Item | Kind | Where it comes from |
|---|---|---|
| Every V0 acceptance criterion has a test at its layer; none is excepted | mechanical | `tools/ci/criteria_map.py`, `e2e/criteria/` |
| Every check in `gate.toml` succeeded on the release commit | mechanical, `--online` | check runs of the commit |
| Each sign-off: the pull request that merged this release's `evidence.toml`, approved on its final commit by the person named, who is in the role's list; the two roles are two people | mechanical, `--online` | the pull request, its files and reviews; `roles.toml` |
| Every runbook exercised on a test network (`NFR-9`): one per named condition and per incident class | human, needs a live test bed | `evidence.toml` `[runbook."CND-..."]`, `[runbook."I-n"]` |
| Reserve equation at exactly zero divergence for the sustained window | human, needs a live test bed running the core | `evidence.toml` `[exit.divergence]`; the window is `gate.toml` `[exit]`, **unruled** (amendment 0021) |
| Every final source leg delivered, refunded or held with an owner | human, needs a live test bed | `evidence.toml` `[exit.legs]` |
| 48 hours on mainnet, two verified daily statements, zero divergence | human, needs mainnet | `evidence.toml` `[soak]` |
| Sign-off of the spec owner and a maintainer | human | `evidence.toml` `[signoff.*]` |

The evidence is one reviewed pull request that changes `release/evidence.toml` and nothing else: `release_commit` (the
full sha of the code released) and an entry per item with an `https` link to what was run, who ran it (`by`) and when
(`on`). A file cannot hold the hash of its own commit, so the gate runs on that later commit and requires the release
commit to be its ancestor with only `evidence.toml` between them. Before the gate, dispatch `ci`, `contracts`,
`evm-contracts`, `e2e-evm` and `e2e-zombienet` on the release commit: the gate reads their checks there and a missing
or skipped one fails. The `DCO` check runs on pull requests only, so the gate does not read it.

## Sign-off: who counts

A sign-off in `evidence.toml` names a `by` and a `pr`. The gate accepts it only if all of these hold:

- the pull request is merged, changes `release/evidence.toml`, and was merged as a commit that follows the release
  commit and is in the history of the commit checked out; the file at that merge is the file being gated (an
  earlier pull request, or an edit after the merge, signs nothing);
- `by` has a standing approval of the pull request's final commit (a comment does not withdraw an approval; a
  dismissal or a change request does; reviews are read in full, 100 to a page);
- `by` is listed for the role in `release/roles.toml`;
- the spec owner and the maintainer are different people.

**Team membership is a reviewed list, not the API.** The workflow reads GitHub with its own `GITHUB_TOKEN`, which
holds no organisation scope (a workflow cannot be given the permission to read a team's members), so the gate
cannot ask whether `by` is in `@bloque-app/bezent-spec-owner` or `@bloque-app/bezent-maintainers`. `roles.toml` is the
copy of those two teams that the spec owner reviews (`/release/` is theirs in CODEOWNERS); the two have to be kept
equal by hand, and an empty or missing list fails the gate. Reading the teams live would need a token with
`read:org`, a secret the gate deliberately does not hold.

## Running the gate

- **It blocks until `ZOMBIENET_RUNS_ON` is set.** The `Kreivo test bed` check (and the zombienet jobs) run on the
  self-hosted pool only when that repository variable names it; unset, they are skipped, and the gate treats a
  skipped check as a failure. A release cannot pass while the pool is not configured.
- **The workflow must be on the default branch to be dispatched.** GitHub offers `workflow_dispatch` only for
  workflows on the default branch. On a `v*` tag push it runs the copy in the tagged commit.
- **Nothing consumes its result yet**, and the gate runs the tagged commit's own code. See the PR description for
  the repository settings (required checks, tag rules, a protected environment) that make it bind; they are
  settings, not files, and are the maintainers' to change.
