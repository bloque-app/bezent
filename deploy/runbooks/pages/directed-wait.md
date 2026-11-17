<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: directed-wait
kind: pages
covers: [REQ-IC-2, REQ-AS-12, REQ-LM-4, REQ-IN-11, REQ-LM-1]
pages: [directed-wait-alert]
observables: [nonfinal_transfers, oldest_nonfinal_transfer_age_seconds]
tests: [services/core/tests/directed_alerts.rs::req_ic_2_directed_deposits_held_or_waiting_past_a_window_page_daily, services/core/tests/directed_alerts.rs::req_ic_2_a_directed_deposit_waiting_for_the_window_past_two_windows_pages]
exercised: no
---
# Directed deposits held or waiting for too long

## What it is

A directed deposit holds `CND-TokenUpgraded` until two operators lift the hold on an upgrade of its token,
`CND-DepositCapacity` until its route has room, and `CND-DepositWindow` until its route's window has room for it behind
every directed deposit ahead of it (amendments 0029 and 0031). All resume by themselves, and nothing is returned for any,
so none pages while it is short. This page says that at least one directed deposit on a deposit route has held
`CND-TokenUpgraded` or `CND-DepositCapacity` for longer than one of the route's windows (`window_seconds`), or
`CND-DepositWindow` for longer than two: a lift nobody has made yet, a route that stays full, or a window queue that does
not drain. A waiting deposit's claim on the window stops counting against requests one window after it began to wait, so
one still waiting a window after that is being outrun, by requests or by the queue's head (amendment 0031, part 5; the
requests' reserve that would bound this is not in force, pending `d-f018-window-reserve`). What it costs is the
depositor's wait, not value.

## Pages

`directed-wait-alert`: an ERROR line
`page: directed deposits on this route have been held by an upgrade of their token (CND-TokenUpgraded) or waited for its capacity (CND-DepositCapacity) for longer than one window, or for its window (CND-DepositWindow) for longer than two; ... kind=directed-wait-alert asset=<asset> network=<network> deposits=<count> repeat=<repeat>`
and an `event_log` entry of the same kind. Once on crossing, then once a day while it stands; `directed-alert-cleared`
once none is held that long.

## Confirm

```sql
SELECT seq, appended_at, payload FROM event_log
WHERE kind IN ('directed-wait-alert', 'directed-alert-cleared') ORDER BY seq DESC LIMIT 10;
```

The deposits it counts:

```sql
SELECT bridge_reference, amount, condition, state FROM transfer_status
WHERE kind = 'deposit' AND request_reference LIKE 'directed:%'
  AND condition IN ('CND-TokenUpgraded', 'CND-DepositCapacity', 'CND-DepositWindow')
ORDER BY bridge_reference;
```

## Do

1. For `CND-TokenUpgraded`: work the lift (`conditions/CND-TokenUpgraded.md`), and lift every open upgrade of the token,
   not only the newest; one left open keeps the deposits after it held, and this page repeats daily.
2. For `CND-DepositCapacity`: find why transfers in flight are not completing (`conditions/CND-DepositCapacity.md`); if the
   capacity is too small for the route's use, a change of it is a bound of the route, made only by the governed process.
3. For `CND-DepositWindow`: read the route table's window (`used`, `remaining`) and the queue
   (`conditions/CND-DepositWindow.md`). Tell the requesters' owners and the owner of the decision that the queue is
   being outrun; if the window is too small for the route's real use, raising it is a change of the route's declaration,
   made only by the governed process. Do not set `routes.request_reserve`: it is not in force until
   `d-f018-window-reserve` is ruled.

## Never

- Never return, refund or fail a waiting directed deposit to clear the page (REQ-IN-11, REQ-AS-12, REQ-RF-1).
- Never lift a hold without the fork suite run against the new implementation (REQ-AS-12, REQ-GV-5).
- No administrative path moves value or edits a status (`REQ-GV-5`).

## Exercise

On the test bed: make the route's window short, hold a directed deposit under an upgrade or a full route past it, or keep
one in the window's queue past two windows, see the page made once and cleared when the deposit moves.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
