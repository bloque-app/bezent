<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0006 — key custody: the property, with KMS for delivery keys

**Status:** ruled by @pandres95 on the decision form, 2026-10-13 (`pl-custody`: "KMS for delivery keys, Vault
otherwise"). Applied to `SPEC.md` v0.3.4.
**Target:** `SPEC.md` v0.3.3
**Date:** 2026-10-13
**Rationale in one line:** the requirement is what keys must never do; the mechanism differs by role. Keys that move
reserve or supply sign inside a key-management service or HSM; other keys may come from a secrets vault. This
matches aledgent's ruling that the property is the requirement and KMS is opt-in.

## A1

- `REQ-CU-1` becomes: "Key material MUST never be readable outside the component that signs with it. **Delivery
  keys** (those that mint, pay out from a reserve, or refund) MUST live in a key-management service or hardware
  security module that signs on request and never releases them. Other keys (pause, sweep fees, statements) MAY
  be held in a secrets vault and loaded only by the signer process (`REQ-AR-3`)."
- `REQ-KY-1` becomes: "Signing keys MUST be held as `REQ-CU-1` says. Key material MUST NOT be readable from a
  process environment, and MUST NOT appear in any configuration, log, URL or error."

## Identifier ledger

| Action | Identifiers |
|---|---|
| Revised | `REQ-CU-1`, `REQ-KY-1` |
