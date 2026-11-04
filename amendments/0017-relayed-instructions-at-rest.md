<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0017 — relayed instructions are checked and not kept

**Status:** proposed by the `F-011` helper, 2026-11-03. **Not ruled and not applied:** neither `SPEC.md` nor the table has
changed.
**Target:** `SPEC.md` v0.5.4 (`REQ-RQ-9`, §1.3)
**Date:** 2026-11-03
**Rationale in one line:** relayed payout instructions hold bank data, bezent needs them only to check a hash, and they sit
in an append-only table where they can never be removed.

## The gap

§1.3 says of a requester fact that "bezent does not hold them or need them". `REQ-RQ-9` says only that the bridge accepts
relayed instructions when they hash to the commitment. The ingress has kept the bytes since `F-002`
(`relayed_instructions.instructions`, append-only), and bezent never pays from them: the attesting party pays, from its own
copy. `REQ-PV-1` keeps them off every network, but the store is not a network, so nothing forbids them there.

**Where the bytes and their kin sit today.** The column is one of four places, and dropping it reaches only the first:

1. `relayed_instructions.instructions`, append-only.
2. `intake.body`, append-only. `record()` stores the canonical JSON of the whole request, and an extras request carries
   the instructions in it as hexadecimal. It is also what a repeat is compared with (`REQ-ID-1`), so it cannot be dropped
   without a digest-only comparison.
3. The destination of the refund of an unassociated arrival on an attested network. It is the payer's payout details, kept
   in clear in `transfers.destination` (and `intake.body` again), copied into the payout report's observation as its
   `recipient`, and returned to requesters in the transfer's status.
4. The same destination again wherever a status or the log echoes it.

A withdrawal's own destination is only its commitment, which is not personal data.

## A1 — Recommended: add to `REQ-RQ-9`

> The bridge MUST NOT keep relayed instructions once they are checked. It keeps the commitment and that they matched.

Then, in the plan and the store, before real fiat flows, while the tables hold no real data:

- `relayed_instructions.instructions` is dropped. The row keeps the transfer, the request and the commitment: its existence
  is the match.
- The extras request is recorded in `intake` without the bytes: the digest of the body already identifies a repeat, so the
  body keeps the commitment in their place.
- The destination of the refund of an unassociated arrival on an attested network is a commitment or a reference to the
  party's own record, as a withdrawal's is, and not payout details. If the party needs them to pay, it holds them. This one
  is a larger change than the column, and is not drafted here; it needs its own ruling on what such a refund names.
- The core's second check before a hand-over (`F-011`, `hand_over`) reads the bytes today, against a compromised ingress
  writing a row. Without the bytes the core can only compare the row's commitment with the redemption's. A row for the
  right commitment and wrong bytes would then pass it, and the party, which holds the real bytes, would not pay on them.
- The ingress's `instructed` check (`reports.rs`) and the store's `bezent_attested_hand_over_open` read only that the row
  exists, and keep working.

## Trade-offs, stated plainly

- **Lost:** after the fact, nobody can show from bezent's store which bytes were relayed. The party can, and the commitment
  on chain binds them. A dispute over what a payout was told to do rests on the party's copy.
- **Lost:** the core's own recheck weakens as above. The attested trust model already rests on the party (`REQ-BE-5`).
- **Gained:** a breach of bezent's store discloses no bank data, and no request to delete personal data can run into an
  append-only table.

*Rejected: encrypting the bytes at rest.* It keeps a thing bezent does not need, and adds a key to custody for it.

## What `F-011` does meanwhile

It leaves the table and the ingress's writing of the bytes as they are, and it adds readers of them. The PR (#66) makes
the core read `relayed_instructions.instructions` in `hand_over` to hash them against the commitment again, and the
ingress and the store ask whether a row exists (`instructed` in `reports.rs`; `bezent_attested_hand_over_open`). So the
change this amendment proposes is no longer a column nobody reads: it removes the core's recheck, and that is a change to
code this PR adds. The drop itself waits for the ruling, because it takes a column out of a table in the deployed schema
and a check out of the core. It must land before the first real instructions are relayed, and the payout details of an
unassociated refund (item 3 above) must be settled before the first one on an attested network.

## Identifier ledger

| Action | Identifiers |
|---|---|
| Revised, if ruled | `REQ-RQ-9` |
