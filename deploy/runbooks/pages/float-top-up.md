<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: float-top-up
kind: procedure
covers: [REQ-TR-5, REQ-IC-2]
pages: []
observables: [fee_short_transfers]
tests: [services/core/tests/conditions.rs::cnd_fee_short_holds_until_the_fee_position_can_pay]
exercised: no
---
# Top-up of a delivery account's fee float

## What it is

Each delivery key pays its network's fees from a float on that network: the issuing network's delivery account holds a
float of the network's native token (a storage deposit is charged on a mint and for each new reference after it, PLAN
`DEC-18`), and each origin network's delivery account holds that network's gas token. A fee is never taken out of a
delivery (`REQ-TR-5`, guardrail 10); costs go against surplus.

## Pages

**None exists.** No service reads a float's balance (`features/F-014-operations/NOTES.md`, "Fee positions"), so no alert
floor fires. The signal that exists is late: transfers held in `CND-FeeShort` (`fee_short_transfers` above zero), which
means a delivery already could not pay. Check the floats on a schedule, from the deployment's own notes, until the balance
read exists.

## Confirm

```sql
SELECT metric, asset, network, direction, value FROM ops_observables() WHERE metric = 'fee_short_transfers';
```

and the delivery account's native balance on its network from public chain data.

## Do

1. Fund the delivery account from the operator's own funds, from the deployment's own notes. This is a transfer of
   the fee token only; it is **not** a deposit and not a surplus contribution of the bridged asset.
2. Record it: `SELECT operator_record('float-top-up', '<network, account name>', '{}'::jsonb, '<why>');` (an amount may be
   stated in the detail; no key material, no address of a private account).
3. Held transfers resume by themselves on the next cycle that finds the position able to pay (`CND-FeeShort`).

## Never

- Never fund a delivery account from the reserve, or take the fee out of a delivery or a refund (`REQ-TR-5`, `INV-14`).
- Never send the bridged asset or its origin token to a delivery account as a top-up: an arrival at a reserve is
  a deposit or owed mint (`INV-1`, `INV-2`).
- Never raise the key's bound on chain to cover a shortfall (`REQ-KY-3`, `REQ-GV-2`).
- Never release a held transfer, or change its status, once the float is topped up: it resumes by itself, and no
  administrative path edits a status or moves value (`REQ-GV-5`).

## Exercise

On the test bed: drain the float, see transfers held in `CND-FeeShort`, top up and see them resume.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
