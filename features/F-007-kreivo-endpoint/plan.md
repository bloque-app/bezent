<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-007 — Kreivo endpoint

Milestone `M-1`. Serves `REQ-OB-*`, `REQ-NX-*`, `REQ-EO-1`, `REQ-EO-4`, `REQ-HN-3`, `AC-C2.1`, `AC-D2.3`, `AC-I4.1`
(plan `DEC-3`, `DEC-5`). Read SPEC v0.4.0 §4.4(a), §8.1, §8.2, §8.3, and PLAN `DEC-5`, `DEC-6`.

## What it builds

`services/kreivo-endpoint`, on the merged store, signer and contract:

- **Light client:** subxt with its embedded smoldot, using aledgent's patched smoldot (child-trie-root fix,
  aledgent#121; see `aledgent/services/settle` and its Cargo pins). Kreivo's RPC is
  never trusted for an outcome. Chain specs from a pinned file; the network is identified by genesis hash, and a
  mismatch with the configured network fails closed at start-up (`AC-I4.1`).
- **Observation:** follow the finalized head; read the contract's events (`Minted`, `Redeemed`, `RefundMinted`, pause
  and bounds events) in contiguous block ranges, store each as an observation keyed by coordinate (block number, hash,
  event index) with the event's sequence number; a gap in the contract's event sequence or in block ranges is visible
  and filled before anything after it is decided (`AC-D2.3`, `REQ-OB-3`). Redemptions become observations the core
  turns into withdrawals (redeemer, reference, backend, payout or commitment).
- **Submission:** for a recorded intent (mint, refund mint), build the extrinsic with a mortal era born at the
  finalized head (`max_era` 64), dry-run it through the light client, record the exact bytes, get the signature from
  the signer over the link, verify it (the core's check, `AC-I1.2`), persist the signed bytes (`REQ-EO-1`), submit.
  One nonce lane per delivery key, declared (`REQ-EO-4`).
- **Non-execution proof:** an extrinsic is proven not executed when its era has expired at a finalized block and
  its nonce was consumed by a different transaction or not at all; the proof is the coordinates, checkable from
  chain data (`AC-C2.1`, `REQ-NX-1`, `REQ-NX-2`).

## Not now

Moving transfers through their states (the core, `F-003`); the end-to-end run on zombienet (`T-013-05`, after PR #19).
Tests here use a mocked chain interface plus, where cheap, a recorded fixture of real Kreivo blocks.

## Done when

Tests named after `AC-I4.1`, `AC-D2.3`, `AC-C2.1`, `REQ-EO-1`, `REQ-EO-4` pass; nothing is signed without a passing
dry run; the endpoint decides nothing (it writes observations and signed bytes, never a transfer state).
