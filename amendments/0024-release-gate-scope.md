<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0024 — which releases the full gate applies to

**Status:** ruled by @pandres95 on the decision form, 2026-11-06 (`d-gate-scope`): "As recommended". Applied to
`SPEC.md` v0.5.8 (§15, release scope) and `PLAN.md` v0.6.7 (`DEC-19`), and built in `tools/ci/release_gate.py`.
**Target:** `SPEC.md` v0.5.7 (§15, release scope); `PLAN.md` `DEC-19`
**Date:** 2026-11-06
**Needs:** nothing further. Numbered 0024 because 0023 is the last on `develop`, and 0016 and 0017 are held by pull
request #66.
**Rationale in one line:** §15 and `DEC-19` describe one release, the first, and the gate was built to ask every
release for the evidence of that one: a month of runbook exercises, a test-bed window and a 48-hour soak for a change
that moves no value would stop all work, and a gate that is always in the way is the one that gets bypassed.

## The gap

1. **What the specification says.** §15 gives V0's exit criteria and says "nothing else in this document may be traded
   against them". It speaks of one release and says nothing about the ones after it.
2. **What was built.** The release gate (`F-013`, `release/`) asks every release for all of it: every runbook exercised,
   the sustained window, every final leg, the soak, and the sign-off of the spec owner and a maintainer.
3. **What follows.** Either every later release repeats a proof whose point was *before any value*, or the gate is
   switched off for the later ones, and nothing says which of the two is the rule.

## The ruling

**The full gate applies only to a release that moves value: the first release that lets a bridged asset carry
value, and each release that adds a bridged asset or a network to what carries value. Every other release needs its
automated checks green and the sign-off of one spec owner.**

### How a release is classed

- **By an explicit field the gate reads.** `release/evidence.toml` carries a top-level `class`, either
  `"value-moving"` or `"ordinary"`. It is in the evidence because the evidence is one reviewed pull request, approved
  on its final commit by the sign-off roles (`release/README.md`): the class is attested by the people who approve it,
  and a release cannot choose its own without their review. The spec owner's approval is required in both classes, so
  an ordinary class on a release that should have been value-moving has to pass the person who knows.
- **A missing class is refused.** Absent, not a string, or any other value: the gate fails and runs no lighter path. It
  never defaults to `ordinary`.
- **The gate has one source for the class.** No workflow input or tag name carries it as well, so the two cannot
  disagree.
- **What each class needs**, both read from `release/gate.toml`, which the spec owner reviews:

| | `value-moving` | `ordinary` |
|---|---|---|
| Automated checks of `gate.toml` green on the release commit | yes | yes |
| Release commit named by the evidence, and only the evidence after it | yes | yes |
| Every runbook exercised on a test network (`NFR-9`) | yes | no |
| Reserve equation at zero divergence for 24 hours (amendment 0021), every final leg delivered, refunded or held | yes | no |
| The soak of `DEC-19` (48 hours, two verified daily statements, zero divergence) | yes | no |
| Every V0 criterion has a test and none is excepted or known unmet | yes | no |
| Sign-off | spec owner and a maintainer, two people | one spec owner |

An ordinary release still reads the pull request that merged its evidence, the approval of its final commit and the
reviewed list of spec owners (`release/roles.toml`), exactly as a value-moving release does. It skips only the evidence
of what was proven before value moved.

### What is left as it was, and what the ruling does not say

- The two lists the ruling does not settle, and how they were left: an ordinary release is held to the **same list of
  checks** as a value-moving one (so while the self-hosted pool is not configured, neither can pass), and it does
  **not** have to show the criteria map free of exceptions and known-unmet entries, since the map itself is green on a
  pull request with them. A reviewer who wants either tightened changes `gate.toml`.
- Deciding that a release moves value is the spec owner's call at review. The ruling names three cases and the gate
  does not try to detect them from the diff.
- **`SPEC.md` names no technology** (`CLAUDE.md`, guardrail 1): the specification says "bridged asset" and "network";
  the first asset to carry value (bUSDC) and the mechanism are in `PLAN.md` `DEC-19`.

## What changes

- **`SPEC.md` v0.5.8, §15, release scope:** the release classes and the rule above, in words that name no technology.
- **`PLAN.md` v0.6.7, `DEC-19`:** the mechanism (the `class` field, `gate.toml`'s two sign-off lists, refusal when it is
  missing) and that the first release is a value-moving one with bUSDC.
- **`release/gate.toml`, `release/evidence.toml` (shape), `release/roles.toml`, `release/README.md`,
  `tools/ci/release_gate.py` and its self-test.**
