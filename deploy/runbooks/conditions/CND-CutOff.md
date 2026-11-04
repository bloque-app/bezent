---
id: CND-CutOff
kind: condition
covers: [REQ-LM-1, REQ-IC-5]
pages: []
observables: [nonfinal_transfers, oldest_nonfinal_transfer_age_seconds]
tests: [services/core/tests/conditions.rs::cnd_cutoff_a_step_past_the_route_cut_off_holds_until_it_reopens, services/ingress/tests/requests.rs::req_lm_1_cnd_cutoff_no_request_is_refused_for_a_cut_off, crates/store/tests/lifecycle_graph.rs::cnd_cutoff_is_a_named_condition, services/core/src/cutoff.rs::cnd_cutoff_route_is_closed_only_inside_a_declared_window]
exercised: no
---
<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
# CND-CutOff

## What it is

The transfer's next step falls inside a closed window of its route's declared cut-offs. The core holds the condition
in `Source-Final`, orders nothing, and resumes by itself when the route reopens (`REQ-LM-1`). A cut-off refuses no
request: the ingress accepts it and the transfer waits. A window is declared per route as weekdays and a start and end
time in UTC; a declaration the core cannot read closes the route (F-003 NOTES item 8). Refunds pass no cut-off. It
sends nothing to review: the gate conditions clear by themselves (F-003 NOTES item 24).

## Pages

No page. The signal is the transfer's status showing the condition, the requesters' condition webhook (once per route,
`REQ-IC-5`), and the metrics `nonfinal_transfers` and `oldest_nonfinal_transfer_age_seconds` on the route. A held
count that grows while the route's declared window is open points to an unreadable declaration.

## Confirm

```sql
SELECT metric, asset, network, direction, value FROM ops_observables()
WHERE metric IN ('nonfinal_transfers', 'oldest_nonfinal_transfer_age_seconds')
  AND (value IS NULL OR value > 0) ORDER BY metric, asset, network;
```

The route's declared cut-offs are not readable by an operator login; read them through the route declaration the
requesters see (`REQ-LM-1`, machine-readable) or the deployment's own notes. Compare the current UTC time with the
window.

## Do

1. Wait: it resumes at the next cycle after the window closes, with no one acting.
2. If the window should be open and the route stays held, the declaration is wrong or unreadable. The routes are the
   migration owner's declarations, outside the operator's reach and outside this runbook: take it to the deployment's
   own change process. Meanwhile pause nothing for this: a pause would only add a second hold.
3. Tell requesters the transfer is delayed, not failed (REQ-IC-3).

## Never

- Do not refund or fail a transfer on a timer or because it is held (REQ-RF-1, REQ-RV-1).
- Do not edit the route declaration, a status, or a transition by SQL; no grant exists for it (REQ-GV-5).
- Do not UPDATE or INSERT into transfers, transitions, book or observations (REQ-GV-5).
- Do not mint before final or deliver before escrow is final to get past a window (INV-6, INV-7).
- Do not match by amount or time (REQ-OT-2).

## Exercise

On the test bed: declare a window that is closed now, request a deposit and check it holds the condition with nothing
ordered; move to the window's end and check it resumes. Declare an unreadable window and check the route stays closed.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
