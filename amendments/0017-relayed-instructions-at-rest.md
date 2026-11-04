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

## A1 — Recommended: add to `REQ-RQ-9`

> The bridge MUST NOT keep relayed instructions once they are checked. It keeps the commitment and that they matched.

Then, in the plan and the store, before real fiat flows, while the table holds no real data:

- `relayed_instructions.instructions` is dropped. The row keeps the transfer, the request and the commitment: its existence
  is the match.
- The core's second check before a hand-over (`F-011`, `hand_over`) reads the bytes today, against a compromised ingress
  writing a row. Without the bytes the core can only compare the row's commitment with the redemption's. A row for the
  right commitment and wrong bytes would then pass it, and the party, which holds the real bytes, would not pay on them.

## Trade-offs, stated plainly

- **Lost:** after the fact, nobody can show from bezent's store which bytes were relayed. The party can, and the commitment
  on chain binds them. A dispute over what a payout was told to do rests on the party's copy.
- **Lost:** the core's own recheck weakens as above. The attested trust model already rests on the party (`REQ-BE-5`).
- **Gained:** a breach of bezent's store discloses no bank data, and no request to delete personal data can run into an
  append-only table.

*Rejected: encrypting the bytes at rest.* It keeps a thing bezent does not need, and adds a key to custody for it.

## What `F-011` does meanwhile

Nothing: the table and the ingress are unchanged. The change is small, but it drops a column of a table in the deployed
schema and takes away a check, so it waits for the ruling. It must land before the first real instructions are relayed.

## Identifier ledger

| Action | Identifiers |
|---|---|
| Revised, if ruled | `REQ-RQ-9` |
