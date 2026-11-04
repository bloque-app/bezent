<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: CND-Withdrawn
kind: condition
covers: [REQ-IN-7, REQ-OB-4, REQ-RV-1, REQ-RV-2, INV-16]
pages: []
observables: [reviews_open, reviews_oldest_age_seconds, open_gaps, nonfinal_transfers]
tests: [services/core/tests/conditions.rs::cnd_withdrawn_an_inclusion_that_stops_matching_the_chain_goes_to_review, services/core/tests/lifecycle.rs::ac_a2_3_an_observation_withdrawn_before_finality_is_withdrawn_and_mints_nothing]
exercised: no
---
# CND-Withdrawn

## What it is

An observation that stopped matching the chain before finality (`REQ-IN-7`, `REQ-OB-4`). Two cases in the code. (1) A
source leg in `Accepted`: the transfer goes to the terminal state `Withdrawn`, with no condition; nothing was minted and
anything waiting on it stops (`AC-A2.3`). (2) A destination leg seen included while `Delivered` and then withdrawn: the
transfer goes to `Needs-Review` holding `CND-Withdrawn`, because the inclusion is now unknown, not failed
(F-001 NOTES S5, `REQ-RV-1`). Only the second is a condition an operator acts on. The core rests each move on a final
head and derives no disposition from a non-final observation (`INV-16`).

## Pages

No page. Case 1 is visible in the transfer's status. Case 2 is a row in `reviews` (owner `operations`, due one
business day after entry), counted by `reviews_open` and aged by `reviews_oldest_age_seconds`. A reorganisation deeper
than expected is its own incident (`REQ-IC-2`): see `../incidents/`.

## Confirm

```sql
SELECT transfer_id, bridge_reference, kind, state_before_review, entered_at, due_at
FROM reviews WHERE condition = 'CND-Withdrawn';
SELECT metric, network, value FROM ops_observables() WHERE metric IN ('open_gaps', 'reviews_open');
```

Look at the destination network at a final block, from public chain data (`REQ-ST-1`): is a leg carrying the
transfer's reference there now, in a different block?

## Do

1. Case 1: nothing to do. If the arrival is real and re-included, the requester binds the new coordinate with a new
   deposit request; the old coordinate is not reused.
2. Case 2: wait for the chain to settle and for the endpoints to record the new observation at a final block. The
   review ends only by evidence (`REQ-RV-2`): record, as your own login, a resolution naming the coordinate of the
   ordinary edge (`Delivered` if the leg is included again, `Delivering` for a new attempt once the previous one is
   proven not executed, `Refunding` after the same proof):
   `INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, block_hash, block_time, justification) VALUES ('review-resolution', <id>, '<state>', '<network>', <n>, '<hash bytea>', '<time>', '<why>')`.
   A resolution into `Delivering` or `Refunding` moves value and needs another login's `manual_approvals` row. The core
   withholds a new attempt or a refund while an inclusion of the payout stands (F-003 NOTES item 26 d, e).
3. Tell the requester the transfer is delayed, not failed (REQ-IC-3).

## Never

- Do not refund, retry as a new transaction or fail on an unknown or a timer (REQ-RF-1, REQ-RV-1).
- Do not derive an outcome from a non-final observation (INV-16); do not match by amount or time (REQ-OT-2).
- Do not mint before final or deliver before escrow is final (INV-6, INV-7).
- Do not UPDATE or INSERT into transfers, transitions, book or observations; there is no grant for it (REQ-GV-5).

## Exercise

On the test bed: for case 1, withdraw the arrival's observation before finality and check the deposit is `Withdrawn`
with nothing minted. For case 2, withdraw an included leg of a `Delivered` transfer, check the review, and resolve it
with evidence once the new observation is final.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
