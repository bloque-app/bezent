<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: unpaired-direction
kind: pages
covers: [REQ-IC-2, REQ-AS-11]
pages: [unpaired-direction]
observables: [open_gaps, observer_lag_estimate_seconds]
tests: [services/core/tests/directed_alerts.rs::ac_a6_6_an_unpaired_direction_pages_once, services/evm-endpoint/tests/directed.rs, services/full-chain/tests/full_chain/directed.rs::s9_ac_a6_6_spam_at_the_minimum_only_pages_and_an_unpaired_direction_pages_once]
exercised: yes
---
# An unpaired direction

## What it is

The EVM endpoint pairs each `Directed` log of a declared directing contract with the admitted token's transfer into the
reserve **immediately before it** in the same transaction, from the same sender, of the same amount, and records the pair
as one directed arrival at the transfer's coordinate (`AC-A6.6`, `INV-10`). A `Directed` log with no such transfer is
recorded as an `UnpairedDirection` (observation kind `unpaired_direction`, migration 0143). It binds nothing: no deposit,
no return, no mint (`AC-A6.6`). With the declared code it should never happen, since the code moves the value and records
the direction in one call; so one is either a fault in the code, a change of the token or of the endpoint's reading, or
a reason the endpoint names: a sender that is the vault itself, or a refund address the contract refuses.

## Pages

`unpaired-direction`: an ERROR line
`page: a declared directing contract's Directed log paired with no transfer into the reserve (AC-A6.6). ... kind=unpaired-direction observation=<id> at=<coordinate> directing=<contract> sender=<sender> reason=<reason>`
and an `event_log` entry of the same kind, with the observation's detail. Once per observation.

### A page for a log that is not final

The page fires when the observation is recorded, before it is final: a direction is worth a look at once. A reorganised
log is observed again at its new coordinate (another block hash, perhaps another index), and that new observation pages
too, so **one log can page twice**. Before acting on a second page, compare the two entries' `detail` (the transaction
hash, the contract, the sender, the amount): the same transaction at two coordinates is one event. Act on the one whose
observation is final (`observation_outcomes.outcome = 'final'`); the other's is `withdrawn`.

## Confirm

```sql
SELECT seq, appended_at, payload FROM event_log WHERE kind = 'unpaired-direction' ORDER BY seq DESC LIMIT 10;
```

The entry's `coordinate` is the `Directed` log's (network, block, hash, log index) and its `detail` is what the endpoint
recorded: the contract, the sender, the amount, the recipient and refund it named, and the endpoint's reason. Read the
transaction from the origin network's public data at that coordinate, with both providers: every log of it, in order.

## Do

1. Read the transaction. If the value did move into the reserve but not immediately before the `Directed` log (another
   log in between), the code is not the declared code, or the token emits differently: **pause the deposit route**
   (`operator_pause`) until it is understood. The arrival is then an ordinary arrival at the reserve, unassociated with an
   owner (`conditions/CND-Unassociated.md`), and leaves only by a two-person release.
2. If no value moved (the `Directed` log alone), nothing is owed and nothing is minted. Record what you found
   (`operator_record`).
3. If the reason is the vault as sender, or a refused refund address, the endpoint read it right and the code would have
   reverted; that is a fault of the code: report it as an incident of the directing code (`REQ-AS-11`) and pause the route.
4. Check the endpoint is reading the right contracts from the right block (its `[[directing]]` configuration, from the
   deployment's own notes) and that it records no gaps (`open_gaps`).

## Never

- Never make a deposit, a mint or a return for an unpaired direction by hand: it binds nothing, by rule (`AC-A6.6`).
- Never edit or delete the observation: observations are append-only (`INV-10`).
- No administrative path moves value or edits a status (`REQ-GV-5`).

## Exercise

On the test bed: emit a `Directed` log through a contract the endpoint watches without the transfer before it (a test
contract at the declared address on a fork), see the observation recorded as `unpaired_direction` and the page made once.
`s9` of the full-chain suite does it: the declared `DirectedDeposit`'s code is replaced on the fork, for one call, by
code that emits `Directed` alone, then put back.

Status: exercised on the full-chain bed on 2026-11-17 (`EXERCISES.md`; NFR-9), by an automated scenario, with the Kreivo half a
model. Not yet on Kreivo's own runtime.
