<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: reserve
kind: pages
covers: [REQ-IC-2, REQ-BK-1, REQ-BK-3, REQ-BK-8, REQ-BK-10, NFR-5, NFR-6, INV-1, INV-4]
pages: [reserve-divergence, reserve-alert, reserve-check-incomplete, reserve-check-failed, reserve-check-stalled, supply-mismatch-pause, book-quarantine]
observables: [reserve_equation_outcome, reserve_equation_age_seconds, reserve_amount, observer_lag_estimate_seconds, open_gaps, pauses_in_force]
tests: [services/core/tests/reserve_check.rs::req_ic_2_nfr_5_m2_an_evaluation_that_cannot_be_made_beyond_the_bound_pauses, services/core/tests/reserve_check.rs::inv_1_req_ic_2_m1_an_observation_the_book_cannot_post_is_quarantined_paged_and_paused, crates/store/tests/reserve_check.rs::req_bk_1_req_ic_2_the_core_pauses_only_an_assets_routes_on_that_assets_divergence, services/core/tests/reserve_check.rs::ac_e2_1_a_divergence_pauses_the_assets_routes_keeps_observing_and_pages_a_human, services/core/tests/operations.rs::ac_i2_1_an_operators_pause_holds_new_work_in_each_scope_and_a_second_operator_lifts_it]
exercised: no
---
# Reserve pages

## What it is

The reserve equation (reserve, in transit, supply, owed mint, owed payout, surplus) is re-derived from the networks at
final points and compared with the book, at zero tolerance, at least once a minute per bridged asset (`NFR-5`,
`REQ-BK-8`). Each page below is one way that check tells a person something is wrong. The check pauses the asset's
routes itself in four of the seven cases; lifting that pause is always an operator's act (`REQ-GV-2`), and the core
never lifts it.

| Page | Meaning | The core has paused |
|---|---|---|
| `reserve-divergence` | A term or an item differs between the networks and the book, or surplus is negative. | yes, the asset |
| `supply-mismatch-pause` | The on-chain component's count of the asset differs from the issuance (`INV-4`). Also raises `key-compromise`. | yes, the asset |
| `book-quarantine` | An observation the book could not post was set aside; everything else is still posted and checked. | yes, the asset it moves |
| `reserve-check-stalled` | The equation could not be made for longer than the smallest bound of the asset's networks: a finality stall or an observation gap. | yes, the asset |
| `reserve-check-incomplete` | One evaluation could not be made (no final point, a reading or the store failed). Paged once; recorded as incomplete, never as holding. | no, until it stalls |
| `reserve-check-failed` | The evaluation itself errored. | no, until it stalls |
| `reserve-alert` | Surplus is below its alert level or floor, a reversal is not restored within a business day, or the floor or level is not declared (`REQ-BK-10`, `REQ-BE-6`). | no (mints may pause: see below) |

## Pages

Every row above is an ERROR line `page kind=<kind> asset=<asset> detail=<json>` and an `event_log` entry of the same
kind, with the differences itemised. `alerts.md` has the filter for each.

## Confirm

```sql
SELECT metric, asset, network, value FROM ops_observables()
WHERE metric IN ('reserve_equation_outcome', 'reserve_equation_age_seconds', 'reserve_amount',
                 'observer_lag_estimate_seconds', 'open_gaps', 'pauses_in_force') ORDER BY metric, asset, network;
SELECT id, asset, evaluated_at, outcome, reason, differences FROM reserve_evaluations
ORDER BY id DESC LIMIT 10;
SELECT id, asset, cause FROM pauses ORDER BY id;
SELECT seq, kind, payload FROM event_log WHERE kind = '<the page kind>' ORDER BY seq DESC LIMIT 5;
SELECT * FROM book_quarantine ORDER BY id DESC LIMIT 5;
```

The outcome is 0 holds, 1 diverges, 2 could not be made. The itemised `differences` say which term and which item
(a network, a reference) the networks and the book disagree on, in minor units. An incomplete or stalled evaluation
names its reason: look first at `observer_lag_estimate_seconds` and `open_gaps` for the networks the asset uses (a network that
stopped recording final heads, or a gap) and at the endpoint's own health (`deploy/health.md`).

## Do

1. **Do not lift the pause yet.** The pause is the correct state until the cause is understood (`REQ-IC-2`). Work in
   flight is still observed and completed (`INV-15`); tell requesters it is delayed, never failed (`REQ-IC-3`).
2. If the page is not paired with a pause (`reserve-check-incomplete`, `reserve-check-failed`, `reserve-alert`) and you
   cannot yet say it is harmless, pause the asset yourself: `SELECT operator_pause('<asset>', NULL, NULL, '<page, evaluation id>');`
3. Find the cause from the differences. Typical causes, each established from public chain data and the store, not
   from a guess: an endpoint that stopped recording (restart it; `deploy/health.md`); a gap (the endpoint fills it from
   the archive source; the gap closes by itself when read); a provider disagreement (the endpoint refuses a final
   point the providers do not agree on; wait for them or change provider, from the deployment's own notes); a real
   movement bezent did not book (an arrival from an address that is not a declared funding address is owed mint, not
   surplus: `INV-1`; it is never absorbed); a reserve short because value left the reserve outside a referenced
   transfer (`INV-14`): that is a custody incident, so treat it as `key-compromise`
   (`pages/key-compromise.md`).
   **A statement timeout.** If `reserve_evaluations.reason` (or the page's `error`) reads `canceling statement due to
   statement timeout` (SQLSTATE `57014`) on every pass, the evaluation is being cut off by the store's statement bound,
   not by anything on a chain. It fails closed: it is recorded as incomplete and the asset's routes stay paused, and
   it will repeat every pass until the statement fits. The evaluation (`as_of`) sums every posting of the asset in one
   statement; the bound is 60 s for each statement of the core's store session. Measured once, on a synthetic test
   book of 4.6 million postings of one asset (5.6 million in all), it took about 5 s. Only that size was measured: if
   it grew in step with the postings 60 s would be tens of millions of postings of one asset, but a hash aggregate that
   spills can grow faster, so treat that as an estimate. A timeout at a much smaller book, or one that starts
   suddenly, is a database that is unwell. Look at the database
   first (locks, load, a stuck session), because raising the bound does not mend a database that is slow. If the book
   has legitimately outgrown it, **the lever is `statement_timeout_ms` in the core's configuration** (milliseconds;
   default `60000`; zero is refused at start, and so is anything above `2147483647`, PostgreSQL's own limit). Raise it
   only with the care below, then restart the core. Whether that needs a release depends on how the configuration
   reaches the core: where the file is mounted into the image, restarting the core is enough; where the configuration
   is baked into the image, changing it means building a new image and rolling it out, and there is no change without
   that.

   **Raising it can make the health check restart the core.** The core writes its liveness beat once per pass, after
   the cycle and the reserve check, and the statement publish (up to its own 60 s budget) comes after the beat, so
   it sits between two beats. The reserve check evaluates the assets one after another, so each asset whose
   evaluation hits the bound adds the whole bound. The health check allows `3 x cycle_seconds + 120 s` between beats
   (`PASS_ALLOWANCE`), 135 s at the default `cycle_seconds = 5`. The time between beats in the worst case is:

   `N x statement_timeout + 60 s (statement publish) + the pass's other work`

   where `N` is the number of assets whose evaluation times out in the same pass. At the default of 60 s and one
   asset that is 60 + 60 = 120 s plus the other work, under 135 s with about 15 s to spare. Two assets timing out in
   one pass, with a statement due, is 180 s: over the limit already at the default. At `120000` one asset gives
   120 + 60 = 180 s, over 135 s, so the health check restarts the core in the middle of the pass, which does not end
   the problem (the restart starts the same statement again). Without a statement due the 60 s drops out, but
   `120000` with two assets is still 240 s.

   **No raised value is safe by itself** with the default `cycle_seconds`: the room above the default is 135 - 120 =
   15 s for one asset, and the other work of the pass uses it. To raise the bound, `cycle_seconds` has to rise with
   it, so that `3 x cycle_seconds + 120 s` is greater than `N x statement_timeout + 60 s + the other work`. For one
   asset at `120000` and about 15 s of other work that is 195 s, so `cycle_seconds` above 25: use 30
   (limit 210 s). The cost is that everything the cycle does (routes, deadlines, retries) then runs every 30 s instead
   of every 5 s; weigh that before choosing it over fixing the query or the database. The health logic is not
   changed by any of this; the limit simply moves with `cycle_seconds`. The other services have the same key
   (`statement_timeout_ms`; the ingress's default is `30000`), and it bounds a wait for a lock as well as a query.
   The pause is lifted as in step 6 once an evaluation holds.
4. A `book-quarantine` names the observation it set aside and the reason; correcting the book is a correction entry
   (`INV-17`), made by the reserve-book tooling, not an edit. Resolve the reason first, then follow F-004 NOTES 14.
5. For a surplus alert the fix is to fund the reserve from a declared operator funding address (an arrival there is
   surplus from chain data, never a deposit: `INV-1`, `REQ-IN-3`). Do it from the deployment's own notes, then
   `SELECT operator_record('surplus-contribution', '<asset, network>', '{}'::jsonb, '<why>');`.
6. When the next evaluation holds and the cause is fixed, lift the pause with two people:
   `SELECT operator_request_unpause(<pause id>, '<why>');` by one, `SELECT operator_approve_unpause(<request id>, '<why>');`
   by another. The core re-pauses if the equation still diverges (`INV-4`: a supply-mismatch pause is lifted only by an
   operator and a book correction does not lift it).

## Never

- Do not edit the book, a reserve reading, an evaluation or an observation to make the equation hold; there is no
  grant for it, and `INV-17` makes the book append-only (`REQ-GV-5`).
- Do not lift a pause because the next evaluation happened to hold once without knowing why it diverged.
- Do not mint, refund or pay out to restore a figure (`INV-6`, `INV-7`, `REQ-RF-1`).
- Do not count an operator top-up as a deposit's source (`INV-1`, `INV-2`, `REQ-IN-3`), and do not merge tokens of
  different issuers (`REQ-AS-3`).
- Do not treat "could not look" as "holds" or as "does not hold" (`REQ-NX-3`).

## Exercise

On the test bed: record an arrival the book cannot post, see `book-quarantine` and the pause; stop an endpoint for longer
than the bound and see `reserve-check-stalled`; lift each pause with two operators and see the work resume.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.
