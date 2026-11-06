<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0026 — a deposit binds only an arrival from its requester's declared senders

**Status:** **Ruled by @pandres95 on the decision form, 2026-11-06 (`d-requester-arrivals`): "Bind by sender
(recommended)".** Applied to `SPEC.md` v0.5.9 (§2, §3, `REQ-PN-3`, `REQ-IN-3`, `REQ-IN-6`, `REQ-CV-2`, `REQ-RQ-3`,
`AC-A2.5`, §10 `ERR-SenderNotDeclared`); `PLAN.md` v0.6.8 binds it. Built by migration 0135 and the ingress. The items
under "Open after the ruling" stay open.
**Target:** `SPEC.md` v0.5.8 (v0.5.7 on `develop`, and v0.5.8 as pull request #99 leaves it)
**Date:** 2026-11-06
**Needs:** nothing further for the ruling. Each item under "Open after the ruling" needs a decision item of its own.
Numbered 0026, and the specification and plan versions v0.5.9 and v0.6.8, because pull request #99 (the gate rulings) takes
amendments 0024 and 0025, `SPEC.md` v0.5.8 and `PLAN.md` v0.6.7. If either merges first, the other renumbers to what is
next. Migration 0135 (#99 uses 0130).
**Rationale in one line:** an arrival at the shared reserve carries no owner and a deposit grant is per kind, so any
requester holding it can bind another party's arrival to a destination of its own, and the rightful request is then
refused `ERR-SourceConsumed`; this must be closed before a second requester holds a deposit grant.

## The gap

Amendment 0018 recorded it ("The exposure the binding rules do not close") and pinned it in a test
(`services/ingress/tests/requester_binding_today.rs`, now `bind_by_sender.rs`). What bound a deposit was: a final
observation of kind `arrival`, `convertible_arrival` or `conversion_output` at the asset's reserve, with no on-chain
reference, not from a declared funding address, not consumed. Nothing tied it to a requester. A second requester with
the deposit grant bound an ordinary arrival and a swap's output another was waiting for; the rightful one was refused.
The mint stayed backed (`INV-1`, `INV-2`, `INV-6`); attribution and direction were lost. With one instance serving
Bloque Test and Bloque (`PLAN.md` `DEC-15`), Test's credential was production deposit authority.

## Ruling

`d-requester-arrivals`, @pandres95 on the decision form, 2026-11-06: **"Bind by sender (recommended)".** Each requester
declares its senders (origin-network addresses); a deposit binds only an arrival whose sender is one of the requesting
requester's declared senders. The other ways 0018 named (a grant scoped to a reserve or a route, a deposit address, a
reference on chain) are not taken.

## What a sender is

The specification speaks of the arrival's **funder** "as the backend reports it" (§2 "Funder", "Declared sender"),
never of a network. Per network kind, for the plan:

- **EVM origin networks.** The `from` of the ERC-20 `Transfer` log at the arrival's coordinate (topic 1 of the log the
  endpoint observes). The EVM endpoint records it as `observations.sender`, spelled with its EIP-55 checksum
  (`services/evm-endpoint/src/observe.rs`, `spell`). Declarations are compared without regard to case on a network of
  address format `evm`, exactly on any other, as `funding_arrival()` compares (0032). It is the address the token moved
  out of, not the transaction's signer: a `transferFrom` by a sweeper contract has the swept address as its sender, and
  a transfer a Safe or a router makes from its own balance has that contract.
- **Attested networks.** Not settled here. No attested backend is built on `develop` (`F-011`, pull request #66), and a
  deposit binds only an observation on an origin network. What an attested report says the funder was, and whether it
  says it at all, is for `F-011`; as built, an observation that records no sender matches no declaration, so a deposit
  of it is refused. Open (item 3).

## What happens to an arrival no requester may bind

It stays an unassociated arrival with its owner (`CND-Unassociated`, `REQ-IN-6`, unchanged): not minted, not absorbed,
not dropped. Every deposit request naming it is refused `ERR-SenderNotDeclared` and binds nothing, so the requester whose
declared sender it is can still bind it later (a declaration made after the arrival counts; the store records no
"from block" for it). Its release is the refund of an unassociated arrival, with a second approver (`REQ-RQ-6`,
unchanged). The refund path is not a deposit, and this ruling does not touch it.

## Conversions (A1b, amendment 0018)

- **A deposit naming a convertible arrival** is checked on that arrival, when it binds it: the convertible's transfer
  into the reserve has the funder's address as its sender.
- **The conversion's output** is sent from the venue's pool, which is nobody's declared sender. It is bound only
  through `deposit_conversion_record()` (0120), by the requester that made the deposit (`REQ-RQ-4`), so the rule
  reaches it through the deposit whose convertible arrival's sender was checked. `REQ-CV-2` says so.
- **A deposit naming an output directly** (0018, Open 2; still not removed) is checked like any other: its sender is the
  pool, so it binds only for a requester that declared the pool. Declaring a venue's pool as a sender gives every
  requester that does so every output of that pool, which reopens the exposure for outputs; the deploy note says not
  to. Whether the direct path stays is still 0018's Open 2.

## Which rows of amendment 0018's table this closes

| 0018's exposure | After this ruling |
|---|---|
| Ordinary arrivals bound by any requester with the deposit grant | **Closed**, for requesters with disjoint declared senders |
| A conversion output bound directly by any requester | **Closed**, unless a requester declares the venue's pool |
| A conversion output named for another requester's swap through a deposit of one's own convertible arrival | **Not closed** (item 1) |
| Bloque Test's credential as production deposit authority | **Closed for ordinary arrivals** from Bloque's senders, if Test's declared senders are disjoint from Bloque's; **not for conversion outputs** (item 1) |

## Applied text (`SPEC.md` v0.5.9)

- **§2** "Declared sender": an account the operator records, for one requester and one origin network, as one that
  requester's value is sent from.
- **§3** Requester: requests deposits of arrivals from its declared senders.
- **`REQ-PN-3`** (new): each requester has a recorded declaration of its senders per origin network, made by the
  operator; its deposit authority extends only to arrivals whose funder is one of them; no request declares, adds or
  asserts a sender (`REQ-RQ-7`).
- **`REQ-IN-3`**: only a requester one of whose declared senders is the arrival's funder may bind it; any other request
  is refused (`ERR-SenderNotDeclared`) and binds nothing.
- **`REQ-IN-6`**: value from a funder no requester declared is among the arrivals that cannot become a deposit.
- **`REQ-CV-2`**: the declared sender is checked on the convertible arrival; the output named for it is bound through
  that deposit, by its requester, and is not checked again.
- **`REQ-RQ-3`**: the arrival's funder must be one of the requester's declared senders; otherwise `ERR-SenderNotDeclared`.
- **`AC-A2.5`** (new, layer service): refused, binds nothing, the rightful requester may still bind it; an arrival no
  requester declared stays unassociated.
- **§10** `ERR-SenderNotDeclared` (new): the arrival's funder is not one of the requester's declared senders on its
  network.

## Built (migration 0135, the ingress)

- `requester_senders (requester, network, address)`: the migration owner's declaration, like the other requester
  declarations; no service login writes it, the ingress reads it. `sender_declared()` reads it.
- The ingress refuses a deposit whose arrival's sender its requester did not declare, with `ERR-SenderNotDeclared`
  (a transfer in `Refused`, bound to nothing). It checks after the source is found and is not a funding top-up, and
  before consumption, so another requester learns nothing of whether the arrival is bound.
- The store refuses, whoever writes, the answer that binds a deposit to a request whose requester did not declare the
  sender of the deposit's source (`intake_answers_guard_sender`, BZ001). A transfer carries no requester; its request's
  answer is what ties it to one. The requester a request is recorded for is the ingress's to authenticate (`REQ-PN-1`),
  so this backs the ingress's rule against a fault in it; it is not a defence against a login holding the ingress's
  role.
- `refusal_error` gains `ERR-SenderNotDeclared`. The OpenAPI document names it on `POST /v0/deposits`.

## Open after the ruling

None of these is applied; each is the owner's.

1. **The pairing of an output with a convertible arrival.** A requester with any deposit awaiting its conversion can
   still name another party's swap output as its own (0018, A1b: that the arrival fed that conversion is the requester's
   word). Nothing ties the amounts: a deposit of one unit of a convertible can claim an output of any size, and with one
   conversion member per vault nothing on chain says whose swap made it. The ruling narrows it to requesters with a
   convertible arrival of their own awaiting, from a sender of their own, and leaves it open on every route that admits a
   convertible. So Bloque Test's credential is still production deposit authority over conversion outputs.
2. **A sender shared by two requesters.** Nothing refuses declaring one address for two requesters; for that address
   the old exposure returns between them (whoever names the arrival first binds it). Whether the store should refuse
   it is not ruled; the deploy note tells operators to keep each requester's senders disjoint.
3. **Attested backends.** What a sender is on an attested backend (above).
4. **A sender that is a contract shared by many parties** (a public router, an exchange's hot wallet, a bridge's
   relayer). Declaring one makes every arrival through it the declaring requester's. Which addresses may be declared is
   the operator's judgement; nothing in the specification bounds it.
5. **Arrivals before a declaration.** A declaration applies to every unconsumed arrival from that sender, earlier ones
   included. Whether a declaration should apply only from a block onward (as a funding address does, 0032) is not ruled.
6. **Deposit addresses as senders.** If evmhot sweeps per-user deposit addresses straight into the vault (`PLAN.md`
   `DEC-9`), each deposit address is the sender of its sweep, so each must be declared for its requester. As built a
   declaration is one row per address, written by the migration owner; that does not keep up with deposit addresses
   created continuously. Whether sweeps go through an address of Swaps' own first, or declarations get another writer or
   form (a derivation rule, a sweeper's identity), is not ruled.

## Identifier ledger

New: `REQ-PN-3`, `AC-A2.5`, `ERR-SenderNotDeclared`. None of them was used before.
