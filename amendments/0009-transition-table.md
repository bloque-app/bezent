<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0009 — the transfer state graph, made normative

**Status:** ruled by @pandres95 on the decision form, 2026-10-13 (`sp-transitions`). Applied to `SPEC.md` v0.5.0.
**Target:** `SPEC.md` v0.4.0 (§5.4)
**Date:** 2026-10-13
**Rationale in one line:** §5.4 names the states but not which may follow which. The store (`F-001`, merged) enforces
a graph and refuses every edge the spec does not make clear, which leaves `Needs-Review` with no way out and no
second delivery attempt. The lifecycle (`F-003`) needs the graph settled first.

## A1 — Transitions (new table in §5.4, normative)

| From | To | When |
|---|---|---|
| `Received` | `Refused` | refused at request with a named error |
| `Received` | `Accepted` | within route rules; the source leg is not yet final |
| `Accepted` | `Source-Final` | the source leg is final and consumed for it (`INV-2`) |
| `Accepted` | `Expired` | the source leg never arrived before the request's expiry |
| `Accepted` | `Withdrawn` | the source observation no longer matches the chain, before finality (`REQ-IN-7`) |
| `Accepted` | `Needs-Review` | the source leg is observed but neither final nor withdrawn past its deadline (`REQ-RV-1`) |
| `Source-Final` | `Delivering` | a destination transaction is signed and recorded |
| `Source-Final` | `Needs-Review` | a condition that blocks delivery (`CND-AwaitingConversion`, `CND-DestinationRefused`) is past its deadline (`REQ-CV-6`, `REQ-IN-9`) |
| `Delivering` | `Delivered` | the destination leg is included |
| `Delivering`, `Delivered` | `Delivering` | a **new attempt** after the previous one is proven not executed at finality (`REQ-RF-5`); each attempt is numbered and recorded |
| `Delivering`, `Delivered` | `Refunding` | the destination leg is proven not executed at finality, and a refund transfer exists |
| `Delivering`, `Delivered` | `Needs-Review` | a leg unknown past its deadline; a partial or mismatched execution (`REQ-TR-3`, `REQ-TR-5`) |
| `Delivered` | `Completed` | the destination leg is final and executed in full (`REQ-TR-3`) |
| `Refunding` | `Refunded` | its refund transfer is `Completed` |
| `Needs-Review` | any state reachable by **one ordinary edge** from the state held before review | the evidence is that edge's coordinate, verified by bezent (`REQ-RV-2`); a resolution that moves value has a second approver (`REQ-GV-5`) |
| any non-terminal | itself | it takes or clears a named condition (§10) |

**Attempts and references.** A new attempt carries the same on-chain reference as the attempt it replaces: a
reference is consumed only by a leg that executed (`INV-2`), and the previous attempt is proven not executed before
the next is signed. The signer may therefore sign one transaction per (reference, attempt), and attempt *n*+1 only
once the proof of non-execution of attempt *n* is recorded (found in the review of `F-005`, PR #21).

Not edges, on purpose: `Received` straight to `Source-Final` (a deposit bound to a final arrival is created at
`Source-Final`, `REQ-IN-3`); `Refunding` to `Needs-Review` (the refund transfer has its own lifecycle and review).

## Identifier ledger

| Action | Identifiers |
|---|---|
| Added | `REQ-TR-6` ("A transfer MUST change state only along the edges of the §5.4 transition table."), `REQ-EO-6` ("A new attempt MUST carry the same on-chain reference, and MUST NOT be signed before the previous attempt is proven not executed at finality.") |
| Revised | §5.4 (table added) |
