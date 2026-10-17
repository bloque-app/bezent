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
