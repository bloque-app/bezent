<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0005 — open questions closed on 2026-10-13

**Status:** ruled by the owner on the decision form, 2026-10-13 (`oq-2`, `oq-3`, `oq-6`, `oq-7`, `oq-8`, `oq-11`,
`oq-12`). Applied to `SPEC.md` v0.3.3 together with amendment 0004 (`br-dust-rule`).
**Target:** `SPEC.md` v0.3.2
**Date:** 2026-10-13
**Rationale in one line:** seven of the eight open questions are answered. `OQ-14`, a reversal after a reported-final
payment, stays open for discussion (`gen-reversal`).

## A1 — Fast path (`oq-2`)

`OQ-2` is closed: there is no fast path in V0. `DEF-1` stays deferred.

## A2 — Who resolves reviews (`oq-3`)

`REQ-RV-1` gains: "The owner of `Needs-Review`, `CND-Unassociated` and `CND-AwaitingInstruction` is the operator's
operations function, and the service level is one business day." `OQ-3` is closed.

## A3 — Bounds (`oq-6`)

`OQ-6` is closed: each asset's supply ceiling, mint window and per-transfer maximum are sized
from the requester's current volumes, and the plan records the values. No number is added to the spec.

## A4 — Completion budgets (`oq-7`)

`NFR-2` gains: "Each budget is first measured on a test network that reproduces the route, then published." `OQ-7`
is closed.

## A5 — Surplus (`oq-8`)

Add **`REQ-BK-10`**: "The operator MUST keep each asset's surplus above a floor it declares, and an alert MUST fire
before the floor is reached. The operator tops up the surplus from its own funds." `OQ-8` is closed.

## A6 — Statements (`oq-11`)

`REQ-PR-1`'s "on a stated cadence" becomes "at least daily", and the statement is "signed by a key used for nothing
else". `OQ-11` is closed.

## A7 — Volume (`oq-12`)

`NFR-10` becomes: "bezent MUST sustain, without redesign, ten times the requester's volume at the start of V0:
transfers a day, peak per minute and deposit addresses outstanding. The plan records the figures." `OQ-12` is
closed.

## Identifier ledger

| Action | Identifiers |
|---|---|
| New | `REQ-BK-10` |
| Revised | `REQ-RV-1`, `REQ-PR-1`, `NFR-2`, `NFR-10` |
| Closed | `OQ-2`, `OQ-3`, `OQ-6`, `OQ-7`, `OQ-8`, `OQ-11`, `OQ-12` |
