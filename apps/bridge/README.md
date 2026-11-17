<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# bezent bridge (web app)

F-018, Milestone 3 (amendment 0029; `features/F-018-directed-deposits/plan.md`, part 4). A static, client-only page
where anyone deposits USDC on Polygon through the `DirectedDeposit` contract, naming a Kreivo recipient. It holds no
key and no secret, runs no server code, and loads no third-party script.

Built so far: the scaffold (T-018-12), the reference derivations (T-018-13) and the deposit (T-018-14). Redeem
(T-018-15) and the status page (T-018-17) come next.

## Layout

| Path | What |
|---|---|
| `src/config/networks.ts` | The networks, with the contract's address pinned per network; `resolveNetwork` refuses one whose address is unset |
| `src/config/build.ts` | What the build was made for (`BRIDGE_DEPLOYMENT`), fixed at build time |
| `src/lib/refs.ts` | `bz-d-`, `bz-u-`, the mint's on-chain reference and the refund payout's, from a coordinate (T-018-13) |
| `src/lib/ss58.ts` | SS58 decoding and the recipient's validation (prefix 2, checksum; never raw hex) |
| `src/lib/amount.ts` | Amounts in minor units, inside the contract's limits |
| `src/lib/recipient.ts` | Whether a Kreivo account can receive bUSDC (amendment 0029, Open 7) |
| `src/evm/` | EIP-6963 discovery, the chain switch, the permit's typed data, `depositWithPermit` and the `approve` + `deposit` fallback, the arrival's coordinate from the receipt |
| `src/kreivo/` | Kreivo through polkadot-api: smoldot in a worker (live networks) or a loopback RPC (the local test bed only) |
| `src/main.ts`, `index.html`, `src/ui/` | The page |
| `csp.ts` | The Content Security Policy and the `_headers` file |
| `test/` | Unit tests; `test/vectors/` are generated from the Rust code |
| `scripts/hash.mjs` | The build's hash |

## Commands

```sh
npm ci
npm run lint && npm run typecheck && npm test
npm run build            # dist/, for BRIDGE_DEPLOYMENT=test (Polygon Amoy) by default
npm run hash             # the bundle's hash; two builds of one revision print the same
```

`BRIDGE_DEPLOYMENT` is `test` (Polygon Amoy), `production` (Polygon) or `testbed` (the local test bed: the anvil
fork of Polygon on 127.0.0.1:8545 and the zombienet Kreivo on ws://127.0.0.1:9944). A test bed build needs the
contract the bed deployed: `BRIDGE_DEPLOYMENT=testbed BRIDGE_TESTBED_DIRECTED_DEPOSIT=0x… npm run build`. Point the
wallet's chain 137 at the fork yourself: the app refuses a contract whose `token()` is not the pinned USDC, so a
wallet left on the real Polygon is refused, not sent there.

A network whose `DirectedDeposit` address is unset is refused: today that is Polygon Amoy (T-018-04 deploys it) and
Polygon (T-018-18). Nothing falls back to another address.

## The references never drift

`test/vectors/ingress-refs.json` and `core-refs.json` are written by Rust tests from `services/ingress/src/refs.rs`
and `services/core/src/refs.rs`, and the same tests fail in `cargo test` when a file no longer matches its
derivation. After a deliberate change to a derivation (itself a breaking change):

```sh
BEZENT_WRITE_VECTORS=1 cargo test -p bezent-ingress --lib bridge_vectors
BEZENT_WRITE_VECTORS=1 cargo test -p bezent-core-service --test bridge_app_vectors
```

## Defence

- **CSP** (`csp.ts`), in a `<meta>` element and in `_headers`: scripts and the worker from this origin only,
  `'wasm-unsafe-eval'` for smoldot's WebAssembly, `connect-src 'self' wss:` (the light client's peers), images from
  this origin and `data:` (wallet icons), no frame, form, object or base.
- **Reproducible build**: no timestamps or source maps; CI builds twice on separate runners, compares
  `scripts/hash.mjs`'s bundle hash, and publishes the build and its hash as an artifact.
- **Pinned addresses**, shown in the page; the contract's `token()` is checked against the pinned token before
  anything is signed.
- **Before signing**, the page shows the connected EVM account (depositor and refund address) beside the recipient
  as SS58, and checks the recipient on Kreivo again.

## Licence

`Apache-2.0 OR MIT` (amendment 0029, Open 12). The bundle also carries smoldot (GPL-3.0-or-later with the Classpath
exception) and Kreivo's chain specification (GPL-3.0-only), each as its own file; `docs/oss/LICENSING.md` §4.4 says
why that is compatible and what a release owes. `THIRD-PARTY-NOTICES.txt` in the build lists every bundled package
and its licence.
