<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-004 — tasks

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-004-01 | Postings for every movement the core records; balanced, append-only, corrections as entries | `REQ-BK-7`, `INV-17`, `REQ-AS-9` | One test per movement kind; bBTC remainder to surplus |
| T-004-02 | Reserve-reading interface (Kreivo supply, vault balances, Swaps' attested reports); fakes for tests | `REQ-BK-8`, `REQ-BE-4` | Interface used by the check |
| T-004-03 | The one-minute check, zero tolerance, pause and page on divergence, itemised terms | `REQ-BK-1`, `REQ-BK-2`, `INV-1`, `NFR-5`, `REQ-IC-2` | `AC-E1.1`, `AC-E1.2` pass |
| T-004-04 | Surplus floor alert; reversal handling and mint pause under the floor | `REQ-BK-3`, `REQ-BK-10`, `REQ-BE-6` | Tests pass |
| T-004-05 | Daily signed statement, append-only store interface | `REQ-PR-1`, `REQ-PR-2`, `REQ-BE-3`, `REQ-BK-9`, `REQ-CV-4` | `AC-E2.1`, `AC-E2.2` pass; signature verifies |
