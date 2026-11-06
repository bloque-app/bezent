<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: CND-Unassociated
kind: condition
covers: [REQ-IN-3, REQ-IN-6, REQ-RV-1, REQ-RQ-6, REQ-GV-5, REQ-ST-6]
pages: []
observables: [unassociated_arrivals, unassociated_arrivals_oldest_age_seconds]
tests: [services/core/tests/lifecycle.rs::ac_a4_1_an_arrival_that_cannot_become_a_deposit_is_unassociated_with_an_owner, services/core/tests/lifecycle.rs::ac_a4_2_an_unassociated_arrival_is_never_minted_and_leaves_only_by_a_two_person_release, services/core/tests/conversions.rs::ac_a5_2_req_cv_2_req_cv_5_inv_6_an_output_named_directly_is_bound_and_minted]
exercised: no
---
# CND-Unassociated

## What it is

A final, executed arrival at a reserve address that no deposit binds, or one that cannot become a deposit (a token not
admitted, an amount outside the route's bounds; a convertible token until a deposit names it, `REQ-CV-1`): `REQ-IN-3`,
`REQ-IN-6`. It is a row of the
`unassociated_arrivals` view, not a transfer state. It is never minted, absorbed into reserve or dropped (`REQ-IN-6`).
Its owner is `operations`, due one business day after it became final (`REQ-RV-1`). It leaves by a requester binding it
with a deposit request that names its coordinate (`REQ-IN-3`), or by a two-person release of its refund.

## Pages

No page. The signal is the metric `unassociated_arrivals` (count per asset and network) and
`unassociated_arrivals_oldest_age_seconds`; compare the age with the one-business-day due time (`REQ-ST-6`).

## Confirm

```sql
SELECT observation_id, network, token, amount, admitted_for, final_at, owner, due_at
FROM unassociated_arrivals ORDER BY final_at;
SELECT metric, asset, network, value FROM ops_observables() WHERE metric LIKE 'unassociated_arrivals%';
```

`admitted_for` is empty where the token is not admitted. A funding arrival from a declared surplus funding address is
not a deposit and never becomes one (`INV-1`, `INV-2`; the core logs it and does not mint).

## Do

1. Find who sent it and what they meant, from the requester and the deployment's own notes. If they mean a deposit, the
   requester binds it with a deposit request naming the coordinate; nothing else is needed (`REQ-IN-3`).
2. If it is to be returned, a requester with the refund grant requests the refund with the coordinate and a destination
   (`REQ-RQ-6`). The refund is a transfer in `Received`. One operator records the release as their own login:
   `INSERT INTO manual_actions (kind, transfer_id, justification) VALUES ('release-unassociated', <refund transfer id>, '<why>')`.
   A different operator login approves it:
   `INSERT INTO manual_approvals (action_id, justification) VALUES (<action id>, '<why>')`. The store refuses an approval
   by the action's own login (`REQ-GV-5`). Without both, the refund does not leave `Received` (`REQ-IN-6`).
3. A surplus contribution as the other release in `REQ-IN-6`: the code records it with `operator_record` (kind
   `surplus-contribution`); how the book treats it is not established here, take it from the deployment's own notes.

## Never

- Do not mint against it, or treat it as a deposit by matching on amount or time (`REQ-OT-2`, `REQ-OT-2`, `INV-6`).
- Do not refund on a timer or because it is old (`REQ-RF-1`, `REQ-RV-1`); age raises the priority, not the action.
- Do not merge a token of another issuer into an asset (`REQ-AS-3`).
- Do not INSERT or UPDATE transfers, transitions, book or observations; there is no grant for it (`REQ-GV-5`).

## Exercise

On the test bed: send an admitted token to a reserve address with no deposit request, wait for it to be final, check it
in `unassociated_arrivals`, then have a requester ask for its refund, record the release with one login and approve it
with another, and watch the refund proceed. Try the approval with the same login and expect the store to refuse it.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
