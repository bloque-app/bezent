<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0020 — an attempt whose preparation never happens goes to review at the route's deadline

**Status:** proposed, not ruled. Nothing in `SPEC.md` is changed by this file, and the code of issue #48 does not depend on it:
`REQ-EO-3` already requires the behaviour, and this amendment only makes the transition table say so.
**Target:** `SPEC.md` v0.5.3 (§5.4, §5.2, `REQ-RV-1`)
**Date:** 2026-11-04
**Rationale in one line:** amendment 0013 names one occasion for an attempt that cannot be delivered, "the delivery attempt's
preparation was refused without a named condition and the route's deadline is past". An attempt whose preparation never
happens at all (the endpoint is down, so no preparation, refused or not, is ever written) is not "refused", yet `REQ-EO-3`
requires the final source leg to be delivered, refunded, or held in a named condition or `Needs-Review` with an owner.

## A1

In the §5.4 rows `Source-Final` -> `Needs-Review` and `Delivering`, `Delivered` -> `Needs-Review`, and in the matching text
of the §5.2 `Needs-Review` row and `REQ-RV-1`, "the delivery attempt's preparation was refused without a named condition"
becomes "the delivery attempt's preparation was refused without a named condition, or never happened". The deadline is the
same: the route's, counted from the attempt's own clock (the source leg's finality for the first attempt, the previous
attempt's proof for a later one).

Neither reading refunds, retries or fails the transfer (`REQ-RF-1`, `REQ-RV-1`); the review ends only by evidence
(`REQ-RV-2`).

## Scope

The occasion is read only where an endpoint prepares the delivery. A backend that has no endpoint preparing anything (the
attested backend of PR #66) has no preparation to be missing, and this amendment says nothing about it.

## Identifier ledger

| Action | Identifiers |
|---|---|
| Revised | §5.4 transitions `Source-Final` -> `Needs-Review` and `Delivering`, `Delivered` -> `Needs-Review`; the `Needs-Review` row of §5.2; `REQ-RV-1` |
