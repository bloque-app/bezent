<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-004 — Reserve book, reserve check, statement

Milestone `M-1`. Serves `REQ-BK-1`–`REQ-BK-10`, `REQ-PR-1`, `REQ-PR-2`, `REQ-BE-3`, `REQ-BE-6`, `REQ-CV-4`,
`REQ-IC-2`, `REQ-AS-9`, `INV-1`, `INV-17`, `NFR-5`, `AC-E1.1`, `AC-E1.2`, `AC-E2.1`, `AC-E2.2` (plan `DEC-4`, `DEC-16`,
`DEC-17`). Read SPEC v0.5.1 §5.3 (the reserve equation, the book, surplus and its floor), §4.6, §8.7, and PLAN
`DEC-16`.

## What it builds

In `crates/core` and `services/core`, on the merged store (the append-only reserve-book tables of `F-001`), the core
lifecycle (`F-003`) and the Kreivo endpoint (`F-007`); the EVM endpoint (`F-009`) is in progress, so its reserve
readings come through an interface with a fake in tests.

- **The book:** double-entry postings per asset for every movement the core records (arrival, mint, redemption,
  payout, refund, conversion, network cost against surplus, surplus contribution or withdrawal, a reversal under
  `REQ-BE-6`), balanced per asset, append-only, corrections as new entries (`INV-17`, `REQ-BK-7`). The bBTC remainder
  lands in surplus and payouts scale exactly (`REQ-AS-9`).
- **The check, every minute per asset:** re-derive every term of `Σ Reserve + InTransit = Supply + OwedMint +
  OwedPayout + Surplus` from the networks at final points (reserve balances from the endpoints, supply from Kreivo,
  attested terms from Swaps' reports), compare with the book at zero tolerance (`REQ-BK-1`, `REQ-BK-8`, `INV-1`,
  `NFR-5`). Any difference pauses the asset's routes and raises a page (`REQ-IC-2`); surplus below its declared floor
  alerts (`REQ-BK-10`) and, past the floor after a reversal, pauses mints (`REQ-BE-6`). Itemised InTransit, OwedMint and
  OwedPayout (`REQ-BK-2`); convertible holdings itemised outside the equation (`REQ-CV-4`).
- **The daily statement:** a JSON document per asset, signed with a dedicated ed25519 key used for nothing else
  (behind the signer's key trait, a file key in tests), naming each asset's backend and assurance class, the
  published addresses, the itemised terms, unconverted holdings, which terms rest on reports, and that it is a signed
  claim, not a proof of solvency (`REQ-PR-1`, `REQ-BE-3`, `REQ-BK-9`). Written to an append-only store behind an
  interface; the object-locked bucket of `DEC-16` is a deployment concern (`F-014`), a local directory in tests.

## Not now

The bucket and its object lock (`F-014`), the EVM endpoint's real reserve readings (`F-009`), mainnet keys.

## Done when

`AC-E1.1`, `AC-E1.2`, `AC-E2.1`, `AC-E2.2` pass, each test named after its id; `INV-1` is checked every minute in a
time-controlled test; a seeded divergence pauses and pages; a bBTC remainder lands in surplus; a reversal below the
floor pauses mints; the statement verifies with its public key and lists every required field.
