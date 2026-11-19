<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: CND-TokenUpgraded
kind: condition
covers: [REQ-AS-12, REQ-GV-5, REQ-IN-11, AC-A6.8]
pages: [origin-token-upgraded, directed-wait-alert]
observables: [nonfinal_transfers, oldest_nonfinal_transfer_age_seconds, unassociated_arrivals]
tests: [services/core/tests/directed.rs::ac_a6_8_an_upgrade_holds_directed_intake_until_two_operators_lift_it, services/core/tests/directed_alerts.rs::req_as_12_an_origin_tokens_upgrade_pages_once_and_holds_directed_intake, services/core/tests/directed.rs::req_as_12_deposits_held_by_an_upgrade_are_not_in_flight, services/core/tests/directed.rs::req_lm_4_a_deposit_held_by_an_upgrade_after_source_final_stays_in_flight, services/core/tests/directed_alerts.rs::req_ic_2_directed_deposits_held_or_waiting_past_a_window_page_daily]
exercised: no
---
# CND-TokenUpgraded

## What it is

A directed deposit of an admitted origin token whose code changed (its proxy emitted `Upgraded`, recorded as a
`token_upgrade` observation) at or before the deposit's arrival, with no lift of that change recorded (amendment 0031,
`d-f018-usdc-upgrade`, ruled: "Yes, hold and alert"; SPEC `REQ-AS-12`, §10). The deposit waits at `Source-Final` before
its mint is ordered; a mint already signed is in flight and goes on (`INV-15`). The store refuses a mint order for a held
deposit. While the hold stands, a directed arrival of that token that could never be minted is **not returned** either:
it stays final and unbound (`unassociated_arrivals`), and is returned once the hold is lifted. A requester's deposit of the
token is never held. An upgrade the network withdrew holds nothing; one observed and not yet final already holds. A
deposit held at `Source-Final` is not in flight: it never fills the route's capacity against requesters' deposits; when
the hold is lifted it takes its place in flight only if the route has room, and otherwise waits in
`CND-DepositCapacity`. One held later, on a new attempt in `Delivering` or a resolution's delivery in `Needs-Review` (an
upgrade observed late), was in flight already and stays in flight while held.

After a hold, the window bounds intake, not mints. A held deposit took its share of the route's window when it was made,
and keeps it while held; the shares leave the window as they age out of it, not as the deposits are minted. On the lift
the held deposits mint together, as deposits held by a pause do (as far as the route's capacity lets them into flight),
so the mints of one lift may be more than one window would allow. That is expected (amendment 0031, part 3), not a
breach of the window.

The time held does not count toward the route's deadline: the first mint attempt's clock starts when the deposit leaves
the condition.

## Pages

`origin-token-upgraded`, once per upgrade observation (`pages/token-upgraded.md`). While deposits are held for less than
one of the route's windows nothing more pages: the signal is their status and `nonfinal_transfers` on the deposit route.

`directed-wait-alert` pages, per deposit route, once any directed deposit has held `CND-TokenUpgraded` (or
`CND-DepositCapacity`) for longer than one of the route's windows (`window_seconds`): a lift not made yet. Once on
crossing, then once a day while it stands; `directed-alert-cleared` once none is held that long (`pages/directed-wait.md`
quotes its line).

## Confirm

```sql
SELECT o.id, o.network, o.block_number, o.token, o.detail ->> 'implementation' AS implementation, x.outcome
FROM observations o LEFT JOIN observation_outcomes x ON x.observation_id = o.id
WHERE o.kind = 'token_upgrade' ORDER BY o.id DESC LIMIT 10;
SELECT kind, actor, justification, detail, recorded_at FROM admin_actions
WHERE kind IN ('token-hold-lift-request', 'token-hold-lift') ORDER BY id DESC LIMIT 10;
```

Then read the held deposits in `transfer_status` (condition `CND-TokenUpgraded`).

## Do

1. Work the upgrade first (`pages/token-upgraded.md`): read the new implementation, and run the fork suite
   (`services/evm-endpoint/tests/fork.rs`, and the directing code's fork tests) against it, at a block after the upgrade.
2. If the suite passes and nothing the bridge relies on changed, lift **every open upgrade of the token** on that
   network, not only the newest: a deposit stays held while any upgrade at or before its arrival is unlifted, so each
   one needs its own request and approval. The open ones (not withdrawn, not lifted):
   ```sql
   SELECT u.id, u.block_number, u.detail ->> 'implementation' AS implementation FROM observations u
   WHERE u.kind = 'token_upgrade' AND u.network = '<network>'
     AND lower(COALESCE(u.token, u.detail ->> 'token')) = lower('<token>')
     AND NOT EXISTS (SELECT 1 FROM observation_outcomes x WHERE x.observation_id = u.id AND x.outcome = 'withdrawn')
     AND NOT EXISTS (SELECT 1 FROM admin_actions a WHERE a.kind = 'token-hold-lift'
                     AND a.detail ->> 'observation_id' = u.id::text)
   ORDER BY u.id;
   ```
   Run the fork suite against the implementation each one installed (the last is what runs now). For each, one operator
   requests the lift, naming the upgrade observation and what was run:
   `SELECT operator_request_token_hold_lift(<observation id>, 'fork suite run against <implementation> at block <n>: passed');`
3. A different operator checks the same and approves it:
   `SELECT operator_approve_token_hold_lift(<request id>, '<what you checked>');`
   The store refuses an approval by the login that made the request. Once the last open upgrade is lifted, the held
   deposits resume at the core's next cycle (waiting in `CND-DepositCapacity` if the route is full), and the
   never-mintable arrivals held back are returned. One open upgrade left keeps every deposit at or after it held, and
   `directed-wait-alert` keeps paging daily.
4. If anything the bridge relies on changed, do not lift: pause the asset's routes (`operator_pause`) and escalate. The
   held deposits keep waiting; nothing is returned or refunded meanwhile.

## Never

- Never lift the hold without the fork suite run against the new implementation, and never approve your own request
  (REQ-AS-12, REQ-GV-5).
- Never return, refund or fail a held deposit because of the upgrade (REQ-AS-12, REQ-RF-1).
- Do not UPDATE or INSERT into transfers, transitions, book or observations; there is no grant for it (REQ-GV-5).

## Exercise

On the test bed: upgrade a test token's proxy on a fork, make a directed deposit after it, check it holds the condition
with nothing minted; request and approve the lift under two logins and see the mint ordered.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
