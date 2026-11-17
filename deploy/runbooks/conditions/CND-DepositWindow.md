<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: CND-DepositWindow
kind: condition
covers: [REQ-IN-11, REQ-LM-1, REQ-LM-3]
pages: []
observables: [nonfinal_transfers, oldest_nonfinal_transfer_age_seconds]
tests: []
exercised: no
---
# CND-DepositWindow

## What it is

A directed deposit (amendment 0029) over what its route's window has left. There is no request to refuse, because the
value has already moved, so the deposit waits at `Source-Final`, mints nothing, and resumes by itself as the window rolls
over, in the order of arrival (SPEC §10, `REQ-IN-11`). It is never returned for this reason (amendment 0029, Open 1,
ruled: wait). Directed deposits and requesters' deposits share the one window. Not on this base: the core's directed
intake that sets the condition is `F-018` (`T-018-07`), so no test is listed yet.

## Pages

No page. The signal is the deposit's status (`Source-Final`, holding the condition) and the metrics
`nonfinal_transfers` and `oldest_nonfinal_transfer_age_seconds` on the deposit route. The alert on how much of the
window directed deposits take is `F-018`'s and is not on this base.

## Confirm

```sql
SELECT metric, asset, network, direction, value FROM ops_observables()
WHERE metric IN ('nonfinal_transfers', 'oldest_nonfinal_transfer_age_seconds') ORDER BY metric, asset, network;
```

Then read the held deposit's status in `transfer_status` (condition `CND-DepositWindow`), and the route table for what
the window has left.

## Do

1. Wait: it resumes when the window rolls over, with no one acting, oldest arrival first.
2. If the wait is unacceptable, the window is a bound of the route, changed only by the governed process (REQ-GV-2,
   REQ-GV-3). After it, record it: `SELECT operator_record('bound-change', '<asset>', '{}'::jsonb, '<why>');`.

## Never

- Do not return, refund or fail a directed deposit because the window is spent (REQ-IN-11, REQ-RF-1).
- Do not mint before the arrival is final (INV-6), and do not let a later arrival overtake an earlier one.
- Do not match by amount or time (REQ-OT-2).
- Do not raise a bound on your own, or by any path that is not the governed one (REQ-GV-2).
- Do not UPDATE or INSERT into transfers, transitions, book or observations; there is no grant for it (REQ-GV-5).

## Exercise

On the test bed, once `F-018` builds it: spend the route's window, make a directed deposit and check it holds the
condition with nothing minted and nothing returned; let the window roll over and see the mint ordered, signed and
delivered.

Status: not exercised (NFR-9). No test proves the behaviour on this base yet (`F-018`).
