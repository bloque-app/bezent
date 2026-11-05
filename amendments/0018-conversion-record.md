<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0018 — which convertible arrival a deposit records, and what holds `CND-AwaitingConversion`

**Status:** proposed by the `F-010` helper, 2026-11-04; revised after review of pull request #69 (the exposure, option A1d, the
proposal). **Ruled by @pandres95 in chat, 2026-11-05 (`d-cv-conversion-link`): A1b alone.** "And yes, A1b, please. Doing both
doesn't make sense IMO." A1d is not taken. Applied to `SPEC.md` v0.5.5; built by issue #10 (migration 0120). The exposure
below, and the choices listed under "Open after the ruling", stay **open**.
**Target:** `SPEC.md` v0.5.4 (§5.10 `REQ-CV-1`, `REQ-CV-2`, `REQ-CV-6`; §5.4; §7.A `REQ-RQ-3`, `REQ-RQ-4`; §10
`CND-AwaitingConversion`; `AC-A5.1`, `AC-A5.2`; §15)
**Date:** 2026-11-04; ruled and applied 2026-11-05
**Needs:** nothing further for A1b. The exposure needs its own decision item (`d-requester-arrivals`), and so does each choice
under "Open after the ruling". Numbered 0018 because 0016 and 0017 are proposed in pull request #66.
**Rationale in one line:** `REQ-CV-2` says the deposit "records the convertible arrival and the converted output, with both
amounts", and `REQ-CV-1` says the convertible arrival "is held in `CND-AwaitingConversion`"; the specification never says
which arrival a given conversion's output belongs to, or what object holds the condition, and nothing on chain says it today.

## The gap

1. **No link on chain today.** A convertible arrival and the conversion that spends it are separate transactions. The swap spends
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

## The exposure the binding rules do not close

This is true today, with no option of this amendment applied, and it is wider than the pairing question. Found while
reviewing `F-010`; checked by running a second requester against the real ingress; that run is now kept as
`services/ingress/tests/requester_binding_today.rs`, which pins today's behaviour (not a desired one, and it names no
criterion) and goes when `d-requester-arrivals` is ruled.

- What binds a deposit: `source_bindable()` (migration 0034) admits a final observation of kind `arrival` or
  `conversion_output` at the asset's reserve, carrying no on-chain reference, and `decide_deposit`
  (`services/ingress/src/intake.rs`) adds that the token is admitted, the recipient is that asset's reserve, and the sender
  is not a declared funding address. The unique index `transfers_source_consumed_once` (migration 0003) stops a second
  use. Nothing ties the observation to a requester, a route or an expected arrival: the destination is whatever the request
  says, and a grant is only a kind (`requester_grants (requester, grant_kind)`; `REQ-PN-2`).
- So **any requester holding the deposit grant can bind any final, unconsumed `conversion_output` at the reserve**, whoever's
  swap made it, or a surplus output nobody meant to deposit, and mint it to a destination it chooses. Whoever names the
  coordinate first wins; the rightful requester's request is then refused with `ERR-SourceConsumed`.
- **An ordinary deposit has the same exposure.** An arrival is a plain token transfer into the vault: bezent issues no deposit
  address (`DEC-9`), the transfer carries no reference (the rule above requires `on_chain_reference` to be null), and the
  sender is arbitrary. Nothing binds an ordinary arrival to a requester either. Run against the real ingress: a second
  requester with only the deposit grant bound an ordinary arrival and a swap's output to its own destination, and the first
  requester's request for the same coordinate was refused `ERR-SourceConsumed`.
- The mint stays backed: it is for the observed amount, of a final arrival, once (`INV-1`, `INV-2`, `INV-6`). The loss is
  attribution and direction: the party whose value arrived does not receive the asset, and the party that claimed it does.
  The refund path needs a second approver (`REQ-RQ-6`); the deposit path needs none.
- **Bloque Test and Bloque.** `PLAN.md` (`DEC-15`, 0.6.0) has one mainnet instance serve both. A reserve is one row per
  (asset, network) (`reserves`, migration 0010), there is one deposit route per asset and network, and grants are per kind,
  so both requesters are granted the same route and bind the same vault. Bloque Test's credential is therefore **production
  deposit authority**: it can claim any production arrival or conversion output to an address it picks. `DEC-15`'s "granted
  only what it needs" cannot be expressed with per-kind grants. Until ruled, Test's deposit credential should be handled
  and rotated as a production one, and whether it should hold the deposit grant on mainnet at all is for the owner. Each
  vault also has a single `conversionMember` (`contracts/evm/script/DeployVault.s.sol`), so on chain Test's and Bloque's
  conversions are not distinguishable by who made them.
- What would close it for ordinary arrivals is a different decision (a grant scoped to a reserve or route, or an arrival
  tied to its requester by a deposit address or a reference). It needs its own decision item. Nothing here builds it.

## Options

The owner rules. A proposal follows the options.

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

- **A1d. Link on chain, by reference.** The conversion's transaction carries a reference, by the `ReferenceEmitter` pattern
  of `REQ-OT-2` (the redeemer's account together with the reference it chose; here the requester together with a reference
  it chose, in a namespace of its own as amendment 0008 gave redemptions). The role admits one more call,
  `emitReference(bytes32)`, which moves no value, so `REQ-CV-7`'s "no other path" holds. The deposit request names the
  output and the reference, and bezent checks that a `Reference` log with that value sits in the output's own transaction
  (the same transaction hash, never an amount or a time). It works with A1a or A1b: it replaces "the requester names the
  pair" by "the chain carries the pair's reference".
  - *Can it be one transaction?* Today the role makes two plain calls, `approve` and then `exactInputSingle`, each through
    `execTransactionWithRole`. Swap and emit in one transaction needs a `MultiSendCallOnly` batch through the Roles modifier,
    and Roles admits a batch only through a transaction unwrapper registered per (target, selector) on the modifier, not per
    role (`Periphery.sol`). The deployment registers `ReferencedTransferUnwrapper` at (`MultiSendCallOnly`, `multiSend`), and
    it reverts for any batch but the 356-byte (transfer, `emitReference`) pair (`ReferencedTransferUnwrapper.sol`). **So a
    swap-and-emit batch is refused today.** It can be made atomic with (i) an unwrapper that admits an (`exactInputSingle`,
    `emitReference`) pair (the swap's parameters are all static, so a fixed layout like the existing one is possible),
    registered either on the same slot, which touches the delivery pairing of `INV-14` and needs it re-reviewed, or on a
    second batch target; and (ii) `emitReference` scoped for the conversion role. Both widen a live vault's Roles, which
    `VaultGuard` sends through the Delay queue; `DeployVault`, `VerifyVault` and `Conversion.t.sol` change with them. None
    of this is built or designed here.
  - *Other ways, not relied on.* The router's `multicall` dispatches to the router itself, so it cannot call the emitter, and
    the role scopes only `exactInputSingle` (by the router's interface as known; its source is not in `lib/`, so not checked
    here). If Swaps' conversion member were a contract, or a delegated account, it could batch the role call and the emit
    with no vault change; whether Polygon supports that, and whether a `Reference` that any caller in the same transaction
    can emit (`ReferenceEmitter` is open to everyone) is evidence enough, are not established here and are the owner's.
  - *If it cannot be atomic,* a reference emitted in another transaction is no stronger than the request's own word, since
    anyone can emit one. Then A1d closes nothing and should not be chosen.
  - *Cost to Swaps:* one batch per conversion through the role (a delegate call to `MultiSend`, so more gas than a plain
    call), the reference known before the swap (which fits A1b's order, the request first, better than A1a's), and the
    reference carried in the deposit request. *Cost to bezent:* the endpoint pairs a `Reference` only after a vault outflow
    (`observe.rs`), so one after a `Conversion` is recorded today as `UnpairedReference`; the ingress would need the new rule
    (find it by transaction, check it) and the store's binding rule a place for it. New behaviour, not built; no migration
    here.

**A2 — what bezent verifies about the pairing (A1a and A1b; the exposure applies to every option).** The requester names the pair; bezent cannot check that the
arrival fed the conversion, and, as the section above shows, cannot check that the requester is the one the value belongs
to. Proposed: bezent verifies that both coordinates are final observations at the same reserve, of the right kinds, that the
arrival's token is admitted as a convertible for the asset and the output is the origin token, and that neither is already
consumed; causality is the requester's, as price and slippage are (`REQ-CV-5`). The mint is against the output alone
(`REQ-IN-4`), so it cannot be more than was delivered, and it stays backed. **That is not "misreports the record".** A wrong
pairing, or a claim by a requester that does not own the arrival, mints another party's output, or a surplus output, to a
destination the claimant chooses; the party whose value it was loses it, and no check of A2 prevents it. It applies to
ordinary arrivals too. Only A1d, if atomic and namespaced, narrows it, and only for conversion outputs.

**A3 — the amounts reported (A1a and A1b).** Three: the convertible arrival's amount, the convertible amount the
conversion spent, and the output's. Mint is the output's, under every rule of §5.5 (`REQ-CV-5`).

**Does each option close the exposure above?**

| Option | Closes it? |
|---|---|
| A1a, A1b, A1c | No. The pairing stays the requester's word, and any requester with the deposit grant can still bind any final output. |
| A1d | For conversion outputs only, and only if the batch is atomic and the reference carries the requester's namespace. Not for ordinary arrivals. With one conversion member per vault, the on-chain actor is the same for Bloque Test and Bloque, so the namespace in the reference, not the signer, is what would separate them. |
| Any | Ordinary arrivals stay exposed. That needs its own decision (a grant scoped to a reserve or route, or an arrival tied to its requester). |

**Proposal (as put to the owner before the ruling).** The specification's text leans to A1b: `REQ-CV-2` says
"a conversion on a deposit" and §5.4 puts the condition on a transfer, so the deposit exists first and `REQ-CV-6` applies to
it unchanged. A1b with A1d as its binding is the option that best meets the text and the only one that puts the link on chain;
it closes the exposure for conversion outputs only if the batch can be made atomic through the Delay queue, as above. If it
cannot, no option closes it: take A1b on the text alone and record the exposure as open, with ordinary arrivals open under
every option.

## What the proposed text would touch (as proposed)

`REQ-CV-1`, `REQ-CV-2`, `REQ-CV-6`, `REQ-RQ-3` (A1a) or `REQ-RQ-4` (A1b), and `REQ-CV-7` and `REQ-OT-2`'s family for A1d's reference; A1c leaves the request surface as it is, §5.4's `Source-Final` row, §10's
`CND-AwaitingConversion` description, `AC-A5.1` and `AC-A5.2`. The proposed wording is not written until an option is
chosen: four different texts would have to be kept in step.

## What was built meanwhile, and what waited (as proposed)

Built in the `F-010` pull request, none of it depending on this amendment: tests at the layers §9 names for what the
specification already defines (a convertible arrival is never minted against; a deposit is minted for exactly the output the
swap delivered, once final; the statement itemises unconverted holdings outside the equation; a real swap on a fork of the
network is observed as an input and an output with both amounts in the output's record; a deposit request goes through the
real ingress to the mint, refused for a convertible arrival and for the swap's input, minted for exactly the output).

Waits for the ruling, not built: the record of the convertible arrival with its amount against a deposit
(`REQ-CV-2`, last sentence); `CND-AwaitingConversion` as a condition an arrival holds (`AC-A5.1`, "holds" half); the
deadline of `REQ-CV-6` acting on a real conversion rather than a hand-inserted transition; the link between a refund of a
convertible arrival and a later conversion output (`features/F-002-ingress/NOTES.md`, "Left open"). Its migration, if any,
is 0065 or later.

## Ruling

`d-cv-conversion-link`, @pandres95 in chat, 2026-11-05: **A1b alone.** A1d is not taken ("Doing both doesn't make sense IMO"),
so no reference is emitted with a conversion, the vault's Roles are unchanged, and the link between an arrival and its
conversion is the requester's word. The exposure is therefore **not closed** by this ruling, for conversion outputs or for
ordinary arrivals; the table above stands.

## Applied text (`SPEC.md` v0.5.5)

- **`REQ-CV-1`**: a convertible arrival is still never minted against; a deposit request may name a final one, and the deposit
  begins at `Source-Final` holding `CND-AwaitingConversion` until its conversion's output is named. One no deposit names is
  an unassociated arrival (`REQ-IN-6`, unchanged).
- **`REQ-CV-2`**: the deposit is minted against the output. When it named the convertible arrival, its requester names the
  output by the deposit's request reference; the deposit's source becomes the output, the arrival is recorded against it and
  never minted against, each is consumed once (`INV-2`), and the deposit records both, with both amounts. bezent verifies what
  A1b needs of A2: the output is a final conversion output at the reserve address the convertible arrived at, of a token
  admitted for the deposit's asset, not consumed. That the arrival fed that conversion is the requester's word.
- **`REQ-CV-6`**, applied to the deposit unchanged: past the route's deadline, counted from the convertible arrival's
  finality (as for any condition held at `Source-Final`), the deposit goes to `Needs-Review`, never to a refund on a timer
  (`REQ-RF-1`). When a conversion counts as complete by that deadline is not stated (Open 7).
- **`REQ-RQ-3`**: the coordinate may be of a convertible arrival. **`REQ-RQ-4`**: the one extra of such a deposit, the output's
  coordinate, written once, by the deposit's requester only (`ERR-ReferenceConflict` for a second). It is part of requesting
  a deposit, so it rides on the deposit grant (`REQ-PN-2`, unchanged).
- **§5.4** `Source-Final` row and **§10** `CND-AwaitingConversion`: the condition is held by a deposit at `Source-Final` whose
  source is a convertible arrival. No state or edge is added: the deposit reaches `Source-Final` by the ordinary edge from
  `Accepted`, taking the condition on that edge, and `Source-Final -> Needs-Review` on the deadline is the existing edge.
- **`AC-A5.1`** ("Given a deposit request naming a final arrival of a convertible token") and **`AC-A5.2`** ("… and its
  requester names it for the deposit"). **§15**: Epic A lists `REQ-CV-6` and `REQ-RQ-4`.

## Built (issue #10, migration 0120)

- The store: `deposit_conversions` (one per deposit; the arrival and the output each unique) and `deposit_conversion_record()`,
  the one guarded update that records the conversion and moves the deposit's source and amount from the arrival to the output.
  The source binding and the amount stay write-once for everything else (`transfers_guard_update`). A coordinate a deposit
  recorded as its convertible arrival is never bound again; a binding and a move lock the coordinate, so neither passes the
  other unseen. `source_bindable('deposit')` admits a `convertible_arrival` (with an `arrival`'s exclusions); a refund still
  never names one. `unassociated_arrivals` leaves a recorded arrival out. Nothing is minted against a convertible arrival: the
  mint order's guard (0049, 0056) admits only a token admitted for the asset.
- The ingress: `POST /v0/deposits` takes a convertible arrival; `POST /v0/deposits/conversion` names the output, by the
  deposit's request reference; the status reports the recorded arrival (`converted_arrival`).
- The core: the edge `Accepted -> Source-Final` of such a deposit carries `CND-AwaitingConversion`; once the source is the
  output, and the output's block is at or before the deadline, the condition clears, resting on the output's final point, and
  the mint is ordered against the output. Past the deadline, `Needs-Review`.
- `REQ-ST-7`: the deposit's source and amount change once, in place. What it was asked and what each network did stay
  stateable from durable state: `deposit_conversions` holds the arrival, its token and amount, the output, its amount and the
  request that named it, and the status reports the arrival (`converted_arrival`).

## Open after the ruling

None of these is applied; each is the owner's.

1. **The exposure** (above): the pairing is the requester's word, and any requester holding the deposit grant can bind any
   final, unconsumed output, or ordinary arrival, first. Open under `d-requester-arrivals`.
2. **A deposit that names an output directly.** Before the ruling a deposit could name a `conversion_output` as its source,
   with no convertible arrival recorded (`F-010`, pull request #69). The ruling does not remove that path, so it is unchanged;
   such a deposit records no convertible arrival, and `REQ-CV-2`'s record is worded for a deposit that named one. Whether the
   direct path should stay is open.
3. **Which amount the route's limits measure while a deposit awaits** (A3's units). The deposit's amount is the convertible
   arrival's observed amount, in the convertible's minor units, until the output is named; then it is the output's, in the
   origin token's. The limits and the window are checked as for any deposit, once, at the request, on the arrival's amount, and
   not again on the output's. The store declares no convertible's precision. Open.
4. **A3's third amount**, the convertible amount the swap spent (`detail.conversion.amount_in` of the output). It is recorded in
   the output's observation as before and not reported with the deposit. Open.
5. **A2's "admitted as a convertible for the asset".** No declaration in the store says which convertibles an asset admits;
   the endpoint's classification (`convertible_arrival`, from its configured convertibles) is the evidence, and the asset is
   the one of the reserve the arrival is at. Open.
6. **An output named after review.** A deposit in `Needs-Review` takes no conversion request (`ERR-SourceConsumed`); its review
   ends by evidence (`REQ-RV-2`), and whether a named output is such evidence is not decided. Open.
7. **When a conversion completes by its deadline.** As built, the condition clears only when the output named for the deposit
   is from a block at or before the deadline (on the output's network's clock) and the deposit has not gone to review; an
   output from a later block leaves the deposit to go to review once the issuing network's final head passes the deadline.
   The specification says only that a conversion not completed by its deadline goes to review. Open.
