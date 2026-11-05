<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0019 — when the deadline of a payout that is signed late starts

**Status:** proposed by the issue #35 helper, 2026-11-04. **Not ruled and not applied to `SPEC.md`.** Nothing in the
issue #35 pull request depends on it: none of the options below is built. Point 4 of the gap
is built and is put to the owner.
**Target:** `SPEC.md` v0.5.4 (§5.4 `Delivering`, `Delivered` -> `Needs-Review`; `REQ-RV-1`, `REQ-NX-4`, `NFR-2`; §10)
**Date:** 2026-11-04
**Needs:** a decision item on bezent's form (none exists yet). Numbered 0019 because 0018 is proposed in pull request #69;
renumber if that changes.
**Rationale in one line:** a payout that was held back (a pause, a cut-off, a gate, slow signing) and is signed after the
route's deadline is reviewed, and (on an origin network) its cancel is ordered, on the next cycle, although it was signed a moment ago and can
still be included; the specification does not say whether that is intended.

## The gap

1. **What the specification says.** The deadline is the route's completion budget (`NFR-2`), and "completion runs from the
   final block of the source leg to the final block of the destination leg". §5.4 sends a leg "unknown past its deadline"
   to `Needs-Review`, and for the next attempt counts the deadline "from the proof it rests on". `REQ-NX-4` says bezent
   SHOULD force non-execution "when a delivery is past its deadline and not included". Nothing says the clock stops, restarts
   or gets a grace period while a transfer waits to be signed.
2. **What the specification says about waiting.** `REQ-GV-1`: a pause stops new work, and transfers in flight are "never
   failed or refunded because of the pause". `CND-Paused` and `CND-CutOff` are conditions with no deadline of their own;
   `CND-PayoutWindow` (amendment 0012) says the withdrawal "waits and resumes by itself". None says what the wait does to
   the clock.
3. **What the core does.** The clock of the first attempt starts at the source leg's final block (item 24 of
   `features/F-003-core-lifecycle/NOTES.md`), and only the time held in `CND-PayoutWindow` is left out (review M-1 of
   pull request #52, not an amendment). Time under a gate counts. A payout released from a pause after the deadline is
   therefore signed, observed unknown on the next cycle, sent to review, and on an origin network has its cancel
   ordered: the cancel races a payout that is on its way. The cancel moves no value and the race is decided by the nonce
   (`REQ-NX-4`; review S-4), and a payout that wins is attached as evidence and closes the review as `Delivered`; the cost
   is a review and a cancel for a payout that was not late by its own account. The nonce race exists only on origin
   networks, where a transaction never expires. On the issuing network a transaction expires with its era, no cancel is
   ordered (`REQ-NX-1`), and a late payout costs a review and nothing else.
4. **What the sum decides that the specification leaves open (a point for the owner).** Review M-1 left out the time
   held in `CND-PayoutWindow`, and issue #35 had to say what "the time held" is when a transfer takes and leaves the hold
   more than once. The core now (a) counts the time *between* holds toward the deadline, (b) drops the time before the
   first hold, so the clock restarts where the first hold was taken, and (c) leaves out the sum of every hold of the
   attempt. `REQ-GV-1`, `NFR-2` and `CND-PayoutWindow` rule none of the three. It is built this way because it is the
   smallest change that stops a flapping provider from restarting the clock at each exit; it is a choice about what the
   completion budget measures, so it is listed here for a ruling with the options below, and changes with them.

## Options

- **A1a. Count from when the attempt became signable.** The clock of an attempt starts at the later of its own point and the
  moment its preparation first passed (the dry run passed, so a signing intent can be recorded). Time spent waiting for a
  gate or for a pause to lift no longer counts, for every condition, not only `CND-PayoutWindow`. Trade-off: `NFR-2`'s
  completion budget no longer measures the route end to end, and a pause can hold a transfer for as long as it lasts with no
  review at all; `REQ-EO-3` then needs the held condition's owner and a page of its own.
- **A1b. A grace period after signing.** The clock stays as it is; a payout whose signature was recorded after the deadline
  is neither reviewed nor cancelled until a further declared period (per route) has passed from the signature. Trade-off: one
  more declaration per route, and a deadline that is longer by the grace for every late signature.
- **A1c. Leave it.** The current behaviour is the specification: a late payout is reviewed and cancelled, the race is
  decided on chain, and an included payout resolves the review as `Delivered`. Trade-off: reviews for payouts that were
  merely late.

- **A1d. A1a restricted to a pause or a cut-off.** The helper's recommendation. Count from when the attempt became
  signable, but only for time spent under a pause (`CND-Paused`) or a cut-off (`CND-CutOff`) that was taken and left;
  the time under a gate, under `CND-PayoutWindow` (already left out, as above) or in a refused preparation counts as it
  does now. Trade-off: two kinds of waiting are measured differently from the rest, and the core must record when a
  pause or a cut-off was taken and left on the transfer, as it does for the payout window.

The helper recommends **A1d (A1a restricted to the time between a pause or a cut-off being taken and being left)**, because
`REQ-GV-1` already forbids failing or refunding a transfer *because of* the pause and a review is closer to that than it
looks; but this is a ruling about what the completion budget means, so it is not the helper's to make.

## What waits

None of A1a to A1d is built (the sum of point 4 is). If A1a, A1b or A1d is ruled, the change is in `services/core/src/engine.rs` (`delivering_deadline`,
`unprepared_deadline`) and needs a feature note and tests (`AC-B1.4` and `REQ-NX-4` at the service layer, §9).

## A point for the owner: a final inclusion that is not what was ordered

Not about the deadline, but about the same hold (issue #35, `resolution-blocked-by-inclusion`), raised by a review of
the fix round of that issue, and put here because it is the nearest unruled amendment. A payout observed included
and **final** whose amount or recipient is not the one ordered (`REQ-TR-3`, `REQ-TR-5`) is a leg that stands, so a
refund or a new attempt is withheld for as long as it stands, which is for ever: the hold ends only when the endpoint
writes `withdrawn`, and a final inclusion is never withdrawn. It also cannot close the review as `Delivered`, because
the leg is not what the transfer ordered. The transfer stays in `Needs-Review`, a resolution into a refund or a new
attempt pages once a day, and nothing releases. `REQ-GV-5` forbids a path that moves value or edits a status, so none
is built. The question is what the specification wants for value that left to the wrong place or in the wrong amount:
(a) nothing inside bezent, the recovery being outside it and the transfer staying in review as a record (what runs
today); (b) a terminal state or resolution for "delivered, not as ordered" that records the fact and closes the review
without moving value; (c) something else. Today's behaviour is pinned by
`req_rf_1_issue_35_a_final_inclusion_that_fails_the_amount_check_is_never_released` in `services/core/tests/cancel.rs`,
which says it is not a ruling. Until it is ruled an operator escalates to the owner
(`deploy/runbooks/pages/payout.md`).

## Identifier ledger

| Action | Identifiers |
|---|---|
| Revised (if ruled) | §5.4 transition `Delivering`, `Delivered` -> `Needs-Review`; `NFR-2` or `REQ-GV-1`, by option |
