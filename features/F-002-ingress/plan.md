<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-002 — Ingress and requester API (M-0 part)

Milestone `M-0` (webhooks by `M-1`). Serves `REQ-RQ-1`, `REQ-RQ-3`, `REQ-RQ-4`, `REQ-RQ-6`, `REQ-RQ-7`, `REQ-RQ-8`,
`REQ-RQ-9`, `REQ-ID-1`, `REQ-ID-3`, `REQ-ID-4`, `REQ-PN-1`, `REQ-PN-2`, `REQ-ST-4`, `REQ-LM-*`, `NFR-1` (plan
`DEC-3`, `DEC-15`). Read SPEC v0.4.0 §3, §7.A, §7.B, §7.C, §8.3, §8.4 and the `AC-D1.*`, `AC-G1.*`, `AC-H1.*` criteria.

## What it builds now

`services/ingress`: an HTTP + JSON service on the `F-001` store (`crates/store`, the `bezent_service` role).

- **Authentication and grants:** scoped service credentials per requester (`REQ-PN-1`, `REQ-PN-2`): grants per kind
  (requesting deposits, withdrawals' extra data, refunds of unassociated arrivals, attesting). Credentials are
  hashed at rest; nothing secret in logs or errors.
- **Requests** (`REQ-RQ-*`): deposit (coordinate of an arrival, destination, refund destination), withdrawal extras
  by redemption reference (refund destination, expiry, relayed attested instructions checked against the
  commitment, `REQ-RQ-9`), refund of an unassociated arrival. Each is written durably before the answer
  (`REQ-RQ-1`); the answer carries the bridge reference and the state. Idempotency keys on (requester, reference)
  (`REQ-ID-1`, plan `DEC-15`): same body → original status, different body → `ERR-ReferenceConflict`. No request may
  carry a claimed outcome (`REQ-RQ-7`): coordinates are pointers, verified later by the core.
- **Limits and backpressure** (`REQ-LM-*`, §8.4): route table with min/max and capacity; refusals with the named
  `ERR-*`; `ERR-Overloaded` rather than accept what the store cannot hold (`REQ-LM-4`).
- **Queries** (`REQ-RQ-8`): status by request reference, bridge reference and coordinate; the route table; reserve
  per (asset, network) and the latest reserve-equation evaluation as stored (empty until `F-004`); the sequenced log
  from a position (`REQ-ST-4`), gapless.
- An OpenAPI document generated from the code, committed under `services/ingress/openapi.json`.

## Not now

Webhooks (`REQ-ST-5`, `REQ-IC-5`; `M-1`), moving transfers past `Received`/`Refused`/`Accepted` (the core, `F-003`),
verifying coordinates against chains (endpoints), the operator surface (`F-014`).

## Done when

`AC-D1.1`, `AC-D1.2` (retries and conflicts), `AC-H1.1`–`AC-H1.3` (status and log reads) and `AC-G1.1`–`AC-G1.3`
(limits) pass as integration tests against Postgres, each named after its criterion; a request carrying an outcome
field is refused (`REQ-RQ-7`); one requester cannot read or collide with another's references (`REQ-ID-4`); intake
latency is measured in a benchmark test and reported against `NFR-1` (reported, not gated, on the laptop).
