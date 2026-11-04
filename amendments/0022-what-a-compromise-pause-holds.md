<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0022 — what a compromise pause holds, and the chain half of it

**Status:** proposed by the issue #77 helper, 2026-11-04. **Not ruled and not applied to `SPEC.md`.** The pull request for
issue #77 builds what `REQ-IC-2`, `REQ-IC-4` and `AC-I1.2` already require, and none of the options below.
**Target:** `SPEC.md` v0.5.4 (`REQ-GV-1`, `REQ-IC-2`, `REQ-IC-4`, `INV-15`, `NFR-8`, §7.F, §10 `CND-Paused`)
**Date:** 2026-11-04
**Needs:** a decision item on bezent's form (none exists yet). Numbered 0022 because 0016 to 0021 are taken across the
repository and open pull requests; renumber if that changes.
**Rationale in one line:** a detection now pauses bridge new work at once, but the specification does not say whether an
intent recorded before the pause and not yet signed is new work, or whether bezent itself must reach for the on-chain
pause, which is the only thing that stops a stolen key.

## The gaps

1. **Where new work ends.** `REQ-GV-1` and `INV-15` stop "new work only" and let "transfers in flight" finish; `CND-Paused`
   says the same. Neither term is defined. The core's gate treats everything before the first recorded signature of an
   attempt as new work: it holds the transfer in `Source-Final` and records no further intent (and since issue #77, an
   attempt with a recorded signature passes the pause as in flight). The endpoints and the signer read no pause. So a
   signing intent recorded before the pause commits is still signed and submitted after it. Normally that is the next
   poll of the endpoint, a window of seconds, but it has no bound: an endpoint or signer down at the pause signs that
   intent when it returns, hours into the incident, while the core reports the transfer as `CND-Paused`.
2. **The chain half.** §7.F gives the on-chain component a pause (pause role or governance) and `AC-I2.1` has a chain
   half, but `REQ-IC-2` and `REQ-IC-4` say only that "bezent" pauses. A stolen key signs by itself: no off-chain pause
   stops it. What stops it is the component's pause, the vault's guardian and, as the backstop, the bounds of
   `REQ-KY-3`. Today each of those is a person's act.
3. **"The affected routes".** `REQ-IC-2` says "the affected routes"; `REQ-IC-4` and `AC-I1.2` say "pause". The build is
   global, which is `F-005`'s recorded reading and always covers the affected routes, at the cost of more outage than a
   narrower pause when the alert is about one origin network's key.
4. **The time from detection to pause.** `NFR-8` bounds a pause once it exists, not how long a detection may take to
   become one.

## Options

### A1. A recorded intent that is not yet signed

- **A1a. It is in flight.** What runs today; nothing to build. The window above stays, and the core's `CND-Paused` can
  sit beside a signature made during the pause.
- **A1b. It is new work: an attempt is in flight from its first recorded signature.** The signer refuses a mint or payout
  of a deposit or withdrawal whose attempt has no signature and whose route a pause covers; the endpoint keeps it and
  asks again; a replay of a recorded signature, refund mints, cancels and statements are untouched. It takes one store
  function (`SECURITY DEFINER`, because the signer reads neither `pauses` nor `transfers`) and one refusal. It bounds the
  window at `NFR-8` for every pause, not only a compromise's. It stops neither a stolen key nor a compromised signer: its
  gain is that a healthy bezent stops adding transactions during an incident.

Recommended: **A1b**. Wording, for `REQ-GV-1`: "New work is everything before an attempt's first recorded signature. An
attempt with a recorded signature is in flight."

### A2. The on-chain pause

- **A2a. A person, from the runbook.** What runs today (`deploy/compromise-pause.md`). The pause-role holder and the
  guardian are paged by the same alert.
- **A2b. bezent pauses the on-chain component.** A new intent purpose, signed by the pause-role key (`REQ-HN-9` keeps it
  apart from the delivery key; `docs/key-custody.md` already allows it in the signer's process), ordered by the core or
  the raiser on a global compromise pause. It is the one thing that would stop a stolen delivery key's mints inside the
  window without a person. Only governance can unpause the contract (`AC-I2.2`), so a false alarm costs a governance
  action, and some raisers can fire falsely today (`features/F-005-signer/NOTES.md`, "The compromise pause", the table).
  It does not extend to the vault: the guardian is a hardware key or a multi-party account by design (`F-008` NOTES).

Recommended: **A2a now; A2b once the raisers flagged in that table are hardened.** Wording if A2b: add to `REQ-IC-4` "and
pause the on-chain component of the issuing network".

### A3. The scope of a compromise pause

- **A3a. Global, always.** What is built. Wording for `REQ-IC-4`: "pause, globally".
- **A3b. Narrow it where the detection can say.** A key of one origin network pauses that network's routes; the issuing
  network's key, and anything about the signer itself (`source: signer-link`, `signer-guard`), stay global, since every
  route mints or refunds there or signs there. It keeps deposits and other networks running during an incident in one lane, and
  rests on the keys being independent, which custody on one signer does not establish.

Recommended: **A3a.**

### A4. A bound on detection to pause

Add to `NFR-8`: "A pause a detection requires MUST be in force when the detection is recorded, in one transaction with
it, whichever component detected it and whether or not any other component is running." It states what is built, so no
code waits on it.

## What this changes in `SPEC.md`

Nothing until ruled. The options above name their own wording. `INV-15` and `AC-I2.1` stand under each.

## What waits

- A1b is not built. The test `ac_i2_1_an_attempt_signed_before_the_pause_is_in_flight_and_moves_on` records today's
  behaviour and changes with the ruling.
- A2b is not built.
