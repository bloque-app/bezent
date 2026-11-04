<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0018 — which convertible arrival a deposit records, and what holds `CND-AwaitingConversion`

**Status:** proposed by the `F-010` helper, 2026-11-04; revised after review of pull request #69 (the exposure, option A1d, the proposal). **Not ruled and not applied to `SPEC.md`.** Nothing in the
`F-010` pull request depends on it: the parts of `US-A5` that need it are not built (see "What waits").
**Target:** `SPEC.md` v0.5.4 (§5.10 `REQ-CV-1`, `REQ-CV-2`, `REQ-CV-6`; §5.4; §7.A `REQ-RQ-3`; §10 `CND-AwaitingConversion`;
`AC-A5.1`, `AC-A5.2`)
**Date:** 2026-11-04
**Needs:** a decision item on bezent's form (none exists yet; `q-f010` only queues the build). Numbered 0018 because 0016 and
0017 are proposed in pull request #66; renumber if that changes.
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
reviewing `F-010`; checked by running a second requester against the real ingress (a scratch test, not kept).

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

**Proposal (for the owner to rule; nothing is built on it).** The specification's text leans to A1b: `REQ-CV-2` says
"a conversion on a deposit" and §5.4 puts the condition on a transfer, so the deposit exists first and `REQ-CV-6` applies to
it unchanged. A1b with A1d as its binding is the option that best meets the text and the only one that puts the link on chain;
it closes the exposure for conversion outputs only if the batch can be made atomic through the Delay queue, as above. If it
cannot, no option closes it: take A1b on the text alone and record the exposure as open, with ordinary arrivals open under
every option.

## What the proposed text would touch

`REQ-CV-1`, `REQ-CV-2`, `REQ-CV-6`, `REQ-RQ-3` (A1a) or `REQ-RQ-4` (A1b), and `REQ-CV-7` and `REQ-OT-2`'s family for A1d's reference; A1c leaves the request surface as it is, §5.4's `Source-Final` row, §10's
`CND-AwaitingConversion` description, `AC-A5.1` and `AC-A5.2`. The proposed wording is not written until an option is
chosen: four different texts would have to be kept in step.

## What is built meanwhile, and what waits

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
