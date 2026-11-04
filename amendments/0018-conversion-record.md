<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0018 — which convertible arrival a deposit records, and what holds `CND-AwaitingConversion`

**Status:** proposed by the `F-010` helper, 2026-11-04. **Not ruled and not applied to `SPEC.md`.** Nothing in the
`F-010` pull request depends on it: the parts of `US-A5` that need it are not built (see "What waits").
**Target:** `SPEC.md` v0.5.4 (§5.10 `REQ-CV-1`, `REQ-CV-2`, `REQ-CV-6`; §5.4; §7.A `REQ-RQ-3`; §10 `CND-AwaitingConversion`;
`AC-A5.1`, `AC-A5.2`)
**Date:** 2026-11-04
**Needs:** a decision item on bezent's form (none exists yet; `q-f010` only queues the build). Numbered 0018 because 0016 and
0017 are proposed in pull request #66; renumber if that changes.
**Rationale in one line:** `REQ-CV-2` says the deposit "records the convertible arrival and the converted output, with both
amounts", and `REQ-CV-1` says the convertible arrival "is held in `CND-AwaitingConversion`"; the specification never says
which arrival a given conversion's output belongs to, or what object holds the condition, and nothing on chain can say it.

## The gap

1. **No link on chain.** A convertible arrival and the conversion that spends it are separate transactions. The swap spends
   the vault's pooled balance of the convertible, not a particular arrival's. Matching an arrival to an output by amount or
   by time is barred (guardrail 5, `REQ-OT-2`, `REQ-RB-4`), so the last sentence of `REQ-CV-2` has no defined way to say
   *which* arrival a deposit records.
2. **No holder for the condition.** A named condition is held by a transfer in a state (§5.4: "A transfer that cannot
   proceed … holds a named condition"). A convertible arrival is not a transfer. `REQ-RQ-3` and `REQ-IN-3` bind an arrival
   *of an admitted token*; the store refuses a deposit that names a `convertible_arrival` (`REQ-CV-1`; migration 0034,
   `crates/store/tests/arrival_kind.rs`), and `REQ-IN-6` already gives a final arrival that cannot become a deposit the
   condition `CND-Unassociated`. `AC-A5.1` ("it holds `CND-AwaitingConversion`") and `REQ-IN-6` therefore name two
   conditions for the same arrival.
3. **A deadline with nothing to attach to.** `REQ-CV-6` sends "a conversion that does not complete by its deadline" to
   `Needs-Review`, and §5.4 lists `Source-Final` with `CND-AwaitingConversion` past its deadline. No transfer can be in that
   state today, so the engine's branch for it (`services/core/src/engine.rs`, `source_final`) is unreachable in production;
   its test (`services/core/tests/conditions.rs`) reaches it only through a hand-inserted transition.
4. **"Both amounts" is two readings.** The convertible arrival's amount (what reached the vault) and the convertible amount
   the swap spent (the conversion input, already recorded in the output's `detail.conversion.amount_in`) differ whenever
   Swaps converts more or less than one arrival.

## Options

None is recommended here; the owner picks.

**A1 — which arrival, and what holds the condition**

- **A1a. The request follows the conversion and names both coordinates.** `REQ-RQ-3` gains a coordinate for the convertible
  arrival; a deposit in a convertible token is requested once the output is final, names the output as its source (what is
  minted against) and the convertible arrival as the arrival it records, each consumed once (`INV-2`). Until then the
  convertible arrival is an unassociated arrival whose condition is `CND-AwaitingConversion` (`CND-Unassociated` stays for
  every other kind), and `REQ-CV-6`'s deadline runs from its finality and acts on the arrival, not on a transfer
  (a review item on the arrival, like the unassociated arrival's owner of `REQ-IN-6`). No state is added.
- **A1b. The request comes first.** A deposit may name the convertible arrival as its source and begins at `Source-Final`
  holding `CND-AwaitingConversion`; a later extra, by request reference (as `REQ-RQ-4` does for a withdrawal), names the
  conversion's output, and the transfer's source becomes the output (the arrival is recorded, not consumed for minting).
  This is §5.10 and §5.4 taken at face value, and `REQ-CV-6` applies to the transfer unchanged. It needs a store change:
  a deposit's source moves once, from a convertible arrival to an output, with both consumed.
- **A1c. Neither: report the pair from the swap alone.** The deposit records the output and the conversion input that the
  output's own transaction shows (the conversion's input leg, observed at its coordinate), and no convertible *arrival* is
  recorded against it; `REQ-CV-2`'s sentence is reworded to "the converted output and the convertible amount that
  conversion spent". `AC-A5.1`'s condition then belongs to the holdings (`REQ-CV-4`), not to an arrival.

**A2 — what bezent verifies about the pairing (A1a and A1b only).** The requester names the pair; bezent cannot check that
the arrival fed the conversion. Proposed: bezent verifies that both coordinates are final observations at the same reserve,
of the right kinds, that the arrival's token is admitted as a convertible for the asset and the output is the origin
token, and that neither is already consumed; causality is the requester's, as price and slippage are (`REQ-CV-5`). The
mint is against the output alone (`REQ-IN-4`), so a wrong pairing misreports the record and cannot mint more than was
delivered.

**A3 — the amounts reported (A1a and A1b).** Three: the convertible arrival's amount, the convertible amount the
conversion spent, and the output's. Mint is the output's, under every rule of §5.5 (`REQ-CV-5`).

## What the proposed text would touch

`REQ-CV-1`, `REQ-CV-2`, `REQ-CV-6`, `REQ-RQ-3` (A1a) or `REQ-RQ-4` (A1b); A1c leaves the request surface as it is, §5.4's `Source-Final` row, §10's
`CND-AwaitingConversion` description, `AC-A5.1` and `AC-A5.2`. The proposed wording is not written until an option is
chosen: three different texts would have to be kept in step.

## What is built meanwhile, and what waits

Built in the `F-010` pull request, none of it depending on this amendment: tests at the layers §9 names for what the
specification already defines (a convertible arrival is never minted against; a deposit is minted for exactly the output the
swap delivered, once final; the statement itemises unconverted holdings outside the equation; a real swap on a fork of the
network is observed as an input and an output with both amounts in the output's record).

Waits for the ruling, not built: the record of the convertible arrival with its amount against a deposit
(`REQ-CV-2`, last sentence); `CND-AwaitingConversion` as a condition an arrival holds (`AC-A5.1`, "holds" half); the
deadline of `REQ-CV-6` acting on a real conversion rather than a hand-inserted transition; the link between a refund of a
convertible arrival and a later conversion output (`features/F-002-ingress/NOTES.md`, "Left open"). Its migration, if any,
is 0065 or later.
