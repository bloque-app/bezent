<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: CND-Unassociated
kind: condition
covers: [REQ-IN-3, REQ-IN-6, REQ-RV-1, REQ-RQ-6, REQ-GV-5, REQ-ST-6, REQ-AS-11]
pages: [directed-binding-alert]
observables: [unassociated_arrivals, unassociated_arrivals_oldest_age_seconds]
tests: [services/core/tests/lifecycle.rs::ac_a4_1_an_arrival_that_cannot_become_a_deposit_is_unassociated_with_an_owner, services/core/tests/directed.rs::req_in_6_an_undeclared_directed_arrival_is_returned_only_by_a_two_person_release, crates/store/tests/directed_core.rs::req_in_6_an_undeclared_directed_arrival_leaves_only_by_a_two_person_release, services/core/tests/directed.rs::req_in_10_a_directed_arrival_bound_by_another_row_pages, services/core/tests/lifecycle.rs::ac_a4_2_an_unassociated_arrival_is_never_minted_and_leaves_only_by_a_two_person_release, services/core/tests/conversions.rs::ac_a5_2_req_cv_2_req_cv_5_inv_6_an_output_named_directly_is_bound_and_minted]
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

No page for an unassociated arrival itself. The signal is the metric `unassociated_arrivals` (count per asset and
network) and `unassociated_arrivals_oldest_age_seconds`; compare the age with the one-business-day due time (`REQ-ST-6`).

`directed-binding-alert` (`F-018`): a **directed** arrival is bound by a transfer that is not bezent's own deposit or
return (its request reference is not the coordinate's `directed:` label, or a request answers it), for example a
requester's deposit bound to the coordinate before the endpoint observed the arrival as directed, which the store refuses
to make final. Such an arrival is neither decided by the core nor listed in `unassociated_arrivals`, so this page is the
only signal. Once per transfer; nothing changes. Find it with
`SELECT transfer_id, payload FROM event_log WHERE kind = 'directed-binding-alert' ORDER BY seq DESC;`, and take the
transfer to its requester: no request binds a directed arrival (`REQ-IN-10`, `AC-A6.2`).

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
4. A **directed** arrival in the list (`F-018`) is one the core does not decide: its directing contract is not declared
   in `directing_code` for its token from a block at or before its own (`REQ-AS-11`; the core logs
   `directing-undeclared` once an hour, naming the observation, its network, token and directing contract). Take its
   coordinate from the list, and compare the contract with the declarations: the latest published statement lists them
   (`directing_code`, each with its `from_block`), as does the owner's migration that made them.

   ```sql
   SELECT observation_id, network, block_number, encode(block_hash, 'hex') AS block_hash, position_kind,
          position_index, token, amount FROM unassociated_arrivals WHERE observation_id = <observation>;
   ```

   - **Not declared yet** (no declaration names it) and the contract is bezent's own: the declaration is late. Declare it (the
     owner's migration), with `from_block` by the rule below; the next cycle decides the arrival by itself.
   - **Declared from a later block** (`from_block` above the arrival's block): a declaration is append-only and keyed by
     network and address, so it never covers this arrival. It can never be minted. Release it (below).
   - **Not bezent's code** (or a token admitted for no asset): never declare it to get the arrival through. Release it
     (below), and report the contract as an incident of the directing code.

   The release returns it to the refund address its directing code recorded, and nowhere else. One operator, as their
   own login:
   `SELECT operator_release_directed_arrival('<network>', <block>, '\x<block hash>', '<position kind>', <position index>, '<why>');`
   It returns the release's id, and the return is a transfer in `Received`. A different operator login approves it:
   `INSERT INTO manual_approvals (action_id, justification) VALUES (<release id>, '<why>')`. The core then accepts the
   return and pays it out as any refund of an unassociated arrival. The store refuses the release of a declared arrival
   (the core decides it), of one not final, and of one already bound, and never mints it (`REQ-IN-6`, `REQ-GV-5`).

**The `from_block` rule.** A declaration's `from_block` must be at or below the EVM endpoint's `[[directing]]`
`from_block` for the same contract, and at or below the contract's deployment block, so every arrival the endpoint
observes is covered. On Polygon Amoy, `DirectedDeposit` was deployed at block 53,060,139
(`contracts/evm/deployments/polygon-amoy.json`), and the smoke deposit is at 53,060,226: its declaration must use
`from_block` at or below 53,060,139.

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
