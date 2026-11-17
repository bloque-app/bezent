<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Roadmap

bezent is **in production for bUSDC**: since release v0.4.0 it moves USDC on Polygon to bUSDC on Kreivo mainnet and
back, for its requester. Redemption is open to anyone. The way in that needs no requester (directed deposits and the
bridge web app, `F-018`, amendment 0029) is being built. This file says where it stands and what comes next; `PLAN.md` §5 has
the milestones and their exit criteria, and it is authoritative. Dates are intentionally absent.

| Milestone | Claim that becomes true | State |
|---|---|---|
| `M-0` Skeleton | The parts exist and the contract works on a local Kreivo | Done |
| `M-1` bUSDC on test | A deposit and withdrawal work between Polygon Amoy and the local Kreivo | Done |
| `M-2` More assets | bEURC and bBTC, and USDT0 conversions on the Polygon vault | Planned |
| `M-3` Attested fiat | COPM, MXNB and BRLB move through the requester's reports | Planned |
| `M-4` Production | bezent carries value on Kreivo mainnet | bUSDC is in production through the requester (v0.4.x). `F-018` is open: directed deposits, their returns and the bridge web app, ending in a 1 USDC round trip on mainnet from the web app |
| `M-5` Open source | The code and design are public | This repository |

## Known gaps

- No external audit has been done.
- The zombienet end-to-end job needs a self-hosted x86-64 pool and is opt-in (`.github/workflows/e2e-zombienet.yml`).

Proposals for the roadmap are issues labelled `roadmap`; anything that changes the specification goes through an
amendment first (`CONTRIBUTING.md`).
