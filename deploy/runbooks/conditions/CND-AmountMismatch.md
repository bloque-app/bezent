<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: CND-AmountMismatch
kind: condition
covers: [REQ-TR-3, REQ-TR-5, REQ-RV-1, REQ-RV-2, REQ-GV-5]
pages: []
observables: [reviews_open, reviews_oldest_age_seconds]
tests: [services/core/tests/lifecycle.rs::req_tr_5_a_delivery_of_another_amount_goes_to_review_reporting_both, services/core/tests/review.rs::req_rv_2_a_review_ends_by_evidence_bezent_verifies_then_the_ordinary_step_runs, services/core/tests/review.rs::req_gv_5_a_resolution_that_moves_value_waits_for_its_second_approver]
exercised: no
---
# CND-AmountMismatch

## What it is

A destination leg whose observed amount is not the transfer's amount. bezent charges nothing and deducts nothing, so the
delivered amount must equal the transfer's (`REQ-TR-5`). The core records an `amount-mismatch` event with both amounts
(expected and delivered) and moves the transfer to `Needs-Review` with this condition, from the state it was in
(`Delivering` or `Delivered`). It does not resume by itself and is never completed (`REQ-TR-3`). A leg that moved the
right amount to another recipient is a different case (`recipient-mismatch`, no condition) and goes to review the
same way. Conversions, where amounts differ by design (`REQ-CV-5`), are not this condition.

## Pages

No page. The transfer is a row in the `reviews` view (owner `operations`, due one business day after it entered review,
`REQ-RV-1`), counted by `reviews_open` and aged by `reviews_oldest_age_seconds`.

## Confirm

```sql
SELECT transfer_id, bridge_reference, kind, asset, state_before_review, entered_at, owner, due_at
FROM reviews WHERE condition = 'CND-AmountMismatch';
SELECT metric, value FROM ops_observables() WHERE metric IN ('reviews_open', 'reviews_oldest_age_seconds');
```

Read both amounts from the `amount-mismatch` entry in the sequenced log
(`SELECT seq, kind, payload FROM event_log WHERE kind = 'amount-mismatch' ORDER BY seq DESC LIMIT 5;`),
and check the leg itself on the network, at its coordinate, from public chain data (`REQ-ST-1`).

## Do

1. Establish what the chain shows at final blocks: the leg's coordinate, its amount, its recipient. Tell the requester
   that the delivery is unknown or delayed, never failed (`REQ-IC-3`).
2. End the review only by evidence (`REQ-RV-2`): record, as your own login, a resolution naming the coordinate of the
   ordinary edge the transfer should take:
   `INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, block_hash, block_time, justification) VALUES ('review-resolution', <id>, '<state>', '<network>', <n>, '<hash bytea>', '<time>', '<why>')`.
   The core verifies the evidence against what the endpoints recorded at final blocks before it runs the step. A
   resolution into `Source-Final`, `Delivering` or `Refunding` moves value and needs another login to insert the
   `manual_approvals` row (`REQ-GV-5`).
3. If a shortfall or excess must be made good outside the store (a top-up or a recovery), take the steps from the
   deployment's own notes and record the act with `operator_record` (kind `other`). Whether the code offers a path to
   complete a transfer on a mismatched leg: not established in the code; assume it does not.

## Never

- Do not refund or fail on a timer or an unknown (REQ-RF-1, REQ-RV-1); a refund needs every attempt proven not executed (INV-8).
- Do not complete the transfer by hand, or edit its amount or status (REQ-GV-5).
- Do not match by amount or time (REQ-OT-2).
- Do not UPDATE or INSERT into transfers, transitions, book or observations; there is no grant for it (REQ-GV-5).
- Do not approve your own resolution; the store refuses it (REQ-GV-5).

## Exercise

On the test bed: pay out a withdrawal with a leg of a different amount, check it lands in review with both amounts
reported, and resolve it with evidence under two logins.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
