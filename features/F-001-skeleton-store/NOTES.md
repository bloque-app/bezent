<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-001 — notes

The repository `bloque-app/bezent` was created on 2026-10-13 (`DEC-1`) and became public at `M-5` (`DEC-20`); its public history starts at the import of this tree.

## Review of PR #18: what the store now enforces, and what it leaves to later features

The review's must-fix findings (M1 to M3) and should-fix S1, S2, S3, S6, S7 are fixed in the schema, each with a
store test named after its spec id. The rest is recorded here, with what a later feature must do.

### The state graph (S2): edges §5.4 leaves open are refused

`transfer_edge_needs_coordinate()` in `0003_transfers.up.sql` admits only the edges §5.4 states. These edges are
plausible but not stated there, so the store refuses them until the spec states them (an amendment, then an additive
migration that replaces the function):

| Edge | Why it is open |
|---|---|
| `Received → Source-Final` | A withdrawal whose redemption is already final when it is taken in. `Accepted` means "not yet final", so it cannot pass through `Accepted` honestly either. Needed by the withdrawal feature. |
| `Received → Withdrawn`, `Received → Expired` | The observation stops matching, or the expiry passes, before the request is decided. |
| `Accepted → Needs-Review`, `Source-Final → Needs-Review`, `Refunding → Needs-Review` | §5.4 says only "a leg is unknown past its deadline"; which states have such a leg is not stated. |
| `Delivered → Delivering`, `Delivering → Delivering` as a new attempt | `REQ-RF-5` (§5.8) allows a new destination attempt after a proof of non-execution. A self-transition is admitted for conditions, but nothing marks it as a new attempt. See S5 below. |
| Every way out of `Needs-Review` | `REQ-RV-2` says a review ends by evidence "after which the ordinary steps run", without naming the state it returns to. |

The deposit and withdrawal features (F-002 onwards) meet these first; each needs a spec ruling before it relies on one.

### Units (S6)

Every amount column now states its unit in a column comment. The choice made here, open to the reviewer: a transfer's
amount and every book posting are in the asset's **book unit**, the minor unit of its origin token (the finer side,
`REQ-AS-4`), so a `REQ-AS-9` remainder is exact and every posting of one asset balances in one unit. Issuing-network
values (supply, mints, redemptions) are stored multiplied by the asset's declared ratio. An observation's amount is in
the observed token's own minor units, unscaled (`REQ-ST-3`). The service that converts must use the ratio declared with
the asset (`REQ-AS-2`), and the reserve-check feature (F-004) must compare supply in the same unit.

### S4 (first half): two final outcomes at one height with different hashes

Left open. The store now refuses an outcome decided below the observed block (`REQ-OB-2`,
`req_ob_2_outcome_is_decided_at_or_past_the_observed_block`). It still accepts two `final` outcomes for two
observations at the same `(network, block_number)` with different block hashes, which no chain can produce. A correct
refusal needs a lock per `(network, block_number)` (an advisory lock in the outcome trigger) so two concurrent
inserts cannot both pass; that is more than a trivial fix. **The observer feature (F-002/F-003) must** add that trigger
with its lock and a concurrent test, or record why the observer cannot produce the case.

### S5: milestones are one value per transfer; deliveries can have several attempts

Left open, and the meaning is fixed now so that no data has to change later: **a transfer's `destination_included`
and `destination_final` milestones are those of the attempt that completed it.** They are written by the
`Delivering → Delivered` and `Delivered → Completed` transitions, so a reorganisation before finality, or a second
attempt after a proof of non-execution (`REQ-RF-5`), cannot write a stale one: today the graph admits neither a way back
from `Delivered` nor a new attempt, and a reorganised inclusion sends the transfer to `Needs-Review`. **The feature that
implements `REQ-RF-5` and reorganisation handling (the delivery feature, with `DEC-5`'s six-block depth) must** record
inclusion and finality per signed transaction (on `signed_transactions` or a table beside it), derive the transfer's
milestones from the attempt that completed, and add the edges it needs only after the spec states them (see the table
above). `NFR-3` then counts from the completing attempt's times.

### S1: roles

`0009_roles.up.sql` creates `bezent_service` (no login, no ownership) and grants it SELECT and INSERT on the append-only
tables, UPDATE only on the write-once columns of `transfers`, and SELECT on the log head and the views. The state, the
condition and the milestones are written by `transitions_apply()` and the log head by `event_log_assign_seq()`, both
`SECURITY DEFINER` functions of the migration owner. Temporary objects are refused to `PUBLIC`, so the service cannot
build a trigger of its own that reaches `transfers` at trigger depth 2. The down migrations are no longer in
`crates/store/migrations/`; they live under `crates/store/tests/migrations/` beside symbolic links to the up
migrations, so no service binary embeds them. **Deployment must**: run the migrations as a login that owns the schema
and is not a member of `bezent_service`; give each service a login that is a member of `bezent_service` and of nothing
else. **The restore runbook must** re-run the grants after a restore taken with `--no-privileges` (as the drill does).

### Smaller notes from the review

- **N1** `2::float8` passes the `minor_units` check because it is integral. Harmless while the Rust side never binds
  floats (`INV-11` is also enforced by type).
- **N2** fixed: the guard no longer matches the latest transition by id; the state changes only inside the transition's
  trigger (depth 2), which the service role cannot imitate.
- **N3** fixed: a terminal transfer carries no condition (`inv_5_terminal_state_carries_no_condition`).
- **N4** `CND-Unassociated` is derived, not stored: a final observation with no transfer bound at its coordinate.
  `REQ-ST-6` needs the age of these arrivals; the observer feature must expose that view.
- **N5** Postings have no per-transfer reference. Itemisation of a grouped transaction (`REQ-BK-2`, `REQ-EO-5`) needs one;
  adding a per-posting reference is additive and belongs to the reserve-check feature (F-004).
- **N6** `signed_transactions` accepts attempt 2 before attempt 1 is proven not executed, and gaps in attempt numbers.
  The signer feature (F-005) must enforce `REQ-EO-1` there.
- **N7** `REQ-ST-7` "what it told the requester": no table records it yet; the notifications feature must add it.
- **N8** `Coordinate.block_number` is a `u64` but the column is a signed `BIGINT`; `BlockHash::new` accepts an empty
  hash the SQL refuses; `Amount` carries no asset. The first feature that maps core types to rows must make these
  conversions fallible.
- **N9** `assert_refused(…, "TRUNCATE transfers CASCADE", "")` matches any message; the SQLSTATE check still applies.
