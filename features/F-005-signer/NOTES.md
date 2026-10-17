<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-005 — notes

Decisions taken building the `M-0` stub (`T-005-01` to `T-005-06`), the security review's fixes (PR #21), and what
is left.

## Decisions

1. **The signature table backs the guards in the store.** `signatures` (migration 0012) holds one row per intent
   and one per on-chain reference; a trigger copies the reference and the amount from the recorded intent, so the
   signer cannot sign for a reference or amount the core did not record. The service role reads it and never writes
   it. The signer's guards check first; the store makes a race between two signers harmless.
2. **The intent payload the signer reads** (`intents.payload`, F-001 left it free): `message` (`0x` hex, the exact
   signing payload), `amount` (decimal string of minor units, `INV-11`), `from` (`0x` hex, for a destroy). The core
   (`F-003`) must write these. Key selection is by the intent's `network` and `purpose` through the configuration's
   bindings; the purpose that triggers the Kreivo guard is `destroy`.
3. **The link is a Unix socket with peer credentials and a token**, both ways checked by user id, as aledgent's
   `F-035` built it. No mutual TLS: the core and the signer share a host or a task, **but never a user id**
   (review S4, `REQ-CU-1`): `Callers::new` refuses a caller with the signer's own uid, so the binary does not start
   beside one. Only tests, which run both ends as one user, build the callers without that check.
4. **The window cap sums the key's signatures in the window**, across its purposes, from the store; signing is
   serialised by a process mutex and an advisory lock. The guard trusts the intent's declared amount: decoding the
   call to check that the bytes move that amount comes with the endpoints that build the transactions (`F-007`,
   `F-009`). See Open.
5. **The compromise path writes to the event log.** The signer may write nothing but signatures, so
   `compromise::check` runs in the core, as the service role: a signature that does not verify appends
   `key-compromise` (the alert record, with the key and the coordinate) and `pause-requested` (scope `global`) in one
   transaction. The pause itself, rotation and paging belong to the pause path and operations (`F-014`).
6. **A replay returns the recorded signature**; it never signs again. sr25519 signatures are randomised, so the
   restart test proves this by byte equality.
7. **Production cannot be configured in `M-0`.** The file backend refuses a key file not tagged `test`; a binding of a
   key to a network of another environment fails the start; an intent for such a network is refused at sign time.
8. **A row the signer wrote proves nothing** (review M1, `INV-9`, `AC-I1.2`). The signer is what the compromise
   path must catch, so a signature is bezent's only if it verifies, with the key's public key, over the exact
   `message` of an intent the core recorded. `compromise::check` uses the `signatures` row only to find the candidate
   intent (public key and `r ‖ s`, low-`s` normalised so a malleated secp256k1 signature is no false alarm, review
   N5) and then verifies. The core also verifies every signature the link returns (`Client::sign`, exact length,
   low-`s`, recovery byte) before it hands it on; one over other bytes raises `key-compromise` and the global pause
   request, and is never returned.
9. **Only the coordinator records an intent** (review S5, `INV-9`). Every service holds `bezent_service`, so that
   role lost INSERT on `intents`; the new `bezent_core` role, held only by the core's login, has it.
10. **The signer's start proves its login** (review S3, `REQ-AR-3`). `SET ROLE` is undone by any `RESET ROLE`, so
    `check_login` refuses a login that can become any role but `bezent_signer`, owns anything, has a role attribute
    (superuser, create role or database, replication, bypass row security), or holds a privilege beyond the signer
    role's allow-list. A `RESET ROLE` then gains nothing.
11. **No core dumps** (review S4, `REQ-CU-1`). The process sets `RLIMIT_CORE` to 0 (soft and hard) and, on Linux,
    `PR_SET_DUMPABLE 0`, before it reads a key. On Linux a failure stops the start; on macOS there is no
    `PR_SET_DUMPABLE` and a failure is only logged.

## Open — these block `M-1` (real keys)

- **The guards check what the intent declares, not the bytes** (review S1; issue #23). `message` is signed opaque;
  `amount`, `from` and `purpose` are the core's claims beside it. A wrong or compromised core can record
  `{purpose: "mint", amount: "0", message: <a destroy from a user account>}` and every second-line guard passes. Until
  the endpoints (`F-007`, `F-009`) decode the payload and the signer checks the decoded call against the declared
  fields, the guards must not be described as enforcing `DEC-3`. Also: the window cap sums a key's signatures across
  every network and asset it is bound to, adding minor units of different decimals; `REQ-KY-3` is per asset and
  window, so the sum must be keyed by (key, network, asset), the asset taken from the intent.
- **`AC-I4.2` rests on a configuration label** (review S2; issue #25). "Production" is whatever the configuration
  says about a network id: `{"id": "polygon", "environment": "test"}` lets a test key sign a chain-id-137
  transaction. The test-only backend needs a built-in deny-list of production chain ids and genesis hashes
  (Polygon 137, Base 8453, Ethereum 1, Kreivo's genesis) that no configuration can lift; once the payload is decoded,
  the chain id or genesis inside the bytes must match the declared network.
- **One signature per intent and per reference, forever, against retries** (review S6; issue #24). `REQ-EO-1`
  allows a second signature once the first is proven not executed, and `signed_transactions` already keys on
  (`intent_id`, `attempt`); `transfers.on_chain_reference` is write-once. With migration 0012 as it is, a delivery
  whose first attempt is proven not executed can never be signed again. Amendment 0009 (proposed) keys it by
  (reference, attempt); it is not ruled, so nothing is implemented. Decide with the 0009 ruling, before 0012 reaches a
  database that persists: then `attempt` on intents or signatures, both unique indexes on (…, attempt), the replay
  lookup by (`intent_id`, `attempt`), the cap counting a reference once across its attempts, and the `F-003` rule that
  attempt n+1 is signed only once attempt n's non-execution is recorded.
- Review notes not taken here: N1 (`SET search_path` on `signatures_from_intent`), N2 (the trigger should force
  `signed_at := now()`; the signer can backdate a row out of every cap window), N3 (socket directory owner and mode;
  `fchmod` or a umask before `bind`), N4 (refuse the SHA-256 of the empty token), N6 (the compromise path runs in the
  core and nothing calls it yet; `F-007`, `F-009`, `F-014`), N7 (who BLAKE2-hashes an sr25519 payload over 256 bytes;
  before `F-007`), N9 (the test fixture never closes the signer's pool). Also from S4: `mlock` for key pages, and
  refusing a key file that is group- or other-readable or owned by another uid.
- `M-1`: the AWS KMS backend for delivery keys and Vault for the rest (`DEC-11`), behind `KeyBackend`.
