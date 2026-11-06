<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0021 — how long "sustained" lasts on the test bed

**Status:** proposed by the F-013 helper, 2026-11-04. **Ruled by @pandres95 on the decision form, 2026-11-06
(`d-gate-sustained`): "24 hours".** That is A1b, a fixed window of 24 hours. It is applied to `SPEC.md` v0.5.8 (§15,
release scope) and built: `release/gate.toml` sets `sustained_divergence_free_hours = 24` and the release gate refuses
shorter evidence.
**Target:** `SPEC.md` v0.5.4 (§15 release scope; `NFR-5`, `INV-1`)
**Date:** 2026-11-04; ruled 2026-11-06, applied to `SPEC.md` v0.5.8 the same day
**Needs:** nothing further. Numbered 0021 because 0016 and 0017 are proposed in pull request #66 and 0018 to 0020 were
taken.
**Rationale in one line:** §15 makes "the reserve equation at exactly zero divergence, sustained" an exit criterion of
V0 on a test network, and no document gave the duration, so the criterion could not be shown or refused.

## The ruling

"Sustained" is **24 consecutive hours** on the test bed in which every evaluation of the reserve equation (`NFR-5`, at
least once a minute per bridged asset) is at exactly zero divergence. Any divergence restarts the window. The window is
the test bed's own: the soak on the live instance of `PLAN.md` `DEC-19` is another gate, 48 hours, and stands on top of
it.

- **`SPEC.md` v0.5.8, §15:** the exit criterion reads "the reserve equation at exactly zero divergence for 24 consecutive
  hours", with the restart rule.
- **`release/gate.toml`:** `[exit] sustained_divergence_free_hours = 24`. The gate fails on evidence of fewer hours and
  on a value other than a positive integer.
- **Not taken:** A1a (48 hours) and A1c (a window that must contain named events). A1c's events are still worth
  scripting on the bed; the ruling does not require them, and the gate does not read them.

## The gap (as proposed)

1. **What the specification says.** §15: V0's exit criteria include "the reserve equation at exactly zero divergence,
   sustained", and "nothing else in this document may be traded against them". `NFR-5` evaluates the equation at
   least once a minute per bridged asset, at zero tolerance. `INV-1` says it holds at every final point.
2. **What the plan says.** `DEC-19` gives 48 hours, but for the soak on mainnet, "on top of the test bed, not instead of
   it". For the test bed it repeats the criterion and gives no duration.
3. **What follows.** A release gate has to be able to refuse. Without a duration, "sustained" is whatever the person
   signing says, and no check can compare the evidence with a requirement.

## Options (as proposed)

- **A1a. A fixed number of hours of the bed's evaluations, all at zero.** For example 48 hours, the soak's own length, with
  `NFR-5`'s evaluations once a minute (2,880 consecutive evaluations per asset), and the clock restarting on any
  divergence. Trade-off: the bed must run the core for two days, with zombienet and the forks, on the self-hosted pool.
- **A1b. A shorter fixed window.** For example 24 hours, since the mainnet soak repeats it with real chains afterwards.
  Trade-off: a defect that shows only after a day (a daily statement, a window rollover of the contract's 24-hour mint
  window) may not be seen on the bed.
- **A1c. A window that must contain named events.** At least one rollover of each window (`CND-MintWindow`,
  `CND-PayoutWindow`), one daily statement and one reorganisation injected on the forks, whatever its length.
  Trade-off: more to specify and to script; closest to what could go wrong.

## Recommendation (as proposed)

A1a with 48 hours, or A1c with a floor of 24 hours. The choice is the owner's; the gate takes the value as an integer
number of hours in `release/gate.toml`, so A1c would need a second input.
