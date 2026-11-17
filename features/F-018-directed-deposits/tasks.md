<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-018 — tasks

**Accepted.** Amendment 0029 is applied (`SPEC.md` v0.5.13). T-018-01 to T-018-03, T-018-05 and T-018-06 are drafted, in pull requests #121, #122 and #125; T-018-04 is done (Polygon Amoy); T-018-10 and T-018-11 are built (`NOTES.md`, "The public status route, the statement and the alerts"). Its Open questions are ruled (2026-11-09);
where a ruling removed a task, its identifier is retired and not reused. Sizes are rough, for one engineer with agents, review included. *S* is up to a week,
*M* one to two weeks, *L* two to three.

## Milestone 1 — the contract (S, about a week)

The outside review is ruled (`d-f018-review`, @pandres95, 2026-11-17): a paid audit comes after the 1 USDC mainnet test
(T-018-20) and before the web app is announced or any limit is raised. Before mainnet, the contract gets an adversarial
review in a fresh context, plus fuzz and invariant tests on the Polygon fork.

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-018-01 | `DirectedDeposit.sol`: `deposit`, both `depositWithPermit` forms, `try`-permit then allowance check, immutables, `Directed` event; no pause (Open 4, ruled) | `REQ-RN-5`, `REQ-AS-11`, `REQ-LM-2`, `AC-A6.5` | Unit tests: limits, zero recipient, zero or vault or self refund (each its own error), the `refund` in `Directed`, never a balance, no owner, pause or admin function, nothing between `Transfer` and `Directed` |
| T-018-02 | Fork tests on Polygon (real USDC, the pinned block of `e2e/evm`) | `AC-A6.5`, `AC-A6.6`, `INV-14` | Exact amount from caller to vault; front-run permit still deposits; bytes-signature permit from a Safe; a batch of two deposits gives two adjacent pairs |
| T-018-03 | `DeployDirected.s.sol` and `VerifyVault.checkDirected`; config in `config/polygon.json` | `REQ-AS-11`, `REQ-GV-3` | Refusals of the plan's "Deployment"; the verifier accepts a fresh deployment and refuses a changed immutable or a maximum above the Kreivo per-transfer maximum |
| T-018-04 | Deploy on Polygon Amoy (test USDC, domain `USDC`/`2`). **Done** 2026-11-16, below | `NFR-9` | Address recorded in the test configuration; verifier passes against Amoy |

**T-018-04, done (2026-11-16).** A test reserve vault and `DirectedDeposit` on Polygon Amoy (chain 80002), with test
keys and test USDC only. Every address, block and transaction is in `contracts/evm/deployments/polygon-amoy.json`; the
network constants in `contracts/evm/config/polygon-amoy.json`; the endpoint's Amoy configuration in
`services/evm-endpoint/fixtures/polygon-amoy.toml` (its `[[directing]]` entry waits for #122).

- **Vault:** the full `DeployVault` deployment (SafeL2 1.4.1 with the delivery Roles, the Delay and the guardian's
  Roles, and `VaultGuard`), Safe `0x3530dBB6986Dc584bf3D83a0ed4cbE027238204a`, 31 transactions from block 53 060 034,
  the Safe created in `0x98f4187f9139da7701006a7ce444d38820fe719b8b022ba3f9ba2c72d4c37bee`. Every pinned Safe and
  Zodiac contract is on Amoy at its canonical address with the code hash of F-008 NOTES. It differs from Polygon's
  vault in one way: no conversion role (venue `none`), since Amoy has neither USDT0 nor the declared pool.
- **`DirectedDeposit`:** `0xB8F75BeAE2723637313072fC89C69B7C15EdAec7`, deployed in
  `0xa8befdd606a962d637bb06252e18061790f9ef9db1747d87558505d1646366cb` (block 53 060 139), runtime code hash
  `0x6c32…07f0`, limits 1 to 50 000 USDC. `KREIVO_PER_TRANSFER_MAX` was read at release time from Kreivo mainnet:
  `bounds(Here(101))` of the bezent contract, a `ContractsApi_call` dry run at the finalized head, gives
  `per_transfer_max` 50 000 000 000 on two providers.
- **Verifier:** `VerifyVault` with `DIRECTED_DEPOSIT` passes against Amoy, every vault check included (27 Roles events
  replayed; the directing code matches byte for byte), last run to block 53 060 401.
- **Smoke test:** one directed deposit of 1 USDC with `depositWithPermit` (an EIP-2612 permit on the domain
  `USDC`/`2`/80002), `0x4ee22acf8f994bf62626d7e5d480fdc3d1d5880bcb49751588e31d1fa2c1b2e9` (block 53 060 226). The
  vault's `Transfer` (log 4, from the depositor, 1 000 000) is immediately followed by `Directed` (log 5: the same
  sender and amount, recipient `0x8eaf…6a48`, refund the depositor).

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
| T-018-18 | Deploy `DirectedDeposit` on Polygon mainnet; declaration (`REQ-AS-11`); endpoint configuration; verifier passes. **Contract step done** 2026-11-17, below | `REQ-AS-11`, `REQ-GV-3` | Verifier green against mainnet; address published with the vault |
| T-018-19 | Runbooks: a return that fails, spam at the minimum (alerts only), an `UnpairedDirection` | `REQ-IC-6`, `NFR-9` | Each exercised on the test bed |
| T-018-20 | Round trip from `bridge.bloque.network`: a 1 USDC deposit, mint, redeem the 1 bUSDC (production's redemption minimum is 1 bUSDC, Open 14), payout; one return exercised | `M-4` | Every leg at its coordinate, the reserve check at zero divergence throughout, the statement shows it |

**T-018-18, the contract step done (2026-11-17).** `DirectedDeposit` is deployed and verified on Polygon PoS mainnet
(chain 137) at `0x35495A8273F7450fb7D25bCe6bcd5Ad4aa3F824E`, in
`0x83535c5cca4df35b57fa904db90c1198dcc5aaa19ecb311cab55f384cf3daba5` (block 97 485 770), in front of the production
vault `0x584A9E72ebC1cdBA2F20233764eef08d528792ca`, for native USDC, limits 1 to 50 000 USDC. Its runtime code hash,
`0x399c…b1ba`, matches a local build. `KREIVO_PER_TRANSFER_MAX` was read from Kreivo mainnet at release time:
50 000 000 000. Every value is in `contracts/evm/deployments/polygon.json`. The web app pins the contract, the vault,
the code hash and the vault's reference emitter for Polygon, and Kreivo's refused recipients (the bezent contract's
account and aledgent's two settlement contracts on Kreivo mainnet), so a production build resolves Polygon
(`apps/bridge/src/config/networks.ts`). What is left of T-018-18 is the deployment's: the declaration (`directing_code`,
and the refused recipients in `ledger_accounts`), the EVM endpoint's `[[directing]]` (the same address, `from_block`
97 485 770), and the release that carries them.

**T-018-19, done (2026-11-17).** Three runbooks, each exercised on the full-chain bed (`services/full-chain`, Polygon an
anvil fork with the real USDC and the vault and `DirectedDeposit` from the repository's scripts, Kreivo the in-memory
model), and recorded in `deploy/runbooks/EXERCISES.md`:

- **A return that fails:** `deploy/runbooks/procedures/directed-return-failed.md`, exercised by `s8` (a USDC-blacklisted
  refund address: the dry run refuses every preparation with the Roles modifier's `ModuleTransactionFailed()`, which
  hides the token's reason, so the runbook reads the token; review at the deadline; a two-person new attempt once the
  blacklist is lifted pays it once, to the same address).
- **Spam at the minimum:** `deploy/runbooks/procedures/directed-spam.md`, exercised by `s9` (alerts only; the route's
  pause is the one lever).
- **An `UnpairedDirection`:** `deploy/runbooks/pages/unpaired-direction.md`, exercised by `s9`.

The same suite runs a directed round trip end to end (`s7`: deployment, declaration, `[[directing]]`, a
`depositWithPermit`, the directed arrival, the mint with no request, a redemption and its payout, and a return to a
refund address), with the reserve check at zero divergence throughout, in CI's `e2e-full-chain` job. `pages/directed-mints.md`
and `pages/directed-returns.md` are exercised there too. Kreivo's runtime is not: that is T-018-20's mainnet test.

## Milestone 5 — evmhot sweeps through the directing contract (S, in evmhot, outside this repository)

Ruled by @pandres95, 2026-11-08 (plan, part 5). Until it is done, Swaps binds sweeps by coordinate, as today.

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-018-21 | evmhot sweeps each deposit address by calling `DirectedDeposit.deposit` from that address (`depositWithPermit`, or `approve` then `deposit`), naming as recipient a Bloque Kreivo account set per chain and token in evmhot's configuration (which one is @pandres95's call; Swaps attributes each sweep to its user from the deposit address), instead of a plain transfer into the vault; balances above the maximum are swept in several calls | `REQ-IN-10`, `AC-A6.1`, `AC-A6.6` | On the test bed, a sweep is one directed arrival, minted for the recipient it names with no deposit request; a sweep outside the limits reverts and moves nothing; Swaps no longer binds sweeps by coordinate |
