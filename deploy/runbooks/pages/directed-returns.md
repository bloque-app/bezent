<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: directed-returns
kind: pages
covers: [REQ-IC-2, REQ-RF-2, REQ-IN-11]
pages: [directed-returns-alert]
observables: [nonfinal_transfers, oldest_nonfinal_transfer_age_seconds, reserve_amount]
tests: [services/core/tests/directed_alerts.rs::req_ic_2_directed_returns_above_the_threshold_page, services/full-chain/tests/full_chain/directed.rs::s7_ac_a6_1_ac_a6_4_a_directed_round_trip_and_a_return]
exercised: yes
---
# Directed returns above their threshold

## What it is

A directed arrival that can never be minted under its route as declared (below the minimum, above the maximum, a
recipient bezent refuses by rule) is returned by bezent to the refund address its caller named, with
no second approver (amendment 0029, A5, Open 3; `REQ-IN-11`, `REQ-RF-2`). A directed deposit whose recipient the issuing
network refuses may also be refunded, through its review, with a second approver (Open 2). Each return is a payout from
the reserve: it draws on the vault's delivery allowance, which withdrawals share, and its gas is paid against surplus.
A well-behaved web app never sends what cannot mint, so returns are rare by construction, and there is no cap on them
(Open 6, ruled): a count above the threshold pages.

**An accepted exposure (amendment 0031, `d-f018-return-routing`: "Accept, with an alert").** The caller of the directing
code names the refund address, and bezent returns what can never mint there by itself. So anyone can route value through
the reserve to a third party's address, at bezent's gas cost and on the allowance withdrawals share, by sending what can
never mint (an amount outside the route's limits, a recipient on the refused list) and naming someone else as `refund`.
Nothing is stolen, since the value is the funder's own, but the reserve becomes a hop between two parties that never
deposit. This alert is the mitigation; a run of returns to addresses that are not the funders' (compare the
`directed-return` entries' `refund` with the arrival's sender) is that case.

The count is per deposit route: returns of directed arrivals and refunds of directed deposits made in the trailing 24
hours. The threshold is the core's configuration (`[directed_alerts] returns_per_day`, 3 by default).

## Pages

`directed-returns-alert`: an ERROR line
`page: more directed arrivals were returned on this route in the last 24 hours than the threshold; ... kind=directed-returns-alert asset=<asset> network=<network> returns=<returns> threshold=<threshold> repeat=<repeat>`
and an `event_log` entry of the same kind. Once on crossing, then once a day while it stands; `directed-alert-cleared`
when back at or below the threshold, after which a new crossing pages at once.

## Confirm

```sql
SELECT seq, appended_at, payload FROM event_log
WHERE kind IN ('directed-returns-alert', 'directed-return', 'directed-alert-cleared') ORDER BY seq DESC LIMIT 20;
```

The `directed-return` entries say why each arrival was returned (`payload ->> 'reason'`). The returns themselves:

```sql
SELECT bridge_reference, state, condition FROM transfer_status
WHERE kind = 'refund' AND request_reference LIKE 'directed:%' ORDER BY bridge_reference;
```

## Do

1. Read the reasons. One reason over and over points at its cause: amounts below the minimum or above the maximum are a
   client that does not read the contract's limits (the web app reads them); a refused recipient is a client that names the
   Kreivo contract or a ledger's account. An arrival of undeclared directing code (an unadmitted token among them) is
   not returned by bezent: it waits, and an operator's release returns it (`conditions/CND-Unassociated.md`).
2. Check that withdrawals are not waiting on the allowance because of them: transfers in `CND-PayoutWindow`
   (`conditions/CND-PayoutWindow.md`).
3. If they come from one client, tell its owner, from the deployment's own notes. If the returns themselves fail (the
   token refuses to pay the refund address), follow `../procedures/directed-return-failed.md`.
4. If it is abuse that the allowance cannot carry, pause the deposit route (`operator_pause`); arrivals keep being observed
   and returned or deposited when it lifts.

## Never

- Never return an arrival by hand, to any address: bezent returns it, once, to the refund address its directing code
  recorded and to no other (`REQ-RF-2`, `INV-2`). The one release an operator makes, of an arrival of undeclared
  directing code, pays that same address only, and needs a second login (`conditions/CND-Unassociated.md`).
- Never refund a deposit that is unknown or delayed (`REQ-RF-1`, `REQ-RV-1`).
- No administrative path moves value or edits a status (`REQ-GV-5`).

## Exercise

On the test bed: set `returns_per_day` to 0, send one directed deposit that can never be minted, see it returned and the
page made once. Through the declared contract an amount below the minimum reverts when the route's minimum is the
contract's, so `s7` of the full-chain suite names a recipient bezent refuses instead.

Status: exercised on the full-chain bed on 2026-11-17 (`EXERCISES.md`; NFR-9), by an automated scenario, with the Kreivo half a
model. The page is raised by calling `directed_alerts::check` directly, not by the core's `Watch` loop, which the bed does not
run (nor its `[directed_alerts]` configuration). Not yet on Kreivo's own runtime.
