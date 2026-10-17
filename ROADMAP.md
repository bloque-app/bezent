<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Roadmap

bezent is **in progress and not in production**. This file says where it stands and what comes next; `PLAN.md` §5 has
the milestones and their exit criteria, and it is authoritative. Dates are intentionally absent.

| Milestone | Claim that becomes true | State |
|---|---|---|
| `M-0` Skeleton | The parts exist and the contract works on a local Kreivo | Under way: the store, the ingress intake, the signer stub and the Kreivo contract are merged; the lifecycle and the test bed are being built |
| `M-1` bUSDC on test | A deposit and withdrawal work between Polygon Amoy and the local Kreivo | Not started as a whole: the EVM vault is merged; the reserve book, the chain endpoints and the Swaps module follow |
| `M-2` More assets | bEURC and bBTC, and USDT0 conversions on the Polygon vault | Planned |
| `M-3` Attested fiat | COPM, MXNB and BRLB move through the requester's reports | Planned |
| `M-4` Production | bezent carries value on Kreivo mainnet | Planned. Waits on measured bounds, every runbook exercised, and Kreivo's mainnet enactment |
| `M-5` Open source | The code and design are public | This repository |

## Known gaps

- No external audit has been done.
- The zombienet end-to-end job needs a self-hosted x86-64 pool and is opt-in (`.github/workflows/e2e-zombienet.yml`).
- The trademark clearance for "bezent" and counsel's review of the licence table are pending (`GOVERNANCE.md`).

Proposals for the roadmap are issues labelled `roadmap`; anything that changes the specification goes through an
amendment first (`CONTRIBUTING.md`).
