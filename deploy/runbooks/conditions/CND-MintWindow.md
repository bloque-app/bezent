<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: CND-MintWindow
kind: condition
covers: [REQ-LM-3, REQ-KY-3, INV-3]
pages: []
observables: [nonfinal_transfers, oldest_nonfinal_transfer_age_seconds]
tests: [services/core/tests/conditions.rs::cnd_mint_window_holds_until_the_window_rolls_over]
exercised: no
---
# CND-MintWindow

## What it is

A mint (a deposit, or a refund mint) whose dry run the issuing network refused as above the window or above the
ceiling (the core reads `AboveWindow` or `AboveCeiling` in the refusal). The on-chain component bounds mints per window
and in total (`INV-3`, `REQ-KY-3`); bezent does not widen the bound. The core holds the condition, mints nothing, orders
again after its retry interval, and the first order whose dry run passes clears it. The window rolls over on chain, so
it resumes by itself (SPEC §10). No deadline sends it to review (F-003 NOTES item 24). The test covers the
`AboveWindow` wording; `AboveCeiling` maps to the same condition in the code, with no test of its own.

## Pages

No page. The signal is the transfer's status (a deposit in `Source-Final` holding the condition) and the metrics
`nonfinal_transfers` and `oldest_nonfinal_transfer_age_seconds` on the deposit route. There is no metric of its own.
Not established in the code: how long a window lasts or how much of it is left; the on-chain component's own reading
shows that, from the deployment's own notes.

## Confirm

```sql
SELECT metric, asset, network, direction, value FROM ops_observables()
WHERE metric IN ('nonfinal_transfers', 'oldest_nonfinal_transfer_age_seconds') ORDER BY metric, asset, network;
```

Then read the held deposit's status in `transfer_status` (condition `CND-MintWindow`) and the core's
log for the dry run's refusal. A refusal of the ceiling, not the window, does not roll over: take it to the people who
govern the bound.

## Do

1. Wait: it resumes when the window rolls over, with no one acting.
2. If it is a ceiling and not a window, or the wait is unacceptable, a bound can be raised only by the governance
   process, with several signers (REQ-GV-2, REQ-GV-3, REQ-KY-3), from the deployment's own notes. After it, record it:
   `SELECT operator_record('bound-change', '<asset>', '{}'::jsonb, '<why>');`.

## Never

- Do not refund or fail a deposit because the window is spent (REQ-RF-1, REQ-RV-1).
- Do not mint before the arrival is final (INV-6), and do not mint from a funding top-up as if it were a deposit (INV-1, INV-2).
- Do not match by amount or time (REQ-OT-2); do not merge issuers (REQ-AS-3).
- Do not raise a bound on your own, or by any path that is not the governed one (REQ-GV-2).
- Do not UPDATE or INSERT into transfers, transitions, book or observations; there is no grant for it (REQ-GV-5).

## Exercise

On the test bed: spend the mint window, request a deposit and check it holds the condition with nothing minted; let
the window roll over and see the mint ordered, signed and delivered.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
