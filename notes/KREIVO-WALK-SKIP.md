<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Kreivo walk: skip stretches proven unchanged

Design note for `F-007` (the Kreivo endpoint's historical walk). Informative; `SPEC.md` wins. It answers one request from
@pandres95 in chat on 2026-11-06: the walk must not take hours ("There's NO way we should wait for hours"). Production
reads about 38,000 blocks an hour, and about 500,000 were left.

**Verdict: sound, with one residual the owner has to accept, and only after an amendment.** The residual is the issuing
network's Root (§5). The amendment is to `REQ-OB-3` (§7). Until it is ruled the mode ships switched off
(`skip_unchanged = false`).

## 1. What the walk derives from the blocks it reads

These are read from `main` at `b7e5260` (`observe.rs`, `chain/light.rs` `block_events`, `proof.rs`, `supply.rs`, `main.rs`).

| Derived | Where from | Does the walk read it from old blocks? | Covered by the fingerprint? |
|---|---|---|---|
| Observations (`observations`, kind `log`): every `Contracts::ContractEmitted` of the configured contract, decoded, with its seq checked | `System::Events` of each block | yes | **yes**: the contract's storage root (F2, below) |
| The `final` outcome of each one (`observation_outcomes`) | the finalized head the step reads under | no: one row per observation | follows from the row above |
| Each observation's block time | `Timestamp::Now` of a block **that has an event** | only for those blocks | blocks with events are always read block by block |
| `observation_ranges`, outcome `read` | the step | it is the record itself | a skipped stretch is one `read` row (§7) |
| `observation_ranges`, outcome `gap`: a seq jump, a contract event that does not decode, a block that cannot be read, an event of **any** pallet that does not decode, a header that does not verify | the step | yes | a failed proof is still a gap; see the one intended difference below |
| `observation_walk_anchors` | the walk back from the finalized head | headers | anchors are kept; each skipped stretch's upper end is anchored too |
| Extrinsic results (`ExtrinsicSuccess`, `ExtrinsicFailed`) | `System::Events` | **read, then dropped by the follower** | nothing to cover |
| The delivery account's nonce, our transactions' inclusions (kind `transaction`), non-execution proofs | the **prover**, from the finalized head back to each transaction's era, with its own reads | **no** | nothing to cover: the prover does not depend on the watermark |
| Supply readings, `final_heads`, (on `develop`) the fee position | `Supplies` at **the finalized head only** | **no** | nothing to cover |
| `Assets` events (mint, burn, transfer of the bridged assets) | nowhere | **no** | nothing to cover |

So the walk derives exactly one thing from old blocks: the contract's events, plus the times of the blocks that hold them.

**The netting case does not hold for the current code.** The request assumed that today's walk sees a mint and a burn of
the same amount by an asset's admin outside the contract "as events". It does not: no `Assets` event is read anywhere,
so the full walk and a skipping walk both miss a netted pair. A pair that does not net is caught the same way in both:
by the supply readings at the next finalized head (`INV-4`, the component's count against the issuance). The tests run
the netting case as a parity check (both walks record the same thing), not as a gain.

**One intended difference.** Today a block becomes a gap if *any* event in it does not decode, even one of an unrelated
pallet (issue #102 was that). A stretch whose two ends prove the contract's storage unchanged holds no event of the
contract, so the skip records it as read. The proof is the reason, not a guess.

## 2. The fingerprint

It is read at each end of a stretch: one or two `state_getReadProof`s per end, each verified against the state root of a
header that hashes to the block asked for. Keys are computed, and values decoded, with **that end's own runtime's
metadata** (`chain::runtime`, by `System::LastRuntimeUpgrade` and the proven hash of `:code`). The head's metadata is
never used for an old end.

| # | Value | Why | Load-bearing |
|---|---|---|---|
| F1 | `System::LastRuntimeUpgrade` (raw) and the hash of `:code` | Same runtime at both ends, and none in between: `spec_version` only rises through `set_code`. The runtime must also be one whose code-change paths were read (Kreivo 0.17.1, spec 134, `87f8728`; 0.18.1, spec 136, which does not touch `config/contracts` or `contracts-store`). Any other runtime is read block by block. | yes |
| F2 | `Contracts::ContractInfoOf(contract)` (raw; its `code_hash` must be the pinned one) and the contract's **child-trie root**, the main-trie value at `:child_storage:default:` ‖ `trie_id` | Every event the contract emits takes a number from `next_seq()`, which adds one to `event_seq`, a storage field (17 `emit_event` calls, 17 `next_seq()` calls, `saturating_add`). A refused message reverts its storage and its events together (pallet-contracts runs each frame in `with_transaction` and rolls back on `did_revert`). So an event between the ends means `event_seq` differs, so the contract's storage differs, so its child-trie root differs. Equal roots mean no event. The root is one main-trie key, so no child-trie proof is needed. | **yes** |
| F3 | `ContractsStore::ContractAccount((app, license))` (must be the contract) and `ContractsStore::Apps(app)` (raw; its `code_hash` must be the pinned one) | The contract has no `set_code_hash` and no `delegate_call`, but Kreivo's contracts store can change an instance's code without Root. The app's publisher calls `publish_upgrade`, and the licence owner calls `upgrade`. A swap A→B→A, with B emitting events that touch no storage, would leave F2 equal. Every publish adds one to `AppInfo.version` (`checked_add`), and `upgrade` refuses when the app's code is the instance's. So, with the app's code equal to the pinned hash at both ends and `Apps(app)` equal, no swap went through the store. | **yes** |
| F4 | The delivery account's nonce (decoded) and each bridged asset's `supply` (decoded) | Asked for, and cheap (same proof). They cover nothing the walk records (§1). They only make the bisection stop at blocks where they change. Decoded fields are compared, not the raw `AccountInfo`, so a balance change does not count. | no |

A stretch is skipped only if every value is equal at both ends and every "must be" holds at both. Anything absent,
undecodable, of an unknown runtime, or served without the hash of `:code` (the light-client side) means "cannot tell",
and the stretch is read block by block.

## 3. Ends, hashes and order

- **The ends are the state after block `w` (the watermark) and after block `b`.** Equal fingerprints prove blocks
  `w+1 … b`, which are recorded as one `read` range (split at `max_range`). There is no skip without a watermark: the
  first start reads `start_block`, the instantiation, normally.
- **Only final, verified blocks are ends** (`REQ-OB-2`, `REQ-OB-5`). `b` is the rung's top, reached from the light
  client's finalized head the way anchors are now. Every other hash (the watermark's, and each bisection midpoint's)
  is `System::BlockHash(n)`, proven in the state of a verified block above it, and its header is then fetched and must
  hash to it with number `n`. Kreivo keeps the last 4,096 (checked live: present at `B-4096`, absent at `B-4097`),
  which is one rung. A hash proven absent falls back to walking headers. Nothing assumes 4,096.
- **The walk to a rung's top jumps.** Anchors are reached by `System::BlockHash` from the anchor above (one proof per
  4,096 blocks) instead of one header per block. Without the jump the skip would still walk 500,000 headers.
- **Bisection is depth-first, left half first**, so ranges reach the store in order (its trigger refuses a range that
  starts past the watermark plus one). A stretch of at most `SKIP_LEAF` (16) blocks whose ends differ is read block by
  block the normal way: header walk, events, seq check.
- **Reorgs.** Every end is at or below the finalized head and reached from it by proven hash. Nothing above the
  finalized head is ever an end, so no reorg can undo a skip.

## 4. Upgrades inside a stretch

F1 differs across one, so the stretch is split until the upgrade sits inside a leaf, which is then read block by block
with each block's own metadata (issue #102). In the block that applies an upgrade, `:code` is already new while
`LastRuntimeUpgrade` names the old runtime, and the next block has both new. Both differ from their neighbours, so the
bisection stops on them. Each end is read with its own runtime's keys and decoding, so a storage layout that changed
between the ends cannot be misread as "equal".

## 5. What the fingerprint cannot see: Root on the issuing network

Each of these needs Root, and none leaves a witness that rises monotonically:

- `Contracts::set_code` straight from Root (not through the store): A→B→A, B emits and touches no storage;
- `System::set_code_without_checks`: the same for the runtime;
- `System::set_storage` on the keys above.

The full walk would record such events, and a skipping walk would not. Kreivo's Root (its governance, or the relay
chain) can already mint a bridged asset outside the contract, so it is inside the trust base today. But this is a
specific new blind spot: **the owner accepts it or the mode stays off.** Forged events from another code under the
contract's account are not bezent's events, and recording them would not be right either.

## 6. Fail closed

| Condition | Outcome |
|---|---|
| A fingerprint the chain cannot give (light-client range, unknown runtime, no store app configured, a value absent) | that stretch is read block by block |
| A fingerprint read that is unavailable | the rest of the rung is read block by block (whose own failures are gaps, as now) |
| A proof that does not verify, or a proven hash whose header is another block | a **gap** from the stretch's first block, with the reason |
| Ends differ | split; a leaf is read block by block |

## 7. `SPEC.md`: does a skipped stretch count as read?

`REQ-OB-3` asks that observation be "complete over contiguous ranges of blocks", and forbids moving past "a range it
failed to read". It does not say that reading a range means fetching every block's events, but `F-007` and `AC-D2.3`
have always meant that. Recording a range as read because of a proof about its two ends is new behaviour.
`CLAUDE.md` says new behaviour goes through an amendment first, so here is the draft. The next free number across both
repositories' branches is **0027** (0024 to 0026 are on `develop`).

> # Amendment 0027 — a range proven unchanged is a range read
>
> **Status:** draft, unruled. **Target:** `SPEC.md` `REQ-OB-3` (and `AC-D2.3`'s wording); `PLAN.md` `F-007`.
> **Date:** 2026-11-06. **Cites:** the owner's ruling in chat, 2026-11-06, that the issuing network's historical walk
> must not take hours ("There's NO way we should wait for hours").
>
> **Rationale.** A walk that fetches every block's events reads about 38,000 blocks an hour, so a backlog of 500,000
> blocks blocks every other duty for half a day. Nearly every stretch holds nothing bezent observes, and that can be
> proven at the stretch's two ends.
>
> **Change.** Append to `REQ-OB-3`:
>
> "A range of blocks MAY be recorded as read without reading each block when bezent proves, at two final blocks of the
> network, the one before the range and the range's last, that every value it derives from the range is unchanged:
> each value read with a proof against a verified header of that block, under the runtime that wrote that block's
> state, and each one a value that every event bezent observes from the range would change. A range whose proof
> cannot be made is read block by block. A proof that does not verify is a visible gap. The values that make the proof,
> and the paths by which an observed event could leave them unchanged, MUST be stated per network in the plan."
>
> **Residual stated.** On the issuing network the stated values are the contract's storage root, the contract's
> instance and its store app's version, and the runtime (`PLAN.md` `F-007`). Root on the issuing network can swap the
> contract's code and back, or rewrite storage, within a range, and a proven range does not see it. The owner accepts
> this residual by ruling this amendment.

## 8. Cost, measured

On the archive node, read-only, 2026-11-06:

- The contract's child-trie root is the same at 39,867,043 (its instantiation), 40,100,000 and 40,150,000. Between
  40,150,000 and the head (40,607,379) the fingerprint changes at four blocks only: `:code` at 40,163,713, the runtime
  at 40,163,714 (spec 134 to 136), and the contract's storage at 40,216,930 and 40,262,203. A bisection found these
  with 54 block probes in 65 s, from a client far from the node.
- So the backlog is about 120 rungs that each cost a few proofs, plus about four leaves that are read in full. That is
  minutes, not hours. The live replay in the pull request has the measured times.
