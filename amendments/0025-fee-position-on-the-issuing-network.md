<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0025 — the fee position on the issuing network, and where its floor lives

**Status:** ruled by @pandres95 on the decision form, 2026-11-06 (`d-fee-position-scope`): "As recommended". He added:
"This will be soon be overriden by usage subscriptions, but in case we don't have it in time, let's do your
recommendation." Applied to `PLAN.md` v0.6.7 (`DEC-18`, `F-014`) and built. `SPEC.md` is unchanged.
**Target:** `PLAN.md` `DEC-18`, `F-014`; `SPEC.md` v0.5.7 (`REQ-ST-6`, `AC-H3.1`, the glossary's "fee position") stands as
it is
**Date:** 2026-11-06
**Needs:** nothing further. Numbered 0025 after 0024 (the release gate's scope), which this ruling shares a day with.
**Rationale in one line:** `REQ-ST-6` asks for fee positions to be observable and the glossary defines one only as the
native token an address needs to pay network costs, so which amount of a Substrate account that is, and which
configuration holds the alert floor, were the plan's to decide and were open (`F-014` notes, Open 2).

## The ruling

1. **On the issuing network, the fee position is the delivery account's free balance less its frozen balance.** Not the
   free balance alone (a frozen part cannot pay), and not the reserved part (a storage deposit is held from the account
   and is not spendable). Where frozen exceeds free the position is zero, and the reading keeps both figures.
2. **The alert floor lives in bezent's own configuration**, a floor per network, in the smallest unit of that network's
   native token. The core reads it and pages when a network's fee position is below it. A low balance is therefore a
   page like any other, with its runbook (`deploy/runbooks/pages/float-top-up.md`), and not an alarm that only the
   infrastructure holds.
3. **This is a stopgap, as the owner says.** Usage subscriptions are expected to replace the float; if they arrive in
   time this ruling is moot, and nothing built here depends on it beyond the reading, the floor and the page.

## What changes

- **`PLAN.md` v0.6.7:** `DEC-18` (the float's measure and where its floor lives) and `F-014`.
- **Built:** the Kreivo endpoint records the delivery account's fee position at each final head it records, as the EVM
  endpoint does since #83 (migration 0130); the core's configuration holds the floors and the core pages
  `fee-position-low`.
- **`AC-H3.1`** now has every `REQ-ST-6` observable on both networks at the service layer. Its entry in
  `e2e/criteria/known_unmet.toml` is not touched here: removing it is the claim that the criterion is met, and the spec
  owner reviews that.

## What the ruling does not say

- **An unknown or stale reading** (a network with a floor whose fee position cannot be read) raises no page of its own.
  It is the missing-data alarm on `fee_position_amount` and the growing `fee_position_age_seconds` that catch it
  (`deploy/runbooks/alerts.md`). A reviewer who wants a page for it adds a kind.
- **A network with no floor** is logged at start and has no page; the core does not refuse to start, so that a
  deployment's configuration can be updated after the release that adds the setting.
