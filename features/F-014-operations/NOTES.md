<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
# F-014 · Operations: notes

Issue #14. Serves `REQ-ST-6`, `REQ-IC-1` to `REQ-IC-6`, `REQ-GV-1`, `REQ-GV-2`, `REQ-GV-5`, `NFR-5`, `NFR-6`, `NFR-8`,
`NFR-9`; acceptance `AC-H3.1`, `AC-I2.1`, `AC-I3.1`, `AC-I3.2`, `AC-I4.1`. This file is what was found, what was built
and what is left. The runbooks are in `deploy/runbooks/`; read `alerts.md` there for the paging answer.

## What existed (not rebuilt)

| Piece | Where |
|---|---|
| `deploy/health.md`, `deploy/statements.md`, the container health checks | `deploy/`, `crates/health`, each service's `health.rs` |
| Pages as log lines: the reserve check's `LogPager`, the engine's three `kind =` errors | `services/core/src/reserve/check.rs`, `engine.rs` |
| `event_log` kinds for each page (the log is sequenced and gapless) | `services/core/src/store.rs::event` |
| The review path: `manual_actions`, `manual_approvals` with the session login as actor and a second approver | migrations 0014 to 0017, `F-003` notes |
| `compromise::raise` (alert and `pause-requested` to the log), the core's check of every returned signature | `services/signer/src/compromise.rs` |
| The pause: `pauses` (route, asset, direction, global), read by the core's gate and the ingress | migrations 0010, 0031, 0040; `engine.rs` |
| The reserve check, readings, evaluations, quarantine | `F-004` |
| Network identity at start-up, failing closed | `services/*-endpoint/src/network.rs` (`AC-I4.1`) |
| No metrics endpoint, no counter: the services have none | (confirmed by search) |

## What this change adds

- **Observables** (`AC-H3.1`). Migration 0090 `ops_observables()`: oldest non-final transfer per route, unknown legs
  (count, age), an estimate of each network's observer lag (`observer_lag_estimate_seconds`) and open gaps per network, the reserve equation per asset (outcome, age), reserve per
  network, undelivered and dead notifications, arrivals in `CND-Unassociated` with age, reviews, pauses, transfers held in
  `CND-FeeShort`. The core reads it with its own login each minute and writes CloudWatch embedded-metric lines to its
  log (`services/core/src/observe.rs`), because the log is the only exporter a service with no port has. An unknown value
  is left out and named, never a zero. Test: `crates/store/tests/operations.rs`, `services/core/tests/operations.rs`.
- **The operator surface and its record** (`AC-I3.1`, `REQ-GV-1`, `REQ-GV-2`). Migration 0091: `admin_actions`
  (append-only; actor is the session login and the time the store's; a justification is required) and four functions an
  operator calls with their own login: `operator_pause`, `operator_request_unpause` and `operator_approve_unpause` (a
  second login lifts it), and `operator_record` for an act done elsewhere (key rotation, a bound, a route, a float).
  Before, only the schema owner could pause or lift a pause, with no record. The operator also reads `pauses`,
  `transfer_status` and `event_log` now, for the runbooks' "confirm". The surface is SQL under the operator's own login;
  there is no CLI, and none is needed for the acceptance.
- **No surface edits a status or moves value** (`AC-I3.2`). A catalogue test of the operator role (no table privilege
  beyond SELECT, INSERT only on the records, the callable functions as the store defines them, none of the writing ones
  touching another table), and each forbidden write attempted. Fails if a grant is widened.
- **Pages an alarm can match.** Three engine pages recorded their kind as a literal, which renders `kind="..."`; they
  now render `kind=...` like the reserve check's. A source scan fails on a new quoted page. The signer's and the
  endpoints' `key-compromise` alerts only ever reached the event log; the core now logs each once as a page
  (`services/core/src/signals.rs`).
- **Runbooks** (`REQ-IC-6`): one for each of the 11 active named conditions and the 16 incident classes, one for each page
  family, and the float top-up, with an index. `services/core/tests/runbooks.rs` fails when a condition or class has none,
  when a page kind in the source has none (or one on the base is marked absent, or the reverse), when a runbook names a
  test or an observable or a requirement that does not exist, when it claims an exercise `EXERCISES.md` does not hold,
  and when a runbook holds a private identifier.

## `AC-*` status

The criteria map's exceptions for `AC-H3.1` and `AC-I3.1` (F-013, PR #75) are removed in this change, as the map requires once a criterion has its test. Both stay **partial**, below, and are listed with `AC-I1.2` in `e2e/criteria/known_unmet.toml`: the map reports them `known unmet` and the release gate fails on them.

| Criterion | Layer (SPEC §9) | State here |
|---|---|---|
| `AC-H3.1` | Service, over the store | **partial**: every `REQ-ST-6` item but fee positions is one read, tested; fee positions are not observable (Open 2) |
| `AC-I1.2` | Custody + service | **Pause applied (issue #77, migration 0095); still listed `known unmet`.** The signer and the core detect a signature bezent did not record, refuse it and raise `key-compromise`, and the store pauses the bridge in the same transaction. Unmet: on an EVM network a signing intent recorded before the pause has no bound (amendment 0022 A1), and the on-chain pause is a person's act (A2), from a page nobody receives yet (`alerts.md`). The entry stays in `e2e/criteria/known_unmet.toml` until the spec owner reviews it. Open 1 |
| `REQ-IC-2` | (requirement) | Met for "a signature it did not record" (and a late mint) by the store's pause on the alert (issue #77), with the bound of amendment 0022 A1 open (Open 1). The reserve-equation divergence, a stall that outlasts the route's budget and an unevaluable reserve are paused by the reserve check (`reserve_check.rs::req_ic_2_nfr_5_m2_*`, `ac_e2_1_*`), and a supply mismatch pauses the asset under `INV-4`. A deeper-than-expected reorganisation and an observation gap are not assessed by this change |
| `AC-I2.1` | Service + chain | service half passes (global, asset, network, direction and route; in flight continues; a second login lifts); the chain half is `F-006`'s, `contracts/bezent` |
| `AC-I3.1` | Store | **partial**: passes for the pause, its lifting and the acts recorded with `operator_record`; recording an act done elsewhere is voluntary, and the schema owner edits declarations with no record (Open 3) |
| `AC-I3.2` | Store | passes |
| `AC-I4.1` | Service | passes (existing tests of both endpoints, named; the EVM one shown to fail with its check removed) |

## Measured, and what needs a live environment

- **`NFR-8`** (pause within one block or 30 s): the off-chain part is the core's next cycle (`cycle_seconds`: the default is five
  seconds, and the deployment's own template sets the same value; a test fails if the default or the repository's
  fixture exceeds ten) plus the cycle's own time. A test pauses as an operator, runs one cycle and
  measures the elapsed time (6 to 14 ms against a local store). The ingress reads `pauses` on each request.
  Not measured: a pause on chain (the contract's), and a cycle under production load.
- **`NFR-6`** (each observer within 60 s of its network's final head): **estimated, not measured.** The store never holds
  the network's final head, only the heads an endpoint recorded, so the distance behind the network cannot be read. What
  is observable is `observer_lag_estimate_seconds` (before this change `observer_lag_seconds`, seconds since a head's
  `seen_at`, which overclaimed): the clock less the latest recorded head's block time less the network's configured
  `finality_seconds`, floored at zero, which is the reserve check's own basis for a stale head. It is wrong by the
  difference between the real and the configured finality time and by clock difference. A test shows it per network
  (`ac_h3_1_observer_lag_estimate_and_open_gaps_are_per_network`). Measuring `NFR-6` exactly needs the endpoint to record
  the network's head beside its own (a live network); whether an endpoint stays within 60 s needs one running.
- **`NFR-9`** (every runbook exercised): none has been. `deploy/runbooks/EXERCISES.md` is empty on purpose.
  What exists is a test of the behaviour under each runbook, listed in it.

## Paging, in one line

A page reaches nobody: the alarms are code with actions disabled, no service runs, and the destination is a chat
channel. `deploy/runbooks/alerts.md` has the table, the three kinds no filter covers and the list of needs.

## Open

1. **Closed by issue #77 except for what amendment 0022 leaves open.** A key-compromise alert now pauses the bridge in the transaction that records it (migration 0095). The text below is what stood before it, kept for the record: no process turned the request into a pause, so
   `REQ-IC-2` and `REQ-IC-4` ("MUST pause") are met by a person, within minutes of the page, not by bezent. Building it
   needs the store to let the core insert a global pause for that cause (the pause guard of migration 0031 admits only an
   asset's routes on a reserve cause) and a test. Not done here: it widens what the core may do to a pause and deserves its
   own review. The same applies to a late mint (PR #72, merged), which raises the same request.
2. **Fee positions** (`REQ-ST-6`). No service reads a delivery account's balance. `fee_short_transfers` is the only signal,
   and it fires after a delivery could not pay. Needs a balance read in each endpoint (live network) and a floor from
   configuration (`DEC-18`).
3. **Administrative acts the store does not perform** (key rotation, raising a bound, route admission, credentials,
   surplus movements) are recorded by `operator_record` when the operator does it; nothing forces it. The schema owner can
   still change declarations directly; that login is for migrations and restores, held outside the operators.
4. **Alarms**: see `alerts.md`. Not changed here (read only).
5. Credentials have no revocation or expiry (`F-002` notes); not in this change.
6. PR #72 (the late-mint pages) is on `develop`: its three kinds are recorded with `%`, tested against real emissions
   (`late_mint.rs`) and no longer `not_on_base`. PR #66's attested pages record the kind with `%` too, and their runbook has no `not_on_base` marker, so
   `pages.rs` and `runbooks.rs` pass when it is merged. That was the point of both tests.

No amendment is proposed: nothing here depends on a decision SPEC does not make.
