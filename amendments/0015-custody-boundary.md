<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0015 — key custody: a boundary and its properties, no named store

**Status:** ruled by @pandres95 in chat, 2026-10-29 (`sp-custody-boundary`: a specification never names a technology;
phrase custody as aledgent does, with the signer apart from the core). Follows the ruling `d-delivery-keys`
(2026-10-29). Applied to `SPEC.md` v0.5.4.
**Target:** `SPEC.md` v0.5.3 (§7.(c), `REQ-CU-1`)
**Date:** 2026-10-29
**Rationale in one line:** amendment 0006 wrote technology classes into `REQ-CU-1` ("a key-management service or
hardware security module", "a secrets vault"), against guardrail 1. The requirement is a boundary and what may never
cross it; where a key rests is the plan's choice. aledgent's specification has the same shape (`NFR-8`, `REQ-AR-3` in
`bloque-app/aledgent`), and its move from a sidecar signer to a signing service needed no specification change.

## A1

`REQ-CU-1` becomes:

> Key material MUST never be readable outside the component that signs with it. That component MUST be separate from
> the component that holds bezent's state (`REQ-AR-3`), and MUST be auditable on its own.

The sentences on delivery keys living in "a key-management service or hardware security module that signs on request
and never releases them", and on other keys being held "in a secrets vault", are removed, not reworded. Delivery
keys and other keys meet the same requirement; the difference in where each rests is the plan's.

Unchanged, and together they bound every key, wherever it rests:

- `REQ-AR-3`: no key material in the component that holds bezent's state; the signer signs only a recorded intent and
  logs every signature.
- `REQ-KY-1`: no key in a process environment, configuration, log, URL or error.
- `REQ-KY-3`: every key that can move reserve or mint is bounded on chain.
- `REQ-CU-2`, `REQ-KY-4`: separate keys per network and role, rotatable without stranding transfers in flight.

## Consequence for the plan

`PLAN.md` (the custody section that cites `pl-custody` and amendment 0006, and `RISK-7`) names the store per key,
citing `d-delivery-keys`: the Kreivo delivery key in a signing service that never releases it; the Polygon delivery key
read from a secrets store into a sidecar signer in the same task as its one caller, later moved to a signing service. A
key that never leaves a signing service meets `REQ-CU-1` more strictly than one read into a signer. Decision form item
`q-plan-delivery-keys`.

## Identifier ledger

| Action | Identifiers |
|---|---|
| Revised | `REQ-CU-1` |
