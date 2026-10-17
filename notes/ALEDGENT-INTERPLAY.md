<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# bezent and aledgent — how they meet (from the aledgent session, 2026-10-15)

Source: a first take from the Aledgent session, asked at @pandres95's request (`pl-pr-e2e` note, 2026-10-15).
Informative; the spec and plan bind.

## Operations

- aledgent holds bound chain assets in its settlement contract on Kreivo. A ledger account is credited only when a
  deposit call into that contract, carrying an account reference, reaches finality.
- **Swaps owns the hop.** bezent mints a bridged asset to Swaps' own Kreivo account; Swaps then calls aledgent's
  contract `deposit` with the account reference. Back: aledgent withdraws to Swaps' address; Swaps redeems at bezent.
  This is what bezent's spec already says: delivering into a ledger is the requester's, from its own account
  (`REQ-IN-5`).
- aledgent needs nothing from bezent directly; it observes only its own contract. Swaps correlates bezent's mint and
  redemption events with aledgent's references.
- bezent must never assume:
  1. that minting or transferring straight to aledgent's contract account credits anyone — value arriving there
     outside the contract's calls is a position difference, an incident on aledgent's side;
  2. that aledgent accepts an asset before it is bound there (a reviewed change on aledgent's side);
  3. that on-chain decimals may change (aledgent's adapter refuses to serve on a mismatch) — bezent's are fixed:
     bUSDC 6, bEURC 6, bBTC 8;
  4. that aledgent's withdrawals are instant: batched, then confirmed at inclusion and finality (tens of seconds).
- The contract account must be able to hold each asset (sufficient asset, or existential deposit funded) — a point
  for the asset referendum (`F-015`).

## Pre-release tests

- Worth doing: bezent mints bUSDC → Swaps deposits into aledgent → aledgent withdraws → bezent redeems. Only a shared
  network tests it.
- Network definition stays upstream in aledgent's public `e2e/network`; bezent's harness becomes an optional layer
  on top (its contract and assets at `Here(101–105)`), rebased rather than diverging.
- The combined scenario job belongs to the consumer: the Swaps platform's pre-release job.
- aledgent brings: its release images by digest (public GHCR: ingress, core, settle, signer), its settlement
  contract deployed by its scripts in order, the "aledgent" driver (ingress, core, adapter over the light client,
  Postgres), and asset bindings for `Here(101–105)`.
- Caveat: aledgent's adapter refuses a runtime whose transaction surface differs from mainnet's unless configured,
  so the zombienet Kreivo must be a mainnet release (bezent's is 0.17.1, the mainnet release) or the job maps it.
