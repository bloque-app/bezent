<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-013 — notes

## T-013-05: the contract on the bed (2026-10-13)

- Deployed as aledgent deploys its settlement contract: `ContractsStore.publish`, `request_license`, `instantiate`,
  signed by community 4243's first admin (//Alice), which `EnsureCommunity` takes as the community's origin; on
  mainnet that origin is the community's referendum. The community account publishes and owns the instance.
- Governance is a test account (//Dave) on the bed, so the checks can unpause without a referendum; on mainnet it is
  the community's account (`REQ-HN-10`, F-015).
- First real run of the whole-balance redemption (`Assets::transfer_approved` through `call_runtime`): Kreivo
  dispatched it from the contract, moved the whole balance, reaped the redeemer's asset account and used up the
  approval, as the unit-test mock models it. One difference from what was expected, not from the contract: pallet-assets
  declares `TransferredApproved` but never deposits it, so the pull shows on chain as `Assets.Transferred`.

## Runner pool for the zombienet job

- The zombienet job needs an x86-64 machine with enough memory for a relay chain and Kreivo; GitHub-hosted runners
  are too small for it. It is therefore opt-in: it runs only where the repository variable `ZOMBIENET_RUNS_ON` names a
  self-hosted runner label (see `.github/workflows/e2e-zombienet.yml`), and on manual dispatch.

## The criteria map and the release gate (2026-11-04)

- **Naming convention.** A criterion counts as covered when a runnable test names it in its function name (`ac_a2_1_...`)
  or in the doc comment attached to it (`/// AC-A2.1 ...`), or in the id of an `await check(...)` of `e2e/kreivo`. A mention in a
  module header, a section comment, the body or an error string does not count. An `#[ignore]` test does not count. The map proves
  a test names a criterion at its layer; that it verifies it is for the reviewer: never name a test that does not.
- **Layer.** Decided by the path of the test (`contracts/` and `e2e/kreivo/` chain, `crates/store/` store, `services/signer/` and
  `crates/vault/` custody, the rest service) against `e2e/criteria/layers.toml`. A criterion that cites an invariant needs a test at
  every layer section 9 gives it (`REQ-AR-5`).
- **A new criterion** (an amendment) fails the map until it has a layer entry and a test or an exception.
- **The release gate** says in its header what is mechanical and what is human. The window of "sustained" is unruled
  (amendment 0021); `release/gate.toml` leaves it unset on purpose.

## The full-chain suite (T-013-08, 2026-11-04)

`services/full-chain` (package `bezent-e2e-full-chain`, a test target behind the `fork` feature; workflow
`.github/workflows/e2e-full-chain.yml`) runs the real services together on one PostgreSQL, each on its own login:
the ingress (served over HTTP), the core's cycle and reserve check, the signer (one process-shaped signer, two lanes,
two callers, over its Unix-socket link), the EVM endpoint's follower, submitter and prover, and the Kreivo endpoint's
follower, supplies, submitter and prover, with two operator logins. It lives under `services/` because it links them
(GPL-3.0-or-later, `docs/oss/LICENSING.md`).

- **Polygon is an anvil fork** at the pinned block, with the vault `F-008` deploys. **Kreivo is `FakeChain`, an in-memory
  model: not a chain.** No scenario here runs on zombienet or on a real Kreivo. The Kreivo supply the reserve check
  reads is the harness's word, derived from the model's own events (never from the store); a redemption is an event the
  model is told to emit, not a holder's call that destroys anything.
- **Why not the sidecar's side:** the sidecar's driver writes what an endpoint would have seen, so no endpoint code runs
  on it. `FakeChain` is the highest-fidelity Kreivo side a hosted runner can run today: the Kreivo endpoint's own
  follower, submitter, prover and supply code run against it, and the signer verifies the bytes it signs.
- **Scenarios** (`tests/full_chain/main.rs`): a deposit, a withdrawal, a payout that is not executed and its one refund
  mint (two operators), a reorganisation across an arrival before finality, and a compromise signal with the pause, the
  stopped signing and the lift by two operators. The loops of the endpoints' `main.rs` cannot be called, so the bed repeats
  their order of steps (`Bed::evm_pass`, `Bed::kreivo_pass`) and cites it.
- **Clocks.** The model's block times start in October 2025 and the fork's are today's. A deadline starts at the redemption's
  final block time and is read against the fork's head, so the withdrawal route's completion budget is declared from the two
  clocks (two hours past the fork's present), and the reserve check's clock is the model's.
- **anvil.** `anvil_reorg` is not used: on a fork, in 1.7.1, it drops the transaction but leaves its effect in the token's
  storage. The reorganisation is `evm_snapshot` / `evm_revert` followed by other blocks.
- **What S4 and S5 show.** S4: the EVM follower and the core run over the fork while the arrival is in a block that is not final (the
  scenario asserts anvil's `finalized` is below it) and record nothing; the block is then reverted and replaced, and nothing is
  minted, then or after finality. A follower reading `latest` fails it (checked by mutation: `At::Latest` in `evm-endpoint/src/pair.rs`).
  A reorganisation of an arrival the endpoint already recorded is not exercised: it records only final blocks, so such a record could
  exist only through a follower bug, which the mutation shows S4 catches. S5: while
  the pause stands, the core orders, records and signs no new work, held for the pause; that is the core holding work, not the signer
  refusing a request, and a signing intent recorded before the pause is still signed (`AC-I1.2` stays in `known_unmet.toml`, issue #77).
- **The reserve check runs on the model's clock** (the Kreivo model's block time), so how it treats a stale reading (a lag past its
  bound) is not covered here.
- **Not wired: the same scenarios on zombienet.** The Kreivo endpoint's light client (`LightChain`) would have to be pointed at
  the spawned relay and parachain chain specs, the redemption would be a holder's signed call (polkadot-api, as
  `e2e/kreivo/` signs), and the supply would be read from the chain; the pool's job skips on pull requests and could not be
  run from a laptop. Until then the Kreivo side of these scenarios is a model.
- **Not covered:** the long run of `T-013-08` (zero divergence sustained, every final leg delivered, refunded or held with an
  owner) and the 48-hour soak need a runner for a long run (issue #33).
