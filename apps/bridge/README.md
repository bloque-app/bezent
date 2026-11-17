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
| `src/config/networks.ts` | The networks, with the contract's address, its reserve and its runtime code hash pinned per network; `resolveNetwork` refuses one with any of them unset, one paired with another deployment's Kreivo, and a production Kreivo with no refused recipient pinned |
| `src/config/build.ts` | What the build was made for (`BRIDGE_DEPLOYMENT`), fixed at build time |
| `src/lib/refs.ts` | `bz-d-`, `bz-u-`, the mint's on-chain reference and the refund payout's, from a coordinate (T-018-13) |
| `src/lib/ss58.ts` | SS58 decoding and the recipient's validation (prefix 2, checksum; never raw hex) |
| `src/lib/amount.ts` | Amounts in minor units, inside the contract's limits |
| `src/lib/recipient.ts` | Whether a Kreivo account can receive bUSDC (amendment 0029, Open 7) |
| `src/evm/` | EIP-6963 discovery, the chain switch, the permit's typed data, `depositWithPermit` and the `approve` + `deposit` fallback, the arrival's coordinate from the receipt; `no-ccip.ts` stands in for viem's CCIP-Read code |
| `src/review.ts` | The locked review: the snapshot taken at the Deposit click, the re-check on it, and the comparison of the receipt's `Directed` log with it |
| `src/kreivo/` | Kreivo through polkadot-api: smoldot in a worker (live networks) or a loopback RPC (the local test bed only) |
| `src/main.ts`, `index.html`, `src/ui/` | The page |
| `csp.ts` | The Content Security Policy and the `_headers` file |
| `test/` | Unit tests; `test/vectors/` are generated from the Rust code |
| `scripts/hash.mjs` | The build's hash |
| `notices/` | Third parties' notices for bundled packages that ship no licence file (smoldot, scale-ts), copied into `THIRD-PARTY-NOTICES.txt` |

## Commands

```sh
npm ci --ignore-scripts
npm run lint && npm run typecheck && npm test
BRIDGE_DEPLOYMENT=test npm run build   # dist/; the build refuses to run without BRIDGE_DEPLOYMENT
npm run hash                           # the bundle's hash; two builds of one revision print the same
```

`BRIDGE_DEPLOYMENT` is `test` (Polygon Amoy), `production` (Polygon) or `testbed` (the local test bed: the anvil
fork of Polygon on 127.0.0.1:8545 and the zombienet Kreivo on ws://127.0.0.1:9944). A build has no default. A test
bed build needs what the bed deployed: the contract, its `reserve()` and the keccak256 of its runtime code
(`cast keccak $(cast code <contract>)`):

```sh
BRIDGE_DEPLOYMENT=testbed BRIDGE_TESTBED_DIRECTED_DEPOSIT=0x… BRIDGE_TESTBED_RESERVE=0x… \
  BRIDGE_TESTBED_CODE_HASH=0x… npm run build
```

Point the wallet's chain 137 at the fork yourself: the app refuses a contract whose code hash, `token()` or
`reserve()` is not the pinned one, so a wallet left on the real Polygon is refused, not sent there.

A network whose `DirectedDeposit` address, reserve or code hash is unset is refused: today that is Polygon Amoy
(T-018-04 deploys it, and names the test Kreivo it mints on: none is named yet, and a test network never pairs with
live Kreivo) and Polygon (T-018-18, which also pins Kreivo's refused recipients, the contract's and the ledger
accounts; a production Kreivo with none pinned is refused). Nothing falls back to another value.

## Reproducing a published hash

CI's `Bridge web app` workflow builds each deployment twice and prints, in the `Bundle hash` step's log, every file's
SHA-256 and the bundle's; the run's artifact carries the same list as `dist.SHA256SUMS`. To rebuild one:

1. Check out the revision the run built: the full commit SHA its log prints as `BRIDGE_REVISION` (for a pull
   request, that is GitHub's merge commit, not the branch's head).
2. Use the Node version pinned in `package.json` (`engines.node`, which CI reads too), and the npm it ships with.
   The build warns when another Node runs it.
3. Build with the same settings, the revision spelled out in full: the page shows it, so it is part of the hash,
   and the default (`git rev-parse HEAD`) is right only on a clean checkout of that commit.

```sh
npm ci --ignore-scripts
BRIDGE_DEPLOYMENT=production BRIDGE_REVISION=<full commit sha> npm run build
node scripts/hash.mjs dist        # compare with the run's list, file by file; the last line is the bundle's
```

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
- **Pinned values**, shown in the page: the contract's address, its `reserve()` and the keccak256 of its runtime code
  are pinned per network, and the code hash, `token()` and `reserve()` are checked before anything is signed.
- **The chain, before every send**: the wallet may switch chain at any moment, so the chain is checked right before
  every transaction and every receipt wait, and every transaction names the network's chain, so viem checks
  `eth_chainId` too. Only a wallet that does not implement typed data (4200, -32601) falls back to `approve`.
- **A locked review**: at the Deposit click the page takes a snapshot of the recipient, the amount and the refund
  address, shows it, and disables every input until the deposit ends. The recipient is checked on Kreivo again on
  the snapshot, the snapshot is what is sent, and after the receipt the page shows the `Directed` log's recipient
  (as SS58), amount and refund, and fails loudly if any differs from the snapshot.
- **The recipient check** reads Kreivo with a deadline: a light client that does not answer fails closed with a
  message, and the page stays usable. It also refuses an account at Kreivo's `MaxConsumers` (16), which a first
  mint of bUSDC cannot take a consumer reference from.
- **No CCIP-Read**: viem's clients set `ccipRead: false`, and its CCIP code is left out of the bundle.

## Licence

`Apache-2.0 OR MIT` (amendment 0029, Open 12). The bundle also carries smoldot (GPL-3.0-or-later with the Classpath
exception) in its worker, and Kreivo's (GPL-3.0-only) and Kusama's chain specifications, each as its own file;
`docs/oss/LICENSING.md` §4.4 says why that is compatible and what a release owes. `THIRD-PARTY-NOTICES.txt` in the
build lists every bundled package and shipped file with its licence, and carries the GPL's text and the Classpath
exception's; a bundled package with no licence notice fails the build.
