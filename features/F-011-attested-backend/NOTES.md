<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-011 — notes

Each choice below is a reading of the spec and the plan, not a ruling. What needs a ruling is in
`amendments/0016-attested-delivery.md` and the PR.

## Decisions

1. **A report is an observation, at a coordinate of its own.** Block number: the report's position on its attested
   network. Block hash: BLAKE2b-256 of the bytes signed. Position: log 0. A parallel pipeline would need a new version of
   `source_bindable`, `bezent_final_source`, `transfers_source_verified_final`, `intents_guard_order` and the book's
   postings. As an observation, every guard on a mint holds unchanged, and `REQ-BE-5` reads literally: the report *is* the
   observation.
2. **Two roles, so neither forges alone.** Only the ingress inserts a report. Only the core materialises one, after
   checking its signature, through a `SECURITY DEFINER` function that derives every field from the report row. The
   core keeps no write on `observations`, `observation_outcomes` or `final_heads` (the test of review S6 stands). A
   compromised ingress cannot produce an observation; a compromised core cannot produce one without a report row.
3. **Signed bytes, no canonical form.** The envelope carries the document as a string, and the signature covers those
   exact bytes. A canonical JSON would be one more thing the TypeScript module and this service could disagree on.
4. **The attesting grant is per backend.** `requester_grants` is per kind (`REQ-PN-2`), but `REQ-RQ-4` says "that
   backend's attesting grant", and `DEC-15` puts Bloque Test on the production instance: a global grant would let a test
   requester mint production fiat. `attesting_parties` declares one requester and one party name per attested network.
   The grant is still required; the declaration narrows it. No declaration means nobody may report or add extras.
5. **Reports are serial per network.** The store assigns the sequence under an advisory lock held to commit, so sequence
   order is commit order and materialising in sequence never skips one that commits late. A report the core refuses
   holds its network's queue and pages: a stale head makes the check incomplete, which pauses the asset (`REQ-IC-2`).
6. **The core reads the snapshot rolled forward, not as of its `through` point.** Cutting the book at the snapshot's
   `through` height looks simpler, and it is wrong: a mint rests on the issuing network's block, so an arrival reported
   after the snapshot would leave the book holding the mint and not the arrival, a false divergence and a pause on every
   deposit between two snapshots. Instead the point is the latest report on the network, and the reserve is the
   snapshot plus the movements reported after `through`. The check is then exactly "the snapshot agrees with the book
   at `through`"; later movements move both sides alike.
7. **No snapshot is incomplete; a snapshot past the bound does not hold.** With none yet the evaluation is incomplete,
   never a reserve of zero, and one that stays so pauses through `stalled()`. Past `attested_snapshot_max_age_seconds` the
   snapshot counts as a head that stopped: the `final_head` difference, a pause and a page, as for any network whose head
   stopped. Past `attested_snapshot_interval_seconds`, an alert and nothing else. Configuration and not a store
   declaration, on the owner's instruction: it is the same for every party, and changes with no migration.
   `networks.reserve_point_lag_seconds` still has to be at most a fiat route's budget (`routes_budget_covers_lag_bound`).
8. **No new named error.** Every refusal is an existing `ERR-*` (§10): a new one would be a SPEC change.
9. **A reversal names the arrival it reverses, by the party's report identifier** (guardrail 5: nothing is matched by
   amount or time). The total reversed of one arrival may not exceed it. A restoration is in full, from the party that
   reported the reversal, as `book::restoration` already required (review S8).
10. **The hand-over.** A delivery attempt on an attested backend records no order, and the store refuses one whoever writes
    it. `deliver()` calls `hand_over()`, which enters `Delivering` once the instructions are accepted, after checking
    them again against the redemption's commitment (a row that misses it pages once and hands nothing over). A
    withdrawal with no instructions waits at `Source-Final` with no deadline: §5.4 sends only a held condition there to
    review, and no condition names the wait since `CND-AwaitingInstruction` was withdrawn (see the questions). The gates
    apply until a payout is reported; after that they stop and any condition they left clears, since the reserve has
    already lost that amount and `REQ-OT-5`'s check would count it twice. A payout may be reported before the hand-over
    runs: the leg waits for it.
11. **The commitment was read from the wrong key.** `decide_extras` read `detail ->> 'commitment'`, while the Kreivo
    endpoint records `detail.payout.commitment` (`observe.rs`, `payout_json`). The ingress tests passed because their
    fixture wrote the top-level key. Fixed, and the fixture now writes what the endpoint writes.
12. **Migration 0060.** The M-2 work runs at the same time and may add migrations from 0055. Numbers have gaps already, so
    neither renumbers.
13. **A report is no transfer.** It is recorded in `intake` for its idempotency (`REQ-ID-1`), with no row in
    `intake_answers` unless refused, so `/v0/transfers/by-request` answers `not-found` for one: the report surface answers
    for it. In a requester's `/v0/log`, a refused report appears as a refusal, as any other request's; a recorded one does
    not, since that log is of its transfers and refusals and the party has its report's answer. The core's
    `attested-report-recorded` is in the event log for operators.
14. **Test stores.** `environment::TEST_NETWORKS` gains `fiat-local`, the sidecar's attested test backend, declared only with
    `BEZENT_SIDECAR_ATTESTING`. Any other attested network counts as production (`REQ-KY-6`), the safe side.
15. **Changing a long function by replacing its text.** `0061` changes `transitions_apply_lifecycle`,
    `transfers_guard_writer` and the record function with `replace()` over `pg_get_functiondef`, and stops if the text is not
    found. The migration then shows only what changes, and a contrary ruling takes it out whole. Copying 300 lines to change
    three would hide that.

## Working assumptions, and how to take each back out

The owner has not ruled on any of these. `CLAUDE.md` says never to apply an unruled decision, "not even provisionally"; this
PR does, on the instruction to complete `M-3` on the recommendations, in a draft, unmerged.

| Assumption | Where | To reverse |
|---|---|---|
| 0016 A1: `Delivering` also means handed to the party; a report of non-execution is the proof | Migration `0061`; `Core::hand_over`, and the two lines in `deliver` and `source_final`; `Kind::PayoutNotExecuted` and `decide_not_executed` | Drop `0061`, make `deliver` return before an attested destination, remove the kind. Fiat withdrawals hold at `Source-Final` with their payout in the book. The tests that change: completion, refund, retry, refund of an unassociated arrival |
| 0016 A3: an arrival reversed and not restored binds nothing, and holds a deposit | Migration `0062`; `reversed` in the ingress, the first lines of `Core::gate` | Drop `0062` and those calls. Such an arrival stays mintable, charged to surplus |
| Question 4: one hour, ten minutes | `attested_snapshot_*`, two lines of configuration | Change the values, no release of the store |
| A withdrawal with no instructions waits with no deadline | `hand_over` | See the questions |

## Questions for the owner

Beyond amendments 0016 and 0017, and the snapshot limits, one, with a recommendation:

**A redemption nobody relays for.** The value is destroyed and the withdrawal waits at `Source-Final` forever, silently.
§5.4's `Source-Final → Needs-Review` covers a held condition, and `CND-AwaitingInstruction` was withdrawn (amendment 0007).
Recommended: a line in 0016 sending it to review at the route's deadline, with its owner. Not built: it is not on the list of
what to assume.

