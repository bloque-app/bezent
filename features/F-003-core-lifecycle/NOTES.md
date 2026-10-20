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
committed to attempt n+1), N8–N10. The service role may still write observations, outcomes and signed bytes (S6
beyond final heads and proofs): a follow-up with #31's endpoint role.

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

Still open from the verification: F3 (an attempt ordered but never signed blocks the refund for good; for #35), F4
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

Tests: `services/core/tests/cancel.rs` (named after `REQ-NX-4`, `REQ-RV-1`, `REQ-RV-2`, `REQ-GV-5`, `REQ-NX-1`,
`AC-I1.2`, `CND-PayoutWindow`, and the PR #52 review finding each covers: `m1`, `s2`, `s3`, `s4`); the unit tests of
`engine.rs` and `evm.rs`, and `services/evm-endpoint/tests/submit.rs` (`cnd_payout_window_s2_*`); `ac_b1_4`,
`ac_c1_2` and `s7` keep their original expectation, review at the deadline. The core tests' `TOKEN` is now an EVM
address, as the EVM endpoint requires of an admitted token there.

Not done here (review S-5): transfers already in `Needs-Review` from `Delivering` on an origin network before this
ships get no cancel ordered; the cancel is ordered only on the deadline transition.

## Delivery anomalies (issue #24)

1. **A delivery anomaly is surfaced** (F-007 review; `INV-8`, `REQ-NX-3`, `REQ-RV-1`). An endpoint
    records a `delivery-anomaly` when what it reads contradicts itself (bytes that do not answer their rows, a nonce
    consumed by another payload for the reference, a reference the contract says was used though the chain shows
    the transaction not executed); it proves nothing then. The core reads them per transfer, at the start of every
    step: an anomaly is the transfer's if it is of the transfer's destination network and names one of its intents,
    one of its signed transactions or its leg's on-chain reference (an endpoint cannot send another network's
    transfer to review; a payload that does not have the shape is ignored, never an error). A transfer in
    `Source-Final`, `Delivering` or `Delivered` goes to `Needs-Review` at once, from its state, with no coordinate
    (the store's edge needs none), instead of at its deadline; one already in review gets the event only. Either way
    a `delivery-anomaly-surfaced` event names the anomaly (once: a resolved review is not reopened by the anomaly
    that caused it). Nothing is refunded, retried or failed because of it (`REQ-RF-1`). **Pending a ruling**: the §5.4 rows into
    `Needs-Review` have no occasion for it. Proposed text in the pull request of issue #24 (the same rows as the
    proposal of issue #25).

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
