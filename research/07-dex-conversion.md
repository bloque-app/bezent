<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# 07 — Converting USDT ⇄ USDC on chain, and USDB on Kreivo

Evidence, not doctrine. Read on chain on 2026-10-03 through public RPCs (publicnode) and Bloque's Kreivo RPC.
Scripts: `research/07-scripts/` (`onchain.py`, `quotes.py`: Python + curl; `here.mjs`: PAPI).

## Tokens

| Network | Token | Address | Symbol | Supply | Provenance |
|---|---|---|---|---|---|
| Polygon | USDC | `0x3c499c54…3359` | USDC | 624M | Circle, native |
| Polygon | USDT | `0xc2132D05…8e8F` | **USDT0** | 789M | Tether's omnichain token (LayerZero OFT); Polygon's USDT was upgraded to it |
| Base | USDC | `0x833589fC…2913` | USDC | 4 372M | Circle, native |
| Base | USDT | `0xfde4C96c…9bb2` | USDT ("Tether USD") | 27M | **Bridged from Ethereum** through Base's standard bridge: `l1Token()` = `0xdAC17F95…1ec7` (Ethereum USDT). Not issued by Tether on Base |

## Pools (USDC/USDT)

Uniswap v3 fee tiers: 100 = 0.01%, 500 = 0.05%, 3000 = 0.30%.

| Network | Pool | USDC | USDT |
|---|---|---|---|
| Polygon | Uniswap v3 0.01% `0x31083a78…81b7` | 796 855 | 646 896 |
| Polygon | Uniswap v3 0.05% | 3 469 | 8 660 |
| Base | Uniswap v3 0.01% `0xd56da2b7…5da1` | 72 451 | 99 379 |
| Base | Uniswap v3 0.05% | 5 677 | 5 810 |
| Base | **Aerodrome Slipstream**, tick spacing 1 `0xa41bc0af…fcd1` | 555 655 | 524 856 |

## Quotes (Uniswap QuoterV2, `quoteExactInputSingle`, USDT → USDC, 0.01% pool)

| Amount in | Polygon out (cost) | Base out (cost) |
|---|---|---|
| 1 | 0.9997 (0.027%) | 0.9992 (0.081%) |
| 100 | 99.9726 (0.027%) | 99.9188 (0.081%) |
| 1 000 | 999.7261 (0.027%) | 999.1834 (0.082%) |
| 10 000 | 9 997.2391 (0.028%) | 9 991.3216 (0.087%) |
| 100 000 | 99 970.3914 (0.030%) | 68 642.5834 (**31.4%**: pool exhausted) |

Gas per swap: Polygon ~132k gas at 273 gwei ≈ 0.036 POL; Base ~73k gas at 0.006 gwei ≈ 0.0000004 ETH (plus the
L1 data fee). Aerodrome was not quoted; route Base conversions through it or through an aggregator.

## Kreivo local assets (mainnet, spec 133)

| Id | Symbol | Decimals | Sufficient | Supply |
|---|---|---|---|---|
| `Here(100)` | COPM | 2 | yes | 95 147 535 |
| `Here(101)` | MXNB | 2 | no | 0 |
| `Here(102)` | BRLB | 2 | no | 0 |
| `Here(103)` | — | — | — | does not exist yet (@pandres95's choice for USDB) |

`BridgeBackedAssets` (`runtime/kreivo/src/xcm_config.rs:349-353`) lists only `Sibling{1000,50,1337|1984}`;
`XcmReserveTransferFilter = NotBridgeBacked` (`:415`) therefore allows reserve transfers of `Here(103)`.
