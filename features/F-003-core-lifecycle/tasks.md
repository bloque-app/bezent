<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-003 — tasks

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-003-01 | Migration: store graph = §5.4 table (amendment 0009); per-attempt milestones; (reference, attempt) signatures with the proof gate | `REQ-TR-6`, `REQ-EO-6`, #24 | One store test per allowed and refused edge |
| T-003-02 | `services/core` skeleton; observation and request consumers; idempotent processing | `DEC-3`, `REQ-AR-1`, `REQ-AR-2` | Restart reprocesses nothing twice |
| T-003-03 | Deposit path to the mint intent | `INV-2`, `INV-6`, `AC-A2.*`, `AC-A4.*` | ACs pass |
| T-003-04 | Withdrawal path to the payout intent; refund mint on proven non-execution | `INV-7`, `INV-8`, `REQ-RF-*`, `AC-B1.*`, `AC-B3.*` | ACs pass |
| T-003-05 | Conditions, deadlines, `CND-CutOff` | §10, amendment 0010 A2 | One test per condition |
| T-003-06 | Review: owner, service level, evidence-only resolution, second approver, append-only record | `REQ-RV-*`, `REQ-GV-5`, `AC-C1.*` | ACs pass |
| T-003-07 | Signer intents and verification of returned signatures | `INV-9`, `AC-I1.2` | A bad signature raises the alert |
