<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: payout
kind: pages
covers: [REQ-RV-1, REQ-RF-1, REQ-EO-3, REQ-GV-5, REQ-RN-4]
pages: [payout-window-alert, payout-refusal-alert, resolution-blocked-by-inclusion]
observables: [oldest_nonfinal_transfer_age_seconds, nonfinal_transfers, reviews_open, reviews_oldest_age_seconds]
tests: [services/core/tests/cancel.rs::req_rv_1_issue_35_a_repeated_refusal_pages_once_and_reaches_review_at_the_deadline, services/core/tests/cancel.rs::cnd_payout_window_s3_a_hold_past_its_bound_pages_once_then_daily]
exercised: no
---
# Payout pages

## What it is

A withdrawal's payout on an origin network is held, refused, or blocked from being refunded. Each is a wait with a human
owner, never a failure and never a refund on a timer (`REQ-RF-1`, `REQ-RV-1`).

| Page | Meaning | Runbook |
|---|---|---|
| `payout-window-alert` | `CND-PayoutWindow` has been held past its bound. | `../conditions/CND-PayoutWindow.md` |
| `payout-refusal-alert` | The payout's preparation is refused ten times in a row with no named condition. It goes to review at its deadline. Paged once per attempt. | below |
| `resolution-blocked-by-inclusion` | A refund or a new attempt (by resolution or automatically) waits because the payout is observed included and not withdrawn. Once, then daily. | below |

## Pages

ERROR lines `page: ... kind=<kind> transfer=<bridge reference> detail=<json>` and `event_log` entries of the same kind for
the transfer.

## Confirm

```sql
SELECT bridge_reference, state, condition FROM transfer_status WHERE state IN ('Source-Final', 'Delivering', 'Needs-Review');
SELECT seq, kind, payload FROM event_log WHERE kind IN ('payout-refusal-alert', 'resolution-blocked-by-inclusion')
ORDER BY seq DESC LIMIT 10;
SELECT * FROM reviews ORDER BY entered_at;
```

The payload names the attempt, the number of refusals and the network. Read what the provider answered from the
payload and from the endpoint's own log; the refusal text is the network's, not bezent's.

## Do

1. `payout-refusal-alert`: the cause is outside bezent (a provider that refuses, a payout the network will not accept, a
   fee the account cannot pay). Diagnose it on the network. If a named condition applies (`CND-FeeShort`,
   `CND-PayoutWindow`), follow its runbook. If it keeps being refused, the transfer goes to review at its deadline and
   the review runbook applies (`../conditions/CND-Withdrawn.md` and the review section of F-003 NOTES).
2. `resolution-blocked-by-inclusion`: wait, then read the inclusion. The hold ends when the endpoint writes `withdrawn`
   for it (the resolution then runs), or `final` for an inclusion that paid what was ordered, to whom (the review
   then closes as `Delivered`, by a recorded resolution). There is no override (`REQ-GV-5`). A refund before the
   inclusion is decided could pay twice (`INV-8`).
   **If the inclusion is final and the transfer has an `amount-mismatch` or `recipient-mismatch` event, the hold never
   ends.** Nothing in bezent releases it: it is not withdrawn, and it cannot close as `Delivered`. Do not approve or
   record another resolution to get past it, and do not move value by hand. Leave the transfer in review, keep the daily
   page as the record, and escalate to the spec owner with the transfer's reference, the observation (`observation_id` in
   the mismatch event) and the amounts. Recovering what was paid wrongly is outside bezent and the owner's decision
   (amendment 0019, "A point for the owner").
3. In both cases tell the requester the withdrawal is delayed, never failed (`REQ-IC-3`).

## Never

- Never refund, or order a new attempt, because the inclusion "looks dropped" or the time passed (`REQ-RF-1`, `INV-8`,
  `REQ-NX-3`).
- Never pay out by hand to unblock it, or complete it out of band and repair the record: there is no path
  (`REQ-GV-5`; `../incidents/I-15.md`).
- Never resolve a review without the coordinate of the ordinary edge (`REQ-RV-2`), and never approve your own action.

## Exercise

On the test bed: make the provider refuse a payout ten times in a row (`REFUSAL_PAGE_AFTER` in `services/core/src/engine.rs`; nine pages nothing) and see the single page, still single after more refusals; include a payout, order a refund
and see the hold and the single daily page; withdraw the inclusion and see the resolution run.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
