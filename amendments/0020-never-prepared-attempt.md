<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0020 — an attempt whose preparation never happens goes to review at the route's deadline

**Status:** **Ruled by @pandres95 in the decision form, 2026-11-06 (`d-never-prepared`): Yes.** An attempt that is never prepared
goes to review at the route's deadline, counted from the order, from the state it is in; no refund, no retry, no new page.
Applied to `SPEC.md` v0.5.6 (§5.2, §5.4, `REQ-RV-1`); built by issue #48 (PR #71). The two points under "A point for the
owner" and "What the resolution of a review does" were not part of the ruling and stay **open**.
**Target:** `SPEC.md` v0.5.6 (§5.4, §5.2, `REQ-RV-1`)
**Date:** 2026-11-04; ruled and applied 2026-11-06
**Rationale in one line:** amendment 0013 names one occasion for an attempt that cannot be delivered, "the delivery attempt's
preparation was refused without a named condition and the route's deadline is past". An attempt whose preparation never
happens at all (the endpoint is down, so no preparation, refused or not, is ever written) is not "refused", yet `REQ-EO-3`
requires the final source leg to be delivered, refunded, or held in a named condition or `Needs-Review` with an owner.

## A1

In the §5.4 rows `Source-Final` -> `Needs-Review` and `Delivering`, `Delivered` -> `Needs-Review`, and in the matching text
of the §5.2 `Needs-Review` row and `REQ-RV-1`, "the delivery attempt's preparation was refused without a named condition"
becomes "the delivery attempt's preparation was refused without a named condition, or never happened". The deadline is the
same: the route's, counted from the later of the attempt's own clock (the source leg's finality for the first attempt, the
previous attempt's proof for a later one) and the first final head of the destination network that the core sees at or after
the moment the order for the attempt was written (on that network's clock). The endpoint is thus counted from a head read
after it was asked, and a stall of the head feed before the order does not shorten its time. The route's time is not promised
in full: a stall after that head, or a head older than the order (an observer that lags), counts against it.

Neither reading refunds, retries or fails the transfer (`REQ-RF-1`, `REQ-RV-1`); the review ends only by evidence
(`REQ-RV-2`).

## Proposed text

In the §5.4 row `Source-Final` -> `Needs-Review`, replace "or the delivery attempt's preparation was refused without a named
condition and the route's deadline is past (`REQ-EO-3`)" with:

> or the delivery attempt's preparation was refused without a named condition, or never happened, and the route's deadline,
> counted from the later of the source leg's finality and the first final head seen after the attempt was ordered, is past
> (`REQ-EO-3`)

In the row `Delivering`, `Delivered` -> `Needs-Review`, replace "or the next attempt's preparation was refused without a named
condition and the route's deadline, counted from the proof it rests on, is past (`REQ-EO-3`)" with:

> or the next attempt's preparation was refused without a named condition, or never happened, and the route's deadline,
> counted from the later of the proof it rests on and the first final head seen after the attempt was ordered, is past
> (`REQ-EO-3`)

The matching text of the §5.2 `Needs-Review` row and `REQ-RV-1` changes the same way.

## Scope

The occasion is read only where an endpoint prepares the delivery. A backend that has no endpoint preparing anything (the
attested backend of PR #66) has no preparation to be missing, and this amendment says nothing about it.

## A point for the owner: the lift of a pause

Time under a pause counts toward this deadline, as it does toward the refused-preparation deadline of amendment 0013
(`REQ-GV-1` gives a pause no restart; F-003 NOTES item 24). A pause holds the transfer, so nothing is read under it, and an
endpoint that stayed down through a long pause has an order older than the route's time on the day the pause lifts: the first
cycle after the lift sends the transfer to review, although the endpoint was never given a moment under the lift to answer.
The code does this today and pins it (`req_rv_1_issue_48_time_under_a_pause_counts_but_only_a_lifted_pause_is_read_as_never_prepared`).
The question for the owner is whether the lift should give one grace cycle, so that the route's time is counted again from
the first final head after the lift, and so an endpoint that is ready when the pause lifts is not sent to review for the
pause. Not built: it changes what a pause costs, and a pause is the operator's tool. The same lift touches the page below: a
delivery that a review resolution ran, with the endpoint down through the pause, is already older than the route's time on
the day the pause lifts, so `resolution-unprepared-alert` fires on the first cycle after the lift, whether or not the
endpoint would have answered inside a grace cycle. If the owner wants the grace cycle, it covers this page as well.

## What the resolution of a review does, which is not ruled here

A delivery that a review resolution runs waits in review for its preparation like any other. If the endpoint stays down it
waits with nothing moved, and the core pages once per order (`resolution-unprepared-alert`) after the route's time, counted
as above. No state changes (`REQ-RF-1`, `REQ-RV-2`); this adds an operational page, not an edge.

## Identifier ledger

| Action | Identifiers |
|---|---|
| Revised | §5.4 transitions `Source-Final` -> `Needs-Review` and `Delivering`, `Delivered` -> `Needs-Review`; the `Needs-Review` row of §5.2; `REQ-RV-1` |
