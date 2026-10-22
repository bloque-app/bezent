<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0013 — two more occasions for a transfer to go to review

**Status:** ruled by @pandres95 on the decision form, 2026-10-22 (`sp-needs-review-when`). Applied to `SPEC.md` v0.5.3.
**Target:** `SPEC.md` v0.5.2 (§5.4)
**Date:** 2026-10-22
**Rationale in one line:** `REQ-EO-3` requires every final source leg to end delivered, refunded, or in a named
condition or review, but the transition table gives `Needs-Review` only two occasions from `Source-Final` and
`Delivering`/`Delivered`. An attempt whose preparation is refused without a named condition has no leg, and a delivery
anomaly makes the leg unknown now rather than at a deadline, so neither case had a way into review. Building the
store checks (`F-003`, issue #25, PR #43) and the anomaly surfacing (issue #24, PR #44) found both.

## A1

In the §5.4 transition table, the "When" of two rows becomes:

- `Source-Final` -> `Needs-Review`: "A condition that blocks delivery (`CND-AwaitingConversion`,
  `CND-DestinationRefused`) is past its deadline (`REQ-CV-6`, `REQ-IN-9`); or the delivery attempt's preparation was
  refused without a named condition and the route's deadline is past (`REQ-EO-3`); or a delivery anomaly was recorded
  for the transfer's attempt, which makes its leg unknown (`REQ-NX-3`, `INV-8`)".
- `Delivering`, `Delivered` -> `Needs-Review`: "A leg unknown past its deadline; a partial or mismatched execution
  (`REQ-TR-3`, `REQ-TR-5`); or the next attempt's preparation was refused without a named condition and the route's
  deadline, counted from the proof it rests on, is past (`REQ-EO-3`); or a delivery anomaly was recorded for the
  transfer's attempt, which makes its leg unknown (`REQ-NX-3`, `INV-8`)".

Both proposals are the text in the bodies of PR #43 (the first and the next-attempt occasion) and PR #44 (the anomaly,
"adding to both rows"), accepted by @pandres95 as written. Two points follow from that text and are stated here so they
are not read as more or less than it says:

- The anomaly does not wait for a deadline. The leg is unknown from the moment the anomaly is recorded, so the transfer
  goes to review at once. The anomaly is surfaced from `Source-Final`, `Delivering` and `Delivered`, which is why it is in
  both rows; the wider reading is required by the code that records it, and only the occasion is wider than the summary
  the ruling was given on.
- Neither occasion refunds, retries or fails the transfer. The exit stays `REQ-RV-2` evidence and, for a resolution that
  moves value, a second approver (`REQ-GV-5`).

## A2

The `Needs-Review` row of the §5.2 state table, and `REQ-RV-1`, say what the §5.4 rows now say: the state's meaning
adds the two occasions (a delivery attempt's preparation refused without a named condition past the route's deadline;
a delivery anomaly recorded for the transfer's attempt, which makes its leg unknown at once), and `REQ-RV-1` lists them
beside the unknown leg past its deadline. This is consistency only, the same as amendment 0010 did for the state's
meaning: the transition table is normative (`REQ-TR-6`) and nothing new is decided.

## Why not the other way

If the anomaly waited for the deadline, a leg known to be unreliable would stay in its state until a timer ran out. If
the store refused those transfers instead, nobody could resolve them. Neither occasion refunds, retries or fails the
transfer (`REQ-RF-1`, `REQ-RV-1`): the review ends only by evidence (`REQ-RV-2`).

## Identifier ledger

| Action | Identifiers |
|---|---|
| Revised | §5.4 transitions `Source-Final` -> `Needs-Review` and `Delivering`, `Delivered` -> `Needs-Review`; the `Needs-Review` row of the §5.2 state table; `REQ-RV-1` |
