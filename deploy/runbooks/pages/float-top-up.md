<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: float-top-up
kind: procedure
covers: [REQ-TR-5, REQ-IC-2]
pages: [fee-position-low]
observables: [fee_short_transfers, fee_position_amount, fee_position_age_seconds]
tests: [services/core/tests/conditions.rs::cnd_fee_short_holds_until_the_fee_position_can_pay, services/core/tests/fees.rs::ac_h3_1_req_st_6_a_fee_position_below_its_floor_pages_once_then_daily_and_recovery_rearms, services/kreivo-endpoint/tests/fee_position.rs::ac_h3_1_req_st_6_the_fee_position_is_free_less_frozen_at_the_final_head_with_the_supply]
exercised: no
---
# Top-up of a delivery account's fee float

## What it is

Each delivery key pays its network's fees from a float on that network: the issuing network's delivery account holds a
float of the network's native token (a storage deposit is charged on a mint and for each new reference after it, PLAN
`DEC-18`), and each origin network's delivery account holds that network's gas token. A fee is never taken out of a
delivery (`REQ-TR-5`, guardrail 10); costs go against surplus.

## Pages

`fee-position-low`: a network's delivery account has less native token than the floor the deployment set for that network
in the core's configuration (`fee_floors`; `DEC-18`, amendment 0025). An ERROR line
`page: the delivery account's fee position is below its floor; ... kind=fee-position-low network=<network> amount=<amount> floor=<floor> repeat=<repeat>`
and an `event_log` entry of the same kind. It is made **once when the position crosses below the floor, and then once a
day while it stays below**, from the log, so a restart repeats nothing early and forgets nothing. A position back at or above
the floor is recorded as `fee-position-recovered`, and a later drop pages at once. Both amounts are in the network's
smallest unit (planck on the issuing network, wei on an EVM network) and exact in the page.

What the page does **not** say: a fee position that cannot be read (both providers disagree, the chain does not answer, the
endpoint is down) is not low and is not paged here. Its signals are a missing `fee_position_amount` and a
`fee_position_age_seconds` that grows without bound (`alerts.md`, "Fee positions"). A network with no floor in the
configuration is logged at start (`no fee position floor is configured`) and never paged on. The signal that a delivery
already could not pay is the late one: transfers held in `CND-FeeShort` (`fee_short_transfers` above zero).

## Confirm

```sql
SELECT metric, asset, network, direction, value FROM ops_observables()
WHERE metric IN ('fee_short_transfers', 'fee_position_amount', 'fee_position_age_seconds');
```

`fee_position_amount` is the delivery account's fee position at the network's latest final head, exact, in the smallest unit
(no value means unknown). For an EVM network it is the native balance. For the issuing network it is the account's **free
balance less its frozen balance**: a frozen part cannot pay, and the part a storage deposit holds (reserved) is not counted. The
reading's `detail` keeps all three figures:

```sql
SELECT network, block_number, amount, unknown_reason, detail FROM fee_position_readings ORDER BY id DESC LIMIT 5;
```

(an operator reads it through `ops_observables()`, which has no row for the detail: read the figures from public chain data
when they matter).

## Do

1. Fund the delivery account from the operator's own funds, from the deployment's own notes. This is a transfer of
   the fee token only; it is **not** a deposit and not a surplus contribution of the bridged asset.
2. Check the floor is right: it is in the core's configuration (`fee_floors`), from the deployment's own notes. A floor
   that is too low pages late; one that is too high pages for a balance that can still pay. Changing it is a configuration
   change, recorded like any other.
3. Record it: `SELECT operator_record('float-top-up', '<network, account name>', '{}'::jsonb, '<why>');` (an amount may be
   stated in the detail; no key material, no address of a private account).
4. Held transfers resume by themselves on the next cycle that finds the position able to pay (`CND-FeeShort`).

## Never

- Never fund a delivery account from the reserve, or take the fee out of a delivery or a refund (`REQ-TR-5`, `INV-14`).
- Never send the bridged asset or its origin token to a delivery account as a top-up: an arrival at a reserve is
  a deposit or owed mint (`INV-1`, `INV-2`).
- Never raise the key's bound on chain to cover a shortfall (`REQ-KY-3`, `REQ-GV-2`).
- Never release a held transfer, or change its status, once the float is topped up: it resumes by itself, and no
  administrative path edits a status or moves value (`REQ-GV-5`).

## Exercise

On the test bed: set a floor above the float, see `fee-position-low` once and then not again, drain the float further and
see transfers held in `CND-FeeShort`, top up and see them resume and the position recorded as recovered.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
