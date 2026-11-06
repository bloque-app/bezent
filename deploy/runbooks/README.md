<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
# Runbooks

One runbook for each named condition (`CND-*`, SPEC §10), each incident class (`I-1` to `I-16`, `notes/BRIDGE-REQUIREMENTS.md`
§5) and each family of pages (`alerts.md`), as `REQ-IC-6` asks. `F-014`, `NFR-9`.

## What a runbook is, and is not

A runbook says four things to a person who has been paged or has seen a condition: **the page that sends you here**,
**how to confirm** it from the store and the logs, **what to do**, and **what you must never do**. It names no host,
account, key location, address, bound or threshold: the public repository leaves out the details that would help an
attacker (PLAN §6), and the private deployment repository holds the "where". Where a step needs one, the runbook says
"from the deployment's own notes".

The same rules bind every runbook (`REQ-GV-5`, guardrails 2 to 6 in `CLAUDE.md`):

- **No path moves value or edits a status.** There is no tool, SQL statement or function to do either, and a runbook
  never asks for one. What an operator can do is in `crates/store/README.md` ("Who may call what"): pause
  (`operator_pause`), lift a pause with a second person (`operator_request_unpause`, `operator_approve_unpause`), record an
  act performed elsewhere (`operator_record`), and resolve a review by attaching the coordinate of the ordinary edge
  (`manual_actions`, `manual_approvals`). Each of them records the login that acted and the justification.
- **Pause first, then understand.** A pause stops new work only; transfers in flight keep being observed and completed
  (`REQ-GV-1`, `INV-15`). It is always safe and is the first move when the cause is not understood (`REQ-IC-2`).
- **Unknown is not failed.** Report in-flight transfers as unknown or delayed, never as failed, and never refund them
  (`REQ-IC-3`, `REQ-RF-1`, `REQ-RV-1`).
- **Look, then act.** Every runbook starts with a read-only confirmation. Reading is `SELECT` on the views and on
  `ops_observables()`; the operator's login has nothing else it could change.
- **Every act is recorded.** Anything done outside the store (a key rotated, a bound raised on chain, a float topped up) is
  recorded with `operator_record` the same day, naming what and why (`AC-I3.1`).

## Format

Each file begins with the licence comment, then front matter that `services/core/tests/runbooks.rs` reads, and has these sections, in this order:
`What it is`, `Pages`, `Confirm`, `Do`, `Never`, `Exercise`.

```
---
id: CND-ReserveShort            # the SPEC id, the incident class (I-3), or the family (reserve)
kind: condition                 # condition | incident | pages | procedure
covers: [REQ-IC-2]              # other requirement ids it answers (optional)
pages: [reserve-alert]          # the page kinds, exactly as logged, that send you here (may be empty)
observables: [open_gaps]        # names from ops_observables() it reads (may be empty)
tests: [tests/conditions.rs::cnd_reserve_short_...]   # path::function, each of which must exist
exercised: no                   # yes only when a live exercise is recorded in "Exercise"; see below
---
```

## Exercised?

`NFR-9` asks that each runbook be exercised on a test network before bezent carries value. **None has been.** What exists
today is the test that proves the behaviour the runbook relies on (`tests:`), not a person following the runbook against a
running deployment. `exercised: no` stays until a dated entry in the runbook's `Exercise` section records who ran it, on
which environment, and what they found. `deploy/runbooks/EXERCISES.md` lists them all; it is empty.

## Index

Each row: the page that sends you to the runbook, the observable that shows the condition, the runbook, and one test of the behaviour it relies on (the runbook lists all of them). "none" under Page means the condition raises no page: its signal is the observable, the requester's condition notification, or the status.

### Named conditions (SPEC §10)

| Condition | Page | Observable | Runbook | A test |
|---|---|---|---|---|
| `CND-AmountMismatch` | none | `reviews_open`, `reviews_oldest_age_seconds` | [CND-AmountMismatch](conditions/CND-AmountMismatch.md) | `req_tr_5_a_delivery_of_another_amount_goes_to_review_reporting_both` (+2) |
| `CND-AwaitingConversion` | none | `reviews_open`, `reviews_oldest_age_seconds`, `unassociated_arrivals`, `unassociated_arrivals_oldest_age_seconds` | [CND-AwaitingConversion](conditions/CND-AwaitingConversion.md) | `cnd_awaiting_conversion_mints_nothing_and_goes_to_review_past_its_deadline` (+3) |
| `CND-CutOff` | none | `nonfinal_transfers`, `oldest_nonfinal_transfer_age_seconds` | [CND-CutOff](conditions/CND-CutOff.md) | `cnd_cutoff_a_step_past_the_route_cut_off_holds_until_it_reopens` (+3) |
| `CND-DestinationRefused` | none | `reviews_open`, `reviews_oldest_age_seconds`, `nonfinal_transfers`, `oldest_nonfinal_transfer_age_seconds` | [CND-DestinationRefused](conditions/CND-DestinationRefused.md) | `cnd_destination_refused_mints_nothing_and_resumes_when_the_mint_applies` (+4) |
| `CND-FeeShort` | none | `fee_short_transfers`, `nonfinal_transfers`, `oldest_nonfinal_transfer_age_seconds` | [CND-FeeShort](conditions/CND-FeeShort.md) | `cnd_fee_short_holds_until_the_fee_position_can_pay` |
| `CND-MintWindow` | none | `nonfinal_transfers`, `oldest_nonfinal_transfer_age_seconds` | [CND-MintWindow](conditions/CND-MintWindow.md) | `cnd_mint_window_holds_until_the_window_rolls_over` |
| `CND-Paused` | `reserve-divergence`, `reserve-check-incomplete`, `supply-mismatch-pause`, `book-quarantine` | `pauses_in_force`, `nonfinal_transfers`, `oldest_nonfinal_transfer_age_seconds` | [CND-Paused](conditions/CND-Paused.md) | `cnd_paused_new_work_waits_and_resumes_when_the_route_reopens` (+3) |
| `CND-PayoutWindow` | `payout-window-alert` | `nonfinal_transfers`, `oldest_nonfinal_transfer_age_seconds`, `reserve_amount` | [CND-PayoutWindow](conditions/CND-PayoutWindow.md) | `cnd_payout_window_m1_a_spent_allowance_holds_the_withdrawal_never_review_never_refund` (+4) |
| `CND-ReserveShort` | none | `nonfinal_transfers`, `oldest_nonfinal_transfer_age_seconds`, `reserve_amount`, `reserve_equation_outcome` | [CND-ReserveShort](conditions/CND-ReserveShort.md) | `cnd_reserve_short_ac_b3_1_holds_and_raises_one_condition_per_asset_and_network` (+1) |
| `CND-Unassociated` | none | `unassociated_arrivals`, `unassociated_arrivals_oldest_age_seconds` | [CND-Unassociated](conditions/CND-Unassociated.md) | `ac_a4_1_an_arrival_that_cannot_become_a_deposit_is_unassociated_with_an_owner` (+2) |
| `CND-Withdrawn` | none | `reviews_open`, `reviews_oldest_age_seconds`, `open_gaps`, `nonfinal_transfers` | [CND-Withdrawn](conditions/CND-Withdrawn.md) | `cnd_withdrawn_an_inclusion_that_stops_matching_the_chain_goes_to_review` (+1) |

### Incident classes (`notes/BRIDGE-REQUIREMENTS.md` §5)

| Class | Page | Observable | Runbook | A test |
|---|---|---|---|---|
| `I-1` | `resolution-blocked-by-inclusion` | `unknown_legs`, `unknown_legs_oldest_age_seconds`, `reviews_open`, `reviews_oldest_age_seconds` | [I-1](incidents/I-1.md) | `ac_b1_4_a_delivery_unknown_at_its_deadline_goes_to_review_and_nothing_is_returned` (+3) |
| `I-2` | `key-compromise` | `unknown_legs`, `unknown_legs_oldest_age_seconds`, `reviews_open` | [I-2](incidents/I-2.md) | `ac_c2_1_an_expired_transaction_whose_nonce_was_never_consumed_is_proven_not_executed` (+5) |
| `I-3` | `key-compromise` | `unknown_legs`, `unknown_legs_oldest_age_seconds`, `reviews_open` | [I-3](incidents/I-3.md) | `req_eo_6_new_attempt_is_refused_before_the_previous_is_proven` (+5) |
| `I-4` | none | `oldest_nonfinal_transfer_age_seconds`, `nonfinal_transfers`, `reviews_open` | [I-4](incidents/I-4.md) | `ac_b1_1_nothing_is_paid_out_before_the_redemption_is_final` (+3) |
| `I-5` | `reserve-alert` | `fee_short_transfers`, `nonfinal_transfers`, `reserve_amount`, `reserve_equation_outcome`, `unassociated_arrivals` | [I-5](incidents/I-5.md) | `cnd_reserve_short_ac_b3_1_holds_and_raises_one_condition_per_asset_and_network` (+4) |
| `I-6` | `reserve-check-stalled`, `reserve-check-incomplete` | `open_gaps`, `observer_lag_estimate_seconds`, `unknown_legs`, `nonfinal_transfers` | [I-6](incidents/I-6.md) | `ac_d2_3_a_block_that_cannot_be_read_is_a_visible_gap_filled_before_anything_after_it` (+4) |
| `I-7` | `reserve-check-incomplete`, `reserve-check-stalled`, `key-compromise` | `reserve_equation_outcome`, `reserve_equation_age_seconds`, `unknown_legs`, `open_gaps`, `reviews_open` | [I-7](incidents/I-7.md) | `ac_c1_2_a_missing_observation_is_unknown_never_not_executed` (+4) |
| `I-8` | none | `reviews_open`, `reviews_oldest_age_seconds` | [I-8](incidents/I-8.md) | `req_ob_6_an_executed_payout_is_recorded_executed_and_never_proven` (+3) |
| `I-9` | `resolution-blocked-by-inclusion` | `nonfinal_transfers`, `oldest_nonfinal_transfer_age_seconds`, `unknown_legs`, `reviews_open` | [I-9](incidents/I-9.md) | `inv_6_inv_16_no_mint_rests_on_an_observation_that_is_not_final` (+4) |
| `I-10` | `payout-refusal-alert`, `payout-window-alert` | `oldest_nonfinal_transfer_age_seconds`, `unknown_legs_oldest_age_seconds`, `reviews_oldest_age_seconds`, `observer_lag_estimate_seconds` | [I-10](incidents/I-10.md) | `req_eo_4_one_lane_keeps_order_by_nonce` (+4) |
| `I-11` | `book-quarantine` | `undelivered_notifications`, `undelivered_notifications_oldest_age_seconds`, `dead_notifications`, `reviews_open`, `reviews_oldest_age_seconds` | [I-11](incidents/I-11.md) | `req_st_5_a_delivery_is_dead_lettered_after_the_bound_and_the_log_still_has_it` (+5) |
| `I-12` | none | `oldest_nonfinal_transfer_age_seconds`, `reviews_oldest_age_seconds`, `reserve_equation_age_seconds` | [I-12](incidents/I-12.md) | `req_tr_2_transitions_are_append_only` (+5) |
| `I-13` | `reserve-divergence`, `reserve-alert` | `reserve_equation_outcome`, `reserve_equation_age_seconds`, `reserve_amount`, `reviews_open` | [I-13](incidents/I-13.md) | `req_tr_5_a_delivery_of_another_amount_goes_to_review_reporting_both` (+4) |
| `I-14` | none | `unknown_legs`, `reviews_open`, `reviews_oldest_age_seconds` | [I-14](incidents/I-14.md) | `m1_req_ot_2_two_redeemers_with_one_reference_are_each_paid_by_their_own_payout` (+4) |
| `I-15` | `payout-window-alert`, `payout-refusal-alert` | `reviews_open`, `reviews_oldest_age_seconds`, `nonfinal_transfers` | [I-15](incidents/I-15.md) | `ac_g1_2_a_request_outside_the_limits_is_refused_with_a_named_error` (+4) |
| `I-16` | `key-compromise` | none | [I-16](incidents/I-16.md) | `req_cu_1_signer_refuses_a_caller_with_its_own_uid` (+5) |

### Pages, platform alarms and procedures

| Runbook | Page kinds | Observable | A test |
|---|---|---|---|
| [attested](pages/attested.md) | `attested-withdrawal-unrelayed`, `attested-withdrawal-awaiting-head`, `attested-report-refused`, `attested-instructions-refused` | `oldest_nonfinal_transfer_age_seconds`, `nonfinal_transfers` | none (not on this base) |
| [float-top-up](pages/float-top-up.md) | none | `fee_short_transfers` | `cnd_fee_short_holds_until_the_fee_position_can_pay` |
| [key-compromise](pages/key-compromise.md) | `key-compromise` | `pauses_in_force` | `req_ic_4_an_alert_in_the_log_becomes_one_page_with_kind_unquoted` (+3) |
| [late-mint](pages/late-mint.md) | `late-mint-prover-silent`, `late-mint-watch-failed`, `late-mint-check-failed` | `pauses_in_force` | `services/core/tests/late_mint.rs` |
| [payout](pages/payout.md) | `payout-window-alert`, `payout-refusal-alert`, `resolution-blocked-by-inclusion`, `resolution-unprepared-alert` | `oldest_nonfinal_transfer_age_seconds`, `nonfinal_transfers`, `reviews_open`, `reviews_oldest_age_seconds` | `req_rv_1_issue_35_a_repeated_refusal_pages_once_and_reaches_review_at_the_deadline` (+2) |
| [platform](pages/platform.md) | none | `observer_lag_estimate_seconds`, `open_gaps`, `undelivered_notifications`, `undelivered_notifications_oldest_age_seconds`, `dead_notifications` | `services/kreivo-endpoint/tests/network.rs` (+2) |
| [reserve](pages/reserve.md) | `reserve-divergence`, `reserve-alert`, `reserve-check-incomplete`, `reserve-check-failed`, `reserve-check-stalled`, `supply-mismatch-pause`, `book-quarantine` | `reserve_equation_outcome`, `reserve_equation_age_seconds`, `reserve_amount`, `observer_lag_estimate_seconds`, `open_gaps`, `pauses_in_force` | `req_ic_2_nfr_5_m2_an_evaluation_that_cannot_be_made_beyond_the_bound_pauses` (+4) |
| [statements](pages/statements.md) | `statement-overdue`, `statement-stuck`, `statement-publish-failed`, `statement-abandoned` | `reserve_equation_age_seconds` | `services/core/tests/statement.rs` |

### Requirements of `REQ-IC`, `REQ-ST-6` and what answers them

| Requirement | Observable or page | Runbook | Test |
|---|---|---|---|
| `REQ-IC-1` network identity at start-up | the service refuses to start; the not-running alarm | [platform](pages/platform.md) | `ac_i4_1_another_chain_id_is_refused` and the Kreivo endpoint's `ac_i4_1_*` |
| `REQ-IC-2` unexplained condition: pause, keep observing, page | `reserve-*`, `supply-mismatch-pause`, `book-quarantine`, `key-compromise` | [reserve](pages/reserve.md), [key-compromise](pages/key-compromise.md) | `ac_e2_1_a_divergence_pauses_the_assets_routes_keeps_observing_and_pages_a_human` |
| `REQ-IC-3` unknown or delayed, never failed | `unknown_legs`, `reviews_open` | every runbook's `Never` | `ac_b1_4_a_delivery_unknown_at_its_deadline_goes_to_review_and_nothing_is_returned` |
| `REQ-IC-4` a signature not recorded: pause, rotate, report | `key-compromise` | [key-compromise](pages/key-compromise.md) | `req_ic_4_an_alert_in_the_log_becomes_one_page_with_kind_unquoted` (the pause is by a person, see `features/F-014-operations/NOTES.md`) |
| `REQ-IC-5` once per condition | the requesters' condition notification | the condition runbooks | `services/ingress/tests/webhooks.rs` |
| `REQ-ST-6` observables | `ops_observables()` and the minute line | [alerts](alerts.md) | `ac_h3_1_*` in `crates/store/tests/operations.rs` and `services/core/tests/operations.rs` |

The page kinds, their filters and whether a page reaches a person: [alerts.md](alerts.md). Which runbooks a person has followed: [EXERCISES.md](EXERCISES.md).
