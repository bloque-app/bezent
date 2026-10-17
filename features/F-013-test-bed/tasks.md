<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-013 — tasks

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-013-01 | Fork aledgent's zombienet harness into `e2e/network`, Kreivo 0.17.1 | `DEC-19` | zombienet job green in CI |
| T-013-02 | Genesis override: assets 100–105 at the ruled ids and decimals; the dedicated community as Issuer and Admin of 101–103 | `DEC-7`, `REQ-HN-10`, `REQ-AS-*` | A CI check reads each asset's metadata and roles from the running chain |
| T-013-03 | anvil forks of Polygon, Base, BNB Chain at pinned blocks, one command | `DEC-19` | CI job shows the three forks and USDC, EURC, BTCB balances readable |
| T-013-04 | Local CCTP attester and relay between the Polygon and Base forks, for Swaps' end-to-end flows (optional since amendment 0007 A5) | `DEC-19` | CI: a burn on Polygon is minted on Base |
| T-013-05 | Deploy the `F-006` contract through ContractsStore on zombienet (after `F-006`) | `DEC-6`, `REQ-HN-10` | Contract instantiated by the community; mint, redeem and refund mint once each |
