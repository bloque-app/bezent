<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0030 — registered senders: a requester's sender set that an authorised registrar grows

**Status:** **Proposed.** A design draft for @pandres95; no ruling yet, nothing applied to `SPEC.md` or `PLAN.md`, no
code. Applying it needs the decision under "Open" (item 1) and the items after it.
**Target:** `SPEC.md` at the version current when it is ruled (v0.5.10 on `develop`; open pull requests #112 and #119
also move it, so the version and the migration number are assigned at merge, and the text below names neither).
**Date:** 2026-11-08
**Needs:** @pandres95's ruling on item 1 (which shape), and a decision item for each other item under "Open". Numbered
0030 because 0027 to 0029 are taken by open pull requests (#119 is 0029, #112 is 0028).
**Builds on:** amendment 0026 (`d-requester-arrivals`, "Bind by sender"), which is itself not yet in production.
This amendment answers 0026's Open items 2, 4, 5 and 6 for the one case that forced them.
**Rationale in one line:** 0026 has the operator record each requester's senders by hand, which cannot keep up with
about 89 thousand per-user deposit addresses that grow all the time, and where one can be registered minutes before its
first deposit; a registrar the operator authorises must be able to declare them, inside bounds, without bezent
depending on the registrar at bind time.

## The gap

0026 settles who may bind an arrival: only a requester one of whose declared senders is the arrival's funder
(`REQ-PN-3`, `REQ-IN-3`, `ERR-SenderNotDeclared`). It also settles who writes the declaration: the operator
(`REQ-PN-3`: "the operator records the declaration"), as one row per address by the migration owner. 0026's Open item 6
named the consequence for sweeps and left it unruled.

The new integration is this. The hot-wallet service (evmhot) keeps one deterministic deposit address per user and will
sweep native USDC on Polygon **directly into the reserve vault**, not through an address of the requester's own. The
funder of each arrival is therefore the user's own deposit address, and a sweep moves that address's whole balance in
one `Transfer` log. The requester that binds each sweep is Swaps (the payment-rails swap service), as a declared
requester, by the log's coordinate (`REQ-RQ-3`, `POST /v0/deposits`). Three facts break a fixed list:

1. **Count.** About 89 thousand addresses exist, and nobody can record that many by hand.
2. **Growth.** More are created continuously, so any list recorded once is stale the day after.
3. **Lead time.** An address can be created minutes before its first deposit, so the declaration has to follow the
   registration within minutes, not at the operator's next maintenance window.

A rule that makes the check weaker to fit this (a derivation rule such as "any address under this extended key", or a
requester declaring a wildcard) is not proposed: `REQ-RQ-7` says bezent verifies and the caller does not assert, and a
derivation rule makes the evmhot key hierarchy part of bezent's trust base without anything on chain showing it.

## What stays as 0026 ruled

Nothing in 0026's rule changes. A deposit still binds only an arrival whose funder is one of the requesting
requester's declared senders on that network, and is refused otherwise (`ERR-SenderNotDeclared`); an arrival no
requester may bind stays unassociated, with its owner, refundable only under `REQ-RQ-6`. What changes is **who may
write the declaration**, and **what a declaration records**.

- **Plain senders keep working.** A sender the operator records, as 0026 builds it, stays valid and means exactly what
  it meant. This is the right form for a few long-lived, known senders (a requester's treasury address, a venue's
  pool when the operator decides to declare one).
- A declaration now has a **kind**: `operator` (as today) or `registered` (made by a registrar). Both are checked the
  same way at bind time: the arrival's funder is in the requester's set for that network, whichever kind put it
  there. The kind and the declarer are recorded and shown on the operator surface.

## The two shapes

### (A) Push at registration

When evmhot creates a deposit address, it declares it to bezent as a sender of the Swaps requester on Polygon, through
a new authenticated, append-only route (or store function) that only holders of a **registrar grant** may call. The
grant names the requester and the networks the registrar may declare for. Revocation is explicit and is the same
route's inverse. bezent keeps the whole check inside its own store; at bind time it reads its own rows and no other
system.

- **For.** Self-contained: the bind path has no runtime dependency on evmhot, so evmhot being down, slow or wrong
  cannot stall or corrupt a bind. Auditable: the evidence of why an arrival was bindable is a row in bezent's own
  record (who declared the sender, when, under which grant, with which registrar reference). Fails closed by
  construction: no row, no bind. The operator's role shrinks to authorising the registrar's credential and its
  bounds, which is a small, reviewable act (`REQ-GV-5` applies to the grant).
- **Against.** A new write surface, and a new credential whose compromise is worth modelling (below). bezent now holds
  tens of thousands of rows it did not before (cheap, indexed by `(requester, network, address)`). The declaration
  can lag the address's creation, so a sweep that lands before the declaration is refused until the declaration
  arrives (see "Lag" below). evmhot must make registration and declaration succeed together, or retry.

### (B) Lookup at bind time

bezent asks evmhot `GET /evm/accounts/{address}` when a deposit names an arrival: 200 if the address is registered, 404
if not. A sender is then "declared" if evmhot says so.

- **For.** No new bezent store, no push credential, no lag: the address is a sender as soon as evmhot knows it, and
  evmhot's registry is the single source of truth, so nothing can drift.
- **Against.** The bind path gains a runtime dependency on a service outside bezent's trust base and outside its
  record. If evmhot is down, the check cannot be made, and it must **fail closed**: the bind waits (the request is
  held, not refused, because the arrival is not at fault) or is refused with a retryable error; it must never fall back
  to allowing, and it must never cache a 200 as a standing permission. A 404 is also a statement from evmhot, so an
  evmhot bug or compromise that answers 200 for any address is exactly "the caller asserts" (`REQ-RQ-7`, guardrail 3)
  moved one hop. Auditability is weaker: the evidence lives in evmhot, can change after the fact, and bezent must copy
  the answer into its own record at bind time to keep it, which is shape (A)'s row written later. An address
  registered, bound, then deregistered leaves nothing to verify. Latency and rate of a call per bind also couple the
  deposit rate to evmhot's.

### Recommendation: (A)

Choose (A). The reserve is the thing bezent exists to protect; its binding authority should rest on state it holds and
can show, not on a live answer from a service whose outage or error would then be a bezent outage or a bezent
mis-binding. (A) pays for that with one new credential and a registration step evmhot must complete, both of which are
bounded and checkable. (B)'s only real advantage (no lag, nothing to drift) is bought back in (A) by registering
before the address is shown to the user, which evmhot controls, and by idempotent retry.

## What (A) means precisely

### A registrar

A **registrar** is an actor the operator authorises (alongside requester and governance, §3) to declare senders for
named requesters on named origin networks, and to revoke the ones it declared. A registrar grant is separate from
every requester grant (`REQ-PN-2`): holding it lets a credential declare senders; it does not let it request a deposit,
a refund or a withdrawal, mint, or touch any status. Declaring a sender moves no value and edits no status, so it is not
an administrative path in the sense of `REQ-GV-5`; it is recorded with its actor all the same.

A grant is a tuple the operator records: the registrar, the requester it may declare for, the networks it may declare
on, its bounds (below), and a state (active, suspended). A registrar cannot widen its own grant. A declaration outside
the grant is refused (`ERR-NotAuthorised`).

### A registration

Request: network, address, the registrar's own opaque reference (for example its account identifier; no user identity
is sent or stored), and nothing else. The requester is fixed by the grant (one grant, one requester), so the body
cannot choose it. bezent records `(requester, network, address, kind=registered, registrar, registrar reference,
declared_at)` and nothing is ever updated or deleted: revocation appends a revocation row (`revoked_at`, actor,
reason). The route is idempotent on `(requester, network, address)`: repeating it returns the first record.

### How a requester's set is scoped per network

The set is keyed `(requester, network, address)`. The same address on another network is a different sender, as 0026
already has it; a registrar's grant lists networks, and a declaration on a network outside the grant is refused. This
is also what lets Polygon's registrations stay out of a later Base or BNB Chain instance of the same requester. The
address comparison follows 0026: case-insensitive on a network of address format `evm`, exact otherwise.

### Revocation

Explicit, by the registrar for its own declarations, and by the operator for any. A revoked sender stops matching for
the **next** bind; a deposit already bound stays bound (the arrival was bound while the sender was declared, and that
record is the evidence). Revoking never touches an arrival, a transfer or a mint. The operator may also revoke every
registration by one registrar, or every one since a time, in one act: the response to a credential suspected
compromised (below).

### Lag

An arrival whose funder is not yet declared is refused `ERR-SenderNotDeclared` and left unassociated, as 0026 has it.
It is not lost, not dropped and not refunded on a timer (`REQ-RF-1`); a declaration made afterwards, or the requester's
retry, binds it, because a declaration applies to every unconsumed arrival from the sender (0026, Open 5; this
amendment keeps that and does not add a "from block" for registered senders, item 6). Swaps retries a refused bind
while it expects the sweep; evmhot registers the address before showing it to the user, so the lag is zero for any
deposit that follows the registration.

### Bounds on the declaring credential

Bounds are in the grant, enforced by the store, not by the route:

- **Rate.** A sustained rate and a burst per registrar. A request over it is refused and not queued
  (`ERR-SenderRegistrationLimit`). A one-off backfill of the existing addresses uses the same route at the same rate
  (the operator may raise the rate for the backfill and lower it after).
- **Ceiling.** A maximum number of live registered senders per `(requester, network)`. At the ceiling, further
  registrations are refused until revocations free room, and the operator is alerted well before it (at 80 per cent).
- **Disjointness.** A sender already declared (live) for **another requester** on the same network is refused, whoever
  declares. This also closes 0026's Open item 2 for registered senders; item 3 below asks whether it applies to
  operator declarations too.
- **Reserved addresses.** A reserve vault, a signing or payout address of the bridge, and any address the operator
  records as a venue's pool or as a shared contract (0026, Open 4) are refused as senders from a registrar.
- **Growth alarm.** A rate of registrations well above the registrar's recent rate raises an alert and, past a second
  threshold, suspends the grant automatically (suspended is a state the operator lifts).

### What a compromised registrar credential could do

It can declare an address of the attacker's choosing as a sender of the Swaps requester, so that Swaps is *able* to bind
an arrival from that address. That is the whole of its power; it cannot request a deposit, set a destination, change an
amount, mint, or edit a status. Consequences and why they are bounded:

1. **Swap still chooses what it binds.** A bind is a request by Swaps naming a coordinate and a destination. Swaps
   binds only coordinates it can correlate to a sweep its own ledger expects from evmhot (a deposit address belonging
   to one of its users, a destination derived from its own state). An attacker-declared address sending value to the
   reserve produces an arrival Swaps has no expectation for; Swaps does not bind it, and it stays unassociated and
   refundable to its sender under `REQ-RQ-6`. Whether Swaps does this is Swaps' obligation, not something bezent can
   verify (guardrail 12: rely on Swaps, but not for trust); the cap below is what stays true if Swaps does not.
2. **What the attacker can gain is bounded by what it can send.** The value of any arrival bound is a real,
   final arrival of an admitted token at the reserve; the mint stays backed (`INV-1`, `INV-2`, `INV-6`). The attacker
   cannot create value; at worst it causes a deposit of its own funds to be credited to the destination Swaps names, or
   makes Swaps' work queue noisy.
3. **The sharper attack** is declaring someone else's address (another requester's treasury, a public router or a
   venue's pool) as a Swaps sender, so that Swaps could bind *their* arrivals. Disjointness and the reserved-address
   rule refuse the first and third; a shared contract the operator has not recorded is the residual case (0026, Open 4),
   which is why the operator records known ones and why point 1 (Swaps choosing what it binds) still matters.
4. **Caps** on rate and count limit how many senders an attacker can plant before alarm and automatic suspension.
5. **Detection and recovery.** Every declaration is a row with its registrar and time, so one act revokes everything
   since the suspected time, and the operator rotates the credential. A binding made through a planted sender is
   visible as a deposit whose sender row names the registrar.

(B) has the same worst case (an evmhot that answers 200 for any address) with weaker records and no caps.

## Applied text (proposed, not applied)

No edit to `SPEC.md` here. If ruled, the wording is, in the specification's vocabulary (it names no technology or
network: the registrar is "a registrar", the hot-wallet service is not named):

- **§2** "Declared sender": add "A sender is declared by the operator, or by a registrar the operator authorised for
  that requester and network (`REQ-PN-4`)." New row **Registrar**: "An actor the operator authorises to declare, and to
  revoke, senders for named requesters on named origin networks, within bounds (`REQ-PN-4`, `REQ-PN-5`)."
- **§3** actors: new **Registrar** row. May: declare and revoke the senders its grant names. Never: request a deposit,
  a refund or a withdrawal; mint; assert an arrival or an outcome; widen its own grant.
- **`REQ-PN-2`**: add "declaring senders" to the list of separate grants.
- **`REQ-PN-3`**: replace "the operator records the declaration" with "the operator, or a registrar the operator
  authorised for that requester and network (`REQ-PN-4`), records the declaration". The rest stands; "No request
  declares, adds or asserts a sender (`REQ-RQ-7`)" now also reads: a deposit request carries no sender, and a
  registrar's declaration is an input to the check, never a claim about an arrival (the funder is still read from the
  chain by bezent, `REQ-RQ-7`).
- **`REQ-PN-4`** (new): The operator MUST record each registrar's grant: the requester it declares for, the origin
  networks, its bounds and its state. A registrar MUST be authenticated (`REQ-PN-1` applies). A declaration is
  append-only; revocation appends. Every declaration and revocation records its actor, its time and, for a registrar,
  the registrar's own opaque reference, and is shown on the operator surface. A deposit already bound is not
  affected by a later revocation.
- **`REQ-PN-5`** (new): A registrar's grant MUST bound its rate and the number of live senders it may hold per
  requester and network, and bezent MUST refuse a declaration over a bound (`ERR-SenderRegistrationLimit`), of a sender
  live for another requester on that network, or of an address the operator reserved (a reserve address, a bridge
  address, a recorded pool or shared contract). Bezent MUST alert before a ceiling and MUST suspend a grant whose
  rate passes the operator's second threshold.
- **`REQ-RQ-3`**: unchanged ("the requester's declared senders on its network").
- **`AC-A2.6`** (new, layer service): a registrar declares a sender, then a deposit of an arrival from it by that
  requester binds; the same arrival by another requester is refused (`ERR-SenderNotDeclared`).
- **`AC-A2.7`** (new, layer service): a declaration outside the registrar's grant (another requester or network), over
  its rate or ceiling, of a sender live for another requester, or of a reserved address is refused and records nothing;
  after revocation the next deposit from that sender is refused and a deposit already bound is unchanged.
- **§10**: `ERR-SenderRegistrationLimit` (new): the declaration is over the registrar's rate or ceiling, or of an
  address a bound refuses. A declaration outside the grant is `ERR-NotAuthorised` (existing).
- **§7**: a registrar surface (declare, revoke, read own) beside the request surface (§7.A), with its own credential
  and no other route.

## Plan effects (for `PLAN.md` when ruled)

A feature under F-002 (requesters): the store table gains a kind, a declarer and a revocation; a registrar grant table;
a registrar route on the ingress; the rate and ceiling in the store; the operator reads. The integration (evmhot
registers before showing the address; Swaps binds only coordinates its ledger expects, retrying while a sweep is
unregistered) is outside bezent and belongs to their own plans. `DEC-9` ("sweeps go straight into the vault") is
unchanged by this amendment.

## Open

Each is one question for @pandres95, with a recommendation. None is applied.

1. **Which shape?** (A) push at registration, or (B) lookup at bind time. **Recommend (A)**: it keeps the check inside
   bezent's record and out of an outage coupling, fails closed with nothing to configure, and its cost (a credential
   and a registration step) is bounded.
2. **Is a registrar a new actor with its own grant?** Or should evmhot's declaring credential be a requester grant
   under `REQ-PN-2`? **Recommend a separate registrar grant**: a requester grant lets a credential request deposits,
   which evmhot must never do, and a separate one keeps the credential's power exactly "grow this requester's
   senders".
3. **Disjointness for all declarations?** Refuse a sender live for another requester on a network whether the operator
   or a registrar declares it (this closes 0026's Open 2 outright). **Recommend yes, for all**; an operator who really
   wants a shared sender removes the other declaration first, which is visible.
4. **The numbers.** Start with, per registrar: a sustained 20 registrations per second with a burst of 100 (the
   existing 89 thousand then backfill in about 75 minutes), a live ceiling of 1,000,000 per requester and network,
   an alert at 80 per cent, and automatic suspension at 10 times the trailing daily peak rate. **Recommend these as
   starting values**, and ask @pandres95 to confirm or change them; they are operator configuration, not normative text.
5. **Reserved addresses.** The list of addresses a registrar can never declare: reserve vaults, bridge signing and
   payout addresses, and addresses the operator records as pools or shared contracts. **Recommend** the operator
   maintains it as a recorded list (so it is auditable) and that it starts with the bridge's own addresses and the
   conversion venues' pools; it does not try to detect contracts on chain.
6. **A "from block" for registered senders?** 0026's Open 5 asks whether a declaration should apply only from a block
   onward. **Recommend no**: registered addresses are new by construction, a pre-registration arrival is refused
   until the declaration lands and then binds, and a from-block would add a rule to explain with no attack it stops
   (Swaps picks what it binds).
7. **Bulk revocation.** One act that revokes everything a registrar declared, or everything since a time, for use after
   a suspected compromise. **Recommend yes**; it is the recovery path for the compromised-credential case, and it is an
   append of revocation rows, not an edit.
8. **Backfill of the existing addresses.** Through the registrar route at the grant's rate (**recommended**: one path,
   one record, tested by use), or by a one-off operator import.
9. **What Swaps must do.** Swaps binds only coordinates its own ledger expects from the sweeping service and retries a
   refused bind while it expects the sweep. bezent cannot verify this; it is a condition of the integration, not a
   requirement of the bridge. **Recommend** recording it in the F-012 (Swaps integration) feature and in the
   registrar grant's runbook, not in `SPEC.md`.

## Identifier ledger

New (proposed): `REQ-PN-4`, `REQ-PN-5`, `AC-A2.6`, `AC-A2.7`, `ERR-SenderRegistrationLimit`, and the **Registrar**
actor and glossary entries. None of them has been used.
