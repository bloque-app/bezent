<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: CND-DestinationRefused
kind: condition
covers: [REQ-IN-8, REQ-IN-9, REQ-RV-1, REQ-RV-2, REQ-GV-5]
pages: []
observables: [reviews_open, reviews_oldest_age_seconds, nonfinal_transfers, oldest_nonfinal_transfer_age_seconds]
tests: [services/core/tests/conditions.rs::cnd_destination_refused_mints_nothing_and_resumes_when_the_mint_applies, services/core/tests/directed.rs::req_rf_1_a_refusal_to_mint_a_frozen_asset_never_opens_the_exit, services/kreivo-endpoint/tests/submit.rs::req_in_9_a_refusal_to_mint_is_recorded_with_the_assets_status, services/core/tests/conditions.rs::cnd_destination_refused_past_its_deadline_goes_to_review, services/core/src/engine.rs::req_in_9_a_mint_the_issuing_network_refuses_holds_cnd_destination_refused, services/core/tests/review.rs::req_rv_2_a_review_ends_by_evidence_bezent_verifies_then_the_ordinary_step_runs, services/core/tests/review.rs::req_gv_5_a_resolution_that_moves_value_waits_for_its_second_approver]
exercised: no
---
# CND-DestinationRefused

## What it is

The issuing network refused a deposit's mint because of its destination; nothing was minted (`REQ-IN-8`). bezent never
checks whether a destination can receive the asset (`REQ-IN-9`): the core reads the refusal from the dry run and holds
the condition in `Source-Final`. Only the assets API's refusal to mint to the account is one: the Kreivo endpoint records
it as `Kreivo(Assets(CannotMint))` (`bezent_store::DESTINATION_REFUSALS`), with the asset's status at the same head in
`dry_run -> 'asset' ->> 'status'`. Kreivo answers `CannotMint` for the whole asset's errors too, so it is the
destination's only when that status is `Live`: a frozen asset (`Frozen`), one being destroyed (`Destroying`), one that is
gone or whose status could not be read (`null`) holds no condition, as below (PR #136). The Kreivo endpoint is deployed
before the core (F-018 NOTES, "Deployment order"): an older endpoint records no status, and its refusals hold nothing.
Any other `Kreivo(…)` reason
(`Kreivo(UnknownError)`, `Kreivo(ExtQueryError)`, `Kreivo(Assets(NotAuthorised))`, `Kreivo(Assets(NotALocalAsset))`,
and so on) or a bare `Kreivo` says nothing about the destination: it holds no condition, pages `payout-refusal-alert`
when it keeps coming back, and goes to review at its deadline with no condition (PR #136, review M1). It orders again after its retry
interval, and clears the hold if a later dry run passes (the test: "resumes when the mint applies"). Past the route's
completion budget, counted from the source leg's finality on the destination network's final head, it is a condition
that blocks delivery, and the transfer goes to `Needs-Review` with the condition kept (SPEC §5.4; `REQ-IN-9`).

## Pages

No page. Before the deadline the signal is the deposit's status showing the condition. After it, the transfer is a row
in the `reviews` view (owner `operations`, due one business day later), counted by `reviews_open` and aged by
`reviews_oldest_age_seconds` (`REQ-RV-1`).

## Confirm

```sql
SELECT transfer_id, bridge_reference, condition, state_before_review, entered_at, owner, due_at
FROM reviews WHERE condition = 'CND-DestinationRefused';
SELECT metric, value FROM ops_observables() WHERE metric IN ('reviews_open', 'reviews_oldest_age_seconds');
```

The core's log carries the refusal text. Confirm the asset was `Live` at the refusal before telling the requester the
destination is at fault:

```sql
SELECT p.dry_run ->> 'reason' AS reason, p.dry_run -> 'asset' ->> 'status' AS asset_status, p.dry_run_block_number
FROM prepared_transactions p JOIN intents i ON i.id = p.order_intent_id
WHERE i.transfer_id = <id> AND NOT p.dry_run_passed ORDER BY p.dry_run_block_number DESC, p.id DESC LIMIT 1;
```

A refusal with any other status is the asset's, not the destination's: the whole bridged asset is frozen or being
destroyed on the issuing network, an incident for every deposit of it, not this one; escalate it and do not tell the
requester to change the destination. A destination that the issuing network cannot receive on while the asset is `Live`
is the requester's to correct: the destination is theirs.

## Do

1. Before the deadline: tell the requester and wait. If the cause clears on the network (for example the account can now
   be created), the next order passes and the mint goes through.
2. In review: end it only by evidence (`REQ-RV-2`). The coordinate is the final block the next step rests on; the core
   checks it itself. Record, as your own login, a resolution into the state one ordinary edge from `Source-Final`:
   `INSERT INTO manual_actions (kind, transfer_id, to_state, network, block_number, block_hash, block_time, justification) VALUES ('review-resolution', <id>, 'Delivering', '<network>', <n>, '<hash bytea>', '<time>', '<why>')`.
   A resolution into `Source-Final`, `Delivering` or `Refunding` moves value and needs a second approver, another
   login: `INSERT INTO manual_approvals (action_id, justification) VALUES (<action id>, '<why>')`. The core then runs the
   ordinary step, and the review gates (pause, cut-off, reserve) still apply. For a deposit that went through
   `CND-AwaitingConversion` (a converted deposit), the approval must also name the output it approved, in
   `approved_output_*`; see `CND-AwaitingConversion.md` (Do, step 2) and amendment 0018, Open 6.
3. A refund of a deposit that cannot be minted is not an operator edge from this review: see F-003 NOTES and the
   requester's refund path (`REQ-RQ-6`); not established here beyond that. The one exception is a **directed** deposit
   (amendment 0029, Open 2): when the issuing network itself refused its destination (attempt 1's dry run recorded the
   refusal) and it reached review at the condition's deadline, a resolution into `Refunding`, with a second approver,
   returns it to its refund destination. Its evidence is the coordinate and block time of attempt 1's **newest** dry
   run, by its block and then by row, which must be the refusal (`Kreivo(Assets(CannotMint))`) recorded with the asset
   `Live` and with its block time; a dry run that passed at or after that block, or a newer one refused for anything
   else, closes the exit. Before you record a resolution:
   1. **Wait until attempt 1's newest order has a preparation.** The core orders again after each refusal; while the
      newest mint order has no row in `prepared_transactions`, the network has not answered it yet, and a resolution
      resting on the older refusal may be overtaken by a dry run that passes. Check that this returns no row:
      `SELECT o.id FROM intents o WHERE o.transfer_id = <id> AND o.attempt = 1 AND o.purpose = 'mint' AND NOT EXISTS (SELECT 1 FROM prepared_transactions p WHERE p.order_intent_id = o.id) AND o.id = (SELECT max(id) FROM intents WHERE transfer_id = <id> AND attempt = 1 AND purpose = 'mint');`
   2. **Confirm the asset is `Live`** in the refusal you rest on (`asset_status` below). If it is not, the asset is
      frozen, being destroyed or unknown on the issuing network: do not resolve into `Refunding`; the exit is closed
      and the store refuses it.
   3. Read the evidence:
      `SELECT p.network, p.dry_run_block_number, p.dry_run_block_hash, p.dry_run_block_time, p.dry_run ->> 'reason' AS reason, p.dry_run -> 'asset' ->> 'status' AS asset_status FROM prepared_transactions p JOIN intents i ON i.id = p.order_intent_id WHERE i.transfer_id = <id> AND i.attempt = 1 AND i.purpose = 'mint' AND p.network = i.network ORDER BY p.dry_run_block_number DESC, p.id DESC LIMIT 1;`
   Name `dry_run_block_time` as the resolution's `block_time`, exactly: the core and the store refuse any other time,
   and a refusal recorded without one (by an endpoint older than PR #136) is no evidence.
   `SELECT directed_review_refund_open(<id>)` says whether the exit is open. A review entered because the
   destination or the declared list could not be read, one whose dry run the network refused for another reason, and
   one with a delivery anomaly recorded for the deposit are unknowns, and are never refunded (`REQ-RF-1`).

## Never

- Do not refund or fail on a timer or an unknown (REQ-RF-1, REQ-RV-1).
- Do not mint before final (INV-6); do not match by amount or time (REQ-OT-2).
- Do not approve your own resolution; the store refuses it (REQ-GV-5).
- Do not UPDATE or INSERT into transfers, transitions, book or observations; there is no grant for it (REQ-GV-5).
- Do not merge issuers (REQ-AS-3).

## Exercise

On the test bed: deposit to a destination the issuing network will not credit, check the hold, advance past the
deadline and see the review; resolve it with evidence under two logins.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
