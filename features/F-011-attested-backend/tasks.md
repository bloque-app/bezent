<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-011 — tasks

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-011-01 | Plan, tasks, notes; the attesting contract in `F-012`; the proposed amendment for what the spec leaves open | `DEC-14`, `RISK-8` | Committed before the code |
| T-011-02 | Store: attesting parties and keys, the append-only reports, the record function, no order on an attested network | `REQ-BE-4`, `REQ-BE-5`, `REQ-PN-2`, `REQ-KY-3` | `grant_matrix` and store tests pass; the down migration reverts it |
| T-011-03 | Ingress: `POST /v0/reports`: signature, party, per-kind rules, idempotency; `not-final` for a coordinate not recorded yet; extras scoped to the backend's party | `REQ-RQ-1`, `REQ-RQ-4`, `REQ-ID-1`, `REQ-BE-5`, `REQ-OB-7` | Tests name each refusal |
| T-011-04 | Core: re-verify and record reports; a refused report pages and holds its network's queue | `REQ-BE-4`, `REQ-BE-5` | Test: a forged row never becomes an observation |
| T-011-05 | Core: withdrawals from attested redemptions, by commitment; nothing delivered, no order | `REQ-RQ-9`, `REQ-OT-1`, `REQ-OT-2`, `REQ-OT-3` | `review_findings` S2 kept; a new test with a live redemption's shape |
| T-011-06 | Book: payout, reversal and restoration from observations, resting on their block | `REQ-BE-6`, `REQ-BK-4`, `REQ-BK-7` | Book balances; a reversal below the floor pauses mints |
| T-011-07 | Check: `Recorded::attested`, the attested point, roll-forward, staleness; owed terms on attested networks | `REQ-BK-1`, `REQ-BK-8`, `REQ-BE-4` | Zero divergence through a full cycle; a wrong snapshot diverges and pauses |
| T-011-08 | Statement: the reserve is attested, and the party by name | `REQ-BE-3`, `REQ-BE-4`, `REQ-BK-9` | Statement test |
| T-011-09 | End to end through the ingress router and the core cycle | `REQ-BE-6`, `REQ-RQ-9`, `RISK-8` | One test runs arrival, deposit, mint, reversal, held mint, restoration, release |
