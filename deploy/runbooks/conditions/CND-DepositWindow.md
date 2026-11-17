<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: CND-DepositWindow
kind: condition
covers: [REQ-IN-11, REQ-LM-1, REQ-LM-3]
pages: [deposit-window-alert]
observables: [nonfinal_transfers, oldest_nonfinal_transfer_age_seconds]
tests: [services/core/tests/directed.rs::ac_a6_3_cnd_deposit_window_waits_first_come_first_served_and_resumes_as_it_rolls_over, services/core/tests/directed.rs::cnd_deposit_window_a_window_that_can_never_fit_pages, services/core/tests/directed.rs::req_in_9_a_deposit_that_waited_for_the_window_gets_its_whole_refusal_deadline]
exercised: no
---
# CND-DepositWindow

## What it is

A directed deposit (amendment 0029) over what its route's window has left. There is no request to refuse, because the
value has already moved, so the deposit waits at `Source-Final`, mints nothing, and resumes by itself as the window rolls
over, in the order of arrival (SPEC §10, `REQ-IN-11`). It is never returned for this reason (amendment 0029, Open 1,
ruled: wait). Directed deposits and requesters' deposits share the one window. A deposit that waited gets the whole of
the route's deadline for a later `CND-DestinationRefused`, counted from when it left the condition.

## Pages

While the window will roll over, no page: the signal is the deposit's status (`Source-Final`, holding the condition)
and the metrics `nonfinal_transfers` and `oldest_nonfinal_transfer_age_seconds` on the deposit route.

`deposit-window-alert` pages when a waiting deposit can never fit as its route is declared now: the route was removed
(`route-not-declared`) or its window's limit lowered below the deposit's amount (`above-window-limit`). Once per reason,
then once a day while it lasts; also in the event log. The deposit keeps waiting: nothing is returned or reviewed for
it. Whether such a deposit should instead be returned is a question for the spec, not for the operator.

## Confirm

```sql
SELECT metric, asset, network, direction, value FROM ops_observables()
WHERE metric IN ('nonfinal_transfers', 'oldest_nonfinal_transfer_age_seconds') ORDER BY metric, asset, network;
```

Then read the held deposit's status in `transfer_status` (condition `CND-DepositWindow`), and the route table for what
the window has left. For a page:

```sql
SELECT transfer_id, payload FROM event_log WHERE kind = 'deposit-window-alert' ORDER BY seq DESC LIMIT 20;
```

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

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
