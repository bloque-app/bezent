<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# 01 — evmhot

Evidence, not doctrine. Read 2026-09-30. Paths relative to `evmhot` (branch
`feat/postgres-storage-driver`, `1cc7fc7`) unless stated.

## Which copy matters

- **Production** runs the copy vendored in substrate-rail: `substrate-rail/crates/evm-hot-wallet`
  (`SOURCE.txt`: evmhot `feat/postgres-storage-driver-v2`, `23be9d9`, not in the local clone), served at
  substrate-rail `/evm/*` (`crates/rail-api/src/routes.rs:169-173`). It is multi-chain (`src/config.rs:52,
  125, 139-143`, per-token treasuries), uses a sequential index counter (`src/lib.rs:590-646`,
  `migrations/V3__next_index_counter.sql`), batched catch-up with `getLogs` bisection over allow-listed
  tokens (`src/monitor.rs:103-157, 296-312, 353-380`).
- `origin/feat/evm-hot-v2` (June 2026): `chains.toml` with `chain_id`, per-chain treasury,
  `allowed_token_addresses`, `min_deposits`; a `webhook_deliveries` outbox with lease and retry; admin
  retries behind a bearer token. `/register` and `/block_number` still open.
- The checked-out standalone branch is single-chain legacy.

## Architecture

Single Rust binary (axum, alloy 0.1). Monitor `src/monitor.rs`, sweeper `src/sweeper.rs`, faucet
`src/faucet.rs`, API `src/api.rs`, storage trait `src/db/mod.rs:25-117` with redb and Postgres drivers.

## Deposit addresses

- BIP-44 HD EOAs from `MNEMONIC` (`wallet.rs:16-31`). No CREATE2 forwarders.
- Legacy index = `DefaultHasher(id) & 0x7FFFFFFF` (`lib.rs:404-408`): no collision check, not stable across
  Rust versions (fixed in the vendored copy).
- `accounts(id, derivation_index, address, webhook_url)`; re-registering an id upserts (`pg.rs:96-103`).
- Faucet funds each new address (`lib.rs:424-484`).

## Detection

- HTTP polling from cursor to `head - BLOCK_OFFSET_FROM_HEAD` (default 20) (`monitor.rs:39-76`). WebSocket
  mode processes each head with **no offset** and moves the cursor to head (`monitor.rs:519-527`).
- No `finalized`/`safe` tag anywhere (vendored copy included). No block hash stored; cursor is a number
  (`pg.rs:180-198`). No reorg handling.
- Recorded per ERC-20 deposit: tx hash, log index (`unwrap_or(0)`, `monitor.rs:241`), amount, token. No
  block number, no block hash. Dedup key `"{tx_hash}:{log_index}"` (`pg.rs:249`).
- Silent gaps: block `None` still advances (`monitor.rs:81-153`); `getLogs` empty after 30 retries is
  accepted as empty (`monitor.rs:295-326`); WebSocket failures only logged (`:524-526`); fresh DB starts at
  head with no backfill (`:46-50`); unauthenticated `POST /block_number` moves the cursor (`api.rs:48`).
- Native deposits: top-level `tx.to` only, receipt status unchecked (`monitor.rs:81-146`).

## Sweeps

- Sequential loop over `detected` rows (`sweeper.rs:87-228`). ERC-20 sweeps send full `balanceOf`
  (`:350-380`). Native sweeps take the whole balance, including faucet gas (`:241-312`).
- Nonces from the pending count per transaction (`with_recommended_fillers()`); faucet races
  (`docs/fix-p1-faucet-nonce-race.md` on v2).
- Signed transactions are not persisted; DB updated after `get_receipt` (no timeout); receipt status
  unchecked, so a reverted sweep is marked swept. A crash between broadcast and update eventually marks
  the row swept with no hash (`sweeper.rs:361-376`).
- `swept` is overloaded: also set for unknown tokens and after 10 zero-balance checks.

## Keys

`MNEMONIC` and `FAUCET_MNEMONIC` in environment variables (`config.rs:44-46`); the treasury is only an
address, its key is not held by the service.

## Outbound

None. No payout endpoint. `POST /verify_transfer` is read-only, has no depth rule, and reports an unmined
native transaction as success (`lib.rs:209-243`).

## Interface

Unauthenticated HTTP on `0.0.0.0`: `/health`, `/register`, `/verify_transfer`, `/block_number` (GET/POST).
Webhooks `deposit_detected`, `deposit_swept`, `faucet_funding`, static bearer token, fire-and-forget in the
legacy copy (`monitor.rs:374-430`, `sweeper.rs:559-668`).

Consumers: the Swaps platform's mediums registers addresses and maps webhooks
(`server/services/mediums/src/infrastructure/mediums/evm/index.ts:104, 155-241, 389-480`); Swaps'
`check-evm-deposit` calls `verify_transfer`.
