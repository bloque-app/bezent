<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Motivation

Informative companion to `SPEC.md`. Not part of the specification. Nothing here binds; where it
disagrees with `SPEC.md`, `SPEC.md` wins.

## What @pandres95 asked for (2026-09-30)

> A mirrored bridge that talks to Kreivo through a contract. An off-chain service reads our deposits from
> EVM hot wallets (evmhot), talks to Swaps, and keeps the balance of each asset per origin (Polygon, Base…).
> Those values are minted and burned on Kreivo (zombienet first, then mainnet). A deposit on Polygon is
> verified from a hot wallet, taken into a central treasury, and minted on Kreivo. Withdrawing burns on
> Kreivo and moves out on the chain the user names, netting between Polygon and Base through Binance, or
> an issuer such as Circle where possible, all of it through Swaps. bezent is an interface Swaps
> controls.

How the spec reads each part:

| The ask | In `SPEC.md` |
|---|---|
| "Mirrored", "minted and burned on Kreivo through a contract" | A trusted lock-and-mint bridge; the on-chain component (§7.F) |
| "Reads deposits from EVM hot wallets", "taken into a central treasury" | Deposit addresses, sweeps and the treasury (§5.2) |
| "Stores the balances of each asset per origin" | Reserve per (asset, origin network), and the reserve equation (§5.3) |
| "Burning and moving out on a specified chain" | Withdrawals: escrow, deliver, destroy at finality (§5.6) |
| "Netting between Polygon and Base through Binance or Circle" | Rebalancing through a custodial or protocol venue (§5.7) |
| "An interface controlled via Swaps" | Swaps is the requester: it decides what and where, never whether value arrived (§4.2, §4.3) |
| "zombienet first" | V0 proven on a test network before value (§15) |

### Rulings since (2026-10-01)

- **Rely on Swaps** (plan-level): what Swaps already does, bezent does not rebuild. The limits are in
  `notes/CONSTRAINTS-FOR-PLAN.md`, "Relying on Swaps".
- **Asset Hub is out.** XCM is already trustless and works; a custodian on that path would only add trust.

## Why now

- **aledgent** settles only on Kreivo and never talks to another network. Value from Polygon or Base reaches
  it only if something puts that value on Kreivo, with a deposit that carries an account reference
  (`notes/BRIDGE-REQUIREMENTS.md` §2).
- **The ramp** (Swaps rewrite) was ruled on 2026-09-29 to hold no minting rights. Movement between networks
  belongs to a bridge.
- **Today's path is long.** A USDC deposit on Polygon reaches Kreivo as DUSD through Brale, Asset Hub and a
  reserve transfer. A USDT deposit adds Binance, a conversion and Solana. **No path pays USDC out to a
  user's Polygon or Base address** (research 02). Each extra party is a leg whose outcome is assumed.
- **The incidents** in `notes/BRIDGE-REQUIREMENTS.md` §5 (failed-then-paid, unprovable non-execution,
  re-signing on silence, skipped blocks, rewritable clocks) came from that path and from the code that
  runs it.

## Why trusted, and why not the message-backed design

The Kreivo Bridge spec (2026-09-19, research 04) removes the custodian: escrows on each token's home chain,
messages over a provider and over the Polkadot⇄Kusama bridge, a pallet on Kreivo. It needs a provider per
EVM chain, relayers, a relay contract on Polkadot Hub and runtime work. Its first release, R0, is exactly a
trusted issuer: Bloque minting against reserves it holds and publishes.

This project builds R0 properly, as its own system, so that:

- value can flow now, through networks the operator can already hold tokens on;
- every guarantee the message-backed design gives *except* removing the custodian holds already: exactly
  once, provable non-execution, finality, published reserves, bounded keys;
- the bridged asset's identity is the one the runtime already reserves, so the later hand-over to R1/R2
  needs nothing from holders (`REQ-AS-6`).

## Lessons carried from what exists

- **evmhot** gives a working shape for addresses, monitoring and sweeps, and its webhook outbox (v2). Its
  finality rule, cursor, signing path and custody model do not meet the spec (research 01).
- **Swaps' bridge subsystem** shows the requester's side: lanes, batching, callbacks. Its pro-rata split of
  batch deliveries and its `fees_deducted` field are what the spec forbids (`REQ-EO-5`, `REQ-TR-5`).
- **substrate-rail** signs everything immortal, so non-execution was never provable; it re-signed on
  silence; its watcher skipped blocks. Each is now a requirement.
