<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-008 — EVM contracts: vault, roles, reference emitter

Milestone `M-1`. Serves `REQ-RN-4`, `REQ-RN-5`, `REQ-KY-3`, `REQ-KY-4`, `REQ-KY-5`, `REQ-CV-7`, `REQ-GV-3`, `INV-14`,
`AC-I1.1` (plan `DEC-10`, `DEC-13`). Read SPEC v0.4.0 §4.4(b), §5.10, §8.6, amendment 0007, and PLAN `DEC-10`.

## What it builds

A Foundry project in `contracts/evm/`:

- **ReferenceEmitter:** a stateless contract whose only function emits `Reference(bytes32)`.
- **Vault setup:** a deployment script and its configuration for a Safe (v1.4.1) with a Zodiac Roles v2 modifier, per
  network (Polygon for USDC, Base for EURC, BNB Chain for BTCB). Owners and threshold are parameters (the owners are
  governance; several signers, `REQ-KY-5`).
- **Delivery role** (bezent's signer key): exactly one allowed call shape — a MultiSend of (origin-token `transfer`
  to any address, `ReferenceEmitter.emit(ref)`), the transfer bounded by a Roles allowance that refills per period.
  Nothing else: no swaps, no CCTP, no other token, no delegatecall but MultiSend's.
- **Conversion role** (Swaps' key, `REQ-CV-7`): exact-input single swaps through Uniswap v3 SwapRouter02 (Polygon)
  and Aerodrome Slipstream (Base) only, `tokenIn` in the asset's admitted convertible list, `tokenOut` the origin
  token, `recipient` the vault, per-window allowance on the input. It can never spend the origin token.
- Role changes only by the Safe owners (governance), so `REQ-GV-3`'s delay applies through the Safe's own process
  (record how in NOTES).

## Not now

The EVM endpoint (`F-009`); pause wiring to the core; mainnet deployment (`F-014`).

## Done when

Foundry fork tests (Polygon and Base forks at the blocks pinned by PR #19's harness, `e2e/evm`; fork RPCs from env,
public by default) show: every outflow is bounded by the allowance (`AC-I1.1`); every outflow carries its reference
atomically with the value, and a transfer without the emitter call is refused (`INV-14`, `REQ-RN-5`); the conversion
role cannot spend USDC/EURC, cannot send output elsewhere, cannot exceed its window (`REQ-CV-7`); neither role can
change roles, add modules, or call anything else. CI runs `forge test` on PRs touching `contracts/evm/**`.
