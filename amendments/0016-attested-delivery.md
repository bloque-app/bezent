<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0016 — an attested backend's delivery, and its report surface

**Status:** proposed by the `F-011` helper, 2026-11-03. **Not ruled and not applied:** nothing here reaches `SPEC.md` or
the code until @pandres95 rules on it.
**Target:** `SPEC.md` v0.5.4 (§5.4, §7.A)
**Date:** 2026-11-03
**Rationale in one line:** §5.4 defines delivery by a transaction bezent signs, and an attested backend's payout is one
bezent never signs, so a fiat withdrawal has no normative way past `Source-Final`; and §7.A lists no request by which an
attesting party reports.

## The gap

`Delivering` means "a destination transaction is signed and recorded", and `REQ-TR-6` confines a transfer to the table's
edges. For an attested backend the requester relays the instructions (`REQ-RQ-9`) and pays on its rail; `REQ-BE-5` makes
its report of the payout the observation. `F-011` therefore holds such a withdrawal at `Source-Final` with its payout
posted to the book. It never completes, `REQ-OT-6`'s refund mint has no way to start, and `REQ-BE-1`'s "evidence of
non-execution" has no carrier.

## A1 — Recommended: read the §5.4 words per assurance class

- `Delivering` becomes "a destination transaction is signed and recorded, or, on an attested backend, the relayed
  instructions are accepted (`REQ-RQ-9`)". The `Source-Final → Delivering` row reads the same.
- `REQ-BE-5` gains: "that requester's report of an arrival, a payout, **a payout's non-execution** or a reserve balance
  **is** the observation."
- `REQ-NX-1` gains "on a public backend", so the attested proof of non-execution is the party's report. `REQ-NX-2` is
  already public-only through `REQ-BE-4`.

`Delivered`, `Completed`, `Refunding` and `Refunded` then need no change: their words rest on a leg, which a report is.

*Trade-off.* A wrong report of non-execution can cause a refund mint beside a payout that happened. That is the same
trust `REQ-BE-5` already places in the party for a mint, and the mint window bounds it.

*Rejected: a terminal state of its own for fiat.* It forks every status reader and the sequenced log for one backend.

## A2 — Recommended: add the report to §7.A

**`REQ-RQ-10`** **Report, for an attested backend:** the party's report of an arrival, a payout, a reserve balance, a
reversal or a restoration, under its own identifier, authenticated as that party's (`REQ-BE-4`, `REQ-BE-5`). Only the
requester holding that backend's attesting grant may report. A report that names a transfer or an earlier report is
refused when that does not exist or does not fit (`ERR-SourceNotFound`, `ERR-SourceMismatch`, `ERR-SourceConsumed`).

The surface exists in `F-011` at plan level (`DEC-14`). This records it where a reader of §7.A looks for it.

## A3 — A question, not yet text: a reversal of an arrival not minted yet

`REQ-BE-6` speaks of what was minted ("nothing already minted is clawed back"). It does not say what happens to an arrival
reversed before its mint: `F-011` keeps the arrival owed and mintable, charged against surplus, because nothing specifies
otherwise. Recommended ruling: refuse to bind it (`ERR-SourceConsumed`) and hold an unminted deposit on it, since minting
against a payment already known reversed spends surplus for no reason.

## Identifier ledger

| Action | Identifiers |
|---|---|
| Revised, if ruled | `REQ-BE-5`, `REQ-NX-1`, the `Delivering` state and the `Source-Final → Delivering` edge of §5.4 |
| Added, if ruled | `REQ-RQ-10` |
