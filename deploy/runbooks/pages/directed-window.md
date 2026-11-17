<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: directed-window
kind: pages
covers: [REQ-IC-2, REQ-LM-1]
pages: [directed-window-alert]
observables: [nonfinal_transfers, oldest_nonfinal_transfer_age_seconds]
tests: [services/core/tests/directed_alerts.rs::req_ic_2_directed_deposits_holding_the_window_above_the_threshold_page]
exercised: no
---
# Directed deposits holding the route's window

## What it is

Directed deposits and requesters' deposits share the route's one window, first come first served by coordinate: there is
no separate share for directed deposits (amendment 0029, Open 5, ruled). A directed deposit over what the window has left
waits in `CND-DepositWindow` and resumes as it rolls over (Open 1), and while it waits it is a claim on the window ahead of
anything later, so a requester's deposit sees the window as used and is refused `ERR-WindowExceeded` (migration 0145). What
it costs is requesters' service, not value. The share directed deposits hold is watched: this page says it is high.

The share is per deposit route: the amounts of directed deposits the window counts now (made, or out of
`CND-DepositWindow`, within the window) and of those still waiting in it, over the window's limit. The threshold is the
core's configuration (`[directed_alerts] window_share_percent`, 50 by default); the page names the share in basis points.

## Pages

`directed-window-alert`: an ERROR line
`page: directed deposits hold more of this route's window than the threshold; ... kind=directed-window-alert asset=<asset> network=<network> share_bps=<share> threshold_bps=<threshold> repeat=<repeat>`
and an `event_log` entry of the same kind. Once on crossing, then once a day while it stands; `directed-alert-cleared`
when back at or below the threshold.

## Confirm

```sql
SELECT seq, appended_at, payload FROM event_log
WHERE kind IN ('directed-window-alert', 'directed-alert-cleared') ORDER BY seq DESC LIMIT 10;
```

The directed deposits waiting for the window:

```sql
SELECT bridge_reference, amount, state FROM transfer_status
WHERE kind = 'deposit' AND request_reference LIKE 'directed:%' AND condition = 'CND-DepositWindow'
ORDER BY bridge_reference;
```

## Do

1. Tell the requesters' owners the window is crowded: their deposits may be refused `ERR-WindowExceeded` until it rolls
   over (`conditions/CND-DepositWindow.md` for the waiting ones).
2. If the route's window is too small for its real use, raising it is a change of the route's declaration, made and
   recorded as the deployment's own notes say, within the on-chain bounds (`REQ-KY-3`); it is not this runbook's to make.
3. If it is abuse, pause the deposit route (`operator_pause`): new deposits wait, nothing is returned.

## Never

- Never return or refuse a waiting directed deposit to free the window: it waits by rule (amendment 0029, Open 1), and no
  path exists to move it.
- Never reserve part of the window for anyone by hand: there is no separate share (Open 5). The one reserve is the
  route's `request_reserve`, which bounds only a queue older than one window (amendment 0031, `d-f018-window-queue`,
  pending confirmation as `d-f018-window-reserve`); it is a route declaration, changed as the route's bounds are.
- No administrative path moves value or edits a status (`REQ-GV-5`).

## Exercise

On the test bed: make the window small, send two directed deposits that together exceed it, see the second wait in
`CND-DepositWindow` and the page made once.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
