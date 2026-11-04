<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: CND-FeeShort
kind: condition
covers: [REQ-LM-3, REQ-TR-5, REQ-ST-6]
pages: []
observables: [fee_short_transfers, nonfinal_transfers, oldest_nonfinal_transfer_age_seconds]
tests: [services/core/tests/conditions.rs::cnd_fee_short_holds_until_the_fee_position_can_pay]
exercised: no
---
# CND-FeeShort

## What it is

A delivery whose dry run was refused for want of a fee position that can pay for the transaction (`REQ-LM-3`). The core
holds the condition in the transfer's state (`Source-Final`, or `Delivering` for a later attempt), signs nothing, and
orders again after its retry interval; the first order whose dry run passes clears the hold and the delivery goes on.
The core names it from the dry run's refusal text (a fee or payment shortfall); a refusal it cannot name holds no
condition and is a different case (review at the route's deadline, F-003 NOTES item 24). No deadline sends
`CND-FeeShort` to review (F-003 NOTES item 24), so a long hold is silent unless the metric is watched. No fee comes out
of a delivery (`REQ-TR-5`): the cost is the deployment's, against surplus.

## Pages

No page. The signal is the metric `fee_short_transfers` (transfers held, per asset, network and direction), which is the
only signal of a short fee position the store has: no service reads a delivery account's balance yet (migration 0090).
Watch it with `oldest_nonfinal_transfer_age_seconds` on the same route.

## Confirm

```sql
SELECT metric, asset, network, direction, value FROM ops_observables()
WHERE metric IN ('fee_short_transfers', 'nonfinal_transfers', 'oldest_nonfinal_transfer_age_seconds')
  AND (value IS NULL OR value > 0) ORDER BY metric, asset, network;
```

The held transfer's condition shows in `transfer_status` (`SELECT bridge_reference, state, condition FROM transfer_status WHERE condition = 'CND-FeeShort';`); the core's log names the
refusal the dry run returned.

## Do

1. Identify the network and the fee position from the metric's row, then top up the delivery account's fee float from
   the deployment's own notes (the account, source and amount are not in this repository).
2. Record it the same day: `SELECT operator_record('float-top-up', '<network, account name>', '{}'::jsonb, '<why>');`
   (a name, never key material: REQ-KY-1).
3. Wait. The held deliveries resume by themselves on the next order whose dry run passes.

## Never

- Do not refund or fail a transfer on a timer or an unknown (REQ-RF-1, REQ-RV-1).
- Do not take the fee out of the delivery or change its amount (REQ-TR-5); do not match by amount or time (REQ-OT-2).
- Do not treat a float top-up as a deposit or as reserve (INV-1, INV-2).
- Do not UPDATE or INSERT into transfers, transitions, book or observations; there is no grant for it (REQ-GV-5).
- Do not mint before final or deliver before escrow is final to get a held delivery moving (INV-6, INV-7).

## Exercise

On the test bed: make a delivery's dry run refuse for fees, check the transfer holds the condition and
`fee_short_transfers` reads 1, top up, record it with `operator_record`, and see the next order pass and the metric
return to 0.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
