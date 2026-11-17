<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: directed-mints
kind: pages
covers: [REQ-IC-2, REQ-ST-6]
pages: [directed-mints-alert]
observables: [fee_position_amount, fee_position_age_seconds, nonfinal_transfers]
tests: [services/core/tests/directed_alerts.rs::req_ic_2_directed_mints_above_the_threshold_page_once_and_clear_and_nothing_is_capped, services/full-chain/tests/full_chain/directed.rs::s9_ac_a6_6_spam_at_the_minimum_only_pages_and_an_unpaired_direction_pages_once]
exercised: yes
---
# Directed mints above their threshold

## What it is

Anyone may send value through the directing code declared for a reserve, and each final directed arrival becomes a
deposit minted for the recipient it names, with no request (amendment 0029, `REQ-IN-10`). There is **no count cap**
(amendment 0029, Open 6, ruled: alerts only). What a flood of directed deposits costs bezent is the issuing network's
storage deposit: each new mint reference takes some of the delivery account's native float, plus its fee, and each
redemption back costs a payout on the origin network. A griefer loses almost nothing, since every unit deposited can be
redeemed one for one. So the count is watched, not capped: this page says it is high.

The count is per deposit route: the directed deposits whose mint was included (`Delivered`) in the trailing 24 hours. The
threshold is the core's configuration (`[directed_alerts] mints_per_day`, 200 by default).

## Pages

`directed-mints-alert`: an ERROR line
`page: more directed deposits were minted on this route in the last 24 hours than the threshold; ... kind=directed-mints-alert asset=<asset> network=<network> mints=<mints> threshold=<threshold> repeat=<repeat>`
and an `event_log` entry of the same kind, carrying the count and the threshold. It is made **once on crossing**, then
once a day while the count stays above the threshold. Back at or below it, `directed-alert-cleared` is logged, and a
later crossing pages at once. The log is the memory: a restart repeats nothing early and forgets nothing.

## Confirm

The page and its count:

```sql
SELECT seq, appended_at, payload FROM event_log
WHERE kind IN ('directed-mints-alert', 'directed-alert-cleared') ORDER BY seq DESC LIMIT 10;
```

The delivery account's float on the issuing network, which is what the mints spend:

```sql
SELECT metric, network, value FROM ops_observables()
WHERE metric IN ('fee_position_amount', 'fee_position_age_seconds') ORDER BY network;
```

The directed deposits are the deposits whose request reference is a `directed:` label (bezent's own; amendment 0029, A2):

```sql
SELECT state, count(*) FROM transfer_status
WHERE kind = 'deposit' AND request_reference LIKE 'directed:%' GROUP BY state;
```

## Do

1. Decide whether it is use or abuse: many small deposits from few senders at the minimum is the spam the amendment names
   ("New exposures", "Spam at the minimum"; follow `../procedures/directed-spam.md`); deposits of ordinary sizes from many
   senders is use. Read the senders and
   amounts from the origin network's public data at the arrivals' coordinates.
2. Watch the float: if `fee_position_amount` on the issuing network is falling toward its floor, top it up
   (`float-top-up.md`) before `fee-position-low` pages and deliveries wait in `CND-FeeShort`.
3. If it is abuse and the float cannot keep up, **pause the deposit route** (`operator_pause`): it stops new mints and keeps
   every arrival observed. A paused directed deposit waits in `CND-Paused` and resumes when the pause is lifted, with a
   second person (`operator_request_unpause`, `operator_approve_unpause`). Nothing is lost.
4. If the threshold is wrong for the route's real use, change it in the core's configuration, from the deployment's own
   notes, and record the change (`operator_record`).

## Never

- Never refuse, return or hold a directed deposit by hand: there is no such path, and the alert is an alert, not a cap
  (amendment 0029, Open 6). Pause the route instead, which holds new work and moves nothing.
- Never pay a mint's storage deposit or fee out of the reserve, or take it out of a delivery (`REQ-TR-5`).
- No administrative path moves value or edits a status (`REQ-GV-5`).

## Exercise

On the test bed (`s9` of the full-chain suite does each step): set `mints_per_day` to 3, make five directed deposits at
the contract's minimum through the directing code, see one page and then none while it stands, raise the threshold and
see `directed-alert-cleared`.

Status: exercised on the full-chain bed on 2026-11-17 (`EXERCISES.md`; NFR-9), by an automated scenario, with the Kreivo half a
model. The page is raised by calling `directed_alerts::check` with the thresholds, as the core's `Watch` loop does; the bed
does not run that loop or read `[directed_alerts]` from a configuration file. Not yet on Kreivo's own runtime.
