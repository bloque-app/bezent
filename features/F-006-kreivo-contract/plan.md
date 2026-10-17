<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-006 — Kreivo contract

Milestone `M-0`. Serves SPEC §7.F (`CTR-HOME`), `REQ-HN-*`, `REQ-RQ-9`, `REQ-OT-6`, `REQ-RF-4`, `REQ-GV-2`,
`REQ-GV-3`, `INV-2`, `INV-3`, `INV-4`, `INV-8` (plan `DEC-6`, `DEC-7`).

## What it builds

One ink! contract in `contracts/bezent/`, a Cargo workspace of its own excluded from the root one. It follows
aledgent's settlement contract (`aledgent/contracts/settlement/`: ink! 5.1.1,
cargo-contract 5.0.3, `kreivo-apis` bindings, its `contracts.yml` workflow) and uses kreivo#512's `assets__mint` and
`assets__burn` (merge `87f8728`, Kreivo 0.17.1, `apis/src/contract/`, clone at
`a checkout of virto-network/kreivo`, remote `gh`).

## Messages

As built after review (PR #20; details and reasons in NOTES.md).

| Message | Caller | Refuses when | Event |
|---|---|---|---|
| `new(roles, raise_delay, assets)` | instantiation | roles not distinct or zero or the contract itself; delay 0 | — |
| `mint(asset, to, amount, reference)` | delivery role | mint reference used before (own namespace); above per-transfer max, the sliding mint window or the supply ceiling; asset or mint direction paused; unknown asset; zero | `Minted` |
| `redeem(asset, amount, backend, payout, reference)` | anyone | asset or redeem direction paused; below the asset's minimum; backend not declared for the asset, or payout not fitting it (20-byte non-zero EVM address; 32-byte commitment); (redeemer, reference) used before | `Redeemed` |
| `refund_mint(redeemer, reference)` | delivery role | no such redemption; already refunded; sliding window or ceiling exceeded. Not refused by a pause (`REQ-GV-1`) | `RefundMinted` |
| `pause(scope)` / `unpause(scope)` | pause role or governance / governance only | — | `Paused`, `Unpaused` |
| `lower_bounds(asset, bounds)` | pause role or governance | any field a raise; cancels a scheduled raise | `BoundsSet` |
| `schedule_raise(asset, config)` / `apply_raise(asset)` | governance / anyone after the delay | early; also adds an asset (with backends and initial supply) | `RaiseScheduled`, `BoundsSet` |
| `schedule_raise_delay(d)` / `apply_raise_delay()` | governance / anyone after the current delay | `d` = 0 | — |
| `set_roles(roles)` | governance | zero, duplicate or self accounts; cannot change governance | `RolesSet` |
| `schedule_governance(acct)` / `accept_governance()` | governance / the new account, after the delay | early; wrong caller | — |
| reads: `roles`, `scheduled_governance`, `bounds`, `backends`, `scheduled_raise`, `raise_delay`, `paused`, `redemption(redeemer, reference)`, `mint_reference_used`, `minted_in_window`, `supply`, `event_seq` | anyone | — | — |

Redemptions are keyed by (redeemer, reference), mint references sit in their own set (`REQ-ID-4`, `INV-2`). The mint
window slides over buckets and never lets any span of window length exceed the bound (`INV-3`). A redeemer may approve
the contract first; redeem then pulls with `transfer_approved`, so a whole balance can be redeemed.

**Redeem order matters** (`REQ-HN-8`, constraints note): pull `amount` from the caller into the contract through the
assets API, check the contract's own balance rose by `amount`, then `assets__burn` from the contract's account only.
Never burn from the caller: Admin can burn from anyone, and `call_runtime` reports only success or failure.

Every event carries the reference and a contract-wide sequence number that increases by one per event (`REQ-HN-7`).
A redemption record keeps redeemer, asset, amount, backend, payout and refunded flag; `refund_mint` mints to the
redeemer, once (`INV-8`, `REQ-OT-6`). The proof that a payout was not executed is the core's job; the contract only
enforces once.

## Not in this feature

ContractsStore deployment, the dedicated community and genesis assets (`F-015`, `F-013`); the zombienet end-to-end
run (`F-013`, after this lands); fee or dust logic (the core's, `REQ-AS-9`).

## Done when

Unit tests with a mocked chain extension cover every refusal in the table and `AC-B5.1`, `AC-B5.2`, `AC-D2.2`,
`AC-I2.1`, `AC-I2.2`, each test named after the criterion or invariant (`INV-2`, `INV-3`, `INV-4`, `INV-8`).
`cargo contract build --release` produces the `.contract` artefact in CI.
