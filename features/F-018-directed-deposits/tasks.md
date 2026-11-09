<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-018 — tasks

**Proposed.** None is started: each waits for amendment 0029 to be applied. Its Open 1 to 13 are ruled (2026-11-09);
where a ruling removed a task, its identifier is retired and not reused. Sizes are rough, for one engineer with agents, review included. *S* is up to a week,
*M* one to two weeks, *L* two to three.

## Milestone 1 — the contract (S, about a week, plus an outside review before mainnet)

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-018-01 | `DirectedDeposit.sol`: `deposit`, both `depositWithPermit` forms, `try`-permit then allowance check, immutables, `Directed` event; no pause (Open 4, ruled) | `REQ-RN-5`, `REQ-AS-11`, `REQ-LM-2`, `AC-A6.5` | Unit tests: limits, zero recipient, zero or vault or self refund (each its own error), the `refund` in `Directed`, never a balance, no owner, pause or admin function, nothing between `Transfer` and `Directed` |
| T-018-02 | Fork tests on Polygon (real USDC, the pinned block of `e2e/evm`) | `AC-A6.5`, `AC-A6.6`, `INV-14` | Exact amount from caller to vault; front-run permit still deposits; bytes-signature permit from a Safe; a batch of two deposits gives two adjacent pairs |
| T-018-03 | `DeployDirected.s.sol` and `VerifyVault.checkDirected`; config in `config/polygon.json` | `REQ-AS-11`, `REQ-GV-3` | Refusals of the plan's "Deployment"; the verifier accepts a fresh deployment and refuses a changed immutable or a maximum above the Kreivo per-transfer maximum |
| T-018-04 | Deploy on Polygon Amoy (test USDC, domain `USDC`/`2`) | `NFR-9` | Address recorded in the test configuration; verifier passes against Amoy |

## Milestone 2 — endpoint and core (L, two to three weeks)

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-018-05 | EVM endpoint: `directing` configuration, the `Directed` filter, pairing, kinds `directed_arrival` and `UnpairedDirection` | `AC-A6.6`, `INV-10`, `DEC-12` | Fork and mock tests: one observation per pair at the `Transfer`'s coordinate, never also an `Arrival`; providers disagreeing on a pair is a gap; empty configuration changes nothing |
| T-018-06 | Migration: the kinds; `source_bindable()`'s two carve-outs (the core's directed deposit, the core's return) and nothing else; `directed_deposit()`; the destination guard; audit of every reader that assumes a deposit has an intake answer (webhooks, `by-request`, the log, refund destination lookup) | `REQ-IN-10`, `REQ-IN-11`, `AC-A6.2`, `INV-2` | Store tests: a requester's deposit or refund naming one is `ERR-SourceNotFound` and refused by the store whoever writes; the core's deposit and return bind only with the observed destination; a transfer with another destination or refund destination is refused whoever writes; the refund destination comes from the observed event's `refund` and a zero, vault or directing-contract refund is refused; no reader fails on a deposit with no answer |
| T-018-07 | Core: directed intake (route check, refused recipients, `CND-DepositWindow` (Open 1, ruled: wait); no share and no count cap, Open 5 and 6, ruled) | `REQ-IN-10`, `REQ-IN-11`, `AC-A6.1`, `AC-A6.3` | Engine tests: minted for the recorded recipient; a pause, a hold and the mint window park it; a deposit over the window waits and resumes as it rolls over, and is never returned |
| T-018-08 | Core: the return of a never-mintable directed arrival (closed to minting, then `bz-u-…` payout to the event's refund address; no second approver, Open 3 ruled) | `REQ-IN-11`, `REQ-RF-1`, `REQ-RF-2`, `AC-A6.4` | Returned once, to the event's refund address only, with its own reference and proofs; the reserve book posts it; a second return is refused |
| T-018-09 | The review edge for a directed deposit that can never receive (Open 2, ruled yes) | `REQ-RV-2`, `REQ-GV-5`, `INV-8` | A resolution into `Refunding` runs only with a second approver and no signed mint attempt |
| T-018-10 | Public status route, its own listener, no credential (Open 8, ruled yes) | `REQ-ST-8`, `REQ-ID-4` | Answers only directed deposits and withdrawals, by coordinate or redemption reference; never a requester's field; a requester-bound transfer's coordinate answers "not found" |
| T-018-11 | Statement and published addresses list the directing contract; alerts for directed mints and returns against thresholds, directed use of the window, and `UnpairedDirection` | `REQ-AS-11`, `REQ-BK-5`, `REQ-ST-6` | Statement fixture shows it; alerts fire in tests |

## Milestone 3 — the web app (M to L, two to three weeks)

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-018-12 | `apps/bridge/` scaffold: TypeScript, viem, polkadot-api with smoldot, Cloudflare Workers static assets, CI, licence `Apache-2.0 OR MIT` (Open 12, ruled), SPDX, CSP, reproducible build | `DEC-22` | CI builds twice to the same hash; `reuse lint` passes; smoldot's GPL-with-Classpath-exception terms checked against the licence and recorded in `docs/oss/LICENSING.md` |
| T-018-13 | Reference derivations in TypeScript (`bz-d-`, `bz-u-`, the on-chain reference) with test vectors generated from the Rust code | `REQ-ST-1` | Vectors from `services/ingress/src/refs.rs` and the core match byte for byte |
| T-018-14 | Deposit: EIP-6963, chain switch, SS58 recipient, recipient pre-check on Kreivo, exact-amount permit, fallback approve and deposit | `US-A6` | On Amoy and the test bed, a deposit from MetaMask mints; a recipient that cannot receive is refused before signing |
| T-018-15 | Redeem with Talisman, SubWallet and Polkadot.js; whole-balance batch; Kreivo's custom signed extensions registered in the polkadot.js initialisation metadata, so these wallets work with the polkadot.js interface (Open 9) | `US-B5` | On the test bed, a redemption from each extension is signed and paid out, with the custom signed extensions registered at initialisation; without them it fails |
| T-018-17 | Status page from chain data and the public route | `REQ-ST-1`, `REQ-ST-8` | Every state and condition of a directed deposit and a withdrawal is shown, with its coordinates |

T-018-16 (the Virto spike) is retired by amendment 0029, Open 9: passkey accounts are out for now. The identifier is not
reused.

## Milestone 4 — mainnet test starting at 1 USDC (S, two to three days, plus the 48-hour soak: a value-moving release under amendment 0024, Open 11 ruled)

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-018-18 | Deploy `DirectedDeposit` on Polygon mainnet; declaration (`REQ-AS-11`); endpoint configuration; verifier passes | `REQ-AS-11`, `REQ-GV-3` | Verifier green against mainnet; address published with the vault |
| T-018-19 | Runbooks: a return that fails, spam at the minimum (alerts only), an `UnpairedDirection` | `REQ-IC-6`, `NFR-9` | Each exercised on the test bed |
| T-018-20 | Round trip from `bridge.bloque.network`: a 1 USDC deposit, mint, redeem the 1 bUSDC (production's redemption minimum is 1 bUSDC, Open 14), payout; one return exercised | `M-4` | Every leg at its coordinate, the reserve check at zero divergence throughout, the statement shows it |

## Milestone 5 — evmhot sweeps through the directing contract (S, in evmhot, outside this repository)

Ruled by @pandres95, 2026-11-08 (plan, part 5). Until it is done, Swaps binds sweeps by coordinate, as today.

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-018-21 | evmhot sweeps each deposit address by calling `DirectedDeposit.deposit` from that address (`depositWithPermit`, or `approve` then `deposit`), naming as recipient a Bloque Kreivo account set per chain and token in evmhot's configuration (which one is @pandres95's call; Swaps attributes each sweep to its user from the deposit address), instead of a plain transfer into the vault; balances above the maximum are swept in several calls | `REQ-IN-10`, `AC-A6.1`, `AC-A6.6` | On the test bed, a sweep is one directed arrival, minted for the recipient it names with no deposit request; a sweep outside the limits reverts and moves nothing; Swaps no longer binds sweeps by coordinate |
