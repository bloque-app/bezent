<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-005 — Signer and guards (M-0 stub)

Milestone `M-0` stub; complete by `M-1`. Serves `REQ-AR-3`, `REQ-KY-*`, `REQ-CU-1`, `REQ-EO-1`, `INV-9`, `AC-D2.1`,
`AC-I1.2`, `AC-I4.2` (plan `DEC-3`, `DEC-11`). Read SPEC v0.4.0 §4.4(c), §8.3, §8.6, §8.7 and amendment 0006.

## What it builds now

`services/signer`: a separate process that holds **no transfer state** (`REQ-AR-3`) and signs only intents the core
recorded (`INV-9`).

- **Link:** the core sends an intent id; the signer reads the recorded intent and its exact bytes through a
  read-only store role of its own (a new `bezent_signer` role with SELECT on intents only), signs, and writes the
  signature back only through an append-only insert. The channel between core and signer is authenticated
  (mutual auth on a local socket or mTLS); see aledgent's `F-035` (signer service, authenticated core link) for a
  shape that already passed review: `aledgent/features/F-035-*`.
- **Keys (stub):** test keys only, loaded from a file path given at start (never env vars, `REQ-KY-1`), with an
  explicit `network = test` tag; a test key refuses any intent for a production network id (`AC-I4.2`). The KMS
  backend of `DEC-11` is a trait with this file backend as the only M-0 implementation.
- **Guards (second line, `DEC-3`):** refuse an intent not recorded; refuse a one-time reference already signed;
  refuse an amount above the per-window cap configured for the key; for Kreivo, refuse a destroy from any account but
  the contract's. Every refusal is logged with the intent id.
- **Exactly once:** the signer stores nothing; a restart re-signs the same recorded bytes deterministically or
  returns the recorded signature (`AC-D2.1`). A signature observed on chain that matches no recorded intent starts
  the compromise path: an alert record and a pause request (`AC-I1.2`).
- Kreivo signatures (sr25519 and ECDSA accounts) and EVM signatures (secp256k1) behind one interface.

## Not now

The AWS KMS backend and Vault loading (`M-1`), production key ceremonies (`F-014`), the endpoints that submit.

## Done when

Tests named after `INV-9`, `AC-D2.1`, `AC-I1.2`, `AC-I4.2` pass; the signer process has no write access to any table
but its own signature table (a store test proves it); no key material appears in any log, error or config dump (a
test scans captured logs for the key bytes).
