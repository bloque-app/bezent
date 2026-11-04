<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-009 — tasks

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-009-01 | Service skeleton (alloy); two providers; chain-id and pinned-block check | `DEC-8`, `DEC-12`, `AC-I4.1` | Wrong chain or disagreeing providers refused at start-up |
| T-009-02 | Finalized-range log follower, hash re-check, gap fill | `REQ-OB-*`, `AC-D2.3` | Gap and reorg tests pass |
| T-009-03 | Arrivals, convertibles and conversions at the vault as observations | `REQ-IN-*`, `REQ-CV-2`, `REQ-CV-4`, `INV-10` | `AC-A2.1`–`AC-A2.3` pass |
| T-009-04 | Payout preparation (two-step delivery), full decode-and-compare, nonce lane | `DEC-3`, `REQ-EO-1`, `INV-14` | No signature without a matching decode and a passing dry run |
| T-009-05 | Payout legs with token/sender/recipient; reference pairing | `AC-C1.3`, `INV-14`, #9 note F4 | Self, zero and bare-emit cases are not payouts |
| T-009-06 | Non-execution proof, cross-checked; unknown for unidentified consumers | `REQ-NX-*`, `AC-C2.1`, `INV-8` | Re-signed and wrapped copies never prove |
| T-009-07 | Base and BNB Chain instances: one configuration, key lane and signer binding each; the store holds each login to its own network | `DEC-8`, `REQ-OB-6`, `INV-8`, `REQ-AR-3`, `AC-I4.1` | Three example configurations share no key, lane, secret or network; the signer's allow list is exact per instance; a login records its own network only (migration 0056) |
| T-009-08 | bEURC and bBTC at the rounding boundaries: mint, remainder and payout scale exactly | `REQ-AS-4`, `REQ-AS-9`, `REQ-AS-10`, `INV-11` | bBTC (8 against 18, ratio 10^10): the mint is the floor, the remainder is the deposit's surplus, a payout is the redeemed amount times the ratio |
