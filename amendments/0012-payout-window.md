<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0012 — a payout waits when the reserve's outflow allowance is spent

**Status:** ruled by the owner on the decision form, 2026-10-15 (`sp-payout-window`). Applied to `SPEC.md` v0.5.2.
**Target:** `SPEC.md` v0.5.1 (§10)
**Date:** 2026-10-14
**Rationale in one line:** every reserve's outflows are bounded per window by the network (`REQ-RN-4`, `REQ-KY-3`). When
the window's allowance is spent, a payout cannot be signed until it refills. The mint side has `CND-MintWindow` for
the same situation; the payout side has no condition, so the EVM endpoint (`F-009`, PR #39) had nothing to report.

## A1

New named condition **`CND-PayoutWindow`**: "The reserve's outflow allowance for the current window is spent; the
withdrawal waits and resumes by itself when the allowance refills. Nothing is refunded or failed because of it."

## Identifier ledger

| Action | Identifiers |
|---|---|
| Added | `CND-PayoutWindow` |
