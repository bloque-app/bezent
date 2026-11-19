<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0032 — the EVM endpoint reads through a pool of providers, a rotating pair per pass, and a step of the walk is bounded

**Status:** **Accepted** by @pandres95 in chat, 2026-11-19: "the best would be to adopt evmhot's approach: the
round-robin pool." Applied in `PLAN.md` v0.6.14 by this pull request, for release v0.5.1. `SPEC.md` is unchanged.
**Target:** `PLAN.md` v0.6.13 (`DEC-12`) on `develop`; `SPEC.md` v0.5.14 stands as it is (no requirement names the
number of providers).
**Date:** 2026-11-19
**Needs:** nothing further.
**Rationale in one line:** two fixed providers leave the endpoint blind when either runs out or rate-limits; a pool
read as a rotating pair keeps every rule of the cross-check and survives one provider failing.

## Context (the incident of 2026-11-17 to 2026-11-19)

- The EVM endpoint read Polygon through exactly two providers (`DEC-12`; configuration `[[providers]]`, exactly two).
  Provider A was Alchemy's free tier. It ran out on 2026-11-17 and answered HTTP 429 to every request. The start check
  (`AC-I4.1`) failed in a loop, and bezent was blind on Polygon for two days.
- The providers were switched by configuration to two public nodes, Tenderly's public gateway and drpc. Public nodes
  rate-limit (HTTP 429) during a backlog walk.
- Second fault, same incident. The container health check beats only when `follower.step()` returns `Step::Read`, and
  one `step()` walked the whole backlog (about 120 000 blocks, 100 per range). That takes longer than the ECS start
  period (120 s) plus two retries of 10 s, so ECS killed the task in the middle of the walk (exit 137), repeatedly.
  Each range commits in its own transaction, so progress was kept, but the walk was slow and noisy.
- evmhot's reader (payment-rails, `server/services/swap/src/core/busdc/polygon-reader.ts`) takes a list of public RPC
  URLs. Each attempt uses the pair `(i, i+1 mod n)` chosen by a rotation counter, the two must agree exactly, and a
  failure means wait, with the next attempt rotating to another pair.

## The ruling

@pandres95, in chat, 2026-11-19: "the best would be to adopt evmhot's approach: the round-robin pool."

## A1. The pool (`DEC-12`)

`[[providers]]` becomes a list of **at least two**, with no upper bound. Each provider gains a required field,
`operator`, free text naming the company that runs the node (for example "drpc", "tenderly"). The configuration is
refused unless the providers have distinct names, distinct URLs and at least two distinct operators (compared ignoring
case and surrounding space), so at least one valid pair exists.

The "paid providers" wording of `DEC-12` is replaced by "independent providers from at least two operators, read as a
rotating pair". Independence is what the cross-check needs; payment was never the property.

## A2. A rotating pair per pass

A **pair** is two providers of **different operators**. The endpoint lists the valid pairs in a fixed order: every
`(i, j)` with `i < j` and different operators, in configuration order. It holds a rotation counter.

**Each pass of the main loop uses one pair for everything in that pass**: the follower, the submitter, the prover, the
fee and reserve reads and the network checks. The next pass uses the next pair, round-robin, and wraps. The pair never
changes inside a pass or inside one operation, so each rule of `DEC-12` holds as it did with a fixed pair:

- the cross-check at the finalized block, and a disagreement is unknown, never a mint (`REQ-NX-3`, `AC-C1.2`);
- an alarm rests on both providers of the pair (amendment 0022, P2);
- the nonce for preparation is the higher of the pair's two;
- receipts, logs and headers come from the pair.

A pass that fails on a provider (the follower stops with a gap or no final point) is followed by a pass on another pair
whenever the pool has one. The rotation moves on after every pass, as evmhot's does after every attempt. The pair of a
pass is logged at DEBUG only; when a pass failed on a provider error and the rotation moves past its pair, the
endpoint logs that at INFO.

## A3. Broadcast nodes

Unchanged (amendment 0023): signed transactions go to the public broadcast nodes and no others.

## A4. The start check leaves a provider out; it does not stop the start

`AC-I4.1` checks **every** provider of the pool. A provider that fails the check because it cannot be checked (it
fails to answer, for example HTTP 429, or has no block at the pinned height) is **left out of the pool for this run**,
with a WARN naming it and the reason. That is not fatal. The endpoint refuses to start only if no valid pair remains.

A provider that **answers the wrong chain id or the wrong hash at the pinned block** is a refusal to start, with the
text it had before: that is a misconfiguration, not an outage, and starting around it would hide it.

## A5. A step of the walk is bounded

One `step()` of the follower reads at most `max_ranges_per_step` ranges (a new field of the endpoint configuration,
default 20, at least 1; unknown fields stay refused), then returns `Step::Read` with the range it covered. The main
loop already calls `step()` until `CaughtUp` and beats the health check after each `Read`, so the heartbeat now beats
during a long walk. Every existing meaning is kept: a range that cannot be read is a gap (`REQ-OB-3`), the watermark
advances only over blocks recorded, each range is recorded in its own transaction, and `CaughtUp` is returned at the
head.

## What changes

- **`PLAN.md`:** v0.6.14. `DEC-12` is rewritten as above, and a version row is added.
- **Configuration:** `operator` on each provider (required), `max_ranges_per_step`, and at least two providers.
  Every example and fixture carries `operator` and still parses. A deployment that lists two providers adds the
  operator to each; nothing else changes for it.
- **`services/evm-endpoint`:** a `Pool` that hands out one pair per pass; the start check per provider; a bounded step.
- **`deploy/health.md`:** says a step is bounded.

## What does not change

- `SPEC.md` and every requirement in it.
- The cross-check inside a pair, the finality rule, the unknown-is-never-a-mint rule, the alarm rule of amendment 0022
  (P2), the nonce rule, and the public broadcast nodes of amendment 0023.
- The store and its migrations, the Kreivo endpoint, the core and the signer.
