<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0028 — when the reserve check cannot see, it holds; when the money differs, it pauses

**Status:** **Ruled by @pandres95 in chat, 2026-11-07: "Yes, write the amendment and build it."**, in reply to the
proposal below. Applied to `SPEC.md` v0.5.11 (§2 "Hold", `REQ-BK-1`, `REQ-GV-2`, `REQ-IC-2`, `NFR-5`, `NFR-6`,
`AC-E2.1`, `AC-E2.3`, §10 `CND-Paused`); `PLAN.md` v0.6.10 binds it (`DEC-16`). Built by migration 0142 and the core.
The items under "Open after the ruling" stay open.
**Target:** `SPEC.md` v0.5.10
**Date:** 2026-11-07
**Needs:** nothing further for the ruling. Each item under "Open after the ruling" needs a decision of its own.
Numbered 0028 because 0027 is reserved for the draft of pull request #111. If #111 takes `SPEC.md` v0.5.11 or `PLAN.md`
v0.6.10 first, this one renumbers its versions to what is next. Migration 0142.
**Rationale in one line:** the reserve check paused an asset in the same way, and needed the same two-login lift,
whether it could not see the networks or the money actually differed, so a start-up with every term at zero read as an
alarm and held the bridge until two people acted.

## What happened (production, 2026-11-06 and 2026-11-07)

Two pauses were raised at start-up, both with supply, reserve and book all zero:

1. **Pause 1, `REQ-IC-2`, at the cold start.** Nothing had been read yet: no range without a hole, so no final point.
   The evaluation was incomplete, and once it had been incomplete for longer than the smallest lag bound of the asset's
   networks, the check paused the asset's routes under `REQ-IC-2`, naming the evaluation, and paged
   `reserve-check-stalled`.
2. **Pause 2, `REQ-BK-1`, while the walk caught up.** The final heads were fresh, but the point the check could read at
   lagged the latest final head beyond its bound while the endpoints' walk caught up. As built, that lag was pushed as a
   `Difference` (terms `final_point` and `final_head`), so the evaluation came out `Diverges`: a `REQ-BK-1` pause and a
   `reserve-divergence` page, with no term of value differing.

Each needed `operator_request_unpause` and `operator_approve_unpause` from a different login (`REQ-GV-2`). The core
never lifts its own pauses. Neither page said whether money was at stake.

## The gap

`REQ-IC-2` names "a condition it cannot explain" and lists, side by side, a divergence in the reserve equation (the
money differs) and a finality stall or an observation gap (bezent cannot see). `REQ-BK-1` says "any divergence" pauses.
`REQ-GV-2` says unpausing should need several parties. So the specification gives one response to two situations that
are very different to a reader: one where nothing is known to be wrong but nothing can be checked, which ends by itself
once the networks are read again, and one where a term of value is known to be wrong, which needs a person.

## The proposal (as put to the owner)

1. **Cannot see: a hold, not a pause.** While the check is blind (no complete evaluation yet, an evaluation incomplete,
   a reserve point behind the final head beyond the bound, a final head stale), the asset's routes refuse new deposits
   and withdrawals exactly as a pause does; transfers already in flight are still observed. The hold lifts by itself
   once an evaluation holds again (fresh, complete, no differences). A person is paged only if the blind state lasts
   beyond a bound (ten minutes by default, configurable), or at once when it is a finality stall of the chain (the final
   head not advancing), as distinct from our own walk catching up (the final head advancing, our point behind).
2. **The money differs: a pause, as today.** Unchanged: it stays until the two-login lift (`REQ-GV-2`). Lag and
   staleness are no longer counted as a difference of value that produces a `REQ-BK-1` pause.
3. **Every pause and page names its kind** (a "blind" hold or a "money differs" pause) and carries the values (supply,
   reserve, book, the lag figures), so a reader can tell at a glance whether money is at stake.

Fail-closed is kept: while blind, nothing new may start. A hold never masks a money difference the check can see: a
difference of value found while the check also lags is a pause. While it cannot read the terms at all, a difference may
exist unseen; the hold is what stops new work meanwhile. The self-lift is only for the hold; the core still never lifts
a pause of a divergence, of a quarantined observation, of a compromise, or of `INV-4`.

## Ruling

@pandres95, in chat, 2026-11-07, in reply to the proposal above: **"Yes, write the amendment and build it."**

## What "blind" is

The check is blind for an asset, and holds its routes, when its latest evaluation:

- **could not be made** (no final point on a network, a reading or the store failed, the evaluation itself errored):
  recorded as incomplete, as before; or
- **was made but cannot hold**, because a point it rests on lags its network's latest final head by more than the
  network's bound (`networks.reserve_point_lag_seconds`), or because a network's latest final head is older than that
  bound by the check's clock. It is recorded as incomplete too, its reason naming the networks and the lag, and its
  terms and the book's are kept in the record. It is no longer a divergence.

A **difference of value** is what `REQ-BK-8` compares: reserve, in transit, supply, owed mint, owed payout and surplus,
item by item, a negative surplus (`REQ-BK-3`), and the supply mismatch of `INV-4`. An evaluation with any of them
diverges and pauses under `REQ-BK-1`, whether or not it also lags; when it also lags, the asset is held as well, so that
lifting the pause while the check is still blind starts nothing.

"No complete evaluation yet" is held whether or not a hold row exists. The core's gate and the signer's
`pause_holds_intent` treat an asset as held unless its latest evaluation, by id, holds and was made (it names the book
entry it cut at: `reserve_check_holds`). That covers an asset never evaluated, one declared while the core runs, the
moment after a restart or after 0142 is applied, and any race between a hold and the work it holds. The hold row is
still written, for the record and for paging: an asset's first evaluation at a cold start has no final point, so it is
incomplete and holds at once. The core runs the reserve check before its cycle on each pass, and a check's first tick
evaluates every asset however recent its last evaluation, so a restarted core sees each asset before its cycle runs.

If the store refuses even the record of a failed evaluation, nothing durable can be held: the core's gate holds the
asset in memory until an evaluation is recorded again, and, while the check cannot list the assets at all, every asset.

## Catching up or a finality stall

Both endpoints record the network's **live** finalized head in `final_heads` (the EVM follower records the pair's final
point at each step, with the vault's readings; the Kreivo endpoint records the finalized head with its supply readings).
Neither records a head walked from history, and neither records one in the middle of a walk: while a long backlog is
walked the latest head ages as it would without the walk (so `observer_lag_estimate_seconds` and the core's deadlines
still see the lag, `NFR-6`), and what moves is the watermark. So, per network:

- **Catching up**: the latest recorded final head is fresh by the clock (within the network's bound), and the point the
  check can read at lags it beyond the bound. The chain is finalizing; our reading of it (the walk's watermark, or an
  observation still awaiting its outcome) is behind. Held; paged only once the asset has been blind for longer than its
  blind bound.
- **Finality stall**, while the network's walk has also stopped: no final head recorded within the bound by the clock,
  and the watermark has not moved since the latest evaluation at least that bound old that recorded one (never one
  seconds old, so a restarted core's first tick or an asset just quarantined does not take a slow walk for a stopped
  one); or, once the asset has been held for longer than the network's bound, the latest final head is still the one
  recorded when the hold began (`pauses.held_heads`, written for every hold, a cold start's and a recorded failure's
  included), and the watermark has not moved since. The second test does not rest on block timestamps, which can
  mislead the clock. A network on which no head was ever recorded is never a stall: nothing has been seen to stop.
- **When `finality-stall` pages.** Once the walk is judged stopped, at once. Judging it stopped needs a baseline a bound
  old, so a stall can page later than the moment the head stops: up to one bound later after a restart, for a new
  asset, after the deploy of 0142, and when the head stops during a catch-up (the walk then has to reach the head and
  stand still for a bound). The Kreivo endpoint moves its watermark once per 256-block chunk, after a header walk: a
  slow walk there is not taken for a stopped one only while each chunk finishes within the issuing network's bound;
  a chunk slower than that pages as a stall. The slow-finality test shows the delay: at 941 s the origin network
  (bound 840 s) is still `CatchingUp`, since no evaluation is yet a bound old, and its stall is judged a bound after
  the evaluation that held at 880.
- A stale head while the walk still moves is catching up, held and paged only past the blind bound: the walk cannot pass
  the latest head, so once it reaches it and stops, a stall that is real pages.
- "Unknown" (no evaluation at least a bound old that recorded the network's watermark, or a hold with no
  `held_heads`) counts as not stopped, so a stale head is not yet a stall. Unknown can last: after repeated failures
  that record no watermark, or with `held_heads = {}`. Such a stall is still held, and still paged, as
  `beyond-bound`, at the blind bound.

The core cannot tell a chain that stopped finalizing from nothing recording its heads (an endpoint stopped, both of its
providers behind): both look like a final head that does not advance, and both page at once, since neither ends by
itself. The classification decides only when a person is paged, never whether work is held.

## Applied text (`SPEC.md` v0.5.11)

- **§2** "Hold" (new): what the reserve check places on an asset's routes while it cannot see the asset; it stops new
  work as a pause does, and is lifted by bezent itself once an evaluation holds.
- **`REQ-BK-1`**: "Any difference of value, to the smallest unit, MUST pause the asset's routes and page a human
  (`REQ-IC-2`). An evaluation that cannot be made, or that rests on a point too far behind its network's final head, is
  not a divergence: it holds the asset's routes (`REQ-IC-2`). A hold never masks a difference of value the evaluation
  can see: one found while the check also lags is a pause as well." The second reading (below) adds: "A difference of value closes the asset's routes at once; the pause and the page need it seen twice, at a newer final point on every network."
- **`REQ-GV-2`**, appended: "The one exception is a hold (`REQ-IC-2`): bezent lifts it itself once an evaluation of the
  reserve equation holds, and no person lifts it by hand. A pause is never lifted by bezent."
- **`REQ-IC-2`**: split in two. On a condition that shows value is wrong or unexplained (a divergence in the reserve
  equation, a reorganisation deeper than expected, a signature it did not record), bezent MUST pause the affected
  routes, keep observing, and page a human. On a condition where it cannot see (no evaluation of the reserve equation
  that holds: one that cannot be made, a point too far behind the final head, a final head that stops advancing, an
  observation gap), it MUST hold the affected routes and keep observing; it lifts the hold itself once an evaluation
  holds; and it MUST page a human once the hold has lasted beyond a declared bound, or at once when a network's final
  head is not advancing. Every pause and every page MUST say which of the two it is, with the values it rests on.
- **`NFR-5`**, appended: "An evaluation that cannot see is not one at zero tolerance: it holds (`REQ-IC-2`)."
- **`NFR-6`**: "beyond the route's budget it is `REQ-IC-2`" becomes "beyond the route's budget the asset is held
  (`REQ-IC-2`)".
- **`AC-E2.1`**: "Given any difference of value in the reserve equation, whether or not the check also lags, Then the
  asset's routes pause until more than one approver lifts it, transfers in flight continue to be observed, and a human
  is paged with the values."
- **`AC-E2.3`** (new, layer service): "Given the reserve check cannot see an asset (no evaluation yet, one that cannot
  be made, a point behind the final head, a stale final head), Then its routes are held: new work waits as under a pause
  and transfers in flight continue to be observed; the hold lifts itself at the first evaluation that holds; and a human
  is paged only once it has lasted beyond its bound, or at once on a finality stall (`REQ-IC-2`, `REQ-GV-2`, `NFR-5`)."
- **§10 `CND-Paused`**: a pause or a hold covers the route. A requester sees a hold as `CND-Paused` and
  `ERR-RoutePaused`, as it sees a pause.

`REQ-IC-3`, `REQ-IC-4`, `INV-15`, `AC-I2.1` and `AC-I2.2` are unchanged: a hold stops new work only, and every pause is
lifted as before.

## Built (migration 0142, the core)

- **The hold is a row of `pauses`**, of cause `blind`, with a generated `kind` (`hold` for it, `pause` for every other
  row, so no writer can mislabel one). Every reader that refuses work during a pause refuses it during a hold, with no
  reader changed: the core's gate (`Source-Final`, `CND-Paused`), the ingress (`ERR-RoutePaused`, the route table's
  `paused`), the signer's `pause_holds_intent` (amendment 0022 A1b), and the deadline clock (amendment 0019 A1d). A hold
  is asset-scoped, names the evaluation it began at, and there is at most one per asset.
- **Who writes it.** The core may insert a hold only naming an evaluation of that asset that did not hold, with
  `held_heads` (each of the asset's networks' latest final head and watermark then). It may delete a row of `pauses`
  only if it is a hold and the asset's latest evaluation, by id, holds, was made (names a book cut) and is later than
  the one the hold names; every other row is refused by the store's trigger (PostgreSQL has no column-level `DELETE`),
  so the core still lifts no pause. The core no longer inserts a `REQ-IC-2` pause naming an evaluation; such rows
  already recorded stay valid, and stay pauses. `operator_request_unpause` refuses a hold.
- **Unseen means held.** `reserve_check_holds(asset)` (the latest evaluation, by id, holds and was made) is read by the
  core's gate and by the signer's `pause_holds_intent`: an asset for which it is false is held, whatever `pauses` says.
  The latest evaluation is the latest by id in the check, the gate, the signer's read, the delete guard and
  `ops_observables` (a new index); a statement still states the latest complete evaluation by time, as before, and
  under the lock an asset's evaluations are recorded in time order. Every evaluation and every record of a failure takes
  the per-asset advisory lock `bezent/reserve-evaluation/<asset>` before it reads anything and holds it until it is
  recorded, so an evaluation decides on what is true when it gets the lock, and a hold and its lift follow the order the
  evaluations are recorded in. The lock is waited for as any statement is, up to the session's `statement_timeout`; a
  timeout there is a store error like any other: the failed evaluation is recorded, or, if even that cannot be, held in
  memory and paged. Production runs one core, so nothing else is expected to hold the key. A watermark the latest
  evaluation at least a bound old did not record (none, or one recorded before 0142) is unknown, not stopped: the first
  evaluation after the deploy does not page a stale head as a stall at once.
  `held_heads` is best effort: if the heads cannot be read, the hold is recorded with none.
- **The check.** Lag and staleness are no longer `Difference`s. An evaluation diverges only on a difference of value.
  Blind evaluations are incomplete, their reason naming each blind network and whether it is catching up or a
  finality stall. The hold is placed, and an incomplete evaluation's turn is logged, in the evaluation's transaction,
  without a page (`reserve-hold`, `reserve-check-incomplete`, `reserve-check-failed` in the event log). The hold is
  lifted in the transaction of the evaluation that holds (`reserve-hold-lifted`). `reserve-blind` is paged once per hold
  and reason (`why`: `finality-stall` or `beyond-bound`). The blind bound is `networks.reserve_blind_page_seconds`
  (default 600, at most 3600), the smallest of the asset's own networks' (its issuing network and its reserve networks),
  beside `reserve_point_lag_seconds`. When a reading fails, the points and lags already found are kept, so a stale head
  on the same evaluation still pages at once. An evaluation whose failure cannot even be recorded cannot be held in the
  store: the core's gate holds the asset in memory (`Unseen`), and it pages at once (`reserve-check-failed`) and again
  past the blind bound (`reserve-check-stalled`), both `class` `blind`, `stop` `hold`, `recorded` false. While the check
  cannot list the assets, the gate holds every asset.
- **Naming.** Every page about a pause or a hold carries `class` (`blind` or `money-differs`) and `stop` (`hold` or
  `pause`). `values` (each term as the networks show it and as the book holds it, null when nothing could be derived)
  is carried by `reserve-divergence`, `reserve-check-incomplete`, `reserve-hold`, `reserve-blind`,
  `reserve-hold-lifted`, `reserve-check-failed` and `reserve-check-stalled`; `lags` (each network's point, latest final
  head, lag in blocks and seconds, head age, bound, watermark and state) by `reserve-divergence`,
  `reserve-check-incomplete`, `reserve-hold` and `reserve-blind`; `held_heads` by `reserve-hold` and `reserve-blind`.
  `reserve-divergence` also names the networks it is blind on (`also_blind`). `book-quarantine`,
  `supply-mismatch-pause` and the core's `key-compromise` name `class` `money-differs`.
- **Order.** The core runs the reserve check before its cycle on each pass (`bezent_core_service::pass`); a check's
  first tick evaluates every asset.
- **Observables.** `pauses_in_force` counts pauses only; `holds_in_force` and `hold_age_seconds` (per asset) count
  holds.
- Tests: `services/core/tests/reserve_check.rs`
  (`ac_e2_3_req_ic_2_a_cold_start_with_every_term_zero_is_held_not_paused_and_lifts_itself`,
  `inv_1_nfr_5_ac_e2_3_m2_a_walk_catching_up_is_held_never_a_divergence`,
  `ac_e2_3_req_ic_2_nfr_5_m2_an_evaluation_that_cannot_be_made_is_held_and_paged_past_the_bound`,
  `inv_1_nfr_6_ac_e2_3_mf2_a_finality_stall_is_held_and_paged_at_once`,
  `ac_e2_1_ac_e2_3_req_bk_1_a_value_difference_while_lagging_pauses_and_never_lifts_itself`,
  `ac_e2_3_inv_15_new_deposits_and_withdrawals_wait_while_held_and_move_on_when_it_lifts`, and the rewritten
  `nfr_5_ac_e2_3_…`, `req_ic_2_nfr_5_ac_e2_3_sf1_…` and `…_sf_b_…`), `services/ingress/tests/limits.rs`
  (`ac_e2_3_a_blind_hold_refuses_new_requests_and_its_lift_admits_them`), `services/core/tests/conditions.rs`
  (`ac_e2_3_a1b_a_hold_keeps_an_unsigned_intent_unsigned_until_the_core_lifts_it`: the signer refuses under a hold), and
  `crates/store/tests/reserve_check.rs`
  (`req_ic_2_req_gv_2_a0028_the_core_holds_only_while_blind_and_lifts_only_its_own_hold`). From the review of the first
  build: `ac_e2_3_a_pass_runs_the_check_before_the_cycle`,
  `req_ic_2_a0028_evaluations_of_an_asset_take_its_lock_and_run_one_at_a_time`,
  `ac_e2_3_req_bk_1_a_divergence_the_check_can_see_keeps_an_existing_hold`,
  `ac_e2_3_a0028_a_head_unchanged_since_the_hold_began_is_a_stall_whatever_its_block_time`,
  `ac_e2_3_a0028_held_heads_hold_for_a_cold_start_and_a_recorded_failure`,
  `ac_e2_3_a0028_a_stale_head_with_a_moving_walk_is_catching_up`,
  `ac_e2_3_a0028_a_failed_read_keeps_the_lags_and_a_stale_head_pages_at_once`,
  `ac_e2_3_a0028_an_evaluation_the_store_cannot_record_is_held_in_memory` and
  `ac_e2_3_a0028_the_first_tick_evaluates_every_asset` (`reserve_check.rs`);
  `ac_e2_3_a1b_an_asset_the_check_has_not_seen_is_held_with_no_hold_row` (`conditions.rs`);
  `ac_e2_3_nfr_6_a_long_step_moves_the_watermark_and_records_only_its_own_head`
  (`services/evm-endpoint/tests/observe.rs`). From the second review:
  `ac_e2_3_a0028_after_the_deploy_an_unknown_watermark_is_not_a_stall_yet`,
  `ac_e2_3_a0028_an_asset_with_no_evaluation_at_all_is_held`,
  `ac_e2_3_a0028_a_check_that_cannot_list_the_assets_holds_every_transfer` and
  `ac_e2_3_a0028_a_hold_is_recorded_when_the_heads_cannot_be_read`; the lock test now fails if the lock is taken only to
  record. Test fixtures that move value declare that the check has seen their assets (a holding evaluation recorded as
  made).

## Open after the ruling

None of these is applied; each is the owner's.

1. **The release gate's window (§15, amendment 0021).** "Every evaluation … at exactly zero divergence" for 24 hours.
   Before this amendment a lag was a divergence and restarted the window; now it is a blind evaluation. Whether a blind
   stretch restarts the window, or only one longer than the blind bound, or none, is not ruled. Nothing in the code
   measures the window; the evidence is a person's (`release/gate.toml`). Recommended: a blind stretch longer than the
   blind bound restarts it.
2. **An operator lifting a hold by hand.** Built: refused, since it would let new work start while the check cannot
   see. If the check is blind for good (a misconfiguration, an endpoint that never returns), the cause is fixed and the
   hold lifts itself. Whether there should be a two-person override is not ruled.
3. **A finality stall and an observer that stopped** look the same to the core and both page at once (above).
4. **Production pauses 1 and 2** are not reclassified by the migration: they stay pauses, lifted by two operators as
   before.
5. **Requesters** see a hold as `CND-Paused` and `ERR-RoutePaused`. Whether a requester should see the hold as its own
   condition (`REQ-IC-5`) is not ruled; nothing on the requester surface changed.
6. **Residual windows.** An asset the check has not seen is held by the gate and the signer with or without a hold row,
   so a new asset, a restart and the moment after 0142 hold nothing open. What remains: a core whose gate holds an asset
   in memory (the store refused the record) holds it only in that process. The signer does not see that hold (it reads
   only the store), so an intent recorded before it may still be signed; another core or a restart would not hold it
   either, until its own first tick. The ingress does not read `reserve_check_holds`: it keeps taking requests for an
   asset not yet seen, which wait at the core. With the core down nothing is evaluated, but nothing is ordered either.
   **Known limitation, true before this amendment:** `reserve_check_holds` does not ask how old the latest holding
   evaluation is. While the core is dead (or something keeps the asset's evaluation lock and never records), the
   latest evaluation stays `holds`, and the signer keeps signing intents the core recorded before it stopped. A
   freshness window on the store's clock was built and withdrawn in review; whether to bound it is the owner's.
7. **The observables.** `reserve_equation_outcome` is 2 for a blind evaluation, as for any incomplete one ("could not
   be made"); `pauses_in_force` counts pauses only, `holds_in_force` and `hold_age_seconds` holds. An infrastructure
   alarm on `reserve_equation_outcome` must wait out the blind bound, or it pages where the core does not
   (`deploy/runbooks/alerts.md`).
8. **Behaviour that is stricter than before.** A transient failure to read (one evaluation incomplete) now holds new
   work at once, for at least one evaluation period, where before it paused nothing until the bound. A blind
   evaluation is incomplete, so a statement does not state it: a blind stretch longer than the statement's evaluation
   age (15 minutes) ends in `statement-overdue`, where a lag used to be stated as a divergence.
9. **Deployment.** Every service refuses to start unless the store's migrations are exactly its own, so a core and a
   store on either side of 0142 cannot start together. A core already running when 0142 is applied can no longer record
   a `REQ-IC-2` stall pause: its failed evaluation goes to the unrecorded path and pages `reserve-check-failed`. It can
   still raise a `REQ-BK-1` pause from a lag (its own code counts a lag as a divergence), which then needs the two-login
   lift as before. Apply 0142 and restart the core together.

## The second reading (ruling of 2026-11-08)

**Ruling.** @pandres95, 2026-11-08, detailed by the KSM Operator on his delegation: "A second reading is always
necessary — not because nodes lie, but because they're not always up-to-date, and node-scoped forks happen."

- **A single anomalous reading of an asset holds signing for it, silently.** No change: `reserve_check_holds(asset)`
  (migration 0142) is false unless the latest evaluation by id is `holds` with a book cut, so one `diverges`
  evaluation closes the core's gate and the signer's `pause_holds_intent`. The evaluation is logged as
  `reserve-divergence-unconfirmed` (`class: money-differs`, `stop: hold`), not paged.
- **The `REQ-BK-1` pause and the `reserve-divergence` page need the same difference of value seen twice:** the asset's
  previous evaluation by id also `diverges`, and the current one rests on a newer final point on every network. Compared
  per network on `reserve_evaluations.points`: newer is a strictly higher block number, or a strictly higher final head
  (`lag.head_block_number`), than the previous evaluation's point for that network. Two evaluations at the same
  points are one reading seen twice and do not pause. A network the previous evaluation has no point for is not newer.
  The pause is raised once, as before, and lifted by two operators.
- **Blind never pauses by itself** (unchanged), and the hold lifts itself at a fresh `holds` evaluation (unchanged).
- **`INV-4` (supply mismatch) stays immediate.** The mismatch is read from a durable flag, so one gone by the point read
  now is still a breach to raise (PR #50 review M1); the evaluation can `hold` while the `INV-4` pause is due, and
  deferring that pause would leave signing open. The `key-compromise` alert is a compromise alarm with its own dedupe
  and is untouched, as are `REQ-IC-4` (migration 0095), operator pauses (0091) and the book's quarantine.
- **A different node** will also be required for the second reading once a second node exists (#103). That is not part of
  this round: today both readings may come from the same endpoint.

**Known limit, not built.** A flapping D, H, D, H (diverges, holds, diverges, holds) never pauses: the previous
evaluation of each `diverges` is a `holds`. It fails closed during each D, so signing is never open on a difference.
Follow-up: count divergences over a window, not only consecutive ones.

## Identifier ledger

New: `AC-E2.3`, §2 "Hold". None of them was used before.
