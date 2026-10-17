<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-012 — Swaps integration in the Swaps platform

Milestone `M-1` (first module); attested reports in `M-3`. Serves SPEC v0.5.2 §3 and §7.A–§7.C from the requester's
side: `REQ-RQ-1`, `REQ-RQ-3`, `REQ-RQ-4`, `REQ-RQ-6`, `REQ-RQ-7`, `REQ-RQ-8`, `REQ-RQ-9`, `REQ-ID-1`, `REQ-ST-4`,
`REQ-ST-5`, `REQ-TR-5` (plan `DEC-15`, `DEC-19`). Go-ahead from the owner on 2026-10-15; the PR stays a **draft** until
bezent's core is ready.

## What it builds (in the Swaps platform, under that repo's own rules: its CLAUDE.md, API documentation guide, review)

- A `bezent` instruction module in Swaps (`server/services/swap`), following how Swaps' graph pauses on an external
  step: it pauses with a callback token and hands the callback URL over only in `afterPersist()`; bezent's
  notification resolves it; the step joins Swaps' stall-alarm list.
- Requests: deposit (naming the arrival's coordinate at the reserve), redemption extras (expiry; relayed instructions
  for attested backends, never personal data on chain), refund of an unassociated arrival. Request references are
  idempotent per Swaps; outcomes are never asserted (`REQ-RQ-7`): Swaps points at coordinates, bezent verifies.
- The sequenced log as the safety net: Swaps reads bezent's log from its last position when a callback is lost.
- Two habits of Swaps' current bridge subsystem do not carry over: pro-rata splitting of a batch delivery, and
  `fees_deducted` (bezent deducts nothing, `REQ-TR-5`).
- Config: bezent's base URL and Swaps' requester credential from Swaps' existing secret source; nothing hard-coded.
- CI (owner's ruling `pl-pr-e2e`): a job like `swap_aledgent` runs `ghcr.io/bloque-app/bezent-sidecar` pinned by
  digest next to Postgres and drives the module against it;
  until the sidecar's test-only chain-event driver lands (bezent side), the job covers intake, idempotency, status and
  the log, and is marked so. The full-chain pre-release job comes later.

## Not now

Attested reports (`F-011`, `M-3`); the full-chain job; client-facing API changes (none intended: the module is
internal to Swaps).

## Done when

The module's unit tests cover pause and resume, lost-callback recovery from the log, idempotent retries and refusals;
the sidecar job is green; the draft PR is open in the Swaps platform repository, reviewed by a bezent reviewer, and left
as a draft until the owner says the core is ready.
