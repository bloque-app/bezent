<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-003 — notes

Built `T-003-01` to `T-003-07` (implementer, 2026-10-13): migration `0014_lifecycle`, the transition table as data
in `crates/core` (`State`, `Condition`, `edge`, `review_exit`), and the new binary `services/core` (`bezent-core`).
Each choice below is a reading of the spec and plan, not a ruling.

## Decisions

1. **Migration number 0014.** PR #31 (F-007) adds `0013_kreivo_endpoint`; this one is `0014` so neither renumbers.
   Additive only: functions that change get new names (`transitions_apply_lifecycle`, `transfer_edge`,
   `signatures_from_intent_attempt`) and their triggers are pointed at them; the down migration points them back.
2. **The table in two places, proven equal.** The store enforces it (`transfer_edge` and the trigger); the core
   carries it as data; `req_tr_6_the_core_and_the_store_hold_one_transition_table` compares all 144 pairs.
   `Delivering -> Delivering` is a condition unless it carries an attempt number, then a new attempt.
3. **Attempts (`REQ-EO-6`, #24).** `intents.attempt`, `transitions.attempt`; one signature per (network, purpose,
   redeemer scope, reference, attempt); attempt n+1 is recorded, signed, and moved to only once attempt n is
   proven not executed, the new attempt resting on the proof's final block. The signer's own guard and its
   window cap follow (a reference counts once across its attempts).
4. **Redeemer-scoped references (amendment 0008).** `transfers.redeemer`; the one-reference-once index keys on
   (withdrawal or not, redeemer, reference), so a redemption naming a deposit's (public, derived) mint reference
   blocks nothing. Signatures are scoped the same way, so a refund mint is signed beside its payout.
5. **Per-attempt milestones (F-001 S5).** `Delivered` records the attempt's inclusion in `attempt_milestones`; the
   transfer's `destination_included_*` and `destination_final_*` are written by `Completed`, from the completing
   attempt. The ingress status shows the current attempt's inclusion while `Delivered`.
6. **Review.** Every exit needs its evidence coordinate and a recorded `manual_actions` row (actor, justification),
   run once; resolutions into `Source-Final`, `Delivering` or `Refunding` move value and need a
   `manual_approvals` row by another person. Only the new `bezent_operator` role may write either. The core
   verifies the evidence against what endpoints recorded at final blocks before running the step. `reviews` view:
   owner `operations`, due one business day later (UTC, weekends skipped, holidays not declared anywhere).
7. **Deadlines** are the route's `completion_budget_seconds`, in chain time (final heads), from the leg's start;
   refunds use `default_deadline_seconds`. A withdrawal proven not executed before its deadline gets a new attempt;
   after it, the refund mint (`REQ-RF-5`). A deposit always gets a new attempt.
8. **Cut-offs** (`routes.cutoffs`): closed windows `{"weekdays", "from", "to"}` in UTC; an unreadable declaration
   closes the route. `CND-Paused`, `CND-CutOff`, `CND-ReserveShort` are gates before a delivery and clear by
   themselves; `CND-ReserveShort` is raised and cleared once per (asset, network) in the event log. Refunds pass
   no gate (F-006 NOTES).
9. **A withdrawal is accepted at once**, with no route-rule check: the redemption is on chain and the contract
   enforced its bounds. A redemption of an undeclared asset or backend makes no withdrawal (logged).
10. **No Expired from the core:** no V0 request makes an Accepted transfer without a source binding.
11. **Unassociated arrivals** are the `unassociated_arrivals` view (owner, due); their refund leaves `Received` only
    with an approved `release-unassociated` action (store-enforced, `REQ-IN-6`).

## Review of PR #32 (migration `0015_lifecycle_review`, additive)

12. **M1: a payout carries a reference unique to its withdrawal** (`REQ-OT-2`, `INV-2`, PLAN `DEC-10`). A withdrawal's
    bridge reference is derived from (redeemer, redemption reference), no longer from the coordinate;
    `transfers.destination_reference` = SHA-256 over a tag and that bridge reference, the 32 bytes the reference
    emitter writes. The payout intent carries it, unscoped (store-enforced); `on_chain_reference` stays the
    redemption's and the refund mint still carries it, in the redeemer's scope. A leg is matched by that reference
    and its event alone; two observations of it are ambiguous, held as unknown (review past the deadline).
    `Delivered` reads back the exact leg its inclusion rested on, by the transition's coordinate (S4).
13. **M2: refunds close the attempt set** (`INV-8`, `REQ-RF-1`, `REQ-EO-6`). `Refunding` needs every attempt with an
    intent (ordered, prepared or signed) proven not executed (`attempts_unproven`, checked by the store and by the
    core, and for a review's evidence). Once a refund exists, or the transfer is `Refunding`/`Refunded`, no intent
    or signature is recorded for it; both take the transfer's row lock (`transfer_attempts_closed`, FOR SHARE,
    against the transition's FOR UPDATE), so the decision and a new attempt are serialised. Retry or refund is
    decided once: an intent of attempt n+1 is the record, and the route is not read again after it.
14. **M3: the second approver is another login** (`REQ-GV-5`, `REQ-RV-2`). The store stamps `actor`/`approver` from
    `session_user` and `recorded_at`/`approved_at` from `now()`; `bezent_operator` holds column INSERT on what it
    decides only. Each operator needs their own login, a member of `bezent_operator`; a shared login can never
    approve (fails closed). If F-014 authenticates people itself, it still needs one database login per person,
    or this rule changes by decision.
15. **S1–S7.** Reserve check counts every open withdrawal with a payout intent, under an advisory lock per (asset,
    network) held until the order commits. An attested backend's redemption makes no EVM payout and a commitment is
    never a destination (attested payouts are the requester's reports). A leg must move the full amount to the
    destination (`recipient-mismatch` to review otherwise). Review resolutions into `Delivering` pass the pause,
    cut-off and reserve gates, holding the condition in `Needs-Review`. `final_heads` is written by
    `bezent_endpoint` only (made as PR #31 makes it), and `f007_tables.sql` grants proofs and prepared transactions
    the same way. Deadline moves rest on the final head past the deadline.

Still open from the review: N1 (the compromise path runs on its own connection), N4 (withdrawals with no declared
route), N6, N7 (a transfer whose next attempt is never prepared or signed has no deadline edge, now including one
committed to attempt n+1; closed by item 24, issue #25, for an attempt whose preparation was refused; one whose
preparation never happens, the endpoint being down, is still open: issue #48), N8–N10. The service role may still write observations, outcomes and signed bytes (S6
beyond final heads and proofs): a follow-up with #31's endpoint role. (Done, issues #35 and #46: migration 0045 left
`bezent_service` with nothing, and each service role holds what its service needs; the store test `grant_matrix`
asserts every role's grants, so only the endpoints' roles insert observations, outcomes and signed bytes.)

## Verification review of PR #32 (migration `0017_lifecycle_verification`, additive)

Numbered **0017**: `0016` is taken by PR #31's branch, so neither PR renumbers the other.

16. **F2: an attempt is proven only when every signed transaction of it is** (`INV-8`, `REQ-EO-6`). `attempt_proof`
    returns a proof only once every signed transaction recorded for the (transfer, attempt) has one, and then the
    latest, the final block the refund or attempt n+1 rests on; `attempt_gate_open` likewise for a lane. One proof
    of an expired, re-prepared attempt's dead transaction no longer opens a refund or the next attempt while the
    live one is signed. The store no longer leans on one signature per (lane, attempt) for this (#24).
17. **F1: no lifecycle function is PUBLIC's.** Every function of 0014 and 0015 loses PostgreSQL's default EXECUTE
    for PUBLIC. Granted back: `attempt_proof` and `attempts_unproven` to `bezent_core`, `attempt_gate_open` to
    `bezent_signer`, `one_business_day_after` to `bezent_service` and `bezent_operator` (the review views call it).
    `transfer_attempts_closed` (a row lock as the owner) and the trigger functions go to no role.

Still open from the verification: F3 (an attempt ordered but never signed blocks the refund for good; closed by 0051, issue #25), F4
(a leg's token and sender unchecked; for #9), N-a to N-e.

## Forced non-execution and the payout window (issue #41, migration `0042_payout_window`, additive)

18. **Review at the deadline, and the cancel at the same time** (`REQ-RV-1`, `REQ-NX-4`, `AC-B1.4`). The two
    requirements apply together, not in sequence (orchestrator's plan-level reading, 2026-10-15). A `Delivering`
    attempt with no leg observed and no proof goes to `Needs-Review` at its deadline, resting on the final head past
    it, with its owner and service level, exactly as before #41. In the same step, on a network whose transactions
    never expire (`networks.role = 'origin'`), the core orders a cancel of each of the attempt's signed transactions
    without a proof, and records which (`review-cancel-ordered`). The cancel moves no value and takes the same two
    steps as a delivery (PLAN `DEC-3`): the order `{"order": {"cancel": <signed transaction id>}, "amount": "0"}`;
    F-009's preparation and dry run; the signing intent `{"message", "amount": "0", "prepared"}` over those bytes; the
    signer (which reads a cancel's bytes); the signature verified like a delivery's (`AC-I1.2`). The core drives
    these steps while the transfer is in review. A failed dry run gets a new order after `retry_preparation_seconds`;
    one signing intent per signed transaction, never a second cancel. A leg observed twice (ambiguous) is not
    forced: something carrying the reference executed.
19. **A cancel is no attempt of the transfer's.** Its intents carry no transfer, no reference and attempt 1, as F-009's
    endpoint and the signer already take them; the order names the signed payout. So `attempt_proof()` and
    `attempts_unproven()` still read only the payout's own signed transactions (the cancel executes; it has no proof
    of non-execution and must not hold the refund). The store adds no cancel-specific guard: the EVM endpoint refuses
    a cancel of anything but a signed, unsettled transaction of its lane, and the signer signs only a transfer of
    nothing to the key itself, pinned to the chain, 21000 gas, fee-capped.
20. **The cancel's proof is evidence for the review** (`REQ-RV-2`, `REQ-GV-5`, `INV-8`). When the attempt's proof of
    non-execution is final (`nonce-consumed-by-other`, read by `attempt_proof()` as before), the core attaches it to
    the review by itself, once per review: a `review-evidence` event with the proof's kind, network, final block and
    time. Nothing moves on it. The review ends as any other, by a recorded resolution naming that final block; a new
    attempt or the refund mint moves value and waits for its second approver, then runs as the ordinary step. If the
    cancel stays unknown, the review stays open, with the ordered cancel on record and no evidence attached; nothing
    is refunded (`REQ-RF-1`, guardrail 4). There is no separate deadline for the cancel: the review holds.
21. **The issuing network orders no cancel** (`REQ-NX-1`): a Kreivo transaction expires with its era, and F-007's
    `nonce-unconsumed` proof at finality is the proof. Unknown at the deadline goes to review as before; the era's
    expiry, once final, is attached to that review as its evidence in the same way.
22. **`CND-PayoutWindow`** (amendment 0012; domain value in `0042`). A dry run refused by the vault's Roles modifier
    over the payout token's delivery allowance holds the condition by a self-transition in the transfer's state
    (`Source-Final`, or `Delivering` for attempt n+1). It never blocks delivery in the §5.4 sense, so no deadline
    sends it to review and nothing refunds it; each new order is prepared again, and the first whose dry run passes
    resumes it (the hold is left, then `Delivering`).
    - *Named from a structured signal only* (PR #52 review S-2). The EVM endpoint decodes each provider's revert data
      strictly as `ConditionViolation(uint8, bytes32)` and records `dry_run.violation = {status, key}` only when both
      providers returned the same one (`DEC-12`). The core holds the window only for status 17
      (`AllowanceExceeded`) with, as key, the delivery allowance key of the one token admitted for the asset on that
      network, `keccak256("bezent.delivery.allowance" ‖ token)` (F-008). The reason's text is never read for it: words
      alone, a refusal the providers disagree on, another status or another key hold nothing (not even
      `CND-FeeShort`, whose rule would match the key's hex), and such a failure leaves an earlier window hold.
    - *Held time does not count toward the deadline* (review M-1). Taking and leaving the hold rest on the
      destination network's latest final head. An attempt's clock starts at the later of its own point (the previous
      attempt's proof, or the source leg's finality for the first) and the last point where the transfer left
      `CND-PayoutWindow`. A payout released after its original deadline is therefore neither reviewed nor cancelled
      for the time it waited; unknown past the restarted deadline, it goes to review with its cancel as any other.
    - *Alert* (review S-3). A hold longer than `payout_window_alert_seconds` (core configuration; two daily allowance
      periods, 172 800 s, by default), on the destination network's clock, pages a human (`payout-window-alert`,
      also in the event log), once, then once a day while it lasts. It stays a hold: no review, no refund.
23. **The payout included after its cancel was ordered** (review S-4). Whichever of the payout and its cancel consumes
    the nonce first decides; the endpoint records the other as not executed. Once the payout's leg is observed (one
    leg, or an ambiguous one), the core stops driving the cancel: no new order and no signing intent, only the
    signatures already returned are verified (`AC-I1.2`). For one leg that moved what was ordered, the inclusion is
    attached to the review as its evidence, once (`review-evidence`, `kind: "included"`, `resolves_to: "Delivered"`,
    at the leg's coordinate); the cancel's own proof of non-execution belongs to the cancel's signed transaction, so
    no proof is read for the payout, a refund is refused, and the review closes on a Delivered resolution. The
    evidence of a cancel that won now carries `kind: "not-executed"`.
24. **An attempt whose preparation never passes has a deadline** (issue #25, review note N7; `REQ-RV-1`, `REQ-EO-3`).
    When the last dry run of an attempt's orders was refused, nothing is signed for it, no named condition explains
    the wait and the route's deadline is past on the destination network's final head, the transfer goes to
    `Needs-Review` from the state it is in: `Source-Final` for the first attempt, `Delivering` (or `Delivered`) for a
    later one. The clock is the attempt's: the source leg's finality for the first attempt, the previous attempt's
    proof for a later one, restarted where the transfer last left `CND-PayoutWindow` (item 22). Only a refusal that
    names nothing is measured: `CND-FeeShort`, `CND-MintWindow` and the gates resume by themselves, `CND-PayoutWindow`
    waits for the allowance (no review, amendment 0012) and `CND-DestinationRefused` has its own deadline. Time spent
    under a gate counts, since §10 gives it no restart. It is never a refund or a retry on its own (`REQ-RF-1`).
    An attempt whose preparation never happens at all (the endpoint is down, so no preparation row, refused or not,
    exists) has the same deadline (issue #48), counted from the later of the attempt's clock and the first final head the
    core sees at or after the order was written, on the destination's clock (a head read before the order would count
    a stall of the head feed against an endpoint that was never asked; the route's time is still not promised in full, since
    a stall after that head or an observer that lags counts): `deliver` asks `unprepared_deadline` where it finds an order
    and no preparation. It is read only on the endpoint path; a backend with no endpoint has no such path. The order's own
    preparation is read before any other row of the attempt, so a late row of an earlier order hides nothing. A delivery a
    review resolution runs is not sent to review again; with no preparation after the same time it pages once per order
    (`resolution-unprepared-alert`) and changes no state. **Ruled:** the
    never-prepared occasion is amendment 0020 (`d-never-prepared`, Yes, 2026-11-06; SPEC v0.5.6). **Not ruled:** a grace
    cycle after a pause lifts, and the resolution page (both in the amendment).
    **Ruled:** the §5.4 rows gain the refused-preparation occasion by amendment 0013 (SPEC v0.5.3, pull request #47).
    A resolution's delivery orders anew and never signs over a preparation of an earlier order (`INV-9`).
25. **An attempt ordered and never signed is cancelled by the refund that needs it gone** (issue #25, the
    verification's F3; `INV-8`, `REQ-RF-1`, `REQ-GV-5`, `REQ-RV-2`; migration `0051`). Attempt n+1, ordered after
    attempt n was proven not executed and then never signed, has an intent and nothing to prove, so
    `attempts_unproven()` listed it for good and the refund was closed to everyone, an approved resolution
    included. Now, running the operator's approved resolution into `Refunding`, the core records the cancellation of
    that attempt (`unsigned_attempt_cancellations`, one row, naming the resolution) and then refunds on the proof of
    the last attempt that was signed. The store accepts the row only in review, under that approved and unrun
    resolution of that transfer, for the transfer's next attempt after one proven not executed at finality, and only
    while no signature and no signed transaction exists for it, under the transfer's row lock the signer's inserts
    share; from then on no intent, signature or signed transaction is recorded for that attempt, whoever writes, so
    a signature cannot land after the refund decision. A signed attempt is never cancelled: it is proven not executed
    or it blocks the refund as before. The evidence check of a resolution into `Refunding` leaves that one attempt
    out of "every attempt proven". **Ruled:** `INV-8` says "every payout attempt proven not executed"; amendment 0014
    (SPEC v0.5.3, pull request #47) reads an attempt for which no signature has been recorded, and which the store has
    closed, as proven. The store can only know that no signature is recorded; that is safe because the signer
    releases a signature only after it is recorded (`INV-9`, amendment 0014). The cancellation is recorded and made
    irreversible by the store. Who authorises it is the refund's own approver pair (`REQ-GV-5`).

Tests: `services/core/tests/cancel.rs` (named after `REQ-NX-4`, `REQ-RV-1`, `REQ-RV-2`, `REQ-GV-5`, `REQ-NX-1`,
`AC-I1.2`, `CND-PayoutWindow`, and the PR #52 review finding each covers: `m1`, `s2`, `s3`, `s4`); the unit tests of
`engine.rs` and `evm.rs`, and `services/evm-endpoint/tests/submit.rs` (`cnd_payout_window_s2_*`); `ac_b1_4`,
`ac_c1_2` and `s7` keep their original expectation, review at the deadline. The core tests' `TOKEN` is now an EVM
address, as the EVM endpoint requires of an admitted token there. Items 24 and 25: `services/core/tests/unsigned_attempts.rs` (`REQ-RV-1`, `INV-8`,
`CND-PayoutWindow`) and, in the store, `crates/store/tests/lifecycle_graph.rs` (`issue_25_*`).

Review S-5 (transfers already in `Needs-Review` from `Delivering` before the cancel shipped get no cancel ordered): **moot,
closed in issue #35.** The cancel is ordered only on the deadline transition, and no such transfer exists: production's
ingress has never run (the ingress and the EVM endpoint are off and the ingress secrets were never written, so no
transfer, no payout and no review has been recorded), and nothing was signed on an origin network. Nothing is built for
it.

## Payout-window and cancel edge cases (issue #35)

26. **A refusal the providers disagree on** (`REQ-RV-1`, `REQ-EO-3`; `CND-PayoutWindow`). A refusal that names no
    condition (a Roles refusal the two providers did not return alike, or any reason nobody resumes from) was already
    bounded by item 24 when it was steady. Two things were not. (a) A provider that flaps between the allowance's
    refusal and such a refusal took and left the hold each time, and each exit restarted the attempt's clock, so the
    deadline never came. A deadline now leaves out the time held, summed over every hold of the attempt, and
    restarts where the first hold was taken (the restart of review M-1, once) (`store::held_under_window`,
    `Core::deadline_held`). The holds of an attempt n are the ones recorded after the entry transition of attempt n-1
    (the transition that carries the attempt), chosen by `id`, the order they were recorded in; comparing a hold's block time with the attempt's point mixed
    the destination's clock with the source's and dropped a hold taken at a lagging destination head.
    **A decision the specification leaves open, built as follows and put to the owner in amendment 0019:** the time
    *between* holds counts toward the deadline (it is not held), and the time before the first hold is dropped (the
    clock restarts where the first hold was taken, as review M-1 had it). Nothing in `SPEC.md` rules either.
    (b) The long-hold page (review S-3) counted only the current hold; it now counts the attempt's holds already
    left. It pages once, when the attempt's total crosses the declared bound, and again once a day while the same
    hold lasts; taking the hold again after the crossing is no new crossing and pages nothing until a day into that
    hold (the page records its attempt). `SPEC.md` has no requirement for this page beyond `REQ-EO-3` (a named
    condition has an owner), so the page is a core rule, not a spec one. (c) Nothing paged on a refusal that keeps
    coming back: after ten refused preparations of one attempt, none naming a condition, the core records
    `payout-refusal-alert` once per attempt and logs it as a page. It changes no state: the deadline sends the
    transfer to review, never to a refund (`REQ-RF-1`). The check for an earlier page and the insert are atomic:
    every step runs in a transaction holding the transfer's row lock (`lock_transfer`, `FOR UPDATE`), so two cycles
    never step one transfer at once.
    (d) **A refund waits while an inclusion of the payout stands** (`REQ-RF-1`, `INV-16`, `INV-8`). The specification
    says a refund is never made on an unknown. A payout observed included and not withdrawn, final or not, beside a
    final proof that the attempt did not execute, makes the leg unknown, whatever the proof says: the two cannot both
    hold on one chain, and the core does not choose between them. So a resolution into `Refunding` is *withheld* while
    such an inclusion stands: no state changes, the resolution is not marked refused (it runs by itself once the
    inclusion is withdrawn, and a later resolution is not held up behind it), and the core records
    `resolution-blocked-by-inclusion`. When the inclusion goes final, the withheld refund still waits, and the
    review closes as `Delivered` only by a recorded resolution (`REQ-RV-2`: a review ends by evidence a resolution
    names; nothing closes it on its own). `REQ-RV-2` does not say a final inclusion should close it by itself, so
    this is not built; if the owner wants that, it is an amendment. This derives no disposition from a non-final observation (`INV-16`); it only
    withholds one. The leg is the transfer's own (its reference, destination network and event, `destination_leg`),
    so another transfer's inclusion blocks nothing; the reference is shared by every attempt (`REQ-EO-6`) and an
    observation carries no attempt, so the leg cannot be told apart per attempt, and an earlier attempt's included
    payout is the delivery. Two other paths were read. In `Delivered`, the automatic path read a final proof against an
    inclusion not yet decided and could make the refund mint; it no longer does while the inclusion stands.
    `Delivering` already ignored a proof when a leg was observed.
    (e) **A new attempt waits too, where the chain does not refuse a second payout** (`REQ-RF-1`, `INV-8`, `INV-16`).
    On an origin network nothing refuses a second payout under one reference (`ReferenceEmitter` only emits it;
    `ReferencedTransferUnwrapper` tracks no references), so a new attempt, with a new nonce, beside an inclusion
    that is real would pay twice. The automatic path in `Delivered` (`after_proof`) and a resolution into `Delivering`
    (`resolve`) are withheld exactly as the refund is: no state change, a resolution not marked refused, one page,
    the same release (the endpoint writes `withdrawn`, after which `Delivered` goes to review and the resolution
    runs, or `final`, after which the review closes as `Delivered`). On the issuing network the contract's `mint`
    refuses a reference used before (`ReferenceUsed`, `INV-2`, `bezent/src/lib.rs`), and the reference is shared by
    every attempt (`REQ-EO-6`), so a second mint beside a real inclusion mints nothing; there the new attempt is
    unchanged, and `services/core/tests/review.rs` (`delivered_in_review` then a resolution into `Delivering`)
    relies on it. Which network is which is read from `networks.role`. The one test that had a new attempt
    beside an inclusion on an origin network, `review_findings.rs` `m2_inv_8_…`, reached review that way only as
    setup; it now reaches it with attempt 2 signed and unknown at its deadline, and every assertion stands.
    **A hold has no end of its own, and no override** (`REQ-GV-5`): it lasts until the endpoint writes `withdrawn`
    for the observation, or `final` for an inclusion that is what the transfer ordered (the review then closes as
    `Delivered`, by a recorded resolution). **A final inclusion that fails the amount or recipient check
    (`leg_mismatch`) never releases it**: it is not withdrawn, and it cannot close the review as `Delivered`, so the
    transfer stays in review, a refund or new-attempt resolution is withheld for good and pages daily, and nothing
    moves. No path is added (`REQ-GV-5`): the question of what to do for value that left wrong is the owner's, a
    point in amendment 0019 ("A point for the owner"), and the behaviour is pinned by
    `req_rf_1_issue_35_a_final_inclusion_that_fails_the_amount_check_is_never_released`. It pages once and then once
    a day while it lasts, durably (the last page is read from the event log), a day counted on the destination's final
    head or on the core's clock, whichever passes first, so it does not go quiet while the destination's endpoint
    stalls.
27. **A payout signed after its deadline** (point 2 of the issue): **amendment 0019, ruled 2026-11-06
    (`d-payout-late-signing`, A1d, "Leave pause time out"), `SPEC.md` v0.5.7 §5.4.** The time a transfer is held under
    `CND-Paused` or `CND-CutOff`, taken and left, does not count toward a payout attempt's deadline: a payout a
    pause or a cut-off held past its nominal deadline is neither reviewed nor cancelled on the cycles after its release,
    and its deadline moves by the time held (`Core::deadline_held`, `store::held_under_pause_or_cutoff`). Taking and
    leaving either condition now rests on the destination's final head (`Core::hold`), as `CND-PayoutWindow` did, so
    the time is measured on the deadline's clock; the holds are chosen by `id` after the attempt's entry transition, as
    item 26's, and one recorded before the first `CND-PayoutWindow` hold is not left out twice. It is for a payout only (`purpose`): a mint's deadline is counted
    as before, and applying A1d to mints is open (amendment 0019). Time under a gate, a refused preparation or any other condition counts as before. **A new
    attempt under a pause**: after a proof of non-execution, `after_proof` asks the gate for attempt n+1, which a pause
    holds as `CND-Paused` in `Delivering` or `Delivered`, ordering nothing; the new attempt's clock starts at the proof and
    leaves out the time it was paused. The refund-or-retry choice after the proof still compares the proof with the
    plain deadline from the source leg's finality (no hold left out): not named by the ruling, left as built (amendment
    0019, "Open after the ruling"). Tests: `cancel.rs::req_gv_1_a1d_a_payout_held_by_a_pause_past_its_deadline_is_not_reviewed_and_its_deadline_moves`,
    `cancel.rs::req_gv_1_a1d_a_pause_holds_the_new_attempt_and_its_deadline_moves_by_the_paused_time`; item 26's rule for
    `CND-PayoutWindow` is pinned by `cancel.rs::cnd_payout_window_issue_35_time_between_holds_counts_and_time_before_the_first_does_not`.
28. **Only a final inclusion is evidence for a review** (`REQ-RV-2`, `INV-16`). Item 23 attached the payout's inclusion as
    the review's evidence as soon as the leg was observed. Evidence is never withdrawn, so an inclusion that was later
    reorganised away blocked the cancel's `not-executed` evidence for good. It is attached now only when the observation's
    outcome is `final`; a leg observed and not yet final still stops the cancel being driven (item 23), and a leg that is
    withdrawn frees the cancel and its evidence again.

Tests: `services/core/tests/cancel.rs` (`*_issue_35_*`; the S-4 test that never decided finality now does).

## Delivery anomalies (issue #24)

1. **A delivery anomaly is surfaced** (F-007 review; `INV-8`, `REQ-NX-3`, `REQ-RV-1`). An endpoint
    records a `delivery-anomaly` when what it reads contradicts itself (bytes that do not answer their rows, a nonce
    consumed by another payload for the reference, a reference the contract says was used though the chain shows
    the transaction not executed); it proves nothing then. The core reads them per transfer, at the start of every
    step: an anomaly is the transfer's if it is of the transfer's destination network and names one of its intents,
    one of its signed transactions, or, for a withdrawal only, carries its leg's `reference` (an EVM payout's own,
    derived per transfer; any mint's reference, a refund mint's included, is scoped by its redeemer and is never
    matched alone, the Kreivo endpoint's anomalies name the signed transaction too; `inv_8_a_mints_reference_alone_is_no_match_for_an_anomaly`). An endpoint cannot send another network's transfer to review, and a payload
    that does not have the shape is ignored, never an error. A transfer in
    `Source-Final`, `Delivering` or `Delivered` goes to `Needs-Review` at once, from its state, with no coordinate
    (the store's edge needs none), instead of at its deadline; one already in review gets the event only. Either way
    a `delivery-anomaly-surfaced` event names the anomaly (once: a resolved review is not reopened by the anomaly
    that caused it). Nothing is refunded, retried or failed because of it (`REQ-RF-1`). **Ruled**: the §5.4 rows into
    `Needs-Review` gain this occasion by amendment 0013 (SPEC v0.5.3, pull request #47).
   The late-mint check (`services/core/src/late_mint.rs`, "What decides an attempt") reads the same rows
   with a different question: an anomaly ends an attempt's wait only if it says the attempt did not
   mint. The two kinds that say the prover cannot read the attempt (`recorded_undecodable`,
   `bytes_unreadable` in the payload) do not: the mint stays held and pages, and the transfer goes to review
   through this section (PR #80 review).

Tests: `services/core/tests/anomalies.rs` (`INV-8`).

## Integration points (for review)

- **PR #31 (F-007).** The core implements the two-step delivery of its decision 1: order intent
  (`{"order": …, "amount"}`), then the signing intent over `prepared_transactions.message`
  (`{"message", "amount", "prepared"}`); a failed dry run holds `CND-DestinationRefused` (`Kreivo(…)`),
  `CND-MintWindow` (`AboveWindow`, `AboveCeiling`) or `CND-FeeShort`, and a new order follows after
  `retry_preparation_seconds`. Proofs are read from its `non_execution_proofs` (through `signed_transactions` to
  the signing intent) by `attempt_proof()`/`attempt_gate_open()`. Since the integration (migration `0021_delivery_integration`,
  F-007 NOTES 24 to 27) they read the table directly, and the test-only copy of the two tables is gone. The core matches its observations: `detail.event`
  (`Minted`, `Redeemed`, `RefundMinted`), `token` = `assets.issuing_asset`, `detail.backend` mapped by
  `networks.backend`, `detail.payout.address`.
- **Final heads.** Every transition the core rests on a final point needs that point's block time: endpoints must
  append the final head they decide at to `final_heads`, as `bezent_endpoint` (#31 does not yet).
- **F-002.** The ingress reads a redemption's commitment at `detail.commitment`; #31 writes `detail.payout.commitment`.
  The extras lookup by reference alone is ambiguous now that two redeemers may share one (amendment 0008).
- **F-009.** Payout orders are `{"order": {"to", "asset", "token"}, "amount"}`, the intent's `on_chain_reference` is
  the withdrawal's `destination_reference` (what the emitter writes), and their legs are `detail.event = "Payout"`
  with that reference and the beneficiary as `recipient`.
- **F-004.** `CND-ReserveShort` reads the book's `reserve` balance less every open withdrawal with a payout intent; until the book is
  posted, every withdrawal holds it. A mint remainder (`REQ-AS-9`) is not minted and is the book's.

## Spec gaps (not built; each needs a ruling)

- **"The route's deadline"** (`REQ-IN-6`, `REQ-IN-9`, `REQ-RV-1`, `REQ-RF-5`) is named but `REQ-LM-1` declares no
  deadline per route; the completion budget stands in.
- ~~A withdrawal's refund destination~~: ruled by amendment 0011 (SPEC v0.5.1): always the redeemer.
- **Cut-off form.** `REQ-LM-1` declares cut-offs without a form; the one above is the core's reading.
