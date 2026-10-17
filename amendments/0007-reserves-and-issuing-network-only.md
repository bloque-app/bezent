<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0007 — bezent bridges reserves and the issuing network; reversals are the attesting party's

**Status:** ruled by @pandres95 on the decision form, 2026-10-13 (`pl-scope`: "Yes; Swaps runs DEX and CCTP via
Roles"; `oq-14`: "Swaps covers it; pause past the floor"). A5 ruled in chat (`pl-cross-network`, "Rule that way you recommend"). A1–A6 applied to `SPEC.md` v0.4.0.
**Target:** `SPEC.md` v0.3.4
**Date:** 2026-10-13
**Rationale in one line:** bezent's job narrows to moving value between each asset's reserve and the issuing
network. Getting value into a reserve (deposit addresses, sweeps, routing) and turning other tokens into the
origin token are the requester's, done with tools the requester runs. @pandres95's note (`pl-evmhot`): "our work
limits to bridging between reserves and Kreivo"; evmhot only sweeps; Swaps, then rutent, routes funds to the
reserves.

## A1 — Deposit addresses, sweeps and standing instructions leave bezent

Who gets value into a reserve, and from where, is the requester's. bezent sees only arrivals at a reserve address.

- Withdrawn: `REQ-DA-1` to `REQ-DA-6` (§5.2 keeps its heading and says "*Withdrawn by amendment 0007 (A1)*: getting
  value into a reserve is the requester's"), `REQ-RQ-2`, `REQ-KY-2`, `REQ-PV-5`, `INV-13`, `US-A1` with `AC-A1.1`
  to `AC-A1.4`, `AC-A2.4`, `CND-AwaitingInstruction`.
- `REQ-IN-3` becomes: "A final arrival of an admitted token at a reserve address MUST wait as an unassociated
  arrival (`CND-Unassociated`) until a requester binds it with a deposit request that names its coordinate
  (`REQ-RQ-3`). The deposit then begins at `Source-Final`, with a bridge reference derived deterministically from
  the coordinate."
- `REQ-RQ-3` becomes: "**Request a deposit:** request reference, the coordinate of a final arrival at a reserve
  address, the destination, and the refund destination." A coordinate is consumed once (`INV-2`).
- `REQ-IN-6`: an arrival with no deposit request by the route's deadline stays `CND-Unassociated` with its owner;
  releasing it is unchanged.
- Glossary: **Depositor** becomes **Funder** ("whoever sends value to a reserve address; never a caller of
  bezent"). **Reserve**: "the amount of an origin token bezent holds at its reserve address on one origin network".
  **Sweep**: withdrawn. **Arrival**: "value observed at a reserve address".
- Every other mention follows: §1.4 (requesters ask for deposits and withdrawals, not addresses), §4.1 and its
  diagram (no deposit addresses), `REQ-PN-2` (no "issuing deposit addresses" scope), `REQ-RN-4` (cites `REQ-KY-3`
  and `REQ-CV-7` instead of `REQ-DA-3`), `REQ-KY-4` (roles: delivery, pause, governance; no sweep role), `INV-14`
  ("by a referenced transfer, venue delivery, conversion under `REQ-CV-7`, or recorded surplus withdrawal"), the
  reserve-movement list in §5.3 the same way, `REQ-BK-5`/`REQ-BK-6` (no aggregate at deposit addresses: both sides
  of the equation recompute from public data for a public backend), §4.6, `NFR-10` (no "deposit addresses
  outstanding"), `REQ-CU-1` ("pause, statements" — no "sweep fees"), `REQ-RV-1`'s list of owned conditions, §8.5's
  list of conditions shown with age, and the §15 traceability rows.

## A2 — Conversions are the requester's, inside the reserve

- `REQ-CV-2` becomes: "A conversion on a deposit MUST be executed by the requester under `REQ-CV-7`, as one
  transaction whose output lands in the reserve. The deposit is then minted against **that** output, at its
  observed amount, under every rule of §5.5. The deposit records the convertible arrival and the converted output,
  with both amounts."
- `REQ-CV-3` is withdrawn, with `AC-B4.1` and `AC-B4.2`: every payout is in the origin token; a requester that wants
  another token converts what it receives.
- `AC-A5.2`: "Given the conversion's output arrival at the reserve is final, Then the deposit is minted at that
  arrival's amount, and both amounts are reported (`REQ-CV-2`, `REQ-CV-5`)."
- `REQ-CV-1`, `REQ-CV-4`, `REQ-CV-5`, `REQ-CV-6` are unchanged. `REQ-CV-6`'s review owner is the requester's
  conversion, but the review itself stays bezent's (`REQ-RV-1`).

## A3 — The requester's role on a reserve (new `REQ-CV-7`)

**`REQ-CV-7`** A requester MAY hold a role on a reserve address that lets it convert, at a protocol venue,
convertible holdings at that address into the origin token. The network MUST enforce, by on-chain code
(`REQ-RN-4`), that the role (a) spends only convertible tokens admitted for that asset, never the origin token,
(b) sends the output only to the same reserve address, and (c) is bounded per window. The role MUST NOT move value
out of the reserve address by any other path. Every conversion is observed at its coordinate and recorded like any
other movement.

*Why:* this is how @pandres95's "Swaps runs DEX via Roles" stays inside guardrail 12: the requester converts, but
never holds a key that can take reserve out, never asserts an arrival, and the reserve equation stays bezent's.
Convertible holdings back nothing until converted (`REQ-CV-4`), so the role puts no reserve at risk.

## A4 — Scope statement

§1.2 and §1.5 say plainly that bezent moves value between each bridged asset's reserve and the issuing network:
arrivals at a reserve, mints, redemptions, payouts from a reserve. Deposit addresses, sweeps, routing between
networks and conversion are the requester's. §1.6 (out of scope) gains "issuing deposit addresses and sweeping
them".

## A5 — Payouts only on the reserve's network (`pl-cross-network`)

Ruled by @pandres95 in chat, 2026-10-13 ("Rule that way you recommend"): a withdrawal to another network is the
requester's. bezent pays on the reserve's network; the requester names its own account there as the beneficiary and
delivers onward from its own funds. Every outflow from a reserve stays signed by bezent.

- `REQ-OT-4` becomes: "A withdrawal MUST be paid from the asset's reserve, on the reserve's network, to the
  beneficiary the redemption names. Delivery to another network is the requester's, from what it receives."
- Withdrawn: `REQ-VN-6`, `US-F3` with `AC-F3.1` and `AC-F3.2`. §5.7 keeps its heading. `REQ-VN-1`–`REQ-VN-4` stay:
  they bind the protocol venues the requester converts at under `REQ-CV-7`.
- §1.7's assumption about payouts on another network, §4.2's row "Paying out from the reserve, directly or through
  a protocol venue" ("from the reserve, on its network"), §4.3's venue row, and the Epic F row of §15 follow.
- *Why:* a role on the reserve cannot tell a payout a redemption funded from one none did; keeping cross-network
  delivery with the requester keeps bezent's outflows to one kind, signed by bezent.

## A6 — Reversals after a reported final payment (closes `OQ-14`)

**`REQ-BE-6`** If a payment an attested backend reported final (`REQ-OB-7`) is reversed afterwards, the attesting
party MUST cover it. bezent records the attesting party's report of the reversal as an observation (`REQ-BE-5`);
the reserve falls by the reversed amount and the shortfall is charged against the asset's surplus (`REQ-BK-4`). The
attesting party MUST restore the reserve within one business day. Holders' balances of the bridged asset MUST NOT
be reduced, and nothing already minted is clawed back. If the shortfall takes the surplus below its floor
(`REQ-BK-10`), new mints of that asset MUST pause until the reserve is restored.

- `OQ-14` closes: "*Closed by amendment 0007 (A6):* the attesting party covers it, within one business day;
  holders are never clawed back; mints pause while surplus is below its floor (`REQ-BE-6`)."
- @pandres95's note on `gen-reversal` stands as context: the rails never settle instantly, and a mint always waits
  for the rail's finality (`REQ-OB-7`); `REQ-BE-6` covers only later reversals such as a chargeback or a fraud
  recall.

## Identifier ledger

| Action | Identifiers |
|---|---|
| Withdrawn | `US-B4` (its criteria and `REQ-CV-3` went; applied as a consequence), `REQ-VN-6`, `US-F3`, `AC-F3.1`, `AC-F3.2`, `REQ-DA-1`–`REQ-DA-6`, `REQ-RQ-2`, `REQ-KY-2`, `REQ-PV-5`, `REQ-CV-3`, `INV-13`, `US-A1`, `AC-A1.1`–`AC-A1.4`, `AC-A2.4`, `AC-B4.1`, `AC-B4.2`, `CND-AwaitingInstruction` |
| Revised | `AC-A4.2` (narrowed to an arrival that cannot become a deposit), `REQ-OT-4`, `REQ-IN-3`, `REQ-IN-6`, `REQ-RQ-3`, `REQ-CV-2`, `REQ-RN-4`, `REQ-KY-4`, `REQ-PN-2`, `REQ-BK-5` (no venue delivery), `REQ-BK-6`, `REQ-CU-1`, `REQ-RV-1`, `INV-14` (no venue delivery after A5), `NFR-10`, `AC-A5.2`, `OQ-14` (closed) |
| Added | `REQ-CV-7`, `REQ-BE-6` |
