---
id: CND-Paused
kind: condition
covers: [REQ-GV-1, REQ-GV-2, REQ-IC-2, REQ-IC-3, REQ-IC-5, INV-15]
pages: [reserve-divergence, reserve-check-incomplete, supply-mismatch-pause, book-quarantine]
observables: [pauses_in_force, nonfinal_transfers, oldest_nonfinal_transfer_age_seconds]
tests: [services/core/tests/conditions.rs::cnd_paused_new_work_waits_and_resumes_when_the_route_reopens, services/core/tests/conditions.rs::cnd_paused_work_in_flight_continues, services/core/tests/operations.rs::ac_i2_1_an_operators_pause_holds_new_work_in_each_scope_and_a_second_operator_lifts_it, services/core/tests/operations.rs::ac_i2_1_a_pause_does_not_fail_or_refund_work_in_flight]
exercised: no
---
<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
# CND-Paused

## What it is

New work for a route waits because a pause covers it. A pause names an asset, a network and a direction, each optional
(an empty one matches all); the core's gate holds a transfer in `Source-Final` with this condition, and the ingress
refuses new requests with `ERR-RoutePaused`. A pause stops new work only: transfers in flight keep being observed and
completed, never failed or refunded (`REQ-GV-1`, `INV-15`). The same condition holds new mints of an asset while a
reversal has taken its surplus below the floor, until the reserve is restored (`REQ-BE-6`); that resumes by itself.
Refunds pass no pause (F-006 NOTES). A pause an operator made is lifted by two logins; one the reserve check made is
lifted the same way, the core never lifts its own (F-004 NOTES).

## Pages

A pause made by the operator raises no page. A pause the reserve check made does, and the page names its cause:
`reserve-divergence`, `reserve-check-incomplete`, `supply-mismatch-pause`, `book-quarantine`. Their runbook is
`../pages/reserve.md`; follow it first. Otherwise the signal is the metric `pauses_in_force` and the requesters'
condition webhook (once per route, `REQ-IC-5`).

## Confirm

```sql
SELECT metric, value FROM ops_observables() WHERE metric = 'pauses_in_force';
SELECT id, kind, subject, detail, actor, justification, recorded_at
FROM admin_actions WHERE kind IN ('pause', 'unpause-request', 'unpause') ORDER BY id DESC LIMIT 20;
SELECT asset, evaluated_at, outcome, reason FROM reserve_evaluations ORDER BY id DESC LIMIT 5;
```

```sql
SELECT id, asset, network, direction, cause FROM pauses ORDER BY id;
```

`pauses` lists every pause in force; `cause` is empty for an operator's pause and names the requirement for one the core
made. An operator's own pause also has its `pause_id` in `admin_actions.detail`.

## Do

1. Pause first when the cause is not understood: `SELECT operator_pause('<asset or NULL>', '<network or NULL>',
   '<deposit|withdrawal or NULL>', '<why>');` It returns the pause id. One person may pause (`REQ-GV-2`).
2. Understand the cause. For a reserve page, follow `../pages/reserve.md`. Tell requesters in-flight work is delayed,
   not failed (REQ-IC-3).
3. Lift it with two logins: the first, `SELECT operator_request_unpause(<pause id>, '<why>');`; a different login,
   `SELECT operator_approve_unpause(<request id>, '<why>');`. The held work resumes by itself at the core's next cycle.
4. The surplus-floor hold has nothing to lift: it ends when the reserve is restored. The restoration is the
   attesting party's, from the deployment's own notes; record it with `operator_record` (kind `other`).

## Never

- Do not lift a pause you do not understand; do not lift it alone (REQ-GV-2, REQ-GV-5).
- Do not refund or fail transfers in flight because of a pause (REQ-RF-1, REQ-RV-1, REQ-IC-3).
- Do not mint before final or deliver before escrow is final to get work through (INV-6, INV-7).
- Do not delete from `pauses` or UPDATE or INSERT into transfers, transitions, book or observations; there is no
  grant for it (REQ-GV-5). Do not match by amount or time (REQ-OT-2).
- Do not treat a top-up made to clear a reserve pause as a deposit (INV-1, INV-2); do not merge issuers (REQ-AS-3).

## Exercise

On the test bed: pause one asset as one login and see a deposit hold the condition while one in flight completes; lift
the pause with a second login and see the held deposit proceed; confirm the same login cannot approve its own request.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
