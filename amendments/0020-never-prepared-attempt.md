<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0020 — an attempt whose preparation never happens goes to review at the route's deadline

**Status:** proposed, not ruled. Nothing in `SPEC.md` is changed by this file. **The code of issue #48 depends on it, and PR #71
cannot merge until the owner rules.** `REQ-TR-6` makes the §5.4 table normative, and the rows `Source-Final` -> `Needs-Review`
and `Delivering`, `Delivered` -> `Needs-Review` allow the edge only for a refused preparation, or for an unknown leg or a
recorded anomaly. The store's trigger allows the edge; the occasion (a preparation that never happened) is not ruled.
`REQ-EO-3` requires the final source leg to be delivered, refunded or held in a named condition or `Needs-Review` with an
owner, which is the reason for the amendment, but it does not name this occasion.
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
same: the route's, counted from the later of the attempt's own clock (the source leg's finality for the first attempt, the
previous attempt's proof for a later one) and the moment the order for the attempt was written (on the destination network's
clock), so that an endpoint is never given less than the route's time to prepare an order it has just been sent.

Neither reading refunds, retries or fails the transfer (`REQ-RF-1`, `REQ-RV-1`); the review ends only by evidence
(`REQ-RV-2`).

## Proposed text

In the §5.4 row `Source-Final` -> `Needs-Review`, replace "or the delivery attempt's preparation was refused without a named
condition and the route's deadline is past (`REQ-EO-3`)" with:

> or the delivery attempt's preparation was refused without a named condition, or never happened, and the route's deadline,
> counted from the later of the source leg's finality and the moment the attempt was ordered, is past (`REQ-EO-3`)

In the row `Delivering`, `Delivered` -> `Needs-Review`, replace "or the next attempt's preparation was refused without a named
condition and the route's deadline, counted from the proof it rests on, is past (`REQ-EO-3`)" with:

> or the next attempt's preparation was refused without a named condition, or never happened, and the route's deadline,
> counted from the later of the proof it rests on and the moment the attempt was ordered, is past (`REQ-EO-3`)

The matching text of the §5.2 `Needs-Review` row and `REQ-RV-1` changes the same way.

## Scope

The occasion is read only where an endpoint prepares the delivery. A backend that has no endpoint preparing anything (the
attested backend of PR #66) has no preparation to be missing, and this amendment says nothing about it.

## Identifier ledger

| Action | Identifiers |
|---|---|
| Revised | §5.4 transitions `Source-Final` -> `Needs-Review` and `Delivering`, `Delivered` -> `Needs-Review`; the `Needs-Review` row of §5.2; `REQ-RV-1` |
