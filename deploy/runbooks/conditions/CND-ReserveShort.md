---
id: CND-ReserveShort
kind: condition
covers: [REQ-OT-5, REQ-LM-3, REQ-IC-5]
pages: []
observables: [nonfinal_transfers, oldest_nonfinal_transfer_age_seconds, reserve_amount, reserve_equation_outcome]
tests: [services/core/tests/conditions.rs::cnd_reserve_short_ac_b3_1_holds_and_raises_one_condition_per_asset_and_network, services/core/tests/conditions.rs::cnd_reserve_short_ac_b3_2_resumes_with_no_one_acting_when_reserve_recovers]
exercised: no
---
<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
# CND-ReserveShort

## What it is

A withdrawal in `Source-Final` whose asset's reserve on the destination network, less what every other open withdrawal
may take, cannot cover its amount (`REQ-OT-5`). The core holds the condition as a gate before the payout is ordered;
nothing is ordered, refunded, failed or moved to another network. It raises one condition per (asset, network), not per
transfer (`REQ-LM-3`, `REQ-IC-5`), and clears it once no withdrawal holds it. It resumes by itself when the reserve covers
the amount again (`AC-B3.2`).

## Pages

No page. The signal is the requester's condition webhook (`held` then `released`, once per route), the withdrawal's
status showing the condition, and the metrics `nonfinal_transfers` and `oldest_nonfinal_transfer_age_seconds` on the
withdrawal route, beside `reserve_amount` for the network.

## Confirm

```sql
SELECT metric, asset, network, direction, value FROM ops_observables()
WHERE metric IN ('nonfinal_transfers', 'oldest_nonfinal_transfer_age_seconds', 'reserve_amount',
                 'reserve_equation_outcome') ORDER BY metric, asset, network;
```

The reserve equation outcome should be 0 (holds). The held withdrawals are in `transfer_status`
(`WHERE condition = 'CND-ReserveShort'`) and the raise is the `condition-raised` entry in `event_log`.

## Do

1. Confirm the equation holds (above). If it does not, this is a reserve incident: follow `../pages/reserve.md`.
2. If the reserve is simply short of what is owed, top it up from the deployment's own notes (the funding source and
   address are not in this repository), as a surplus contribution from a declared funding address. Record the act the
   same day: `SELECT operator_record('surplus-contribution', '<asset, network>', '{}'::jsonb, '<why>');`.
3. Wait. The core re-checks each cycle; when the final reading covers the amount the hold clears and the payout is
   ordered with no one acting.

## Never

- Do not refund or fail a withdrawal on a timer or because it is held (REQ-RF-1, REQ-RV-1).
- Do not pay from another network's reserve (`REQ-OT-4`, `REQ-OT-5`).
- Do not treat an operator top-up as a deposit or mint against it (INV-1, INV-2).
- Do not match by amount or time (REQ-OT-2); do not merge issuers (REQ-AS-3).
- Do not UPDATE or INSERT into transfers, transitions, book or observations; there is no grant for it and the runbook
  never asks (REQ-GV-5). Do not deliver before the redemption is final (INV-7).

## Exercise

On the test bed: redeem more than the reserve covers, check the withdrawal holds the condition and the requester gets
one `held` webhook, then fund the reserve and see it clear, the payout ordered and a `released` webhook sent.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
