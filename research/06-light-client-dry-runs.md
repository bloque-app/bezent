<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# 06 — Dry runs over a light client (PAPI + smoldot), tested on Kreivo mainnet

Evidence, not doctrine. Run on 2026-10-02 against Kreivo mainnet (spec 133), at finalized blocks #39 122 6xx.

## Setup

Scripts: `research/06-scripts/` (`dryrun.mjs ws|light`, `raw-smoldot.mjs`, `capture.mjs`, `compare.py`, the two `.wat`
contracts; build with `wasm-tools parse`). Run them from a directory whose `node_modules` has `polkadot-api`.


- `polkadot-api` 2.1.8 and smoldot (JS) 3.3.1, from `blosketools/node_modules`. Kusama from
  `@polkadot-api/known-chains` 0.11.7 from blosketools (checkpoint #34 274 259, so a long warp sync; aledgent's canary pins 0.12.4, #35 434 907); Kreivo from aledgent's
  `deploy/canary/chain-specs/kreivo.json` with the bootnodes replaced: its tcp/30335 entry did not connect from
  here, but the same peer over `/dns/boot-1.kreivo.bloque.network/tcp/443/wss` works (checked separately).
- `getSmProvider` takes a **factory**, `() => Chain | Promise<Chain>`. Passing a chain makes PAPI wait forever
  without an error. That cost one false "hang" in this test.
- Dry-run origin: the Treasury account `5EYCAe5ijiYfyeZ2JJCGq56LmPyNRAKzpG4QkoQkkQNB5e6Z`, used only as a
  dry-run origin. Nothing was signed or submitted.
- Test contracts: two hand-written wasm modules (171 and 203 bytes, `seal0.seal_deposit_event`) whose `deploy`
  emits one event with zero and with one topic. They were uploaded only inside `ContractsApi.instantiate` dry
  runs (`bare_instantiate` takes a plain account id; the origin checks are on the dispatchables).

## Results

| Runtime call | Over RPC | Over smoldot (raw `chainHead_v1_call`) | Over PAPI + smoldot | Byte-identical to the node |
|---|---|---|---|---|
| `Metadata_metadata_versions` | ok | ok, 186 ms | ok | yes |
| `Metadata_metadata_at_version` (406 KB) | ok | ok, 333 ms | ok | yes |
| `AccountNonceApi_account_nonce` | ok | ok, 168 ms | ok, 697 ms | yes |
| `DryRunApi_dry_run_call` (`remark_with_event`) | ok, 1 event | ok, 186 ms | ok, 1 event | yes |
| `ContractsApi_instantiate`, event, no topic | ok, 11 events | ok, 278 ms | ok, 11 events, same gas | yes |
| `ContractsApi_instantiate`, event, one topic | ok, 11 events | ok, 213 ms | ok, 11 events, same gas | yes |
| `chainHead_v1_storage` `:code` hash | — | ok, 223 ms, same hash as `state_getStorageHash` | ok | yes |

## What it says about aledgent's decision 36

aledgent concluded that smoldot cannot dry-run `execute`, because the node appends to `System::EventTopics`
"without reading the value". In `sp-state-machine` 0.53 the first append in a transaction calls `init()`,
which reads the backend (`overlayed_changes/changeset.rs:409`, `ext.rs:473`). So the node does record the
read, and the stated cause does not hold. A contract event with a topic dry-runs fine here.

Not tested here: aledgent's own `execute` (child-trie writes, asset moves through the chain extension and
`call_runtime`), and smoldot 0.19.4 as embedded in subxt. Those are where the earlier failure must be.
