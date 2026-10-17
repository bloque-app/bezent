<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-013 — Test bed (M-0 part)

Milestone `M-0` for the harness; the release gate is `M-4`. Serves `NFR-2`, `NFR-9`, §15 (plan `DEC-19`); carries
the genesis part of `F-015` (`DEC-7`).

## What it builds now

1. **zombienet**, forked from aledgent's harness (`aledgent/e2e/network`, workflow
   `.github/workflows/e2e-zombienet.yml`): `rococo-local` with Kreivo **0.17.1** (para 2281; aledgent's CI pins 0.17.0)
   and a genesis override with the bridged assets at their ruled ids: bUSDC `Here(101)` 6 decimals, bEURC
   `Here(102)` 6, bBTC `Here(103)` 8, MXNB `Here(104)`, BRLB `Here(105)`, COPM `Here(100)` (copy MXNB/BRLB/COPM
   metadata from mainnet). A dedicated community holds Issuer and Admin of 101–103 (`REQ-HN-10`); reuse how aledgent
   creates its community and publishes through ContractsStore.
2. **EVM forks**: one command (`e2e/evm/up.sh` or a small Rust/TS tool, whichever reads better) starts anvil forks of
   Polygon, Base and BNB Chain at pinned blocks, from RPC URLs in the environment (public RPCs by default).
3. **Local CCTP** on the Polygon and Base forks: impersonate each `MessageTransmitterV2`'s attester manager
   (`anvil_impersonateAccount`), enable a test attester, set the signature threshold to 1; a relay script watches
   `MessageSent`, signs the message with the test attester and calls `receiveMessage` on the other fork.
4. **CI**: anvil jobs on `ubuntu-latest`; the zombienet job on an x86-64 self-hosted pool, opt-in through the repository variable `ZOMBIENET_RUNS_ON` (it never runs for fork pull requests).

## Not now

Deploying the bezent contract into zombienet (after `F-006` lands, task `T-013-05`); the end-to-end suite and the
gate (`M-1` onwards); testnet wiring.

## Done when

CI shows: zombienet up with Kreivo 0.17.1 and the six assets at the right ids, decimals and issuer; the three forks
up at their pinned blocks; a USDC burn through CCTP on the Polygon fork minted on the Base fork by the local relay.
Nothing heavy runs on @pandres95's laptop (16 GB): zombienet runs in CI only.
