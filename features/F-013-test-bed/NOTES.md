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
