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
| Each sign-off's pull request was merged and approved by the person named | mechanical, `--online` | the pull request's reviews |
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
