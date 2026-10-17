<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0003 — the reserve book

**Status:** ruled by the owner on 2026-10-13, in the bezent session's chat ("Then, we rule for a book"). Applied to
`SPEC.md` v0.3.2.
**Target:** `SPEC.md` v0.3.1
**Date:** 2026-10-13
**Rationale in one line:** bezent keeps its own, non-provable book of what backs each asset. The spec states the
book's guarantees; the plan chooses its store (a store inside the core, `gen-reserve-book`).

## A1 — The book's guarantees

- Add **`REQ-BK-7`**: a reserve book per bridged asset, double-entry and append-only, covering reserve positions,
  value in transit, owed mints, owed payouts, surplus and the reported supply.
- Add **`REQ-BK-8`**: the book is working state, never the authority. Each reserve check re-derives every term from
  the backends and the issuing network at final points; any difference from the book is a divergence.
- Add **`REQ-BK-9`**: the book need not be provable to third parties. The statement says so, and says per asset
  which terms are publicly recomputable and which rest on an attesting party.
- Add **`INV-17`** (store): the reserve book is append-only.

## Identifier ledger

| Action | Identifiers |
|---|---|
| New | `REQ-BK-7`, `REQ-BK-8`, `REQ-BK-9`, `INV-17` |

## Carried to the plan

The store: a double-entry, append-only store inside the core (`gen-reserve-book`). No separate aledgent
deployment. Revisit if reserves must be disclosed with proofs to auditors or regulators.
