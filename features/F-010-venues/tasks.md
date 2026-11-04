<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-010 — tasks

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-010-01 | Find what exists; record it | `REQ-CV-*`, `DEC-13` | `NOTES.md` lists each requirement, where it is built and where it is tested |
| T-010-02 | A convertible arrival is never minted against | `REQ-CV-1`, `AC-A5.1`, `INV-6` | Store test: no transfer names it, and it stays listed. Seam test (real ingress, then the core): a request naming it is refused; cycles mint nothing |
| T-010-03 | The core mints exactly the output the swap delivered | `REQ-CV-2`, `REQ-CV-5`, `AC-A5.2`, `INV-6` | Seam test (real ingress, then the core): not final, nothing; final, the order is the output's amount, not the input's, and the input's leg is refused; the store refuses a larger claim |
| T-010-04 | The statement itemises unconverted holdings outside the equation | `REQ-CV-4`, `AC-A5.3` | Test on the signed statement |
| T-010-05 | A real swap is observed with both amounts | `REQ-CV-2`, `REQ-CV-7`, `REQ-VN-3` | Fork test on Polygon: Swaps' member converts through the role; input and output recorded at their coordinates |
| T-010-06 | The record of the convertible arrival with both amounts; `CND-AwaitingConversion` held by an arrival | `REQ-CV-2`, `REQ-CV-1`, `REQ-CV-6`, `AC-A5.1` | **Blocked on amendment 0018** (proposed, not ruled) |
