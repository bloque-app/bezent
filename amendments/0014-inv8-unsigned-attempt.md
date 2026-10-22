<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0014 — an attempt that was never signed counts as proven for INV-8

**Status:** ruled by @pandres95 on the decision form, 2026-10-22 (`sp-inv8-unsigned`). Applied to `SPEC.md` v0.5.3.
**Target:** `SPEC.md` v0.5.2 (§12, `INV-8`)
**Date:** 2026-10-22
**Rationale in one line:** `INV-8` allows a refund mint only when every payout attempt is proven not executed at
finality. An attempt that was ordered and never signed has no transaction, so there is nothing to prove, and the
refund was closed for good. Building the store's checks (`F-003`, issue #25, PR #43) found it.

## A1

`INV-8` gains a sentence: "An attempt for which no signature has been recorded, and which the store has closed to
signing under the approved resolution that refunds, counts as proven." Its layer column becomes Store + service +
chain, because the closing is a rule of the store (the guard of migration 0051 in PR #43).

In practice, the core, running the operator's approved resolution into `Refunding`, cancels the transfer's next
attempt when it was ordered and never signed. After that cancellation the attempt counts as proven not executed for
`INV-8`, and the refund can open on the proof of the last attempt that was signed. The store closes an attempt only when it is the transfer's next attempt and the attempt before it is proven. A signed
attempt is never cancelled and keeps the full proof. The store can only know that no signature has been recorded, and
the sentence says so. `REQ-RF-1` (no refund on a timer or on unknown), `REQ-RV-2` (a review ends only
by evidence) and `REQ-GV-5` (a resolution that moves value has a second approver, and no administrative path edits a
status) are unchanged: the cancellation exists only under the approved resolution and is recorded, never edited.

## A2

What the store cannot observe is a signature that was produced and not recorded. `INV-9` therefore gains a sentence, a
requirement on the signer: "The signer releases a signature only once it is recorded." Together with `REQ-AR-3` (the
signer signs only a transaction that matches a recorded intent, and logs every signature), this makes "no signature
recorded" mean "no signature exists outside the signer", so an attempt closed to signing can never be executed. The
specification had no requirement on when a signature is released; this is the new normative sentence, and it states
what the signer already does. A crash between signing and recording leaves nothing that can be submitted.

## Why not the other way

Nothing can ever execute an attempt that has no signature, so closing it to signing makes "not executed" true by
construction. Treating an unsigned attempt as unproven leaves the refund shut with no way to open it.

## Identifier ledger

| Action | Identifiers |
|---|---|
| Revised | `INV-8`, `INV-9` |
