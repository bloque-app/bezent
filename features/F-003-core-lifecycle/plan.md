<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-003 — Core lifecycle, conditions, review

Milestone `M-0` (state machine) and `M-1` (complete). Serves SPEC v0.5.0 §5.4 (with the transition table of
amendment 0009, `REQ-TR-*`, `REQ-TR-6`, `REQ-EO-6`), §5.5, §5.6, §5.8, §5.9, §10 (`CND-*`, incl. `CND-CutOff` of
amendment 0010), `REQ-RV-*`, `REQ-GV-5`, `INV-5`, `INV-6`, `INV-7`, `INV-16` (plan `DEC-3`, `DEC-4`).

## What it builds

`crates/core` (logic) and a `services/core` binary (new; the process that decides, `DEC-3`), on the merged store,
ingress, signer and the Kreivo endpoint's observations.

- **Store graph to the spec:** an additive migration that makes the store's allowed edges exactly the §5.4 table of
  amendment 0009 — the review exits (`Needs-Review` → one ordinary edge from the state held before review, evidence
  coordinate required, second approver recorded when value moves), the review entries from `Accepted` and
  `Source-Final`, and numbered new attempts (`Delivering`/`Delivered` → `Delivering`). Per-attempt milestones; one
  signature per (reference, attempt) and attempt *n*+1 only after attempt *n*'s recorded proof of non-execution
  (`REQ-EO-6`, issue #24).
- **The state machine:** consumes observations (arrivals, redemptions, inclusions, finality, non-execution proofs)
  and requests, and moves transfers along the table only. Deposits: a request naming a final arrival's coordinate →
  `Source-Final` (coordinate consumed once, `INV-2`) → mint intent (`INV-6`: no mint before the arrival is final).
  Withdrawals: a final redemption observation (keyed by redeemer and reference, amendment 0008) → payout intent
  (`INV-7`: no delivery before the redemption is final). Refund mint only on a proven non-execution at finality
  (`REQ-RF-*`, `INV-8`, never on a timer).
- **Intents for the signer:** written with the fields the signer reads (`message`, `amount`, `from`, purpose), as
  `features/F-005-signer/NOTES.md` describes; the core verifies every returned signature (`AC-I1.2`).
- **Conditions and deadlines:** every `CND-*` of §10 held in its state and cleared by itself or by evidence;
  `CND-CutOff` holds a transfer past its route's cut-off until it reopens; deadlines send a transfer to
  `Needs-Review` only where the table allows.
- **Review:** owner and one-business-day service level (`REQ-RV-1`), resolution by evidence only (`REQ-RV-2`), second
  approver for value-moving resolutions, every manual action in the append-only record with actor and justification
  (`REQ-GV-5`).

## Not now

The reserve book and check (`F-004`), EVM payouts (`F-009`), webhooks (`F-002` follow-up), the operator UI (`F-014`).

## Done when

Every edge of the table has a test, and every refused edge too (`REQ-TR-6`); deposits `AC-A2.1`–`AC-A2.3`, `AC-A4.1`,
`AC-A4.2`, withdrawals `AC-B1.1`–`AC-B1.4`, `AC-B3.1`, `AC-B3.2`, dispositions `AC-C1.1`, `AC-C1.2` pass; `INV-6`,
`INV-7`, `INV-16` are checked in the service; a new attempt keeps its reference and is refused before the previous
attempt's proof (`REQ-EO-6`).
