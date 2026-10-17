<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0004 — a bridged asset with less precision than its reserve token

**Status:** ruled by the owner on the decision form, 2026-10-13 (`br-bbtc-decimals`: 8 decimals with a dust rule;
`br-dust-rule`: the remainder goes to surplus). Applied to `SPEC.md` v0.3.3.
**Target:** `SPEC.md` v0.3.2
**Date:** 2026-10-13
**Rationale in one line:** bBTC keeps bitcoin's 8 decimals while its reserve, BTCB on BNB Chain, has 18, so every
deposit can carry a remainder smaller than one bBTC unit. The rule says where that remainder goes.

## A1 — Scaling and the remainder

- `REQ-AS-4` becomes: "An origin token MUST have the same precision as its bridged asset, or more. Amounts are
  exact integer minor units everywhere. Where the origin token has more precision, the ratio is a fixed power of
  ten, declared with the asset (`REQ-AS-2`)."
- Add **`REQ-AS-9`**. A deposit of an origin token with more precision mints the arrival divided by the ratio,
  rounded down. The remainder, smaller than one minor unit of the bridged asset, is recorded in the reserve book
  as **surplus**, with the deposit's reference (`REQ-BK-4`, `REQ-BK-7`). It is never minted, dropped or returned.
- Add **`REQ-AS-10`**. A payout of a bridged asset with less precision pays exactly the redeemed amount times the
  ratio. A payout never creates a remainder.
- `REQ-LM-1`: a route's minimum is at least one minor unit of the bridged asset, expressed in the origin token.
- `DEF-7` is **withdrawn**: precision scaling is now in scope.

## Identifier ledger

| Action | Identifiers |
|---|---|
| New | `REQ-AS-9`, `REQ-AS-10` |
| Revised | `REQ-AS-4`, `REQ-LM-1` |
| Withdrawn | `DEF-7` |

## What this costs, stated plainly

A depositor of BTCB loses at most 10^10 BTCB units (one hundred-millionth of a bitcoin, one satoshi) per deposit
to surplus. That is Bloque's to keep or return as a policy outside bezent.
