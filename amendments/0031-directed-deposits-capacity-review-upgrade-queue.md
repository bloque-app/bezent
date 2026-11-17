<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0031 — directed deposits wait for capacity, a token upgrade holds them, the queue claims one window, and the review edge is judged at resolution

**Status:** **Accepted** by @pandres95's rulings on the decision form, 2026-11-17, and applied by this pull request:
`d-f018-capacity` ("Wait for capacity"), `d-f018-review-exit-later` ("Judge by evidence at resolution"),
`d-f018-usdc-upgrade` ("Yes, hold and alert"), `d-f018-return-routing` ("Accept, with an alert") and
`d-f018-window-queue` ("Cap what waits to one window"). Applied in `SPEC.md` v0.5.14 and `PLAN.md` v0.6.13.
**Target:** `SPEC.md` v0.5.13 and `PLAN.md` v0.6.12 on `develop` (amendment 0029 applied). Applied, they become
`SPEC.md` v0.5.14 and `PLAN.md` v0.6.13. Nothing here changes a milestone: it is `F-018`'s, inside `M-4`.
**Date:** 2026-11-17
**Needs:** the owner's ruling on `d-f018-window-reserve` (part 5): a requests' reserve was proposed to keep a window
queue's head from taking each window's freed room before requests, and it is **not in force** (built, set to 0 on every
route) because review found it starves old deposits. Every other item is ruled and applied. Numbered 0031 because 0030 was the number of a
proposal (#120, registered senders) that was closed on a ruling of 2026-11-08 and whose branch still carries the file;
reusing it would make two different amendments 0030 in the record.
**Rationale in one line:** amendment 0029 left five edges of directed deposits open (how they meet the route's capacity,
when the review edge opens, what an upgrade of the origin token does, who a return may pay, and how long a queue of them
may hold the window against requesters); each is ruled, and each ruling is applied here.

## 1. Capacity (`d-f018-capacity`, ruled: "Wait for capacity")

**Before.** `REQ-LM-4` makes bezent refuse new requests with `ERR-Overloaded` when the route carries as many transfers as
its declared capacity. A directed deposit is not a request: its value has moved before bezent sees it, so there is
nothing to refuse, and amendment 0029 did not say what a directed deposit over capacity does. The core made it and
minted it whatever the route's in-flight count was.

**Ruling.** A directed deposit respects the route's capacity. Over it, the deposit waits in a named condition,
`CND-DepositCapacity`, and resumes by itself when the route has room, in the order of arrival, as `CND-DepositWindow`
does for the window. It is never returned for capacity.

**Applied.**

- A directed deposit is made holding `CND-DepositCapacity` when, at the moment it is made, the route's transfers in
  flight (other than itself) are at or over the declared capacity. It leaves the condition by itself once the transfers
  in flight that do not wait for capacity, plus the directed deposits waiting for capacity ahead of it by coordinate,
  leave room for it. The decision takes the route's lock, the same one a request's capacity check takes, so the two are
  serial.
- The window comes first. A deposit over what the window has left waits in `CND-DepositWindow`; when it leaves that, it
  waits in `CND-DepositCapacity` if the route has no room. A deposit takes its share of the window when it is made, or
  when it leaves `CND-DepositWindow`, exactly as before: a wait for capacity changes nothing in the window's count.
- While it waits, a directed deposit counts toward the route's in-flight count, as every transfer that is not terminal
  does. That is first come first served: a request that comes after it is refused `ERR-Overloaded`, as it would be behind
  an earlier request. It does not starve requesters beyond what the window's ruling already allows (amendment 0029,
  Open 1 and Open 5): capacity is freed as transfers complete, in minutes, not as a day's window rolls over.
- A directed deposit waiting at `Source-Final` for the window (`CND-DepositWindow`), or held there by a change of its
  token's code (`CND-TokenUpgraded`, part 3), carries nothing yet, so it does **not** count toward the in-flight count,
  neither a request's nor another directed deposit's (review M1 and M2 of #142). One held by a change of its token's code
  later, in `Delivering` (a new attempt) or in `Needs-Review` (a resolution's delivery), was in flight already and stays
  in flight while held, so its lift needs no check of capacity and never puts the route past it (review L-a of #142). Counting it would make a deposit that leaves the
  window wait for capacity behind later deposits still waiting for the window, and would refuse requests `ERR-Overloaded`
  for as long as a window queue or a hold lasts. It takes its place in flight when it leaves that wait, and only if the
  route has room; otherwise it waits in `CND-DepositCapacity`, in coordinate order, and from then on it counts.
- The route's deadlines exclude the wait. As for `CND-DepositWindow` (amendment 0029, reviews of #136), the clock of the
  first mint attempt, of `CND-DestinationRefused` and of a preparation refused without a named condition starts when the
  deposit left its last wait, not at the arrival's finality.
- `REQ-LM-4` gains the sentence that a directed deposit over capacity waits and is not refused; `REQ-IN-11` names the
  condition; `AC-A6.3` covers it.

## 2. The review edge is judged at resolution (`d-f018-review-exit-later`, ruled: "Judge by evidence at resolution")

**Before.** The §5.4 edge `Needs-Review → Refunding` of a directed deposit (amendment 0029, Open 2) opened only if the
deposit **entered** review holding `CND-DestinationRefused`, at that condition's deadline. A deposit that entered review
for anything else (its first preparation never came, a destination bezent could not read, a preparation refused without
a named condition) could never take the edge, even when the issuing network later refused its destination plainly, for
example on the preparation of a resolution into `Delivering`.

**Ruling.** Judge the evidence when the review is resolved, not how it began.

**Applied.** The edge is open for a directed deposit in `Needs-Review`, entered from `Source-Final`, with no mint ever
signed, when **the newest preparation of its first mint** (across that attempt's orders, a resolution's delivery
included, newest by the block it was checked at, then by record) is the issuing network's refusal to mint to that
destination, made while the asset could be minted, with the time of that block recorded, and nothing has passed at or
after it. Every other gate stays: a second approver, no signature and no signed transaction on any intent, no delivery
anomaly, no mint of it observed, and the resolution's evidence naming exactly that block and its time.

One case needs a word, so that the edge never refunds on an unknown (`REQ-RF-1`, guardrail 4). The refusal has to be
evidence **as the review stands**: either it is the refusal whose deadline began the review (the case amendment 0029
already allowed), or it was made after the review began (a resolution's delivery, for instance). A refusal made before
a review that began for something unknown (a destination or list bezent could not read, a delivery anomaly) is stale:
the unknown came after it, so it does not open the edge. That keeps `REQ-RF-1`'s promise, and it is the conservative
reading of the ruling. A later preparation that passes, or one refused for anything else (an asset that is not live, an
error bezent cannot name), is now the newest, and closes the edge.

A resolution into `Refunding` approved while an earlier resolution into `Delivering` of the same review waits on a
refused preparation would otherwise stand behind it for ever (a review runs its resolutions oldest first). When the
directed edge is open and a later resolution into `Refunding` is approved, it is judged next, and once it passes its own
checks (the edge and its evidence) the earlier one is superseded: it is recorded as refused with that reason, and its
order, never signed, is cancelled with the refund (amendment 0014's reading). A resolution into `Refunding` refused for
its evidence supersedes nothing (review L1 of #142). Nothing else changes in how a review runs.

`§5.4`'s row, `REQ-RV-2`'s exception, `REQ-IN-11`'s last sentence and the new `AC-A6.9` follow.

## 3. An upgrade of the origin token holds directed intake (`d-f018-usdc-upgrade`, ruled: "Yes, hold and alert")

**Before.** #139 made the EVM endpoint record an admitted origin token's `Upgraded` event as an observation of kind
`token_upgrade`, and the core pages `origin-token-upgraded` on it. It held nothing, pending this ruling.

**Ruling.** Hold and alert. When the origin token's implementation changes, directed intake holds: new directed
arrivals of that token wait under a named condition and are not minted, until an operator lifts the hold after the
fork suite has been run again against the new implementation. The lift is a recorded operator action with a second
approver (`REQ-GV-5`). Requesters' deposits are not held by it. Nothing is returned for it.

**Applied.**

- A new named condition, `CND-TokenUpgraded`: a directed deposit of an admitted token whose code changed at or before
  the block of its arrival, with no lift of that change recorded. It holds new work only, as a pause does: it parks the
  deposit before its mint is ordered, and an attempt already signed is in flight and is not held (`INV-15`).
- Which arrivals: every directed arrival of that token, on that network, at a block at or after the change's, while the
  change is not lifted. A change that is observed and not yet final already holds (holding is the safe side); one the
  network withdrew does not. An arrival made before the change was moved by the old code and is not held.
- A deposit held at `Source-Final` is not in flight (part 1): it carries nothing, so it never fills the route's capacity
  against requesters' deposits of the token, which the hold does not touch. When the hold is lifted, it takes its place
  in flight only if the route has room, and otherwise waits in `CND-DepositCapacity`. One held later, in `Delivering` or
  `Needs-Review`, was in flight already and stays so (part 1).
- After a hold, the window bounds intake, not mints. A held deposit takes its share of the route's window when it is
  made, as any directed deposit does (part 1), and keeps it while held; held deposits leave the window as their shares
  age out of it, not as they are minted. On the lift they mint together, as deposits held by a pause do (as far as the
  route's capacity lets them into flight, the rest waiting in `CND-DepositCapacity`), so the mints of one lift may
  exceed what one window would allow; what the window bounds is what was taken in, not when it is minted (review L-b
  of #142).
- What the hold does at intake: a mintable arrival becomes its deposit, holding `CND-TokenUpgraded`; one that could
  never be minted is **not returned while the hold stands** (a return is a payout of the same token, ordered by bezent
  with no second approver, and the hold exists because nobody has yet checked how the new code moves value). It stays
  undecided, final and unbound, and is decided as before once the hold is lifted. This is the conservative reading of
  "nothing is returned".
- The lift. One operator requests it, naming the change and saying that the fork suite was run again against the new
  implementation (`operator_request_token_hold_lift`); a different operator approves it
  (`operator_approve_token_hold_lift`). Both are recorded in `admin_actions` with their logins and justifications, as a
  pause's lift is. Once approved, the held deposits leave the condition by themselves at the core's next cycle.
- The time held does not count toward the route's deadline, as for the window and capacity waits.
- `origin-token-upgraded` still pages, once per observation; its runbook now describes the hold and the lift, and the
  new condition has its own.
- New `REQ-AS-12`; `REQ-IN-11` names the condition; new `AC-A6.8`.

## 4. A return may pay a third party (`d-f018-return-routing`, ruled: "Accept, with an alert")

**The exposure.** The directing code takes the `refund` address from its caller (amendment 0029, `d-refund-dest-swaps`),
and bezent returns a never-mintable directed arrival there by itself, with no second approver (Open 3). So anyone can
route USDC through the reserve to a third-party address: send an amount the route can never mint (above its current
maximum after a change of limits, say, or to a recipient on the refused list), naming someone else as `refund`. The value
is the funder's own, so nothing is stolen, but the payout is bezent's: its gas, against surplus, and a draw on the
vault's delivery allowance that withdrawals share. It also makes the reserve a hop in a transfer between two parties
that never deposit.

**Ruling.** Accepted, with an alert. The mitigation is the `directed-returns-alert` #139 added, which already counts every
return of a directed arrival and every refund of a directed deposit per route in the trailing day, and pages above its
threshold. No code changes; its runbook names the case. The exposure stays open and is kept visible here.

## 5. The queue claims one window at most (`d-f018-window-queue`, ruled: "Cap what waits to one window")

**Before.** A directed deposit waiting in `CND-DepositWindow` is a claim on the window ahead of anything later, so first
come first served by coordinate holds for requesters' deposits too (amendment 0029, Open 1 and 5). Every waiting deposit
counted, for as long as it waited. A queue several windows deep would refuse requesters' deposits `ERR-WindowExceeded`
until the queue drained, several windows later.

**Ruling.** Cap what waits to one window. Waiting directed deposits may claim at most the next window. Those waiting
beyond it still wait, but do not count against requesters' room in the window, so a queue cannot lock requesters out for
more than one window. First come first served among directed deposits holds.

**Applied.** The window is a trailing one, so "the next window" is read in both its senses, time and amount:

- **Time.** A waiting directed deposit counts against requests only during one window (the route's window length) from
  when it began to wait. After that it keeps waiting, but a request no longer sees it.
- **Amount.** What waiting deposits claim against requests is never more than one window's limit.
- **Among directed deposits nothing changes.** A waiting deposit resumes only when the shares taken in the window, plus
  every directed deposit waiting ahead of it by coordinate (however long they have waited), plus its own amount, fit.
  So the queue still drains in coordinate order, and the window's limit is never exceeded by what is actually minted.
- **The queue's head may still take a window's freed room before requests (pending `d-f018-window-reserve`).** The two
  points above are what is built, and they do not meet the ruling by themselves: the core cycles every few seconds, so
  as each window's room frees, the queue's head takes it within seconds, before a request can come, and a queue several
  windows deep can still keep requesters out for as long as it is deep (review M3 of #142). That gap is open.
- **The requests' reserve: built, not in force, awaiting `d-f018-window-reserve`.** Review M3 of #142 proposed a
  route-level setting, `routes.request_reserve`: a directed deposit that has waited longer than one window, or waits
  behind one that has, would take room only up to (1 − `request_reserve`) of the window's limit, the rest left to
  requests, and one whose amount alone is over that share would take room only in an empty window. It is built, and set
  to **0 on every route**, which turns it off and leaves exactly the two points above. It stays at 0 until the owner
  rules, because the next review found it starves old deposits under ordinary traffic:
  - **H1.** An old deposit larger than (1 − `request_reserve`) of the limit may leave only into a completely empty
    window, so under any trickle of requests it waits indefinitely, and every directed deposit behind it waits with it.
  - **M1.** A smaller old deposit starves too: requests are not bounded by old waiters, so a steady flow of them can
    keep the window's use above what the deposit may join, window after window.

  Neither is acceptable as it stands, and fixing them is a design choice (bounding requests against old waiters, a
  share for the queue's head, or another rule), so it is the owner's (`d-f018-window-reserve`). No claim is made that a
  deposit under the reserve is never stuck.
- **What this means for requesters.** For one window after a queue forms, its claims refuse requests as before. After
  that, a request is checked against the shares actually taken in the window and the claims of deposits that began to
  wait within the last window; a request that fits is accepted, and the queue's next deposit waits for the room after
  it. But the core usually gets there first: right after a window rolls over, the queue's head takes the freed room
  within seconds, and a request finds room only in what the head leaves or between the head's turns. Beyond that,
  directed deposits and requests share the window as amendment 0029's Open 5 ruled: a directed deposit that leaves the
  queue takes its share like any deposit.
- The ingress's route table (`GET /v0/routes`) shows the window's `used` and `remaining` by the same rule, as a request
  sees them.
- **A queue that does not drain pages.** A directed deposit still waiting in `CND-DepositWindow` more than two of the
  route's windows after it began to wait pages `directed-wait-alert` (once, then daily while any does; review M2 of
  #142): its claim stopped counting against requests after one window, so by then it is being outrun. The page changes
  nothing; the deposit keeps waiting.

`REQ-IN-11`, `§10`'s `CND-DepositWindow` and the new `AC-A6.7` follow.

## Text applied to `SPEC.md`

Guardrail 1 holds: no network, token, standard or product is named below.

- **§5.4, the directed row:** "`Needs-Review`, held from `Source-Final`, a directed deposit only | `Refunding` | No mint
  was ever signed for it, and the evidence when the review is resolved is the issuing network's refusal of its
  destination: the newest preparation of its first mint, a resolution's included, is that refusal, made while the asset
  could be minted, and either it is the refusal whose deadline began the review or it was made after the review began;
  no delivery anomaly is recorded for it and no mint of it is observed. A recorded resolution with a second approver
  refunds it to its refund destination (`REQ-IN-11`, `REQ-RV-2`, `REQ-GV-5`). An unknown never opens it (`REQ-RF-1`).
  No other deposit gains this edge (amendments 0029, Open 2, and 0031)".
- **`REQ-RV-2`:** "… except the one edge to `Refunding` of a directed deposit (§5.4), which rests on the issuing
  network's refusal of its destination as the evidence stands when the review is resolved."
- **`REQ-IN-11`:** "A directed arrival that its route's limits, its route's capacity, a pause, a hold, a change of its
  token's code or a bound on the issuing network keep from being minted MUST wait in the named condition that applies
  (§10), and resume by itself, or, under a change of its token's code, once the hold is lifted (`REQ-AS-12`). It MUST
  NOT be returned for any of them. The time it waits for its route's window or capacity, or under the hold of its
  token, MUST NOT count toward the route's deadline (§5.4). While it waits for the window, it claims room ahead of later
  requests only for one window from when it began to wait, and never more than one window's limit; after that it still
  waits, ahead of every later directed deposit, but no longer counts against requests. One that can never be minted …
  (unchanged) … Only the issuing network's refusal of a destination waits for review instead (`REQ-IN-9`); a review of
  that kind MAY resolve into a refund to the refund destination (§5.4), with a second approver, judged on the evidence
  when it is resolved and never on an unknown."
- **`REQ-LM-4`:** add "A directed deposit is not a request: over its route's capacity it waits
  (`CND-DepositCapacity`, `REQ-IN-11`) and is never returned for it; while it waits it counts toward the route's capacity,
  ahead of later requests."
- **`REQ-AS-12` (new):** "When the code of an admitted origin token changes on its network, bezent MUST hold the directed
  arrivals of that token there made at or after the change: each waits (`CND-TokenUpgraded`), and is neither minted nor
  returned, until an operator lifts the hold by a recorded action that a second approver approves (`REQ-GV-5`), after
  bezent's checks of the token's behaviour have been run again against the new code. A requester's deposit of the token
  is not held by it. The change MUST page."
- **§10, new conditions:**
  - "`CND-DepositCapacity` | A directed deposit over its route's capacity; resumes by itself when the route has room, in
    the order of arrival (`REQ-IN-11`, `REQ-LM-4`). The time held does not count toward the route's deadline (§5.4)".
  - "`CND-TokenUpgraded` | A directed deposit of an admitted token whose code changed at or before its arrival, the hold
    not lifted; resumes by itself once an operator's lift, with a second approver, is recorded (`REQ-AS-12`). The time
    held does not count toward the route's deadline (§5.4)".
- **§10, `CND-DepositWindow`:** add "The time held does not count toward the route's deadline (§5.4). A waiting deposit
  counts against requests only for one window from when it began to wait, and the waiting deposits together for no
  more than one window's limit (`REQ-IN-11`)".
- **`AC-A6.3`:** "Given a directed arrival held by a pause, a hold, the mint window, its route's window or its route's
  capacity, Then it waits and resumes by itself, in the order of arrival, nothing is returned, and the time it waited for
  the route's window or capacity does not count toward its deadline (`REQ-IN-11`, `REQ-LM-3`, `REQ-LM-4`)."
- **`AC-A6.7` (new):** "Given directed deposits waiting for their route's window, Then a request is refused for their
  claims only for one window from when each began to wait, and for no more than one window's limit, and they still
  resume in the order of arrival (`REQ-IN-11`, `REQ-LM-1`)."
- **`AC-A6.8` (new):** "Given a change of an admitted origin token's code, Then a directed arrival of that token at or
  after it waits holding `CND-TokenUpgraded`, is neither minted nor returned, and resumes only once a lift with a second
  approver is recorded; a requester's deposit of the token is not held (`REQ-AS-12`, `REQ-GV-5`)."
- **`AC-A6.9` (new):** "Given a directed deposit in review from `Source-Final` with no mint ever signed, Then it may be
  resolved into `Refunding`, with a second approver, only while the newest preparation of its first mint is the issuing
  network's refusal of its destination, the asset mintable, made after the review began or the one whose deadline began
  it; a later preparation that passes, or one refused for anything else, closes it (`REQ-RV-2`, `REQ-RF-1`,
  `REQ-IN-11`)."
- **§15:** Epic A's principal requirements gain `REQ-AS-12`.

## Text applied to `PLAN.md`

- **`DEC-21`:** add the store's side. "A directed deposit over its route's capacity holds `CND-DepositCapacity`
  (`directed_capacity_fits()`, migration 0150) under the route's lock; it counts toward `route_usage_locked()`'s in-flight
  count like any transfer. The window's count (`route_window_used()`, called by `route_usage_locked()` and by the
  ingress's route table) counts a waiting deposit's claim against requests only for one window from when it began to wait,
  and the claims together up to one window's limit; `directed_window_fits()` still counts every deposit waiting ahead.
  The review edge's store function, `directed_review_refund_open()`, judges the newest preparation at resolution. An
  `Upgraded` event of an admitted token (`token_upgrade`, migration 0147) holds the directed deposits of that token at or
  after its block (`directed_token_held()`, `CND-TokenUpgraded`) until two operators lift it
  (`operator_request_token_hold_lift`, `operator_approve_token_hold_lift`, in `admin_actions`) after the fork suite
  (`services/evm-endpoint/tests/fork.rs`) has been run against the new implementation; the store refuses a mint order for a
  held deposit."
- **`DEC-21`'s cost:** add "Anyone can route USDC through the vault to a third party's `refund` address at bezent's gas
  cost by sending what can never mint (amendment 0031, `d-f018-return-routing`): accepted, watched by
  `directed-returns-alert`."
- **§4's `F-018` row** and `features/F-018-directed-deposits/`: done when `AC-A6.1` to `AC-A6.9` hold.

## Identifier ledger

| Action | Identifiers |
|---|---|
| Added | `REQ-AS-12`, `CND-DepositCapacity`, `CND-TokenUpgraded`, `AC-A6.7`, `AC-A6.8`, `AC-A6.9`; in the plan `T-018-22` to `T-018-25`; in the store migration 0150 |
| Revised | §5.4 (the directed row), `REQ-IN-11`, `REQ-LM-4`, `REQ-RV-2`, `AC-A6.3`, §10 `CND-DepositWindow`, §15 (Epic A); in the plan `DEC-21`, §4 (`F-018`) |

None of the new identifiers was used before.
