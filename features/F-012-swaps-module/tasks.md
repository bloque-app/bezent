<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-012 — tasks

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-012-01 | Read Swaps' graph step model, an existing external step (e.g. the aledgent path), the callback-URL-after-persist rule | `DEC-15` | Notes in NOTES.md |
| T-012-02 | `bezent` client: auth, requests, status, log reads; typed errors mapped to Swaps' own vocabulary | `REQ-RQ-*`, `REQ-ST-4` | Unit tests |
| T-012-03 | The graph step: pause with callback token, callback URL only after persist, resume on bezent's notification, log recovery | `REQ-ST-5` | Unit tests incl. lost callback |
| T-012-04 | Stall alarm; no pro-rata splitting; no fees_deducted | `REQ-TR-5` | Tests |
| T-012-05 | CI job with bezent-sidecar pinned by digest | `pl-pr-e2e` | Job green |
