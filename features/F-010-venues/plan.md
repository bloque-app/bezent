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

Left, and waiting for a ruling: the record of the convertible arrival with both amounts (`REQ-CV-2`, last sentence) and the
condition `CND-AwaitingConversion` held by an arrival. `amendments/0018-conversion-record.md` is the proposal; nothing is
built on it.

## Done when

A convertible deposit is minted for what the swap delivered into the vault (`AC-A5.1`–`AC-A5.3`). Until amendment 0018 is
ruled and built, issue #10 stays open.
