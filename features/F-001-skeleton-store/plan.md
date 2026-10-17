<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-001 — Repository, workspace and store

Milestone `M-0`. Serves `INV-5`, `INV-10`, `INV-11`, `INV-17`, `REQ-TR-2`, `NFR-7` (plan `DEC-1`, `DEC-4`).

## What it builds

- The Rust workspace of `DEC-1`: `crates/core`, `services/{ingress,kreivo-endpoint,evm-endpoint,signer}` (each a
  binary that starts, reads its config, and exits cleanly; no behaviour yet), `tools/`. Pinned toolchain, `rustfmt`,
  `clippy -D warnings`. SPDX headers from the first file: `GPL-3.0-or-later` for `services/` and `crates/core`,
  `Apache-2.0 OR MIT` for any reusable crate (`DEC-20`).
- `crates/core` types only: exact integer amounts (a `u128` newtype with checked arithmetic and no float
  conversions, `INV-11`); asset ids; network ids; the three reference kinds as distinct newtypes that never convert
  into each other (`REQ-ID-3`); coordinates (network, block number, block hash, transaction or log index).
- The PostgreSQL schema of `DEC-4`, as `sqlx` migrations under `crates/core/migrations/` (or a `crates/store`
  crate if that reads better): intake, transfers with their state and write-once milestone columns, state
  transitions, observations, the reserve book (double entry, per asset), signer intents with their recorded bytes,
  and the sequenced event log (gapless sequence).
- **Not** in this feature: deposit-address tables (plan `DEC-9` waits on `pl-scope`), any service behaviour,
  webhooks, the reserve check.

## Store rules (enforced by triggers, not application code)

| Table | Rule | Spec |
|---|---|---|
| observations | no UPDATE, no DELETE; unique per (network, coordinate) so one observation is applied once | `INV-10` |
| reserve book | no UPDATE, no DELETE; every posting balances per asset | `INV-17`, `REQ-BK-7` |
| state transitions, event log | no UPDATE, no DELETE; the log sequence is gapless | `REQ-ST-*` |
| transfers | a milestone column, once set, never changes; a transfer in a terminal state never changes | `REQ-TR-2`, `INV-5` |
| amounts | integer columns only (`NUMERIC(39,0)` or two `BIGINT`s; never floating point) | `INV-11` |

Read SPEC §5.3, §5.4, §8.3, §9 for the exact meanings; the spec wins over this table.

## Done when

Each refusal above has a test at the store (a SQL statement that the trigger refuses), named after the `INV-*` it
verifies. A restore drill test dumps a populated database, restores it into a fresh one, and shows every derived
view (transfer state, book balances per asset, log head) is identical (`NFR-7`). CI runs build, fmt, clippy, unit
tests and the store tests against a Postgres service container on every PR.
