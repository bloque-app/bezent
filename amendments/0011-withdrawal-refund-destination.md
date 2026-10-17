<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0011 — a withdrawal's refund always goes to the redeemer

**Status:** ruled by @pandres95 in chat, 2026-10-13 (`sp-refund-dest`: "Yes"). Applied to `SPEC.md` v0.5.1.
**Target:** `SPEC.md` v0.5.0 (§5.8, §7.A)
**Date:** 2026-10-13
**Rationale in one line:** the spec contradicts itself. `REQ-RQ-4` lets a requester add a refund destination to a
redemption, but `REQ-RF-4` says a withdrawal's refund is a refund mint to the redeemer, and the contract can only mint
it there. Building the lifecycle (`F-003`, PR #32) found that the extra can never be honoured.

## A1

- `REQ-RQ-4`: the extras a requester may add to a redemption become "an expiry, and for an attested backend the
  relayed instructions (`REQ-RQ-9`)". The refund destination is removed from the list.
- `REQ-RF-2` becomes: "The refund destination of a **deposit** MUST be given in its request. bezent MUST NOT choose it
  afterwards. A withdrawal's refund destination is always its redeemer (`REQ-RF-4`)."

## Why not the other way

Letting a requester redirect a withdrawal's refund would let it move value that the redeemer destroyed to an account
the redeemer never named; the redeemer is the only party the issuing network proves.

## Identifier ledger

| Action | Identifiers |
|---|---|
| Revised | `REQ-RQ-4`, `REQ-RF-2` |
