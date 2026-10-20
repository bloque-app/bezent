<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-008 — EVM contracts: vault, roles, reference emitter

Milestone `M-1`. Serves `REQ-RN-4`, `REQ-RN-5`, `REQ-KY-3`, `REQ-KY-4`, `REQ-KY-5`, `REQ-CV-7`, `REQ-GV-3`, `INV-14`,
`AC-I1.1`, `REQ-GV-5` (plan `DEC-10`, `DEC-13`). Read SPEC v0.4.0 §4.4(b), §5.10, §8.6, amendment 0007, and PLAN `DEC-10`.

## What it builds

A Foundry project in `contracts/evm/`:

- **ReferenceEmitter:** a stateless contract whose only function emits `Reference(bytes32)`.
- **Vault setup:** a deployment script and its configuration for a Safe (v1.4.1) with a Zodiac Roles v2 modifier, a
  Zodiac Delay modifier, the guardian's own Roles modifier and a Safe guard, per network (Polygon for USDC, Base for EURC, BNB Chain for BTCB). Owners,
  threshold, the guardian and the Delay's cooldown and expiration (within bounds) are parameters (the owners are governance; several signers,
  `REQ-KY-5`); `config/vault.example.env` lists them with the recommended values.
- **Guard and Delay** (`pl-vault-delay`, issue #22): the guard refuses any owner transaction that is not on its
  allowlist, so no owner transaction transfers or approves the vault's tokens (`REQ-GV-5`). Widening changes (a role or
  member added, an allowance raised, a module added, owners or threshold changed, the guard or the Delay removed) go
  through the Delay and take effect only after its cooldown (`REQ-GV-3`); narrowing changes (revoke, lower, pause) stay
  immediate. A queued call may not change the Delay's avatar, target, owner or modules, or take the Delay off the Safe
  (it would lock the owners out); only its cooldown and expiration, within bounds. The post-deployment verifier checks
  all of it.
- **Guardian** (review of PR #41): one independent key that can veto a queued change (`Delay.setTxNonce`) and pause the
  vault (`Safe.disableModule` of the delivery Roles modifier), and nothing else, through a second Roles modifier the
  owners can change only through the Delay (`REQ-GV-2`, `REQ-KY-4`).
- **Delivery role** (bezent's signer key): exactly one allowed call shape — a MultiSend of (origin-token `transfer`
  to any address, `ReferenceEmitter.emit(ref)`), the transfer bounded by a Roles allowance that refills per period.
  Nothing else: no swaps, no CCTP, no other token, no delegatecall but MultiSend's.
- **Conversion role** (Swaps' key, `REQ-CV-7`; Polygon only, `DEC-13`): exact-input single swaps through Uniswap v3
  SwapRouter02 only, `tokenIn` USDT0, `tokenOut` the origin token, `recipient` the vault, per-window allowance on the
  input. It can never spend the origin token. Base and BNB Chain carry no conversion role (`pl-convertibles`).
- Role changes only by the Safe owners (governance), and only through the Delay when they widen (`REQ-GV-3`).

## Not now

The EVM endpoint (`F-009`); pause wiring to the core; mainnet deployment (`F-014`).

## Done when

Foundry fork tests (Polygon and Base forks at the blocks pinned by PR #19's harness, `e2e/evm`; fork RPCs from env,
public by default) show: every outflow is bounded by the allowance (`AC-I1.1`); every outflow carries its reference
atomically with the value, and a transfer without the emitter call is refused (`INV-14`, `REQ-RN-5`); the conversion
role (Polygon) cannot spend USDC, cannot send output elsewhere, cannot exceed its window (`REQ-CV-7`); neither role can
change roles, add modules, or call anything else; a direct Safe-owner transaction that bypasses the Delay is refused
by the guard, a queued change runs only after the cooldown and before it expires, the delivery role pays within its
bounds without delay, and removing the guard or the Delay is itself delayed (`REQ-GV-3`, `REQ-GV-5`). CI runs `forge test` on PRs touching `contracts/evm/**`.
