<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: platform
kind: pages
covers: [REQ-IC-1, REQ-IC-2, REQ-AR-3, NFR-6, NFR-7]
pages: []
observables: [observer_lag_seconds, open_gaps, undelivered_notifications, undelivered_notifications_oldest_age_seconds, dead_notifications]
tests: [services/kreivo-endpoint/tests/network.rs, services/evm-endpoint/tests/network.rs, services/core/tests/health.rs]
exercised: no
---
# Platform alarms (not log pages)

## What it is

Alarms that come from the platform and the metrics, not from a `kind=` log line: a service not running, a store that
cannot be reached, the signer refusing an unauthenticated caller, a database filling, an observer behind, undelivered
notifications. Their patterns and thresholds are in `alerts.md`.

## Pages

None carries a `kind`. They are named by the alarm: `bezent-<service>-not-running`, the store-unreachable alarm, the
signer-unauthenticated alarm, the database alarms, and (once their metrics are alarmed) observer lag, open gaps and
notifications.

## Confirm

```sql
SELECT metric, asset, network, direction, value FROM ops_observables()
WHERE metric IN ('observer_lag_seconds', 'open_gaps', 'undelivered_notifications',
                 'undelivered_notifications_oldest_age_seconds', 'dead_notifications') ORDER BY 1, 3;
```

Then the service's health (`deploy/health.md`: a container's `health` subcommand reads a liveness file) and its log.

## Do

1. **A service that will not start and says "refusing to start"** failed a start-up check closed: the store login is not
   the service's, the environment is not the declared one, or a network's identity is not the declared one
   (`REQ-IC-1`, `AC-I4.1`). Do not work around it. Fix the declaration or the endpoint it points at, from the deployment's
   own notes; the service must never run against a network that is not the one declared.
2. **An observer behind** (`observer_lag_seconds` above 60 for a network, `NFR-6`): look at the endpoint's health file and
   its log. Beyond the route's budget the reserve check stalls and pauses the asset (`pages/reserve.md`).
3. **A store that cannot be reached** for five minutes: the services keep trying and every service's cycle reports it.
   Restore the store from the deployment's own notes. After a restore, re-run the grants (`crates/store/README.md`; F-001
   NOTES) and let each service verify its login at start.
4. **The signer refusing an unauthenticated caller**: something other than the core is dialling the link. Treat as a
   possible intrusion: pause (`operator_pause`), then follow `pages/key-compromise.md` step 2 onward.
5. **Undelivered notifications** (`undelivered_notifications_oldest_age_seconds` growing, or `dead_notifications` above
   zero): a requester's endpoint is down or its secret changed. Requesters read the sequenced log without a subscription
   (`REQ-ST-4`, `REQ-ST-5`), so nothing is lost; tell the requester.

## Never

- Never start a service with a store login, a network or an environment other than the declared one, and never edit the
  declaration to make a start-up check pass (`REQ-IC-1`, `REQ-KY-6`).
- Never restart a service in a loop to clear an alarm that a poisoned row or a stuck network causes; the remedy is the
  cause (`deploy/health.md`, "What the checks do not cover").
- Never run a restore over a live store, or skip the grants after one (`NFR-7`).
- Never "repair" a transfer's status or the book by hand to clear an alarm: no administrative path moves value or edits a
  status (`REQ-GV-5`).

## Exercise

On the test bed: point an endpoint at a network that is not the declared one and see it refuse to start; stop an endpoint
and see the lag metric rise past 60 s and the alarm fire.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
