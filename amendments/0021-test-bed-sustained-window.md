<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0021 — how long "sustained" lasts on the test bed

**Status:** proposed by the F-013 helper, 2026-11-04. **Not ruled and not applied to `SPEC.md`.** Nothing built depends
on it: `release/gate.toml` leaves the value unset, and the release gate fails on that line until the owner rules.
**Target:** `SPEC.md` v0.5.4 (§15 release scope; `NFR-5`, `INV-1`)
**Date:** 2026-11-04
**Needs:** a decision item on bezent's form (none exists yet). Numbered 0021 because 0016 and 0017 are proposed in pull
request #66, 0018 and 0019 are on `develop` and 0020 is proposed in pull request #71; renumber if that changes.
**Rationale in one line:** §15 makes "the reserve equation at exactly zero divergence, sustained" an exit criterion of
V0 on a test network, and no document gives the duration, so the criterion cannot be shown or refused.

## The gap

1. **What the specification says.** §15: V0's exit criteria include "the reserve equation at exactly zero divergence,
   sustained", and "nothing else in this document may be traded against them". `NFR-5` evaluates the equation at
   least once a minute per bridged asset, at zero tolerance. `INV-1` says it holds at every final point.
2. **What the plan says.** `DEC-19` gives 48 hours, but for the soak on mainnet, "on top of the test bed, not instead of
   it". For the test bed it repeats the criterion and gives no duration.
3. **What follows.** A release gate has to be able to refuse. Without a duration, "sustained" is whatever the person
   signing says, and no check can compare the evidence with a requirement.

## Options

- **A1a. A fixed number of hours of the bed's evaluations, all at zero.** For example 48 hours, the soak's own length, with
  `NFR-5`'s evaluations once a minute (2,880 consecutive evaluations per asset), and the clock restarting on any
  divergence. Trade-off: the bed must run the core for two days, with zombienet and the forks, on the self-hosted pool.
- **A1b. A shorter fixed window.** For example 24 hours, since the mainnet soak repeats it with real chains afterwards.
  Trade-off: a defect that shows only after a day (a daily statement, a window rollover of the contract's 24-hour mint
  window) may not be seen on the bed.
- **A1c. A window that must contain named events.** At least one rollover of each window (`CND-MintWindow`,
  `CND-PayoutWindow`), one daily statement and one reorganisation injected on the forks, whatever its length.
  Trade-off: more to specify and to script; closest to what could go wrong.

## Recommendation

A1a with 48 hours, or A1c with a floor of 24 hours. The choice is the owner's; the gate takes the value as an integer
number of hours in `release/gate.toml`, so A1c would need a second input.
