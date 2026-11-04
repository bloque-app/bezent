---
id: CND-PayoutWindow
kind: condition
covers: [REQ-RN-4, REQ-KY-3, REQ-RV-1, REQ-EO-3, REQ-IC-5]
pages: [payout-window-alert]
observables: [nonfinal_transfers, oldest_nonfinal_transfer_age_seconds, reserve_amount]
tests: [services/core/tests/cancel.rs::cnd_payout_window_m1_a_spent_allowance_holds_the_withdrawal_never_review_never_refund, services/core/tests/cancel.rs::cnd_payout_window_m1_the_deadline_restarts_at_the_release, services/core/tests/cancel.rs::cnd_payout_window_s2_only_a_refusal_both_providers_agree_on_holds_the_window, services/core/tests/cancel.rs::cnd_payout_window_s3_a_hold_past_its_bound_pages_once_then_daily, services/core/tests/unsigned_attempts.rs::cnd_payout_window_a_spent_allowance_is_never_reviewed_and_its_clock_restarts_when_it_refills]
exercised: no
---
<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
# CND-PayoutWindow

## What it is

The reserve's outflow allowance for the current window is spent: the vault's on-chain role refused the payout's dry
run, and the withdrawal waits (`REQ-RN-4`, `REQ-KY-3`, amendment 0012). The core names it only from a structured
refusal that both providers returned alike (allowance exceeded, with the payout token's delivery allowance key); words
alone, a refusal the providers disagree on, or another status hold nothing. Held, the transfer stays in `Source-Final`
(or `Delivering` for a later attempt). It is never sent to review and never refunded. The first new order whose dry run
passes resumes it. The time held does not count toward the attempt's deadline, which restarts where the hold is left
(F-003 NOTES items 22 and 26). A hold longer than the core's configured bound pages once, then once a day.

## Pages

`payout-window-alert`, also in the event log. The core logs it as an error: "page: CND-PayoutWindow held past its
bound". It names the transfer, the hold, the head and the bound. It changes no state: no review, no refund. The bound
is the deployment's own setting. This file is its runbook.

## Confirm

```sql
SELECT metric, asset, network, direction, value FROM ops_observables()
WHERE metric IN ('nonfinal_transfers', 'oldest_nonfinal_transfer_age_seconds', 'reserve_amount')
ORDER BY metric, asset, network;
```

The held transfer and its condition are in `transfer_status` (`SELECT bridge_reference, state, condition FROM
transfer_status WHERE condition = 'CND-PayoutWindow';`), and the page's own entry in `event_log` (kind
`payout-window-alert`). The page's payload says whether the payout is above the allowance's maximum or
the refill is off: a payout larger than the allowance can ever hold never fits and never resumes.

## Do

1. Decide which it is, from the deployment's own notes and the vault's on-chain allowance (the allowance size, period
   and refill are on chain, not in the store). If the payout fits the allowance, wait: it resumes by itself when the
   allowance refills.
2. If the payout is larger than the allowance maximum, or the allowance is wrong, raising it is a governed act with
   several signers (REQ-GV-2, REQ-KY-3), from the deployment's own notes. After it, record: `SELECT
   operator_record('bound-change', '<asset, network>', '{}'::jsonb, '<why>');`.
3. Tell the requester the withdrawal is delayed, not failed (REQ-IC-3).

## Never

- Do not refund, retry as a new transaction or fail on a timer or an unknown (REQ-RF-1, REQ-RV-1).
- Do not pay from another network, or split a payout to fit by hand (REQ-OT-4); do not match by amount or time (REQ-OT-2).
- Do not raise the allowance by any path that is not the governed one (REQ-GV-2, REQ-KY-3).
- Do not UPDATE or INSERT into transfers, transitions, book or observations; there is no grant for it (REQ-GV-5).
- Do not deliver before escrow is final (INV-7); do not treat a top-up as a deposit (INV-1, INV-2); do not merge issuers (REQ-AS-3).

## Exercise

On the test bed: spend the allowance, redeem, and check the withdrawal holds the condition for longer than the alert
bound and the page appears once; let the allowance refill and see the payout resume and complete without a review.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
