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
7. **A snapshot past the network's bound is incomplete, never trusted.** It uses
   `networks.reserve_point_lag_seconds`; the evaluation is incomplete, and one that stays so pauses through
   `stalled()` as for any network. The bound for an attested network is a declaration: see the PR's questions.
8. **No new named error.** Every refusal is an existing `ERR-*` (§10): a new one would be a SPEC change.
9. **A reversal names the arrival it reverses, by the party's report identifier** (guardrail 5: nothing is matched by
   amount or time). The total reversed of one arrival may not exceed it. A restoration is in full, from the party that
   reported the reversal, as `book::restoration` already required (review S8).
10. **Withdrawals hold at `Source-Final`.** See plan, "Not now". The gates (`CND-Paused`, `CND-CutOff`,
    `CND-ReserveShort`) still apply: they are conditions in place, on an edge the table has. `deliver()` returns before an
    order on an attested destination, and the store refuses one whoever writes it.
11. **The commitment was read from the wrong key.** `decide_extras` read `detail ->> 'commitment'`, while the Kreivo
    endpoint records `detail.payout.commitment` (`observe.rs`, `payout_json`). The ingress tests passed because their
    fixture wrote the top-level key. Fixed, and the fixture now writes what the endpoint writes.
12. **Migration 0060.** The M-2 work runs at the same time and may add migrations from 0055. Numbers have gaps already, so
    neither renumbers.
