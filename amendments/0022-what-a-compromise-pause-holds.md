<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0022 — what a compromise pause holds, and the chain half of it

**Status:** proposed by the issue #77 helper, 2026-11-04. **Ruled by @pandres95 on the decision form, 2026-11-06
(`d-pause-0022`): "As recommended".** That is A1b, A2a, P2 (a second provider's agreement before an EVM lane detector
raises) and P1 kept as built (every detector login keeps its power to pause globally, accepted for now). A1b is applied to
`SPEC.md` v0.5.7 (`REQ-GV-1`, §10 `CND-Paused`) and built (migration 0125, the signer); P2 is a plan matter (`PLAN.md`
`DEC-12`, guardrail 1) and built (the EVM endpoint); A2a and P1 change nothing. A3 and A4 were not on the item: they stay
proposed, and what is built for them is unchanged (a compromise pause is global; the pause is in force when the alert
commits).
**Target:** `SPEC.md` v0.5.4 (`REQ-GV-1`, `REQ-IC-2`, `REQ-IC-4`, `INV-15`, `NFR-8`, §7.F, §10 `CND-Paused`)
**Date:** 2026-11-04; ruled 2026-11-06, applied to `SPEC.md` v0.5.7 the same day
**Needs:** nothing further for A1b, A2a, P1 and P2. A3 and A4 need a decision item of their own. Numbered 0022 because 0016
to 0021 are taken across the repository and open pull requests.
**Rationale in one line:** a detection now pauses bridge new work at once, but the specification does not say whether an
intent recorded before the pause and not yet signed is new work, or whether bezent itself must reach for the on-chain
pause, which is the only thing that stops a stolen key.

## The gaps

1. **Where new work ends.** `REQ-GV-1` and `INV-15` stop "new work only" and let "transfers in flight" finish; `CND-Paused`
   says the same. Neither term is defined. The core's gate treats everything before the first recorded signature of an
   attempt as new work: it holds the transfer in `Source-Final` and records no further intent (and since issue #77, an
   attempt with a recorded signature passes the pause as in flight). The endpoints and the signer read no pause. So a
   signing intent recorded before the pause commits is still signed and submitted after it. Normally that is the next
   poll of the endpoint, a window of seconds. On the issuing network it ends with the transaction's era (`MAX_ERA`, 64
   blocks). On an origin network nothing expires, so it has no bound: an endpoint or signer down at the pause signs that
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

- **A1a. It is in flight.** *Not taken.* What ran until the ruling; nothing to build. The window above stays, and the core's `CND-Paused` can
  sit beside a signature made during the pause.
- **A1b. It is new work: an attempt is in flight from its first recorded signature.** *Ruled.* The signer refuses a mint or payout
  of a deposit or withdrawal whose attempt has no signature and whose route a pause covers; the endpoint keeps it and
  asks again; a replay of a recorded signature, refund mints, cancels and statements are untouched. It takes one store
  function (`SECURITY DEFINER`, because the signer reads neither `pauses` nor `transfers`) and one refusal. It bounds the
  window at `NFR-8` for every pause, not only a compromise's. It stops neither a stolen key nor a compromised signer: its
  gain is that a healthy bezent stops adding transactions during an incident.

Recommended: **A1b**. Wording, for `REQ-GV-1`: "New work is everything before an attempt's first recorded signature. An
attempt with a recorded signature is in flight."

### A2. The on-chain pause

- **A2a. A person, from the runbook.** *Ruled.* What runs today (`deploy/compromise-pause.md`). The pause-role holder and the
  guardian are paged by the same alert.
- **A2b. bezent pauses the on-chain component.** *Not taken.* A new intent purpose, signed by the pause-role key (`REQ-HN-9` keeps it
  apart from the delivery key; `docs/key-custody.md` already allows it in the signer's process), ordered by the core or
  the raiser on a global compromise pause. It is the one thing that would stop a stolen delivery key's mints inside the
  window without a person. Only governance can unpause the contract (`AC-I2.2`), so a false alarm costs a governance
  action, and some raisers can fire falsely today (`features/F-005-signer/NOTES.md`, "The compromise pause", the table).
  It does not extend to the vault: the guardian is a hardware key or a multi-party account by design (`F-008` NOTES).

Recommended: **A2a now; A2b once the raisers flagged in that table are hardened.** Wording if A2b: add to `REQ-IC-4` "and
pause the on-chain component of the issuing network".

### A3. The scope of a compromise pause (not on the item; not ruled)

- **A3a. Global, always.** What is built. Wording for `REQ-IC-4`: "pause, globally".
- **A3b. Narrow it where the detection can say.** A key of one origin network pauses that network's routes; the issuing
  network's key, and anything about the signer itself (`source: signer-link`, `signer-guard`), stay global, since every
  route mints or refunds there or signs there. It keeps deposits and other networks running during an incident in one lane, and
  rests on the keys being independent, which custody on one signer does not establish.

Recommended: **A3a.**

### A4. A bound on detection to pause (not on the item; not ruled)

Add to `NFR-8`: "A pause a detection requires MUST be in force when the detection is recorded, in one transaction with
it, whichever component detected it and whether or not any other component is running." It states what is built, so no
code waits on it.

### Points for the owner

Found in review of the pull request for issue #77. Both were on the item and are ruled: P1 kept as built, P2 built.

- **P1. Who may pause the bridge.** Every detector login (the core, both endpoints, the signer) can now pause it globally,
  by writing an alert under a `dedupe` of its own choosing (0049: free text, unique per detector). Nothing limits how many
  alerts, and so pauses, one login can make, and an alert is permanent. Before, the same act only paged a person. Options:
  keep it (a buggy or captured detector is an outage, not a loss); limit which detector may raise which alert kind, or the
  `dedupe` to a form the store can check; or let a detector request and have a second, independent signal confirm.
  *Ruled: keep it, accepted for now.*
- **P2. A second provider's agreement before the EVM `lane_use` and `lane_runs_code` raise.** `pair.rs` takes the higher
  of two providers' nonces (right for choosing a nonce, wrong for an alarm); `has_code` takes either provider's word; and
  `lane_use`'s `dedupe` holds the nonce, so a flaky provider that flaps between two values adds a row, and so a pause, each
  time. Requiring both providers to agree before either raises would turn a single bad RPC into no pause. *Ruled: build
  it.* Built, below.

## Ruling

`d-pause-0022`, @pandres95 on the decision form, 2026-11-06: **"As recommended"**, for each point the item put:

- **A1b.** A signing intent recorded before the pause and not yet signed is new work: held by the pause, not signed.
- **A2a.** The on-chain pause stays a person's act (`deploy/compromise-pause.md`, step 1). A2b is not taken.
- **P2.** A detected compromise on an EVM network needs the second provider to agree before it pauses. Until now one
  provider's word could do it, through `lane_use` and `lane_runs_code`.
- **P1.** Every detector login keeps its power to pause the bridge globally: accepted for now.

A3 and A4 were not on the item.

## Applied text (`SPEC.md` v0.5.7)

- **`REQ-GV-1`**, appended: "New work is everything before an attempt's first recorded signature. An attempt with a
  recorded signature is in flight." The wording A1b proposed, unchanged.
- **§10 `CND-Paused`**: "Work in flight continues; new work for this route waits, including an attempt recorded before the
  pause that has no recorded signature (`REQ-GV-1`)".

`INV-15` and `AC-I2.1` stand as they were: A1b says where new work ends, which they already stop. A2a, P1 and P2 change no
text: A2a and P1 are what `REQ-IC-4` and §7.F already allow, and P2 is about providers, which `SPEC.md` does not name
(guardrail 1); it is recorded in `PLAN.md` `DEC-12`.

## Built

- **A1b** (migration 0125, `services/signer/src/signer.rs`). The store's `pause_holds_intent(intent)`, `SECURITY DEFINER`,
  is the signer's only read of `pauses` and `transfers`: it says whether a mint or payout of a deposit or withdrawal is
  held, that is, a row of `pauses` covers its route (as the core's `gate` reads it: a row silent on a dimension matches
  every value, the global row included) and no intent of its transfer and attempt has a signature. The signer asks it
  last, after every guard that raises, inside its signing transaction and lock, and refuses with `REQ-GV-1`. It asks again
  immediately before the signature is recorded, in the same transaction, because the key backend's `sign` sits between the
  two reads and a pause may commit meanwhile; the signature is then dropped with the transaction and nothing is recorded
  (test `conditions.rs::ac_i2_1_inv_15_a1b_a_pause_committed_while_the_key_signs_holds_the_signature`). A replay of a
  recorded signature (returned before it asks), a refund mint, a refund's payout, a cancel and a statement are not held.
  The endpoints already treat a signer's refusal as "not signed": the intent stays, and is asked again on the next pass,
  and signed once the pause is lifted. The core already held the transfer in `Source-Final` as `CND-Paused`
  (`gate`). So for every pause, not only a compromise's, the window between the pause and the last new signature is the
  signer's next read of `pauses`, inside `NFR-8`, on every network; on an EVM network it was unbounded.
- **P2** (`services/evm-endpoint/src/pair.rs`, `submit.rs`, `signal.rs`). The lane detectors read each provider on its own
  (`Pair::latest_nonces`, `Pair::codes`). The compromise path is raised only when both report code at the lane, or both
  report its nonce past every nonce bezent signed plus one; the nonce raised on is the lower of the two. One provider's
  report, or a read that fails, is unknown: the endpoint raises nothing, prepares and signs nothing from the lane (as for a
  read that fails today), and pages `kind=lane-use-unknown`, a log line, at most once an hour per lane and read
  (`deploy/runbooks/pages/key-compromise.md`). The higher-of-two nonce that preparation uses, and the start-up check that a
  lane holds no code on either provider (which fails closed and raises nothing), are unchanged.
- Tests: `services/core/tests/conditions.rs::ac_i2_1_inv_15_a1b_an_intent_recorded_before_the_pause_is_not_signed_until_it_is_lifted`,
  `services/kreivo-endpoint/tests/delivery.rs::ac_i2_1_a1b_an_intent_recorded_a_moment_before_the_pause_is_not_signed_until_it_is_lifted`
  (which replaces the behaviour `ac_i2_1_an_attempt_signed_before_the_pause_is_in_flight_and_moves_on` recorded; that test
  now signs before the detection, as its name says), `services/core/tests/cancel.rs::req_nx_4_a1b_a_cancel_is_signed_under_a_pause`,
  and `services/evm-endpoint/tests/submit.rs::ac_i1_2_p2_one_provider_reporting_the_lane_used_outside_bezent_pauses_nothing_and_both_do`.

## Open after the ruling

- **A3 and A4** were not on the item. A compromise pause stays global, and is in force when its alert commits, as built.
- **`REQ-BE-6`'s mint pause** (mints of an asset paused while its surplus is under its floor) is not a row of `pauses`: the
  core's `gate` holds a deposit under it as `CND-Paused`, but the signer does not read it, so a mint intent recorded before
  the surplus fell is still signed. A1b speaks of a pause of a route; whether it reaches this one is not ruled, so it is
  left as built.
- **The EVM consumer raiser** (`proof.rs`, not exactly one lane transaction at a consumed nonce, flagged in
  `features/F-005-signer/NOTES.md`) is not changed; P2 named the lane detectors only. It already needs both providers:
  `block_transactions` and `nonce_at` (`pair.rs`) answer only when the two agree, so a disagreement or an error there is
  `Outcome::Pending`, with no page today. A proof that stays pending that way without a page is open, not changed by
  this change.
