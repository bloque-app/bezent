<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# 05 — Origin networks and venues

Evidence, not doctrine. Public documentation, read 2026-09-30. The plan measures before relying on any
number here.

## Finality

| Network | Signal | Typical time to `finalized` | Source |
|---|---|---|---|
| Polygon PoS | `finalized` block tag (milestones) | seconds; Circle counts 2–3 blocks, ~8 s | [Circle: finality and block confirmations](https://developers.circle.com/cctp/concepts/finality-and-block-confirmations) |
| Base | `finalized` block tag: batch in a finalised Ethereum block | ~15–20 min (Base estimates ~20; Circle ~15–19) | [Base: transaction finality](https://docs.base.org/base-chain/network-information/transaction-finality), [Circle](https://developers.circle.com/cctp/concepts/finality-and-block-confirmations) |

On OP Stack chains, "safe" (batch included on L1, ~2–10 min) is not final. Only `finalized` satisfies
`REQ-OB-1`. ([OP Stack: transaction finality](https://docs.optimism.io/op-stack/transactions/transaction-finality))

## Tokens

| Token | Network | Address | Issuer | Decimals |
|---|---|---|---|---|
| USDC | Polygon PoS | `0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359` | Circle (native) | 6 |
| USDC.e | Polygon PoS | `0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174` | Polygon PoS bridge (not Circle) | 6 |
| USDC | Base | `0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913` | Circle (native) | 6 |

USDC.e must not back the same bridged asset as native USDC (`REQ-AS-3`).

## CCTP (protocol venue)

- CCTP V2 covers Polygon PoS and Base, among others. Burn on the source (`depositForBurn`), Circle attests,
  mint on the destination (`receiveMessage`). ([Circle: finality thresholds and fees](https://developers.circle.com/cctp/cctp-finality-and-fees))
- **Standard transfer** waits for source finality: ~8 s from Polygon, ~15–19 min from Base.
- **Fast transfer** (~8–20 s) issues the attestation before source finality, for a fee; Circle carries the
  risk.

## Binance (custodial venue)

See research 02. Deposits and withdrawals are on chain; the interval between them is not. Withdrawals carry
no reference the bridge can read; the requester must supply the withdrawal's coordinate.
