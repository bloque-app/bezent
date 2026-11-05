<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-010 — Venues: Swaps' conversions on the vault

Milestone `M-2` (issue #10); a gate of the bUSDC launch (`M-4`, `pl-busdc-first`). Serves `REQ-CV-1`, `REQ-CV-2`, `REQ-CV-4`
to `REQ-CV-7`, `AC-A5.1`–`AC-A5.3` (plan `DEC-10`, `DEC-13`). Read SPEC v0.5.4 §5.10 and PLAN `DEC-13`.

## Scope

Only Swaps' conversion role on the Polygon vault, and minting against its output. CCTP left bezent (amendment 0007 A5):
`REQ-OT-4` is revised, `REQ-VN-6` and `US-F3` are withdrawn, and `REQ-CV-3` is withdrawn with `AC-B4.1` and `AC-B4.2`. None of
them is built here. The Base and BNB Chain vaults carry no conversion role (`DEC-13`, `pl-convertibles`).

## What it builds

Mostly nothing new: `F-008`, `F-009`, `F-002`, `F-003` and `F-004` already built the role, the observation, the binding, the
mint and the statement (see `NOTES.md`). This feature adds the tests that name `AC-A5.1`–`AC-A5.3` at the layers §9 names,
including a real swap on the Polygon fork, and records what is left.

Then amendment 0018 as ruled (`d-cv-conversion-link`, 2026-11-05: A1b, the request comes first): a deposit may name a
convertible arrival and holds `CND-AwaitingConversion` at `Source-Final`; its requester names the conversion's output by
the deposit's request reference; the store moves the deposit's source to the output once and records the arrival with both
amounts (migration 0120); the core mints against the output, or sends the deposit to review past its deadline. What the
ruling left open is in the amendment ("Open after the ruling").

## Done when

A convertible deposit is minted for what the swap delivered into the vault (`AC-A5.1`–`AC-A5.3`), a deposit naming a
convertible arrival records it with both amounts (`REQ-CV-2`), and its deadline sends it to review (`REQ-CV-6`).
