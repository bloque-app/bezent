<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-009 — EVM endpoint

Milestone `M-1` (Polygon); Base and BNB Chain in `M-2`. Serves `REQ-IN-*`, `REQ-OT-*`, `REQ-OB-*`, `REQ-NX-*`,
`REQ-RN-*`, `REQ-EO-1`, `REQ-EO-6`, `INV-10`, `INV-14`, `AC-A2.1`–`AC-A2.3`, `AC-C1.3`, `AC-C2.1`, `AC-D2.3` (plan
`DEC-3`, `DEC-8`, `DEC-9`, `DEC-10`, `DEC-12`). Read SPEC v0.5.1 §5.5, §5.6, §8.1–§8.3 and PLAN `DEC-8` to `DEC-12`.

## What it builds

`services/evm-endpoint`, an alloy service, one instance per network, on the merged store, signer, core and the
Safe vault of `F-008` (`contracts/evm`, its NOTES for addresses and the reference-pairing rule).

- **Observation:** `eth_getLogs` over contiguous ranges up to the `finalized` tag, through **two independent providers**
  cross-checked at the finalized block (`DEC-12`); a disagreement is unknown, never an arrival. Store block number,
  block hash, transaction hash and log index for every event; re-check the hash until final; gaps visible and filled.
- **Arrivals at the vault:** every final transfer of an admitted token into the vault is an observation keyed by
  coordinate (`INV-10`), reported with token, amount, sender and recipient; it waits unassociated until a deposit
  request names it (`REQ-IN-3`). Convertible arrivals (USDT0) are itemised, never reserve (`REQ-CV-4`), and Swaps'
  conversions under its role are observed as conversions with both amounts (`REQ-CV-2`).
- **Payouts:** the two-step delivery of `DEC-3`. For a core order, build the Safe `execTransactionWithRole` call of the
  delivery role — MultiSendCallOnly of (origin-token transfer to the beneficiary, `emitReference(ref)`) with `ref` the
  withdrawal's bridge-derived reference (PR #32's M1) — dry-run it (`eth_call` at the latest block), store the exact
  signed bytes, decode and compare every field against the order (target, token, recipient, amount, reference, nonce,
  gas limits, chain id) before asking for the signature. One nonce lane per delivery key, declared.
- **Payout legs:** report each outflow with token, sender (the vault) and recipient, and match a reference only to the
  vault's own transfer in the same transaction (self or zero transfers and bare emits are not payouts; `F-008` NOTES).
- **Non-execution proof:** the delivery key's nonce consumed at a finalized block by a transaction that is not ours (a
  transaction is ours if it is the same signed payload or carries the same reference to the vault), confirmed by both
  providers; an unidentified consumer is unknown, never proof (lesson of PR #31).
- **Network identity:** chain id and a pinned block hash checked at start-up against both providers; fail closed.

## Not now

Base and BNB Chain instances (`M-2`), the reserve book (`F-004`), the production RPC keys (`F-014`).

## Done when

Fork tests (anvil, the pinned Polygon block of `e2e/evm`) show: an arrival is recorded only after finality and once
(`AC-A2.1`–`AC-A2.3`, `INV-10`); a provider disagreement yields unknown; a gap is filled (`AC-D2.3`); a payout carries its
reference atomically and is reported with coordinates (`AC-C1.3`, `INV-14`); a forced non-execution produces a proof
checkable from chain data, and a re-signed or wrapped copy never does (`AC-C2.1`); the wrong chain fails closed.
