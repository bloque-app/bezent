<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0016 — an attested backend's delivery, and its report surface

**Status:** proposed by the `F-011` helper, 2026-11-03. **Not ruled and not applied to `SPEC.md`.** Draft PR #66 builds on
A1 and A3 as working assumptions, on instruction to keep working toward `M-3` while the ruling is pending; each sits in
its own migration and call so a contrary ruling removes it alone (`features/F-011-attested-backend/NOTES.md`). A2 is text
only.
**Target:** `SPEC.md` v0.5.4 (§5.4, §7.A, §8.2, `REQ-BE-5`, `REQ-BE-6`)
**Date:** 2026-11-03
**Rationale in one line:** §5.4 defines delivery by a transaction bezent signs, and an attested backend's payout is one
bezent never signs, so a fiat withdrawal has no normative way past `Source-Final`; and §7.A lists no request by which an
attesting party reports.

## The gap

`Delivering` means "a destination transaction is signed and recorded", and `REQ-TR-6` confines a transfer to the table's
edges. For an attested backend the requester relays the instructions (`REQ-RQ-9`) and pays on its rail; `REQ-BE-5` makes
its report of the payout the observation. Read literally, such a withdrawal stops at `Source-Final`: it never completes,
`REQ-OT-6`'s refund mint has no way to start, and `REQ-BE-1`'s "evidence of non-execution" has no carrier. The refund of an
unassociated arrival on an attested network (`REQ-RQ-6`) stops in the same place: the operator can release it, and nothing
delivers it.

## A1 — Recommended: read the §5.4 words per assurance class

- `Delivering` becomes "a destination transaction is signed and recorded, or, on an attested backend, the delivery is
  handed to the attesting party: the relayed instructions are accepted (`REQ-RQ-9`), or, for the refund of an unassociated
  arrival, it is released (`REQ-IN-6`)". The `Source-Final → Delivering` row reads the same.
- `REQ-BE-5` gains: "that requester's report of an arrival, a payout, **a payout's non-execution** or a reserve balance
  **is** the observation."
- `REQ-NX-1` gains "on a public backend", so the attested proof of non-execution is the party's report. `REQ-NX-2` is
  already public-only through `REQ-BE-4`.

`Delivered`, `Completed`, `Refunding` and `Refunded` then need no change: their words rest on a leg, which a report is.
`REQ-RF-5` applies as it stands: within the route's deadline a report of non-execution starts a new attempt, past it the
refund mint. A payout nobody reports goes to review at its deadline and is never refunded on the timer (`REQ-RF-1`).

*Trade-off.* A wrong report of non-execution can cause a refund mint beside a payout that happened. That is the same
trust `REQ-BE-5` already places in the party for a mint, and the mint window bounds it.

*Rejected: a terminal state of its own for fiat.* It forks every status reader and the sequenced log for one backend.

## What the code does where the amendment is silent

These are choices of the working build (#66), each stated so a ruling can replace it. They are not recommendations to
apply, except the one marked "Proposed".

- **The attempt's clock starts at the hand-over.** An attested attempt has no signed transaction, so its deadline cannot
  start there; `REQ-RF-5` and `REQ-RV-1` measure the route's budget from it. The build starts it when the attempt enters
  `Delivering`, at the attested network's latest final head (its latest report's time), whatever the attempt's source
  leg did, and whatever point the transition rests on. Without this a late relay, a late release of an unassociated
  refund or a late release out of review reaches `Needs-Review` on the first cycle, and a first report of non-execution
  after a late relay is a refund mint instead of a second attempt. The refund-or-retry choice of `REQ-RF-5` is measured
  from the first hand-over (the earliest one recorded for the attempt). The hand-over and its time are one act: with no
  final head on the attested network there is no clock, so nothing is handed over until there is one, and an attempt
  with no recorded hand-over time is never proven not executed, retried or refunded. Otherwise the first hand-over on a
  fresh backend would compare the attested network's report time with the issuing network's clock.
- **A withdrawal nobody relays for.** The value is destroyed, §5.4 has no edge for the wait, and `CND-AwaitingInstruction`
  was withdrawn (amendment 0007). **Proposed, not built:** it goes to `Needs-Review` at the route's deadline, measured from
  its source leg's finality, with an owner. **Built:** one page (`attested-withdrawal-unrelayed`) once it has waited the
  route's budget on the issuing network's clock, and nothing else: its state does not change, and a relay after the page
  hands it over as usual. Going to review is the question, not the page. The same holds for a withdrawal whose
  instructions are relayed but whose attested network has no final head to hand it over at (a fresh backend whose party
  has reported nothing): one page of its own (`attested-withdrawal-awaiting-head`), once, after the same budget on the
  issuing network's clock, with the state unchanged, and the first head hands it over.
- **Reversals once restored.** The sum of reversals that bounds a new one counts only those not restored, so a second real
  chargeback after a restoration is taken. A restoration binds again what a reversal held only once the core has recorded
  it, after it checked the signature, and not when it is taken in: a row nobody verified must not release a hold. The
  ingress and the store apply the same rule: a reversal is restored once its restoration is an observation (the store's
  `attested_arrival_reversed`, 0062). Until then the reversal still counts against the arrival, and a second full
  reversal of it that would fit once the restoration is taken in is not final (`503` `not-final`, nothing recorded, the
  identifier not spent): send it again. One that would not fit even then is `ERR-SourceMismatch`. What a deposit gets in that gap (`ERR-SourceConsumed`) is an open ruling.
- **The attested network's head and clock.** An attested network has a final head of its own, which is the latest report
  recorded, at the time the store took it in. Its deadline clock is that head, so a party that stops reporting stops the
  clock: a handed-over payout then never reaches its deadline, and only the snapshot bound pauses the asset and pages. The
  unrelayed withdrawal's page does not wait on it. A network of this backend is declared with `address_format = 'opaque'`
  (a destination is a commitment, not an address), and the core refuses to start otherwise.

## A2 — Recommended: add the report to §7.A

**`REQ-RQ-10`** **Report, for an attested backend:** the party's report of an arrival, a payout, a payout's non-execution,
a reserve balance, a reversal or a restoration, under its own identifier, authenticated as that party's (`REQ-BE-4`,
`REQ-BE-5`). Only the requester holding that backend's attesting grant may report. A report that names a transfer or an
earlier report is refused when that does not exist or does not fit (`ERR-SourceNotFound`, `ERR-SourceMismatch`,
`ERR-SourceConsumed`).

The surface exists in `F-011` at plan level (`DEC-14`). This records it where a reader of §7.A looks for it. Text only: no
identifier changes until it is ruled.

## A3 — Recommended: an arrival reversed before its mint

`REQ-BE-6` speaks of what was minted ("nothing already minted is clawed back"). It does not say what happens to an arrival
reversed before its mint. `REQ-BE-6` gains:

> While a reversal of an arrival stands unrestored, the arrival MUST bind no deposit and no refund (`ERR-SourceConsumed`),
> and a deposit already bound to it and not yet signed MUST hold `CND-Paused`. A delivery already signed goes on.

*Why.* Minting against a payment known reversed spends surplus for nothing, and refunding one pays it twice. A restoration
releases both, since the reserve is whole again.

*Trade-off.* `CND-Paused` is a route's condition in §10, and this holds one transfer. No other named condition fits and a
new one is a larger change, so the webhook dispatcher tells every requester granted on the route that `CND-Paused` is held
there, while the route still takes other deposits. With one attesting requester that is a misleading notice to the party
that caused it, no worse. A named condition of its own would be cleaner if this is ruled in.

## Identifier ledger

| Action | Identifiers |
|---|---|
| Revised, if ruled | `REQ-BE-5`, `REQ-BE-6`, `REQ-NX-1`, the `Delivering` state and the `Source-Final → Delivering` edge of §5.4 |
| Added, if ruled | `REQ-RQ-10` |
