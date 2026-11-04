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
   never a reserve of zero, and one that stays so pauses through `stalled()`. Past
   `networks.reserve_point_lag_seconds` the snapshot counts as a head that stopped: the `final_head` difference, a pause
   and a page, as for any network whose head stopped. The bound is a declaration: see the questions below.
8. **No new named error.** Every refusal is an existing `ERR-*` (§10): a new one would be a SPEC change.
9. **A reversal names the arrival it reverses, by the party's report identifier** (guardrail 5: nothing is matched by
   amount or time). The total reversed of one arrival may not exceed it. A restoration is in full, from the party that
   reported the reversal, as `book::restoration` already required (review S8).
10. **Withdrawals hold at `Source-Final`.** See plan, "Not now". The gates (`CND-Paused`, `CND-CutOff`,
    `CND-ReserveShort`) still apply until its payout is reported: they are conditions in place, on an edge the table has.
    Once the payout is reported, the gates stop and any condition they left clears: the reserve has already lost this
    withdrawal's amount, so `REQ-OT-5`'s check would count it twice. `deliver()` returns before an order on an attested
    destination, and the store refuses one whoever writes it. The refund of an unassociated arrival on an attested network
    holds in the same place, which amendment 0016 lists.
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
14. **Test stores.** `environment::TEST_NETWORKS` names no attested network, so a store that declares one counts as
    production (`REQ-KY-6`), the safe side. A test deployment of the attested backend adds its network's name there. No
    deployment of it exists yet, so that waits for one.

## Questions for the owner

Beyond amendment 0016 (the state after `Source-Final`, a report of non-execution, the report surface in §7.A, a reversal of
an unminted arrival):

1. **How stale may a snapshot be?** `networks.reserve_point_lag_seconds` per attested network, and how often Swaps sends
   `positions`. Recommended: one hour, with a snapshot every ten minutes. Past the bound the asset pauses.
2. **A report the core refuses** holds its network's queue on an append-only table, and the stale head then pauses the
   asset. It can happen only through a forged row or a retired key, and there is no way forward but a migration owner's
   repair. Recommended: leave it so for V0, with a runbook (`F-014`): stopping is right when the party's signature fails.
3. **Retiring a key.** The core checks the key is declared when it records, so retiring one between intake and recording
   refuses a real report. Recommended: runbook, not code: declare the new key, stop using the old, retire it only when no
   report by it waits. A cycle drains the queue.
4. **Bank data at rest.** `relayed_instructions` keeps the relayed bytes (since `F-002`). §1.3 says bezent does not hold a
   requester's facts or need them, and bezent never pays from these bytes. Recommended: an amendment to keep only the
   commitment and that it matched, before any real fiat flows, since the table is append-only.
