<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# bezent

*bezent* (BEZ-ent; ES/PT be-SÉNT) is Bloque's reserve bridge, from *bezant* + *agent*. It is a **trusted, mirrored
bridge** between EVM networks (Polygon and Base first, BNB Chain next) and Kreivo. Value deposited on an origin
network is held in a reserve there, and a bridged asset is minted for it, one to one, on Kreivo. A withdrawal destroys
the bridged asset and delivers the origin token on the network the requester names. Reserves move between networks by
rebalancing. Words such as *reserve*, *deposit*, *observation* and *finality* have exact meanings, defined in
`SPEC.md`.

> **Status: in progress, not in production.** Part of the code is built and tested (the store, the ingress, the
> signer, the Kreivo contract and the EVM vault); the lifecycle, the chain endpoints and the test bed are being
> built. No deployment holds user value, and nothing here has had an external security audit. `ROADMAP.md` says what
> comes next, and `PLAN.md` §5 has the milestones.

bezent is a trusted issuer in its first release (R0 of the message-backed Kreivo Bridge): the operator holds the
reserves, and bounds, delays and attestations limit what the operator and the requester can do. `OVERVIEW.md` states
plainly what that asks you to trust.

## How the project is organised

Work is spec-driven, as in aledgent. Changes flow down this chain and never back up:

    research/ and notes/   evidence, motivation, constraints (informative)
      -> SPEC.md           what the bridge does; names no technology or network
        -> amendments/     changes to SPEC.md, one file per review pass
        -> PLAN.md         how it is built; binds Kreivo, Polygon, Base, Swaps, CCTP, Binance
          -> features/     one directory per feature: plan.md, tasks.md, NOTES.md
            -> issue -> PR

If code, plan and spec disagree, the spec wins. `CONTRIBUTING.md` explains how to propose a change at each level and
`GOVERNANCE.md` who decides.

| Path | What it is |
|---|---|
| `SPEC.md` | The specification. Normative, self-contained |
| `OVERVIEW.md` | Plain-language digest: the problem, the shape, the flows, what is open. Informative |
| `PLAN.md`, `features/` | Decisions (`DEC-*`), features (`F-*`), milestones (`M-*`), risks |
| `amendments/` | Reviewed changes to the specification |
| `crates/` | `core` (types: amounts, ids, references; Apache-2.0 OR MIT) and `store` (the PostgreSQL schema and its tests) |
| `services/` | `ingress`, `core`, `signer`, `kreivo-endpoint`, `evm-endpoint`, and the test-only `sidecar` |
| `contracts/bezent` | The ink! contract on Kreivo (a workspace of its own) |
| `contracts/evm` | The reserve vault glue: Safe, Zodiac Roles and the reference emitter (Foundry) |
| `e2e/` | The test bed: a zombienet Kreivo, anvil forks of the origin networks, local CCTP |
| `deploy/docker`, `deploy/e2e` | The images' Dockerfile, and how another repository's harness uses the test sidecar |
| `notes/`, `research/` | Motivation, constraints and the evidence behind the plan |
| `docs/` | The specification, plan and overview rendered as pages (`tools/md2artifact.py`) |
| `brand/` | Marks (Pagos Cube LLC; not licensed with the code) |

## Building

The Rust workspace builds with the toolchain in `rust-toolchain.toml`:

    cargo build --workspace
    cargo test --workspace      # the store tests need a PostgreSQL: DATABASE_URL=postgres://…

The contract, the EVM contracts and the test bed have their own toolchains; `.github/workflows/` is the reference for
each. Clone with `--recurse-submodules` for `contracts/evm` (Foundry libraries).

## Relation to other projects

- **aledgent** settles on Kreivo through its settlement contract. The bridge delivers into it with
  `deposit(account_ref)` and is funded by its withdrawals. aledgent never calls the bridge.
- **Swaps**, and later the ramp, is the requester.
- **Kreivo Bridge** (message-backed) is the successor this bridge hands over to.

## Licences

AGPL-3.0-or-later for the services, the store and the ink! contract; Apache-2.0 OR MIT for `crates/core`, the EVM
contracts, the test bed and tooling; CC-BY-4.0 for documents; GPL-3.0-only for Kreivo's chain specification, kept
verbatim. The marks in `brand/` are not licensed with the code. Every file carries its licence (`REUSE.toml`,
`docs/oss/LICENSING.md`).

## Security

Report vulnerabilities privately; see `SECURITY.md`.
