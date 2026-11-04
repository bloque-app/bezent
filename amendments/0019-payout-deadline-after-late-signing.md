<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0019 — when the deadline of a payout that is signed late starts

**Status:** proposed by the issue #35 helper, 2026-11-04. **Not ruled and not applied to `SPEC.md`.** Nothing in the
issue #35 pull request depends on it: the behaviour below is not built.
**Target:** `SPEC.md` v0.5.4 (§5.4 `Delivering`, `Delivered` -> `Needs-Review`; `REQ-RV-1`, `REQ-NX-4`, `NFR-2`; §10)
**Date:** 2026-11-04
**Needs:** a decision item on bezent's form (none exists yet). Numbered 0019 because 0018 is proposed in pull request #69;
renumber if that changes.
**Rationale in one line:** a payout that was held back (a pause, a cut-off, a gate, slow signing) and is signed after the
route's deadline is reviewed, and its cancel is ordered, on the next cycle, although it was signed a moment ago and can
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
   therefore signed, observed unknown on the next cycle, sent to review, and has its cancel ordered: the cancel races a
   payout that is on its way. The cancel moves no value and the race is decided by the nonce (`REQ-NX-4`; review S-4), and a
   payout that wins is attached as evidence and closes the review as `Delivered`, so no value is at risk; the cost is a review
   and a cancel for a payout that was not late by its own account.

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

The helper recommends **A1a restricted to the time between a pause or a cut-off being taken and being left**, because
`REQ-GV-1` already forbids failing or refunding a transfer *because of* the pause and a review is closer to that than it
looks; but this is a ruling about what the completion budget means, so it is not the helper's to make.

## What waits

Nothing is built. If A1a or A1b is ruled, the change is in `services/core/src/engine.rs` (`delivering_deadline`,
`unprepared_deadline`) and needs a feature note and tests (`AC-B1.4` and `REQ-NX-4` at the service layer, §9).

## Identifier ledger

| Action | Identifiers |
|---|---|
| Revised (if ruled) | §5.4 transition `Delivering`, `Delivered` -> `Needs-Review`; `NFR-2` or `REQ-GV-1`, by option |
