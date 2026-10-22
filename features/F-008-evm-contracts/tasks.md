<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-008 — tasks

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-008-01 | Foundry project, pinned deps (Safe 1.4.1, Zodiac Roles v2, forge-std), CI workflow | `DEC-10` | `forge build` and an empty test pass in CI |
| T-008-02 | ReferenceEmitter | `REQ-RN-5` | Unit test |
| T-008-03 | Vault deployment script and per-network config | `REQ-RN-4`, `REQ-KY-5` | Script deploys on a fork; owners and threshold as configured |
| T-008-04 | Delivery role: MultiSend(transfer + emit) within an allowance | `AC-I1.1`, `INV-14`, `REQ-KY-3` | Fork tests: bounded, referenced, nothing else allowed |
| T-008-05 | Conversion role: convertible → origin token, recipient vault, windowed | `REQ-CV-7` | Fork tests on Polygon (Uniswap); Base has none (T-008-08) |
| T-008-06 | Negative suite: role escalation, module addition, delegatecall, other tokens | `REQ-KY-4`, `REQ-GV-3` | All refused |
| T-008-07 | Safe guard and Zodiac Delay on the vault; cooldown and expiration as parameters; verifier checks | `DEC-10`, `REQ-GV-3`, `REQ-GV-5`, `REQ-KY-3` | Fork tests: a direct owner transaction is refused; a queued change runs only after the cooldown and before expiry; the delivery role pays without delay; removing the guard is delayed and the Delay cannot be removed; the guardian alone vetoes a queued change and pauses, and cannot move value, widen or change its scope; the owners cannot remove it at once |
| T-008-08 | Drop the Base conversion role, its config and tests | `DEC-13` | Base deploys with the delivery role only; the Polygon conversion suite is unchanged |
