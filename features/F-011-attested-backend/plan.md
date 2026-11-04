<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-011 — Attested backend: Swaps' reports, commitments

Milestone `M-3`. Serves `REQ-BE-1`–`REQ-BE-6`, `REQ-OB-7`, `REQ-RQ-4`, `REQ-RQ-9`, `REQ-PN-2`, `REQ-PV-1`, `REQ-BK-8`,
`REQ-BK-9`, `REQ-PR-1` (plan `DEC-14`, `DEC-16`). Read SPEC v0.5.4 §4.4(b), §5.3, §5.4, §7.A, and PLAN `DEC-14`, `RISK-5`,
`RISK-8`. Depends on `F-002`, `F-003`, `F-004`. Its other half is the attesting contract in
`features/F-012-swaps-module/attesting-contract.md`; the module in the Swaps platform stays a draft by ruling.

## What it builds

- **Reports, at the ingress.** `POST /v0/reports`, for a requester holding the attesting grant *and* declared as the
  backend's attesting party (`REQ-RQ-4`: "that backend's attesting grant"). A report is a signed envelope: the bytes of
  a JSON document, its ed25519 signature, and the public key, one of the keys declared for the backend (`DEC-14`:
  "signed reports"). Kinds: `arrival`, `payout`, `positions`, `reversal`, `restoration`. The report's own identifier is
  its request reference, so a repeat answers with the original and a different body is `ERR-ReferenceConflict`
  (`REQ-ID-1`). The answer carries the coordinate the report will have.
- **Reports become observations, in the core.** The core re-verifies each report's signature and asks the store to
  record it: an observation with its outcome and a final head, at a coordinate on the attested network (block number:
  the report's position on that network; block hash: BLAKE2b-256 of what was signed; position: log 0), carrying the
  party and its identifiers (`REQ-BE-4`, `REQ-BE-5`). The store function derives every field from the report row, so
  the core cannot record anything the ingress did not take in, and the ingress cannot make an observation at all.
- **The mint path is the ordinary one.** A deposit names the report's coordinate. `source_bindable`,
  `bezent_final_source`, `intents_guard_order`, the book's `arrival` and `mint` postings, and the contract's mint window
  and ceiling apply unchanged. That reuse is why a report is an observation and not a parallel pipeline.
- **Withdrawals.** A redemption to an attested backend makes a withdrawal whose destination is its commitment, marked
  as such, never an address (`REQ-RQ-9`). Relayed instructions are accepted only when they hash to the commitment. The
  store refuses any order on an attested network: bezent signs nothing for a fiat payout. A `payout` report posts owed
  payout against the reserve in the book.
- **Reversal (`REQ-BE-6`).** A `reversal` report names the arrival report it reverses. The reserve falls, the shortfall
  goes against surplus, nothing is clawed back, and while surplus sits under its floor new mints of the asset hold
  `CND-Paused`. A `restoration` report from the same party restores the reserve, in full, and releases them.
- **The check and the statement.** `positions` reports feed `Readings::attested`. The reserve check takes a final point
  on the attested network, rolls the last snapshot forward by the reports after it, and compares with the book at zero
  tolerance. The statement says the reserve is attested and names the party from its declaration.

## Built on working assumptions

`Source-Final → Delivering` is normative: "a destination transaction is signed and recorded" (§5.4, `REQ-TR-6`), and bezent
signs nothing here. The owner asked for `M-3` to be completed on the recommendations of amendment 0016 while the ruling is
pending, on a draft PR, with `SPEC.md` and `PLAN.md` untouched. So:

- **Delivery (0016 A1).** A delivery attempt enters `Delivering` when the instructions are accepted, or for the refund of an
  unassociated arrival. A `payout` report completes it. A `payout-not-executed` report is the proof: a new attempt within
  the budget, the refund mint past it. Migration `0061`, `Core::hand_over`.
- **Reversed arrivals (0016 A3).** One reversed and not restored binds nothing, and holds a deposit on it unminted. Migration
  `0062`, the core's gate, the ingress's `reversed`.
- **Snapshot limits (question 4).** One hour and ten minutes, as configuration.

`NOTES.md` says how to take each back out.

## Not now, and why

- The module's reports side in the Swaps platform (`F-012`, `RISK-8`): described as a contract; bezent's suite drives
  its own request surface.
- A second attester (`DEF-2`). Real keys and declarations for production (`F-014`, operator).

## Done when

A report without the attesting grant, from another backend's party, or with a signature that does not verify is refused
with nothing recorded; a report becomes an observation carrying the party's identifiers; instructions that miss their
commitment are refused and no order is ever recorded on an attested network; a reversal runs end to end through the
request surface and the core: charged against surplus, mints paused under the floor, supply and holders untouched,
released by the restoration; a fiat withdrawal completes on its report, a failed payout ends in a new attempt or one
refund mint, and the refund of an unassociated arrival completes; the check holds at zero divergence through all of it;
the statement says the reserve is attested and names the party. Each test is named after the id it verifies.
