<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: CND-DepositWindow
kind: condition
covers: [REQ-IN-11, REQ-LM-1, REQ-LM-3, AC-A6.7]
pages: [deposit-window-alert, directed-wait-alert]
observables: [nonfinal_transfers, oldest_nonfinal_transfer_age_seconds]
tests: [services/core/tests/directed.rs::ac_a6_3_cnd_deposit_window_waits_first_come_first_served_and_resumes_as_it_rolls_over, services/core/tests/directed.rs::cnd_deposit_window_a_window_that_can_never_fit_pages, services/core/tests/directed.rs::req_in_9_a_deposit_that_waited_for_the_window_gets_its_whole_refusal_deadline, services/core/tests/directed.rs::req_in_9_a_signed_mint_after_the_window_gets_its_whole_delivering_deadline, services/core/tests/directed.rs::ac_a6_7_a_queue_of_several_windows_leaves_requesters_room_after_one_window, services/core/tests/directed.rs::req_in_11_with_the_reserve_off_the_queue_head_takes_the_freed_room_first, services/core/tests/directed_alerts.rs::req_ic_2_a_directed_deposit_waiting_for_the_window_past_two_windows_pages]
exercised: no
---
# CND-DepositWindow

## What it is

A directed deposit (amendment 0029) over what its route's window has left. There is no request to refuse, because the
value has already moved, so the deposit waits at `Source-Final`, mints nothing, and resumes by itself as the window rolls
over, in the order of arrival (SPEC §10, `REQ-IN-11`). It is never returned for this reason (amendment 0029, Open 1,
ruled: wait). Directed deposits and requesters' deposits share the one window. A deposit that waited gets the whole of
the route's deadline for a later `CND-DestinationRefused`, counted from when it left the condition, and so does its
signed mint in `Delivering` (unknown past its deadline; PR #136, review M-a).

The queue claims one window at most (amendment 0031, `d-f018-window-queue`). A waiting deposit counts against
requesters' room in the window only for one window (the route's window length) from when it began to wait, and the
waiting deposits together for no more than the window's limit (`route_window_used()`, which the ingress's checks and its
route table read). A deposit that has waited longer still waits, and still goes before every later directed deposit,
but requesters' deposits no longer see it: once room frees, a request may take it, and the queue waits for the room
after. In practice the core, which cycles every few seconds, usually gets there first: right after the window rolls
over, the queue's head takes the freed room before a request can come. That gap is open, pending the owner's ruling
`d-f018-window-reserve` (amendment 0031, part 5); the route's `request_reserve`, which would keep part of the window for
requests, is 0 on every route and is not in force. While it waits for the window a deposit carries nothing and is not in
flight for the route's capacity. When it leaves the window,
a deposit whose route is at its capacity waits on in `CND-DepositCapacity`.

## Pages

While the window will roll over, no page: the signal is the deposit's status (`Source-Final`, holding the condition)
and the metrics `nonfinal_transfers` and `oldest_nonfinal_transfer_age_seconds` on the deposit route.

`deposit-window-alert` pages when a waiting deposit can never fit as its route is declared now: the route was removed
(`route-not-declared`) or its window's limit lowered below the deposit's amount (`above-window-limit`). Once per reason,
then once a day while it lasts; also in the event log. The deposit keeps waiting: nothing is returned or reviewed for
it. Whether such a deposit should instead be returned is a question for the spec, not for the operator.

`directed-wait-alert` pages, per deposit route, once any directed deposit has waited in the condition for longer than two
of the route's windows (`window_seconds`): its claim stopped counting against requests after one, so by then it is being
outrun, by requests or by the queue's head. Once on crossing, then once a day while it stands; `directed-alert-cleared`
once none waits that long (`pages/directed-wait.md`).

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
