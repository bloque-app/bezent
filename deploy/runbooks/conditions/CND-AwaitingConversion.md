<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: CND-AwaitingConversion
kind: condition
covers: [REQ-CV-1, REQ-CV-2, REQ-CV-4, REQ-CV-6, REQ-RV-1]
pages: []
observables: [reviews_open, reviews_oldest_age_seconds, unassociated_arrivals, unassociated_arrivals_oldest_age_seconds]
tests: [services/core/tests/conditions.rs::cnd_awaiting_conversion_mints_nothing_and_goes_to_review_past_its_deadline, services/core/tests/conversions.rs::req_cv_1_the_store_refuses_a_transfer_naming_a_convertible_arrival, services/core/tests/conversions.rs::ac_a5_1_ac_a5_2_req_cv_1_req_cv_2_req_cv_5_inv_6_the_ingress_binds_the_output_and_the_core_mints_it]
exercised: no
---
# CND-AwaitingConversion

## What it is

A convertible-token arrival waiting for its conversion; never reserve, never minted (`REQ-CV-1`, `REQ-CV-4`). The
conversion is the requester's, in one transaction whose output lands in the reserve (`REQ-CV-2`, `REQ-CV-7`); the deposit
is then bound to that output and minted at its observed amount. In the core, a transfer in `Source-Final` holding this
condition orders nothing, and past the route's completion budget it goes to `Needs-Review` with the condition kept
(`REQ-CV-6`; SPEC §5.4). Not established in the code: what takes the condition on a transfer. The test inserts the
transition by hand, and the store refuses a transfer that names a convertible arrival, which stays in
`unassociated_arrivals` under `CND-Unassociated` (conversions test; amendment 0018 proposes the condition on the
arrival). Treat the account below as what is verified, and read `CND-Unassociated.md` for the arrival.

## Pages

No page. The signal is a row in `reviews` with this condition (owner `operations`, due one business day after entry;
`reviews_open`, `reviews_oldest_age_seconds`), and the convertible arrival in `unassociated_arrivals`
(`unassociated_arrivals`, `unassociated_arrivals_oldest_age_seconds`).

## Confirm

```sql
SELECT transfer_id, bridge_reference, state_before_review, entered_at, due_at
FROM reviews WHERE condition = 'CND-AwaitingConversion';
SELECT observation_id, network, token, amount, final_at, due_at FROM unassociated_arrivals ORDER BY final_at;
```

Convertible holdings are itemised per token and network in the published statement, outside the reserve equation
(`REQ-CV-4`; `deploy/statements.md`). Check that the requester's conversion has an observed output at a final block.

## Do

1. Ask the requester to run the conversion (the price, slippage and any fee are theirs: `REQ-CV-5`). When its output is
   final in the reserve, the requester binds the deposit to the output, and the core mints at the output's amount.
2. In review: end it only by evidence (`REQ-RV-2`), recorded as your own login with `manual_actions`
   (`review-resolution`, `to_state`, and the coordinate columns `network, block_number, block_hash, block_time`), and
   for a resolution into `Source-Final`, `Delivering` or `Refunding` the second login's `manual_approvals` row.
3. Convertible holdings are never written off and never counted as reserve (REQ-CV-6). Moving them is the requester's.

## Never

- Do not mint against a convertible arrival, or against a quoted amount (REQ-CV-1, REQ-CV-5, INV-6).
- Do not refund or fail on a timer or an unknown (REQ-RF-1, REQ-RV-1); do not match by amount or time (REQ-OT-2).
- Do not count convertible holdings as reserve or merge issuers (REQ-CV-4, REQ-AS-3).
- Do not UPDATE or INSERT into transfers, transitions, book or observations; there is no grant for it (REQ-GV-5).
- Do not treat an operator top-up as a conversion output or a deposit (INV-1, INV-2).

## Exercise

On the test bed: send a convertible token to a reserve address, see it listed as unassociated, have the requester
convert it and bind the output, and see the mint at the output's amount. Separately, hold a deposit in this condition
past its deadline and resolve the review with evidence under two logins.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
