<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-018 — tasks

**Proposed.** None is started: each waits for amendment 0029's ruling. The tasks marked *(Open n)* exist only if that
item is ruled as recommended. Sizes are rough, for one engineer with agents, review included. *S* is up to a week,
*M* one to two weeks, *L* two to three.

## Milestone 1 — the contract (S, about a week, plus an outside review before mainnet)

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-018-01 | `DirectedDeposit.sol`: `deposit`, both `depositWithPermit` forms, `try`-permit then allowance check, immutables, `Directed` event; the pause *(Open 4)* | `REQ-RN-5`, `REQ-AS-11`, `REQ-LM-2`, `AC-A6.5` | Unit tests: limits, zero recipient, pause roles, never a balance, nothing between `Transfer` and `Directed` |
| T-018-02 | Fork tests on Polygon (real USDC, the pinned block of `e2e/evm`) | `AC-A6.5`, `AC-A6.6`, `INV-14` | Exact amount from caller to vault; front-run permit still deposits; bytes-signature permit from a Safe; a batch of two deposits gives two adjacent pairs |
| T-018-03 | `DeployDirected.s.sol` and `VerifyVault.checkDirected`; config in `config/polygon.json` | `REQ-AS-11`, `REQ-GV-3`, `REQ-KY-4` | Refusals of the plan's "Deployment"; the verifier accepts a fresh deployment and refuses a changed immutable or a maximum above the Kreivo per-transfer maximum |
| T-018-04 | Deploy on Polygon Amoy (test USDC, domain `USDC`/`2`) | `NFR-9` | Address recorded in the test configuration; verifier passes against Amoy |

## Milestone 2 — endpoint and core (L, two to three weeks)

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-018-05 | EVM endpoint: `directing` configuration, the `Directed` filter, pairing, kinds `directed_arrival` and `UnpairedDirection` | `AC-A6.6`, `INV-10`, `DEC-12` | Fork and mock tests: one observation per pair at the `Transfer`'s coordinate, never also an `Arrival`; providers disagreeing on a pair is a gap; empty configuration changes nothing |
| T-018-06 | Migration: the kinds; `source_bindable()`'s two carve-outs (the core's directed deposit, the core's return) and nothing else; `directed_deposit()`; the destination guard; audit of every reader that assumes a deposit has an intake answer (webhooks, `by-request`, the log, refund destination lookup) | `REQ-IN-10`, `REQ-IN-11`, `AC-A6.2`, `INV-2` | Store tests: a requester's deposit or refund naming one is `ERR-SourceNotFound` and refused by the store whoever writes; the core's deposit and return bind only with the observed destination; a transfer with another destination or refund destination is refused whoever writes; no reader fails on a deposit with no answer |
| T-018-07 | Core: directed intake (route check, refused recipients, directed share and count *(Open 5 and 6)*, `CND-DepositWindow` (Open 1, ruled: wait)) | `REQ-IN-10`, `REQ-IN-11`, `AC-A6.1`, `AC-A6.3` | Engine tests: minted for the recorded recipient; a pause, a hold and the mint window park it; a deposit over the window waits and resumes as it rolls over, and is never returned |
| T-018-08 | Core: the return of a never-mintable directed arrival (closed to minting, then `bz-u-…` payout to the funder; no second approver, Open 3 ruled) | `REQ-IN-11`, `REQ-RF-1`, `REQ-RF-2`, `AC-A6.4` | Returned once, to the funder only, with its own reference and proofs; the reserve book posts it; a second return is refused |
| T-018-09 | The review edge for a directed deposit that can never receive (Open 2, ruled yes) | `REQ-RV-2`, `REQ-GV-5`, `INV-8` | A resolution into `Refunding` runs only with a second approver and no signed mint attempt |
| T-018-10 | Public status route, its own listener, no credential *(Open 8)* | `REQ-ST-8`, `REQ-ID-4` | Answers only directed deposits and withdrawals, by coordinate or redemption reference; never a requester's field; a requester-bound transfer's coordinate answers "not found" |
| T-018-11 | Statement and published addresses list the directing contract; alerts for the caps, returns and `UnpairedDirection` | `REQ-AS-11`, `REQ-BK-5`, `REQ-ST-6` | Statement fixture shows it; alerts fire in tests |

## Milestone 3 — the web app (M to L, two to three weeks; the Virto spike two to three days of it)

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-018-12 | `apps/bridge/` scaffold: TypeScript, viem, polkadot-api with smoldot, Cloudflare Workers static assets, CI, licence *(Open 12)*, SPDX, CSP, reproducible build | `DEC-22` | CI builds twice to the same hash; `reuse lint` passes |
| T-018-13 | Reference derivations in TypeScript (`bz-d-`, `bz-u-`, the on-chain reference) with test vectors generated from the Rust code | `REQ-ST-1` | Vectors from `services/ingress/src/refs.rs` and the core match byte for byte |
| T-018-14 | Deposit: EIP-6963, chain switch, SS58 recipient, recipient pre-check on Kreivo, exact-amount permit, fallback approve and deposit | `US-A6` | On Amoy and the test bed, a deposit from MetaMask mints; a recipient that cannot receive is refused before signing |
| T-018-15 | Redeem with Talisman, SubWallet and Polkadot.js; whole-balance batch | `US-B5` | On the test bed, a redemption from each extension is paid out |
| T-018-16 | Virto spike: `KreivoPassSigner` signs `Contracts.call` to `redeem`; a pass account can hold bUSDC; who grants the membership *(Open 9)* | `US-B5` | A written answer to all three, on zombienet or mainnet with 1 USDC |
| T-018-17 | Status page from chain data and the public route | `REQ-ST-1`, `REQ-ST-8` | Every state and condition of a directed deposit and a withdrawal is shown, with its coordinates |

## Milestone 4 — mainnet test starting at 1 USDC (S, two to three days, plus the 48-hour soak if Open 11 says value-moving)

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-018-18 | Deploy `DirectedDeposit` on Polygon mainnet; declaration (`REQ-AS-11`); endpoint configuration; verifier passes | `REQ-AS-11`, `REQ-GV-3` | Verifier green against mainnet; address published with the vault |
| T-018-19 | Runbooks: a return that fails, spam against the caps, pausing the directing code, an `UnpairedDirection` | `REQ-IC-6`, `NFR-9` | Each exercised on the test bed |
| T-018-20 | Round trip from `bridge.bloque.network`: a 1 USDC deposit, mint, redeem the 1 bUSDC (production's redemption minimum is 1 bUSDC, Open 14), payout; one return exercised | `M-6` | Every leg at its coordinate, the reserve check at zero divergence throughout, the statement shows it |

## Milestone 5 — evmhot sweeps through the directing contract (S, in evmhot, outside this repository)

Ruled by @pandres95, 2026-11-08 (plan, part 5). Until it is done, Swaps binds sweeps by coordinate, as today.

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-018-21 | evmhot sweeps each deposit address by calling `DirectedDeposit.deposit` from that address (`depositWithPermit`, or `approve` then `deposit`), naming as recipient a Bloque Kreivo account set per chain and token in evmhot's configuration (which one is @pandres95's call; Swaps attributes each sweep to its user from the deposit address), instead of a plain transfer into the vault; balances above the maximum are swept in several calls | `REQ-IN-10`, `AC-A6.1`, `AC-A6.6` | On the test bed, a sweep is one directed arrival, minted for the recipient it names with no deposit request; a sweep outside the limits reverts and moves nothing; Swaps no longer binds sweeps by coordinate |
