<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0008 — a redemption's reference is scoped to its redeemer

**Status:** ruled by @pandres95 on the decision form, 2026-10-13 (`sp-redemption-ref`). Applied to `SPEC.md` v0.5.0.
**Target:** `SPEC.md` v0.4.0
**Date:** 2026-10-13
**Rationale in one line:** the contract's review found that if redemptions and mints share one set of references,
anyone can pre-empt a deposit's mint by redeeming the minimum with its reference (the mint reference is derivable
from public data), so a mint is blocked forever (`REQ-ID-4`). The fix keeps mint references in their own set and keys
each redemption by (redeemer, reference). The spec's wording still reads as one global reference per redemption.

## A1

- `REQ-OT-2` becomes: "A withdrawal MUST be bound to its redemption by the redemption's on-chain reference: the
  redeemer's account together with the reference it chose. It MUST NOT be matched by amount or time; the redeemer is
  part of the identifier, not a matching rule."
- `AC-B5.2` becomes: "Given the same redeemer and reference again, Then it is refused (`INV-2`)."
- `REQ-ID-2` gains: "References of different kinds (a mint's, a redemption's) are separate namespaces on the issuing
  network; one never consumes another."

## Identifier ledger

| Action | Identifiers |
|---|---|
| Revised | `REQ-OT-2`, `REQ-ID-2`, `AC-B5.2` |
