<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: directed-spam
kind: procedure
covers: [REQ-IC-2, REQ-GV-1, REQ-GV-2, REQ-GV-5]
pages: [directed-mints-alert, directed-window-alert, fee-position-low]
observables: [fee_position_amount, fee_position_age_seconds, nonfinal_transfers, pauses_in_force]
tests: [services/full-chain/tests/full_chain/directed.rs::s9_ac_a6_6_spam_at_the_minimum_only_pages_and_an_unpaired_direction_pages_once, services/core/tests/directed_alerts.rs::req_ic_2_directed_mints_above_the_threshold_page_once_and_clear_and_nothing_is_capped, services/core/tests/directed.rs::ac_a6_3_a_pause_parks_a_directed_deposit_and_nothing_is_returned]
exercised: yes
---
# Spam at the minimum

## What it is

Anyone may call the directing code, so anyone can send many directed deposits at the contract's minimum (1 USDC on
Polygon). Each one is valid: it is minted to the recipient it names, and it can be redeemed one for one. The griefer
loses almost nothing. bezent pays for each: a new mint reference takes storage deposit and a fee from the issuing
network's delivery account (its native float), and each redemption back is a payout whose gas comes out of surplus
(amendment 0029, "New exposures", "Spam at the minimum").

The owner ruled **alerts only** (amendment 0029, Open 5 and 6): no count cap, no share of the window, no per-sender limit.
bezent refuses, returns or holds nothing because there are many deposits. The one lever is the one every route has: a
**pause** of the deposit route, which stops new mints, keeps observing every arrival, and loses nothing.

## Pages

- `directed-mints-alert` (`pages/directed-mints.md`): more directed deposits minted on the route in the trailing day than
  `[directed_alerts] mints_per_day`. Once on crossing, then once a day while it stands; `directed-alert-cleared` when it
  is back at or below.
- `directed-window-alert` (`pages/directed-window.md`): directed deposits hold more of the route's window than the
  threshold. Spam at the minimum takes little of the window; this page fires for volume, not for count.
- `fee-position-low` (`pages/float-top-up.md`): the delivery account's float is below its floor. This is the cost the spam
  actually imposes.

## Confirm

The page, and how many directed deposits the route minted:

```sql
SELECT seq, appended_at, payload FROM event_log
WHERE kind IN ('directed-mints-alert', 'directed-alert-cleared') ORDER BY seq DESC LIMIT 10;
SELECT state, count(*), sum(amount) FROM transfer_status
WHERE kind = 'deposit' AND request_reference LIKE 'directed:%' GROUP BY state;
```

Whether it is spam: many deposits at or near the minimum, from few senders, to few recipients. The senders and amounts
are the observations' (the origin network's public data at the same coordinates):

```sql
SELECT o.sender, count(*), min(o.amount), max(o.amount), count(DISTINCT o.detail ->> 'recipient') AS recipients
FROM observations o WHERE o.kind = 'directed_arrival' AND o.observed_at > now() - interval '1 day'
GROUP BY o.sender ORDER BY count(*) DESC LIMIT 20;
```

What it costs: the float.

```sql
SELECT metric, network, value FROM ops_observables()
WHERE metric IN ('fee_position_amount', 'fee_position_age_seconds', 'nonfinal_transfers', 'pauses_in_force');
```

## Do

1. If the float keeps up (`fee_position_amount` well above its floor), **do nothing to the deposits**. Record what you
   saw (`operator_record`) and watch. Every deposit is minted; that is the rule.
2. Top the float up before it reaches its floor (`pages/float-top-up.md`), so ordinary deposits and withdrawals do not
   wait in `CND-FeeShort`.
3. If the float cannot keep up, **pause the deposit route** (`operator_pause(asset, network, 'deposit',
   justification)`). New directed deposits are still observed and made, and wait in `CND-Paused` minting nothing; requesters'
   deposits wait too. Withdrawals go on. Nothing is lost and nothing is returned.
4. Lift the pause, with a second person, once the float is topped up or the spam has stopped
   (`operator_request_unpause`, then `operator_approve_unpause` from another login). The waiting deposits mint in order.
5. If the route's real use is above the threshold, raise `[directed_alerts] mints_per_day` in the core's configuration,
   from the deployment's own notes, and record the change. A raised threshold clears the alert at the next check.

## Never

- Never refuse, return, hold or delay a directed deposit by hand, or treat a sender as blocked: there is no such path,
  and Open 6 ruled the alert is not a cap.
- Never pay the float or a fee out of the reserve, or take it out of a delivery (`REQ-TR-5`).
- Never lift a pause alone (`REQ-GV-2`).
- No administrative path moves value or edits a status (`REQ-GV-5`).

## Exercise

On the test bed (`s9` of the full-chain suite does each step): send five directed deposits at the contract's minimum from
one sender, with `mints_per_day` at 3; see every one minted, nothing returned, one `directed-mints-alert` and then none
while it stands; pause the deposit route, send one more and see it wait in `CND-Paused` minting nothing; lift the pause
with two operators and see it minted; raise the threshold and see `directed-alert-cleared`. The reserve check stays at zero
divergence throughout.

Status: exercised on the full-chain bed on 2026-11-17 (`EXERCISES.md`; NFR-9), by an automated scenario, with the Kreivo half a
model. Not yet on Kreivo's own runtime.
