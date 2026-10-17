<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-006 — tasks

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-006-01 | `contracts/bezent/` workspace pinned like aledgent's settlement contract; `kreivo-apis` at the #512 merge | `DEC-6` | `cargo contract build --release` is green |
| T-006-02 | Storage: roles, per-asset bounds and pauses, used references, redemptions, window accounting, event sequence | §7.F, `REQ-HN-9` | Distinct-roles refusal tested |
| T-006-03 | `mint` with every refusal | `INV-2`, `INV-3`, `AC-D2.2` | One test per refusal |
| T-006-04 | `redeem`: pull, check own balance, burn own holding; payout fits backend; reference once | `REQ-HN-8`, `REQ-RQ-9`, `INV-4`, `AC-B5.1`, `AC-B5.2` | Tests show the burn is from the contract's account and a reused reference is refused |
| T-006-05 | `refund_mint` once per redemption, inside the window | `INV-8`, `REQ-OT-6`, `REQ-RF-4` | A second refund is refused |
| T-006-06 | Pause scopes; unpause by governance only | `AC-I2.1`, `AC-I2.2` | Each scope stops its direction; the pause role cannot unpause |
| T-006-07 | Bounds: lowering immediate, raising after the delay | `REQ-GV-3`, `AC-I2.2` | A raise applied early is refused |
| T-006-08 | Ordered events with a sequence; `.github/workflows/contracts.yml` | `REQ-HN-7` | CI green on the PR |
