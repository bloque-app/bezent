<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: CND-AwaitingConversion
kind: condition
covers: [REQ-CV-1, REQ-CV-2, REQ-CV-4, REQ-CV-6, REQ-GV-5, REQ-RQ-4, REQ-RV-1, REQ-RV-2]
pages: []
observables: [reviews_open, reviews_oldest_age_seconds, unassociated_arrivals, unassociated_arrivals_oldest_age_seconds]
tests: [services/core/tests/conditions.rs::cnd_awaiting_conversion_mints_nothing_and_goes_to_review_past_its_deadline, services/core/tests/conversions.rs::ac_a5_1_req_cv_1_inv_6_a_deposit_naming_a_convertible_arrival_holds_cnd_awaiting_conversion_and_mints_nothing, services/core/tests/conversions.rs::ac_a5_2_req_cv_2_req_cv_5_req_rq_4_inv_2_inv_6_the_request_comes_first_then_the_output_then_the_mint, services/core/tests/conversions.rs::req_cv_6_req_rf_1_past_its_deadline_the_deposit_goes_to_review_never_to_a_refund, services/core/tests/conversions.rs::ac_a5_4_req_cv_2_req_rv_2_req_gv_5_inv_6_an_output_named_in_review_mints_once_after_a_two_person_resolution]
exercised: no
---
# CND-AwaitingConversion

## What it is

A deposit at `Source-Final` whose source is a convertible-token arrival, waiting for its requester to name the
conversion's output (`REQ-CV-1`, `REQ-CV-2`; amendment 0018, A1b). Nothing is minted against the arrival, and it is never
reserve (`REQ-CV-4`). The deposit takes the condition on its edge into `Source-Final`. The conversion is the requester's,
in one transaction whose output lands in the reserve (`REQ-CV-7`); the requester then names that output by the deposit's
request reference (`POST /v0/deposits/conversion`, `REQ-RQ-4`). The store moves the deposit's source to the output and
records the arrival with both amounts (`deposit_conversions`); the core clears the condition and mints at the output's
amount. Past the route's completion budget, counted from the arrival's finality, with no output named from a block at or
before it, the deposit goes to `Needs-Review` with the condition kept, and is never refunded on a timer (`REQ-CV-6`,
`REQ-RF-1`). Its requester may still name the output while no refund of the deposit is recorded (amendment
0018, Open 6): the deposit stays in review with its source and amount at the output, and the review ends by a resolution
(below). A convertible arrival that no deposit names is not this condition: it is an unassociated arrival
(`CND-Unassociated.md`).

That the arrival fed that conversion is the requester's word: bezent checks only that the output is a final conversion
output at the same reserve, of the deposit's asset, not consumed (amendment 0018, open).

## Pages

No page. The signal is a row in `reviews` with this condition (owner `operations`, due one business day after entry;
`reviews_open`, `reviews_oldest_age_seconds`). A convertible arrival no deposit names is in `unassociated_arrivals`
(`unassociated_arrivals`, `unassociated_arrivals_oldest_age_seconds`).

## Confirm

```sql
SELECT transfer_id, bridge_reference, state_before_review, entered_at, due_at
FROM reviews WHERE condition = 'CND-AwaitingConversion';
SELECT id, bridge_reference, state, source_network, source_block_number, amount
FROM transfers WHERE condition = 'CND-AwaitingConversion';
SELECT transfer_id, arrival_token, arrival_amount, output_block_number, output_amount, recorded_at
FROM deposit_conversions ORDER BY recorded_at DESC;
```

Convertible holdings are itemised per token and network in the published statement, outside the reserve equation
(`REQ-CV-4`; `deploy/statements.md`). Check that the requester's conversion has an observed output at a final block.

## Do

1. Ask the requester to run the conversion (the price, slippage and any fee are theirs: `REQ-CV-5`). When its output is
   final in the reserve, the requester names it for the deposit (`POST /v0/deposits/conversion`, by the deposit's
   request reference), and the core mints at the output's amount. Only the requester that made the deposit can.
2. In review: end it only by evidence (`REQ-RV-2`), recorded as your own login with `manual_actions`
   (`review-resolution`, `to_state`, and the coordinate columns `network, block_number, block_hash, block_time`), and
   for a resolution into `Source-Final`, `Delivering` or `Refunding` the second login's `manual_approvals` row. When the
   requester has named the output (a `deposit_conversions` row; the deposit's source is the output), the resolution is
   into `Delivering`: the core orders the mint once, at the output's amount. Approve it only after the output is named:
   the approval names the output you reviewed (network, block, hash, position and amount; all or none), and one that names
   none, or another output than the deposit's, is refused
   (`review-evidence-refused`) and must be approved again. Do not
   record a resolution into `Refunding` for a deposit whose conversion may still be named unless you mean to refund: once
   one is recorded and not refused, naming is refused.
3. Convertible holdings are never written off and never counted as reserve (REQ-CV-6). Moving them is the requester's.

## Never

- Do not mint against a convertible arrival, or against a quoted amount (REQ-CV-1, REQ-CV-5, INV-6).
- Do not refund or fail on a timer or an unknown (REQ-RF-1, REQ-RV-1); do not match by amount or time (REQ-OT-2).
- Do not count convertible holdings as reserve or merge issuers (REQ-CV-4, REQ-AS-3).
- Do not UPDATE or INSERT into transfers, transitions, book or observations; there is no grant for it (REQ-GV-5).
- Do not treat an operator top-up as a conversion output or a deposit (INV-1, INV-2).

## Exercise

On the test bed: send a convertible token to a reserve address, have the requester request a deposit naming it and see
it hold this condition, have the requester convert it and name the output, and see the mint at the output's amount. Separately, hold a deposit in this condition
past its deadline and resolve the review with evidence under two logins.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
