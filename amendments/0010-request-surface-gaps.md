<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0010 — three gaps in the request surface

**Status:** ruled by @pandres95 on the decision form, 2026-10-13 (`sp-request-gaps`). Applied to `SPEC.md` v0.5.0.
**Target:** `SPEC.md` v0.4.0 (§7.A, §8.4, §10)
**Date:** 2026-10-13
**Rationale in one line:** building the ingress (`F-002`, PR #22) found three places where the spec names a request
or a limit but not who may make it or what the refusal is called.

## A1 — Who may add a redemption's extras (`REQ-RQ-4`)

A redemption is made by anyone on the issuing network; the extras (refund destination, expiry, relayed
instructions) come from a requester. `REQ-RQ-4` gains: "Extras are written once. For an attested backend only the
requester holding that backend's attesting grant may add them. For a public backend, any requester holding the
withdrawal grant may, and the first accepted wins; a later different body is refused (`ERR-ReferenceConflict`)."

## A2 — Cut-offs hold, never refuse (`REQ-LM-1`)

`REQ-LM-1` declares cut-offs but says nothing of what happens past one. Refusing would make no sense for a deposit
whose arrival is already in the reserve. New condition **`CND-CutOff`**: "A transfer whose next step falls outside its
route's declared cut-off holds `CND-CutOff` until the route reopens, then resumes by itself." No request is refused
for a cut-off.

## A3 — Two named refusals (§10)

- **`ERR-Malformed`**: "The request does not parse, or lacks a field its kind requires."
- **`ERR-CommitmentMismatch`**: "Relayed instructions do not hash to the redemption's commitment (`REQ-RQ-9`)."

## Not a gap

Refunding an unassociated arrival of a token that is not admitted (`REQ-RQ-6`, `REQ-IN-6`) needs no asset: the
request names a coordinate and a destination. The ingress must accept it; that is a fix in PR #22, not an amendment.

## Identifier ledger

| Action | Identifiers |
|---|---|
| Revised | `REQ-RQ-4`, `REQ-LM-1` |
| Added | `CND-CutOff`, `ERR-Malformed`, `ERR-CommitmentMismatch` |
