<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: CND-DestinationRefused
kind: condition
covers: [REQ-IN-8, REQ-IN-9, REQ-RV-1, REQ-RV-2, REQ-GV-5]
pages: []
observables: [reviews_open, reviews_oldest_age_seconds, nonfinal_transfers, oldest_nonfinal_transfer_age_seconds]
tests: [services/core/tests/conditions.rs::cnd_destination_refused_mints_nothing_and_resumes_when_the_mint_applies, services/core/tests/conditions.rs::cnd_destination_refused_past_its_deadline_goes_to_review, services/core/src/engine.rs::req_in_9_a_mint_the_issuing_network_refuses_holds_cnd_destination_refused, services/core/tests/review.rs::req_rv_2_a_review_ends_by_evidence_bezent_verifies_then_the_ordinary_step_runs, services/core/tests/review.rs::req_gv_5_a_resolution_that_moves_value_waits_for_its_second_approver]
exercised: no
---
# CND-DestinationRefused

## What it is

The issuing network refused a deposit's mint because of its destination; nothing was minted (`REQ-IN-8`). bezent never
checks whether a destination can receive the asset (`REQ-IN-9`): the core reads the refusal from the dry run (a reason
the issuing network's assets API returns) and holds the condition in `Source-Final`. It orders again after its retry
interval, and clears the hold if a later dry run passes (the test: "resumes when the mint applies"). Past the route's
completion budget, counted from the source leg's finality on the destination network's final head, it is a condition
that blocks delivery, and the transfer goes to `Needs-Review` with the condition kept (SPEC §5.4; `REQ-IN-9`).

## Pages

No page. Before the deadline the signal is the deposit's status showing the condition. After it, the transfer is a row
in the `reviews` view (owner `operations`, due one business day later), counted by `reviews_open` and aged by
`reviews_oldest_age_seconds` (`REQ-RV-1`).

## Confirm

```sql
SELECT transfer_id, bridge_reference, condition, state_before_review, entered_at, owner, due_at
FROM reviews WHERE condition = 'CND-DestinationRefused';
SELECT metric, value FROM ops_observables() WHERE metric IN ('reviews_open', 'reviews_oldest_age_seconds');
```

The core's log carries the refusal text. A destination that the issuing network cannot receive on is the requester's to
correct: the destination is theirs.

## Do

1. Before the deadline: tell the requester and wait. If the cause clears on the network (for example the account can now
   be created), the next order passes and the mint goes through.
2. In review: end it only by evidence (`REQ-RV-2`). The coordinate is the final block the next step rests on; the core
   checks it itself. Record, as your own login, a resolution into the state one ordinary edge from `Source-Final`:
   `INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, block_hash, block_time, justification) VALUES ('review-resolution', <id>, 'Delivering', '<network>', <n>, '<hash bytea>', '<time>', '<why>')`.
   A resolution into `Source-Final`, `Delivering` or `Refunding` moves value and needs a second approver, another
   login: `INSERT INTO manual_approvals (action_id, justification) VALUES (<action id>, '<why>')`. The core then runs the
   ordinary step, and the review gates (pause, cut-off, reserve) still apply. For a deposit that went through
   `CND-AwaitingConversion` (a converted deposit), the approval must also name the output it approved, in
   `approved_output_*`; see `CND-AwaitingConversion.md` (Do, step 2) and amendment 0018, Open 6.
3. A refund of a deposit that cannot be minted is not an operator edge from this review: see F-003 NOTES and the
   requester's refund path (`REQ-RQ-6`); not established here beyond that.

## Never

- Do not refund or fail on a timer or an unknown (REQ-RF-1, REQ-RV-1).
- Do not mint before final (INV-6); do not match by amount or time (REQ-OT-2).
- Do not approve your own resolution; the store refuses it (REQ-GV-5).
- Do not UPDATE or INSERT into transfers, transitions, book or observations; there is no grant for it (REQ-GV-5).
- Do not merge issuers (REQ-AS-3).

## Exercise

On the test bed: deposit to a destination the issuing network will not credit, check the hold, advance past the
deadline and see the review; resolve it with evidence under two logins.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
