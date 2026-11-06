<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Release gate

`tools/ci/release_gate.py` (workflow `release-gate.yml`, on a `v*` tag or by hand) decides whether a commit may carry
value (`SPEC.md` section 15, `PLAN.md` `DEC-19`, `M-4`). It fails closed.

## Two classes of release (amendment 0024)

The full gate binds only a release that **moves value**: the first release of bUSDC, and each release that adds an asset
or a network (`d-gate-scope`, ruled 2026-11-06). Every other release is **ordinary** and needs its checks green and one
spec owner's sign-off. The release says which it is in the top-level `class` of `evidence.toml`, `"value-moving"` or
`"ordinary"`, and that is the gate's only source for it. **A missing, mistyped or unknown class is refused**: it is never
read as ordinary, and an unclassed release is held to the value-moving requirements while it is reported. The evidence is
one pull request that the spec owner approves, so the class is attested by that review.

| Requirement | `value-moving` | `ordinary` |
|---|---|---|
| Criteria map holds, with no exception and no known-unmet entry | yes | the map holds; exceptions and known-unmet entries are allowed, as on a pull request |
| Every check in `gate.toml` succeeded on the release commit | yes | yes |
| Release commit named in the evidence; only the evidence after it | yes | yes |
| Every runbook exercised, the exit criteria on the test bed, the soak | yes | no |
| Sign-off (`gate.toml`, `[class.*].signoff`) | spec owner and a maintainer, two people | one spec owner |

The table below is the value-moving gate; an ordinary release reads its first three items' mechanical halves and the
sign-off row.

| Item | Kind | Where it comes from |
|---|---|---|
| Every V0 acceptance criterion has a test at its layer; none is excepted or known unmet | mechanical | `tools/ci/criteria_map.py`, `e2e/criteria/` |
| Every check in `gate.toml` succeeded on the release commit | mechanical, `--online` | check runs of the commit |
| Each sign-off: the pull request that merged this release's `evidence.toml`, approved on its final commit by the person named, who is in the role's list; the two roles are two people | mechanical, `--online` | the pull request, its files and reviews; `roles.toml` |
| Every runbook exercised on a test network (`NFR-9`): one per named condition and per incident class | human, needs a live test bed | `evidence.toml` `[runbook."CND-..."]`, `[runbook."I-n"]` |
| Reserve equation at exactly zero divergence for the sustained window: 24 hours | human, needs a live test bed running the core | `evidence.toml` `[exit.divergence]`; the window is `gate.toml` `[exit]` (ruled 2026-11-06, amendment 0021) |
| Every final source leg delivered, refunded or held with an owner | human, needs a live test bed | `evidence.toml` `[exit.legs]` |
| 48 hours on mainnet, two verified daily statements, zero divergence | human, needs mainnet | `evidence.toml` `[soak]` |
| Sign-off of the spec owner and a maintainer (an ordinary release: one spec owner) | human | `evidence.toml` `[signoff.*]`; the roles of the class are `gate.toml` `[class.*]` |

The evidence is one reviewed pull request that changes `release/evidence.toml` and nothing else: `release_commit` (the
full sha of the code released) and an entry per item with an `https` link to what was run, who ran it (`by`) and when
(`on`). A file cannot hold the hash of its own commit, so the gate runs on that later commit and requires the release
commit to be its ancestor with only `evidence.toml` between them. Before the gate, dispatch `ci`, `contracts`,
`evm-contracts`, `e2e-evm` and `e2e-zombienet` on the release commit: the gate reads their checks there and a missing
or skipped one fails. The `DCO` check runs on pull requests only, so the gate does not read it.

## Known unmet criteria

`e2e/criteria/known_unmet.toml` lists criteria that have a test at their layer yet are known not met (partial or
failing), each with what is missing and the issue or pull request that closes it. The criteria map cannot tell a test
that names a criterion from a criterion that is met, so this list is where that is said. The map reports such a
criterion as `known unmet` and stays green on a pull request, as for an exception; the release gate fails a
**value-moving** release while any entry remains (an ordinary release needs the map itself to hold, amendment 0024). Removing an entry claims that the criterion is now met, and needs the spec owner's review (the file is
theirs in `CODEOWNERS`). An entry naming a criterion that is not in `SPEC.md` is an error.

## Sign-off: who counts

A sign-off in `evidence.toml` names a `by` and a `pr`. The gate accepts it only if all of these hold:

- the pull request is merged, changes `release/evidence.toml`, and was merged as a commit that follows the release
  commit and is in the history of the commit checked out; the file at that merge is the file being gated (an
  earlier pull request, or an edit after the merge, signs nothing);
- `by` has a standing approval of the pull request's final commit (a comment does not withdraw an approval; a
  dismissal or a change request does; reviews are read in full, 100 to a page);
- `by` is listed for the role in `release/roles.toml`;
- the roles of the release's class are different people (a value-moving release: the spec owner and a maintainer; an
  ordinary one has only the spec owner). GitHub does not let an author approve their own pull
  request, so the evidence is authored by a third person and approved by the signers.

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
- **Nothing consumes its result yet**, and the gate runs the tagged commit's own code (see below).

## Settings that make the gate bind

Files cannot do these; they are repository settings, and the maintainers' to change. Until they are made, the gate
is a report: it runs, but nothing stops a release it fails, and a release commit can change the gate it is judged by.

1. **`main` and `develop`** (ruleset or branch protection): require pull requests with a review by the code owners
   (this is what makes `CODEOWNERS` bind, including the lines for `tools/ci/`, `release-gate.yml`, the incident classes the gate reads
   (`notes/BRIDGE-REQUIREMENTS.md`) and `CODEOWNERS` itself); dismiss stale approvals; require the checks of `release/gate.toml` that run on pull
   requests, above all `criteria map and release gate self-test`; no bypass actors, and `enforce_admins` on.
2. **Tags `v*`** (tag ruleset): creation, update and deletion limited to named people, so that the tag the gate
   judges cannot be moved after it passes.
3. **A consumer of the result.** `images.yml` (`publish`, on `main`) and any mainnet deployment should run only
   after `release gate` succeeded on the same commit: a protected environment with required reviewers (the spec
   owner), and `needs:` or `workflow_run` on `Release gate`. Today the image build does not read it.
4. **`ci.yml` stays under `*`.** The self-test job lives in a file whose other jobs the maintainers own, so its
   ownership cannot be narrowed by path; the required-check setting in (1) is what stops a pull request from
   removing the job.
