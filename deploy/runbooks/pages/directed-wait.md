<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: directed-wait
kind: pages
covers: [REQ-IC-2, REQ-AS-12, REQ-LM-4]
pages: [directed-wait-alert]
observables: [nonfinal_transfers, oldest_nonfinal_transfer_age_seconds]
tests: [services/core/tests/directed_alerts.rs::req_ic_2_directed_deposits_held_or_waiting_past_a_window_page_daily]
exercised: no
---
# Directed deposits held or waiting for longer than a window

## What it is

A directed deposit holds `CND-TokenUpgraded` until two operators lift the hold on an upgrade of its token, and
`CND-DepositCapacity` until its route has room (amendment 0031). Both resume by themselves, and nothing is returned for
either, so neither pages while it is short. This page says that at least one directed deposit on a deposit route has held
one of them for longer than one of the route's windows (`window_seconds`): a lift nobody has made yet, or a route that
stays full. What it costs is the depositor's wait, not value.

## Pages

`directed-wait-alert`: an ERROR line
`page: directed deposits on this route have been held by an upgrade of their token (CND-TokenUpgraded) or waited for its capacity (CND-DepositCapacity) for longer than one window; ... kind=directed-wait-alert asset=<asset> network=<network> deposits=<count> repeat=<repeat>`
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
  AND condition IN ('CND-TokenUpgraded', 'CND-DepositCapacity')
ORDER BY bridge_reference;
```

## Do

1. For `CND-TokenUpgraded`: work the lift (`conditions/CND-TokenUpgraded.md`), and lift every open upgrade of the token,
   not only the newest; one left open keeps the deposits after it held, and this page repeats daily.
2. For `CND-DepositCapacity`: find why transfers in flight are not completing (`conditions/CND-DepositCapacity.md`); if the
   capacity is too small for the route's use, a change of it is a bound of the route, made only by the governed process.

## Never

- Never return, refund or fail a waiting directed deposit to clear the page (REQ-IN-11, REQ-AS-12, REQ-RF-1).
- Never lift a hold without the fork suite run against the new implementation (REQ-AS-12, REQ-GV-5).
- No administrative path moves value or edits a status (`REQ-GV-5`).

## Exercise

On the test bed: make the route's window short, hold a directed deposit under an upgrade or a full route past it, see the
page made once and cleared when the deposit moves.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
