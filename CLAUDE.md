<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# CLAUDE.md

Operating instructions for coding agents (and a short statement of the rules for people) working in this repository.
Not documentation: `SPEC.md` is the source of truth. Read it before doing anything substantive.

## Name

The product is **bezent**, always lowercase in prose. Use it in new docs, READMEs and UI copy. Brand tokens: Familjen
Grotesk, Geist, Geist Mono; ink #0d0c17, paper #fbfaff; symbol = arch on two piers with the stop crossing over
(`brand/README.md`).

## What this project is

Bloque's trusted, mirrored bridge: reserves held on origin networks (Polygon, Base), a bridged asset
minted on one issuing network (Kreivo) by an on-chain component (an ink! contract), a requester (Swaps)
that decides what moves and where. It is release R0 of the message-backed Kreivo Bridge
(`notes/CONSTRAINTS-FOR-PLAN.md`).

## Spec-driven development

The chain is `research/` → `notes/` → `SPEC.md` → `amendments/` → `PLAN.md` → `features/F-NNN-*/` → issue → PR.
Changes flow **down**, never up.

- Behaviour is decided in `SPEC.md`. If code, plan and spec disagree, the spec wins.
- `PLAN.md` MUST NOT contradict `SPEC.md`. A technical fact that makes a requirement unsatisfiable is an
  amendment first.
- Never implement behaviour that is not specified. Never resolve an `OQ-*` by implementing one of its
  options. Never widen scope past §12 and §13.
- Work inside one feature directory at a time.

## Traceability

Identifier families are in `SPEC.md` §0. Never renumber; add. Tasks are `T-<feature>-<seq>`. Tests name
the `AC-*` or `INV-*` they verify, at the layer §9 names (`REQ-AR-5`). `notes/TRACEABILITY.md` maps the
aledgent requirements note (`BR-*`) to the spec; keep it current when the spec changes.

## Vocabulary

| Say | Never say | Why |
|---|---|---|
| **issuing network** / **origin network** | home chain, remote chain, L1/L2 | The Kreivo Bridge spec uses "home chain" for the *origin*; avoid the collision |
| **bridged asset** / **origin token** | wrapped token, representation | One name each |
| **reserve** | backing, inventory, pool, balance | Reserve per (asset, origin network) |
| **deposit** / **withdrawal** | on-ramp, off-ramp, teleport | Ramps are the requester's; a teleport is not what Kreivo does |
| **escrow**, **destroy** | burn-first, lock | Value is escrowed until delivery is final |
| **rebalance** | netting, top-up, sweep | A sweep is within one network; a rebalance is between two |
| **observation / inclusion / finality** | confirmed, settled | Three distinct conditions (`REQ-TR-4`) |
| **request reference** / **bridge reference** / **on-chain reference** | id, key, hash | Three keys, never conflated (`REQ-ID-3`) |

## Guardrails

1. **`SPEC.md` names no technology or network.** Kreivo, Polygon, Base, evmhot, Swaps, CCTP and Binance
   appear only in `notes/`, `research/` and the plan.
2. **No mint before the arrival is final; no delivery before the escrow is final** (`INV-6`, `INV-7`).
3. **No caller asserts an outcome.** Requesters point at coordinates; the bridge verifies (`REQ-RQ-7`).
4. **No refund on a timer or on unknown** (`REQ-RF-1`, `REQ-RV-1`).
5. **Nothing is matched by amount or time** (`REQ-OT-2`, `REQ-RB-4`).
6. **No administrative path moves value or edits a status** (`REQ-GV-5`).
7. **Keys never live in the process that holds state** (`REQ-AR-3`); never in environment variables
   (`REQ-KY-1`).
8. **Every key is bounded on chain** (`REQ-KY-3`). An EOA treasury does not satisfy it.
9. **Never merge tokens of different issuers** into one bridged asset (`REQ-AS-3`). USDC.e is not USDC.
10. **No fee comes out of a delivery** (`REQ-TR-5`). Costs go against surplus.
11. **Do not build the EVM component inside substrate-rail.** aledgent replaces it.
12. **Rely on Swaps, but not for trust.** Reuse what Swaps does; never let it mint,
    assert an arrival, hold the bridge's keys, or stand in for the reserve equation.
13. **Asset Hub is never an origin network.** XCM serves it.

## Decisions

Open questions (`OQ-*`) and the rulings that close them are recorded in `amendments/`; @pandres95 keeps the
working decision form outside this repository. A ruling reaches `SPEC.md` or `PLAN.md` only through an amendment that
cites the decision it applies. Never apply an unruled decision, not even provisionally.

## House rules (every agent brief carries these)

- **Commit identity and signing.** Plain `git commit -s`; never pass `-c user.*`, `--author`, set `GIT_AUTHOR_*` or
  `GIT_COMMITTER_*`, or use `--no-gpg-sign`. Use the identity and signing key the repository's or the user's git
  configuration already has. If signing fails, stop and ask; do not fall back to an unsigned commit.
- **DCO.** Every commit carries `Signed-off-by:` naming its author (`git commit -s`; `CONTRIBUTING.md`). The `DCO`
  check fails a pull request otherwise.
- **Licences.** A new file gets its SPDX header (`tools/oss/spdx.py --fix`; `docs/oss/LICENSING.md` says which licence
  a path takes). Run `tools/oss/spdx.py --check` and `reuse lint` before pushing.
- **Builds.** One worktree and one build directory per agent. The build directory (`CARGO_TARGET_DIR`) lives outside
  the repository (for example `~/.cache/bezent-target-<agent>`), never under the checkout. Commit named paths only;
  never `git add -A` in a checkout where helpers build.
- **Scope.** Never edit `SPEC.md` or `PLAN.md` unless the task says so. On a spec gap, stop and propose an amendment.
  Work inside one feature directory at a time.
- **Merging.** Only when the task says so, with CI green, after rebasing on the current base.
- **Tracking.** One GitHub issue per feature (title `F-NNN · name`; issue numbers do not equal feature numbers), milestones
  `M-0`…`M-5` as in `PLAN.md` §5. Every pull request names its issue (`Closes #N` or `Part of #N`) and carries the
  milestone.
- **Secrets.** Never commit a secret, a token or a key. Workflows reference secrets only as `${{ secrets.NAME }}`;
  pull requests from forks do not receive them, so tests must pass on the public fallbacks. The signer's published
  test keys (`services/signer/fixtures/`) are test values and say so.
- **No durable work in `/tmp`.**
- **Private matters stay out.** Do not put private hostnames, account identifiers, credentials, people's real names
  (GitHub usernames are fine) or counterparties' terms in commits, issues, pull requests or documents.

## Scratch space

`.scratch/` is gitignored. Name files `YYYY-MM-DD-<slug>.md`; each says what removes it.

## Conventions

British English in `SPEC.md`, the plan and amendments. RFC 2119 keywords in normative text only. State
trade-offs plainly.
