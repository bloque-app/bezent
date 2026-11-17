<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: CND-DepositCapacity
kind: condition
covers: [REQ-IN-11, REQ-LM-4, AC-A6.3]
pages: [deposit-capacity-alert]
observables: [nonfinal_transfers, oldest_nonfinal_transfer_age_seconds]
tests: [services/core/tests/directed.rs::ac_a6_3_cnd_deposit_capacity_waits_in_order_resumes_and_is_never_returned, services/core/tests/directed.rs::req_in_11_a_deposit_that_waited_for_capacity_gets_its_whole_refusal_deadline]
exercised: no
---
# CND-DepositCapacity

## What it is

A directed deposit (amendment 0029) over its route's capacity: the route already carries as many transfers in flight as
it declares (`REQ-LM-1`). A request would be refused `ERR-Overloaded` (`REQ-LM-4`), but a directed deposit is not a
request: its value has already moved, so it waits at `Source-Final`, mints nothing, and resumes by itself when the route
has room, in the order of arrival (amendment 0031, `d-f018-capacity`, ruled: "Wait for capacity"; SPEC §10,
`REQ-IN-11`). It is never returned for capacity. While it waits it counts as in flight, so requests that come after it
are refused `ERR-Overloaded`, first come first served. The window comes first: a deposit that waited in
`CND-DepositWindow` and leaves it while the route is full waits on here. A deposit that waited gets the whole of the
route's deadline for a later `CND-DestinationRefused`, counted from when it left the condition.

## Pages

While transfers in flight complete, no page: the signal is the deposit's status (`Source-Final`, holding the condition)
and the metrics `nonfinal_transfers` and `oldest_nonfinal_transfer_age_seconds` on the deposit route.

`deposit-capacity-alert` pages when a waiting deposit can never have room as its route is declared now: the route was
removed (`route-not-declared`) or its capacity declared zero (`capacity-zero`). Once per reason, then once a day while
it lasts; also in the event log. The deposit keeps waiting: nothing is returned or reviewed for it.

## Confirm

```sql
SELECT metric, asset, network, direction, value FROM ops_observables()
WHERE metric IN ('nonfinal_transfers', 'oldest_nonfinal_transfer_age_seconds') ORDER BY metric, asset, network;
```

Then read the held deposit's status in `transfer_status` (condition `CND-DepositCapacity`), and the route table
(`GET /v0/routes`: `capacity.declared`, `capacity.in_flight`). For a page:

```sql
SELECT transfer_id, payload FROM event_log WHERE kind = 'deposit-capacity-alert' ORDER BY seq DESC LIMIT 20;
```

## Do

1. Wait: it resumes as transfers in flight complete, with no one acting, oldest arrival first.
2. If transfers in flight are stuck, that is the incident to work: see the runbook of the condition they hold.
3. If the capacity itself is wrong, it is a bound of the route, changed only by the governed process (REQ-GV-2,
   REQ-GV-3). After it, record it: `SELECT operator_record('bound-change', '<asset>', '{}'::jsonb, '<why>');`.

## Never

- Do not return, refund or fail a directed deposit because the route is full (REQ-IN-11, REQ-LM-4, REQ-RF-1).
- Do not let a later arrival overtake an earlier one, and do not match by amount or time (REQ-OT-2).
- Do not raise a bound on your own, or by any path that is not the governed one (REQ-GV-2).
- Do not UPDATE or INSERT into transfers, transitions, book or observations; there is no grant for it (REQ-GV-5).

## Exercise

On the test bed: fill the deposit route to its capacity, make a directed deposit and check it holds the condition with
nothing minted and nothing returned; let a transfer complete and see the deposit's mint ordered.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
