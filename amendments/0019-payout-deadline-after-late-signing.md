<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0019 — when the deadline of a payout that is signed late starts

**Status:** proposed by the issue #35 helper, 2026-11-04. **Ruled by @pandres95 on the decision form, 2026-11-06
(`d-payout-late-signing`): "Leave pause time out", option A1d.** A1a, A1b and A1c are not taken. Applied to `SPEC.md`
v0.5.7 (§5.4, `REQ-GV-1`, `REQ-LM-1`, §10 `CND-Paused` and `CND-CutOff`); built in the core (`deadline_held`). Point 4 of
the gap (what pull request #70 built for `CND-PayoutWindow`) is confirmed as built and pinned by a test, below; it is not
written into `SPEC.md`, because the ruling does not rule it.
**Target:** `SPEC.md` v0.5.4 (§5.4 `Delivering`, `Delivered` -> `Needs-Review`; `REQ-RV-1`, `REQ-NX-4`, `NFR-2`; §10)
**Date:** 2026-11-04; ruled 2026-11-06, applied to `SPEC.md` v0.5.7 the same day
**Needs:** nothing further for A1d. Point 4 and the point for the owner at the end stay open.
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

- **A1a. Count from when the attempt became signable.** *Not taken.* The clock of an attempt starts at the later of its own point and the
  moment its preparation first passed (the dry run passed, so a signing intent can be recorded). Time spent waiting for a
  gate or for a pause to lift no longer counts, for every condition, not only `CND-PayoutWindow`. Trade-off: `NFR-2`'s
  completion budget no longer measures the route end to end, and a pause can hold a transfer for as long as it lasts with no
  review at all; `REQ-EO-3` then needs the held condition's owner and a page of its own.
- **A1b. A grace period after signing.** *Not taken.* The clock stays as it is; a payout whose signature was recorded after the deadline
  is neither reviewed nor cancelled until a further declared period (per route) has passed from the signature. Trade-off: one
  more declaration per route, and a deadline that is longer by the grace for every late signature.
- **A1c. Leave it.** *Not taken.* The current behaviour is the specification: a late payout is reviewed and cancelled, the race is
  decided on chain, and an included payout resolves the review as `Delivered`. Trade-off: reviews for payouts that were
  merely late.

- **A1d. A1a restricted to a pause or a cut-off.** *Ruled.* The helper's recommendation. Count from when the attempt became
  signable, but only for time spent under a pause (`CND-Paused`) or a cut-off (`CND-CutOff`) that was taken and left;
  the time under a gate, under `CND-PayoutWindow` (already left out, as above) or in a refused preparation counts as it
  does now. Trade-off: two kinds of waiting are measured differently from the rest, and the core must record when a
  pause or a cut-off was taken and left on the transfer, as it does for the payout window.

The helper recommends **A1d (A1a restricted to the time between a pause or a cut-off being taken and being left)**, because
`REQ-GV-1` already forbids failing or refunding a transfer *because of* the pause and a review is closer to that than it
looks; but this is a ruling about what the completion budget means, so it is not the helper's to make.

## Ruling

`d-payout-late-signing`, @pandres95 on the decision form, 2026-11-06: **"Leave pause time out"**, A1d. Time a transfer spends
under a pause (`CND-Paused`) or a cut-off (`CND-CutOff`), from when it takes the condition to when it leaves it, is not
counted toward its delivery attempt's deadline. Time under a gate, in a refused preparation, or under any other condition
counts as before; `CND-PayoutWindow` is left out as before (point 4).

## Applied text (`SPEC.md` v0.5.7)

- **§5.4**, after the transition table, a paragraph that subtracts from whatever the deadline of a delivery attempt counts
  (the `Delivering`, `Delivered` -> `Needs-Review` edge's "unknown past its deadline", and `REQ-EO-3`'s refused
  preparation): the time held under `CND-Paused` or `CND-CutOff`, taken and left, does not count, so a delivery that waited
  out a pause or a cut-off is neither reviewed nor forced out for that time and its deadline moves by it. It does not
  define the rest of the clock, so it leaves point 4 (`CND-PayoutWindow`) as built and unruled.
- **`REQ-GV-1`**, appended: "The time a transfer waits under a pause does not count toward its deadline (§5.4)."
- **`REQ-LM-1`**, the cut-off sentence: "…holds `CND-CutOff` until the route reopens, then resumes by itself; the time held
  does not count toward its deadline (§5.4)."
- **§10** `CND-Paused` and `CND-CutOff`: the time held does not count toward the transfer's deadline (§5.4).

`NFR-2` and `NFR-3` are unchanged: latency is still measured end to end, from chain milestones, and a transfer held by a
pause still counts its whole time there. The deadline, not the completion budget's measurement, is what A1d changes.

The text says "delivery attempt", not "payout": the core's clock is one for a mint and a payout (`deadline_held`), and
A1d's reason (`REQ-GV-1` forbids failing a transfer because of the pause) holds for both. A mint on the issuing network
gets the same treatment.

## Built

- **The core** (`services/core/src/engine.rs`, `deadline_held` and `hold`; `services/core/src/store.rs`,
  `held_under_pause_or_cutoff`). Taking and leaving `CND-Paused` or `CND-CutOff` now rests on the destination network's
  latest final head, as `CND-PayoutWindow` already did, so each hold is measured on the clock the deadline is read on. The
  deadline of an attempt is the one #70 built plus the sum of its closed pause and cut-off holds, chosen as the window's
  holds are: recorded after the attempt's entry transition, by `id`. A hold recorded before the first `CND-PayoutWindow`
  hold is already inside the time the window rule drops, so it is not left out twice. A hold still open counts nothing
  here, and needs nothing: a transfer held by a pause or a cut-off is held by the gate, which no deadline reaches.
  No migration.
- **Does a pause hold the new attempt of a payout proven not executed? Yes**, as built before this ruling. After the proof
  (`after_proof`), a new attempt is new work for its route: `gate(attempt + 1)` holds it, as `CND-Paused` in `Delivering` or
  `Delivered`, and no order of the new attempt is recorded while the pause covers the route. When the pause is lifted the
  condition clears, resting on the final head of that moment, and the new attempt is ordered. Its clock starts at the proof
  it rests on; with A1d, the time it then waited under the pause is left out (the hold is recorded after the previous
  attempt's entry transition and before the new attempt's, and the new attempt's deadline counts the holds after the
  previous attempt's entry, the same `after` as the window's). A signing intent of the new attempt recorded before the
  pause is held by the signer too (amendment 0022, A1b).
- **Point 4, confirmed.** What #70 built is what the code does: of the time since the attempt's clock started, the time
  **between** two `CND-PayoutWindow` holds counts, the time **before** the first hold does not (the clock restarts where the
  first hold was taken), and the sum of every closed hold is left out (`store::held_under_window`). Pinned by
  `services/core/tests/cancel.rs::cnd_payout_window_issue_35_time_between_holds_counts_and_time_before_the_first_does_not`,
  where each of the three decides the moment of review, and by the earlier
  `req_rv_1_issue_35_a_flapping_refusal_does_not_restart_the_deadline_each_time`.
- Tests: `services/core/tests/cancel.rs::req_gv_1_a1d_a_payout_held_by_a_pause_past_its_deadline_is_not_reviewed_and_its_deadline_moves`
  (`REQ-GV-1`, `REQ-RV-1`, `REQ-NX-4`, `AC-B1.4`, at the service layer): a payout paused far past its nominal deadline is
  signed after the pause and not reviewed or cancelled on the next cycles; it goes to review, with its cancel, exactly at
  the nominal deadline moved by the time paused.

## Open after the ruling

- **Point 4** is confirmed as built, not ruled: it changes when the owner rules it.
- **The refund-or-retry choice after a proof** (`after_proof`) still compares the proof with the plain deadline from the
  source leg's finality, with no hold left out, window or pause. A withdrawal paused past its deadline whose payout is then
  proven not executed is refunded, not retried. A1d names the review deadline (`delivering_deadline`,
  `unprepared_deadline`), not this one, so it is left as built.

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
| Revised (A1d, v0.5.7) | §5.4 (the deadline of a delivery attempt, read by `Delivering`, `Delivered` -> `Needs-Review` and `REQ-EO-3`); `REQ-GV-1`; `REQ-LM-1`; §10 `CND-Paused`, `CND-CutOff` |
| Unchanged | `NFR-2`, `NFR-3` |
