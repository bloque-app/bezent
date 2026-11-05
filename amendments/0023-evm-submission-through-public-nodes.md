<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0023 — signed EVM transactions are sent through public nodes

**Status:** ruled by @pandres95 in chat, 2026-11-05 (form item `d-evm-submit-public`): "Submit on public nodes, and read
on private ones." Built by issue #92. `SPEC.md` is unchanged.
**Target:** `PLAN.md` `DEC-12` (RPC for EVM networks); `SPEC.md` v0.5.4 (`REQ-EO-1`, `AC-D2.1`, `REQ-NX-3`) stand as they are
**Date:** 2026-11-05
**Needs:** nothing further. Numbered 0023 because 0020 is held by another pull request.
**Rationale in one line:** `DEC-12` says how an EVM network is read and nothing about where a signed transaction goes,
so the endpoint sent it to both paid providers; sending is the one call that decides nothing, and a public node can do
it.

## The gap

1. **What the plan says.** `DEC-12` reads each EVM network through two independent paid providers, cross-checked at the
   finalized block, and treats a disagreement as unknown. It is silent on `eth_sendRawTransaction`.
2. **What was built.** `Pair::send_raw` sent the signed bytes to both paid providers, so the paid subscriptions carried
   the writes as well as the reads.
3. **Why sending differs from reading.** A read can lie, and a lie can become a mint, so reads are cross-checked. A
   send carries bytes that are already signed and stored (`REQ-EO-1`). A public node cannot forge or alter a signed
   transaction (the signature covers it); the worst it can do is drop it or answer wrongly, and the endpoint sends
   the same bytes again on the next pass until the paid pair shows the outcome (`AC-D2.1`).

## The ruling

- **Only `eth_sendRawTransaction` moves**, to a list of public broadcast nodes per network. Everything that feeds a
  decision stays on the paid pair, cross-checked under `DEC-12`: the nonce, the fee and gas estimates, the dry run
  (`CND-PayoutWindow`), receipts, logs and headers.
- **Sending is public-only.** There is no fallback to the paid pair. If every broadcast node fails, the send is not
  taken, the bytes stay stored, and the next pass sends them again.

## How it is built

- **Configuration.** A plain, non-secret `broadcast = ["https://…", …]` list in each network's TOML. At least one entry;
  every entry `https://`, except that `http://` is accepted for a loopback address (anvil, tests); no duplicates; no
  credentials in a URL. A configuration that breaks these does not load. This is a breaking change to every endpoint
  configuration.
- **Sending.** The same bytes go to every broadcast node at once, each bounded to 10 s so that one hung node cannot
  stall one send. Re-sends go one row after another in nonce order, so a pass of them has its own budget of 30 s (a
  provider request is bounded at 30 s and a pass at 120 s by the health check); once it is spent the rest is left for
  the next pass and the count deferred is logged at info. The bytes are never changed, and never signed again.
- **A send is taken** when at least one node returns a hash equal to the `keccak256` of the signed bytes. A node that
  returns another hash has failed, and is logged at warn with its name and both hashes. The hash stored is always the
  one computed from the signed bytes; no hash a node returns is stored.
- **Start-up.** Each broadcast node is asked `eth_chainId`. A node serving another chain stops the endpoint with an
  error naming it. A node that cannot be reached is logged at warn and the endpoint starts.

## What this changes in `SPEC.md`

Nothing. `REQ-EO-1` (persist the exact bytes, then send them, and only ever re-send them) and `AC-D2.1` hold under it.

## What it costs

The paid providers no longer carry writes, and a public node may be slower or drop a transaction more often than a paid
one; the re-send absorbs that, at the price of a later inclusion. Two or three independent public nodes per network keep
one operator from holding a payout back. The privacy of what is sent is not a property of either choice: a signed
transaction becomes public when it is mined.
