<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0029 — a deposit that names its own destination needs no requester

**Status:** **Proposed**, 2026-11-08. Nothing here is applied: `SPEC.md` and `PLAN.md` are unchanged, and every text below
is a proposal. The direction is @pandres95's ruling in chat, 2026-11-08 (below); the design that carries it out, and
every item under "Open", wait for his ruling.
**Target:** `SPEC.md` v0.5.11 and `PLAN.md` v0.6.10, as pull request #112 (amendment 0028) leaves them; v0.5.10 and
v0.6.9 on `develop` today. If applied, `SPEC.md` v0.5.12 and `PLAN.md` v0.6.11; if another amendment takes those
versions first, this one takes what is next.
**Date:** 2026-11-08
**Needs:** a ruling on the design (A1 to A6) and on each item under "Open". The feature that builds it is `F-018`
(`features/F-018-directed-deposits/`). Numbered 0029 because 0027 and 0028 are taken by open pull requests (#111, #112).
**Rationale in one line:** today a deposit is minted only when a declared requester binds the arrival, because a plain
transfer to the reserve carries no destination; if the funder names the destination in the same transaction as the
value, through code bezent declares, the arrival describes itself, nobody can redirect it, and anyone can bridge from a
web page.

## The ruling this applies

@pandres95, in chat, 2026-11-08: open the way in, so that it becomes permissionless. "Anyone using a web app can bridge
assets, and then use them on Kreivo. That's the best proof we did it truly permissionless, and the best proof we're
ready to start using this on production." A web app on Cloudflare (working name `bridge.bloque.network`), where a user
signs and executes with MetaMask or a similar wallet. "It's not just a plain amendment, it's a whole feature set." The
compliance question is ruled (open it) and is not reopened here.

## Why a deposit is permissioned today

Read on `develop` at `cb9872f`:

- **The arrival carries no destination.** A deposit on Polygon is a plain ERC-20 transfer of native USDC
  (`0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359`) to the reserve vault. The `Transfer` log names the token, the sender,
  the vault and the amount, and nothing else (`PLAN.md` `DEC-8`, `DEC-9`).
- **A requester names it.** A final arrival waits as `CND-Unassociated` until a requester binds it by coordinate
  (`REQ-IN-3`, `REQ-RQ-3`; the ingress's `POST /v0/deposits`). Where the deposit goes is a requester fact (§4.2, the
  first row; §4.3, "Can the requester send someone else's deposit to the wrong place? Yes").
- **Only the right requester may name it.** Amendment 0026 limits binding to an arrival whose funder is one of the
  requester's declared senders (`REQ-PN-3`, `ERR-SenderNotDeclared`). A person with no requester has no way in.
- **Withdrawals are already open.** Anyone holding a bridged asset may redeem it on the issuing network (§7.F,
  "Redeem: Anyone"; `US-B5`). Only the way in needs opening.

The specification already allows the piece this needs. `REQ-RN-5`: an origin network must let "a movement … carry an
on-chain reference atomically with the value … by the token itself or by on-chain code that moves the value and emits
the reference in one transaction". What follows uses the same allowance for a destination.

## The design

### A1. A directed arrival

A **directed arrival** is an arrival made by **directing code**: on-chain code on an origin network, declared for one
reserve, that in one call takes an admitted token from its caller into the reserve address and records, in the same
transaction, the destination on the issuing network. The code never holds value, so the reserve stays the vault, and
proof of reserves is unchanged (`REQ-BK-5`, `REQ-BK-6`).

On Polygon (the plan's part, `DEC-21` below), the directing code is a small contract, `DirectedDeposit`. It
`transferFrom`s USDC from `msg.sender` straight to the vault, then emits `Directed(sender, amount, recipient)`, with
`recipient` a Kreivo account as `bytes32`. Its limits are immutable, it has no owner, and it cannot be upgraded.
`features/F-018-directed-deposits/plan.md` has the interface.

Because the funder names the destination in the call that moves its own value, nobody else can bind the arrival to
another destination: there is no window between arrival and request for anyone to use. The binding problem that
amendment 0026 solves for plain transfers does not arise.

### A2. Becoming a deposit without a request

A final directed arrival becomes a deposit by itself. It begins at `Source-Final`, as a requester-bound deposit does
(`REQ-IN-3`), with:

- its bridge reference derived from the arrival's coordinate, as today (`services/ingress/src/refs.rs`, `bz-d-…`);
- its destination the recipient the directing code recorded, never anything else;
- its refund destination its funder, the account the token moved out of, which is the directing code's caller;
- no requester, and no request reference. The coordinate stands in for the request reference wherever one is used to
  find the transfer (`REQ-ST-1`, `REQ-ST-2`). `REQ-RQ-8` already answers status by coordinate, but only to an
  authenticated requester (`/v0/transfers/by-coordinate`), so it does not serve a depositor who is not one (Open 8).

Nothing in the request surface opens. `REQ-PN-1` still holds: a directed arrival is not a request. bezent creates the
deposit from its own observation, and the store refuses a directed deposit whose destination or refund destination is
not the observed one (the store-layer check of `F-018`, as amendment 0026's `BZ001` backs the ingress).

From `Source-Final` on, the deposit is an ordinary deposit: every rule of §5.5, every bound of §7.F, the conditions of
§10, review (§5.9) and the reserve book (§5.3). It counts as `OwedMint` from its finality until its mint is final, like
any arrival.

### A3. Coexisting with requesters and amendment 0026

| Arrival at the reserve | Who decides its destination | How it becomes a deposit |
|---|---|---|
| A plain transfer (Swaps, an evmhot sweep until it goes through the directing code, anyone; `F-018` plan, part 5) | A requester, one of whose declared senders is its funder (`REQ-PN-3`) | Unchanged: a deposit request names its coordinate (`REQ-RQ-3`) |
| A directed arrival | Its funder, in the same transaction | By itself (A2). **No requester may bind it**: a deposit request, or a requester's refund request (`REQ-RQ-6`), naming its coordinate is refused `ERR-SourceNotFound` ("does not hold a movement bezent can bind"), and binds nothing. The one transfer besides its deposit that may name it is its return, which bezent orders itself, to its funder (A5) |

- A directed arrival is never also a plain arrival. On Polygon the vault receives one `Transfer` log per directed
  deposit, whose `from` is the user. The endpoint classifies that log as a directed arrival when the directing code's
  event pairs with it (A4), so a requester that declared the user's address as a sender (0026) still cannot bind it.
  One movement is one observation, consumed once (`INV-2`, `INV-10`).
- The directing code is never a declared sender, and never needs to be: it is not the `from` of any transfer into the
  vault. Amendment 0026's Open 4 (shared contracts as senders) does not reach it.
- A plain transfer to the vault keeps needing a requester. A person who sends USDC to the vault directly, without the
  directing code, has made an unassociated arrival (`REQ-IN-6`). The web app never does that.

### A4. Observing it (the EVM endpoint)

Pairing works as payouts are paired today (`F-009` NOTES, decision 5). A `Directed` log counts only when **all** of
these hold:

- it comes from the declared directing code's address;
- the log right before it, in the same transaction, is the admitted token's `Transfer`;
- that `Transfer` has `from` equal to the event's `sender`, `to` equal to the vault, and `value` equal to its `amount`,
  above zero.

The pair is recorded **once, at the `Transfer` log's coordinate**, as an observation of kind `directed_arrival`, with
the recipient and the event's log index in its detail. The `Transfer` log is not also recorded as an `arrival`. A
`Directed` log that does not pair is recorded as `unpaired_direction`: never an arrival, and never bindable. Adjacency,
not order inside the transaction, so a wallet that batches several deposits in one transaction gets one pair each.
Finality, the two-provider cross-check, gap filling and the hash re-check are unchanged (`DEC-8`, `DEC-12`; `REQ-OB-*`).

### A5. What happens to a directed arrival bezent cannot mint

There is no request time on this path: the value has moved before bezent sees it. So `REQ-LM-2` ("refuse before any
value moves") is met where it can be, in the directing code, and everything else either waits or goes back.

| Case | Where it is caught | Outcome |
|---|---|---|
| Below the route's minimum (1 USDC), above its per-transfer maximum (50 000 USDC), a zero recipient, the code paused (A6) | The directing code: the call reverts | **Nothing moves.** `REQ-LM-2` literally. No refund needed, no gas spent by bezent |
| The route or asset paused, or held by the reserve check (amendment 0028), the Kreivo mint window or supply ceiling spent (`CND-MintWindow`), the delivery account's fee position (`CND-FeeShort`) | The core and the contract's dry run, as today | **Waits** in the named condition and resumes by itself (`REQ-GV-1`, `REQ-LM-3`). `REQ-LM-3` says a mint-window shortfall MUST NOT start a refund, so the brief's "refused by the Kreivo contract's bounds refunds" is not proposed for these |
| Over what the route's window (120 000 USDC a day) has left | The core, when it creates the deposit | **Open 1.** Proposed: waits, under a new `CND-DepositWindow`, and resumes as the window rolls over, first come first served by coordinate. The alternative is the return below |
| The issuing network refuses the mint because of its destination (`CND-DestinationRefused`; on Kreivo, a recipient with no provider) | The contract's dry run, as today | Retried until the route's deadline, then `Needs-Review` (`REQ-IN-9`, unchanged). If the user makes the account able to receive in time (the web app says how), it mints. **Open 2** covers what a review can do with it |
| **Can never be minted**: the route's limits changed so that the amount is outside them; the recipient is one bezent refuses by rule (the issuing network's on-chain component, a declared ledger contract account, `DEC-15`); the route no longer declared | The core, when it creates the deposit | **Returned to its funder** (below) |

**The return** reuses the refund of an unassociated arrival (`REQ-RQ-6`, bridge reference `bz-u-…`): a transfer of its
own (`REQ-RF-3`), paid from the reserve on its network through the vault's delivery role, with its own on-chain
reference and its own proofs of non-execution (`REQ-NX-*`). It differs in two points:

1. **bezent orders it, and its destination is fixed.** No requester asks, and nobody supplies a destination: it is the
   funder, who called the directing code itself. `REQ-RF-2`'s purpose (bezent never chooses a refund destination
   afterwards) holds, because the rule chose it before the value moved.
2. **Approval.** `REQ-IN-6` and `REQ-RQ-6` require a second approver to release an unassociated arrival. For a return
   the destination is not anyone's choice, so the second approver guards nothing that the rule does not already fix.
   **Open 3** asks whether it may run without one.

How it fits the existing refund rules:

- **`REQ-RF-1`** (only when the leg it replaces is proven not executed, at finality). The leg is the mint, which was
  never ordered. The deposit is closed to minting in the store before the return is recorded: no mint intent can be
  recorded for it, and none was signed. Amendment 0014 counts such an attempt as proven for `INV-8`; the same reading
  applies here.
- **`REQ-RF-2`** (a deposit's refund destination is given in its request). Reworded: for a directed deposit, it is its
  funder.
- **`REQ-RF-4`, `REQ-RF-5`** (withdrawals) are untouched. A withdrawal's refund is still a refund mint to its redeemer.
- **`DEF-5`** defers "automatic return of unassociated arrivals to their sender", because "a sender is often an
  exchange's shared address. Returning there can lose the value." That reason does not hold for a directed arrival: its
  funder called the directing code and approved the pull, and an exchange's withdrawal does not do that. Proposed:
  narrow `DEF-5` to arrivals that are not directed.
- **Costs.** A return's network cost goes against surplus, like any payout (`REQ-BK-4`), and draws on the vault's
  delivery allowance, the same allowance withdrawals use (`REQ-KY-3`). See "New exposures".

### A6. The pause on the directing code

Proposed (**Open 4**): the code carries one switch, `paused`, and nothing else. An immutable `pauser` may set it, alone
(`REQ-GV-2`: pausing by one party). An immutable `unpauser`, a multi-signature account, may clear it (`REQ-GV-2`:
unpausing by several). Neither can move value or change a limit. Rotating either key means deploying new code. This
stops value flowing into a vault that is itself under attack, which bezent's own pause cannot do: bezent's pause only
parks directed deposits after their value has arrived. The alternative is no switch at all, an ownerless contract
whose only stop is bezent's pause and the web app.

## What stays as it is

- **Route limits**: minimum 1 USDC, maximum 50 000, window 120 000 a day (`REQ-LM-1`, `DEC-17`). The directing code
  checks the first two itself, with the same values. The core still checks all three.
- **Supply bounds** on the Kreivo contract: ceiling, mint window, per-transfer maximum (§7.F, `INV-3`). A directed mint
  is a mint like any other, ordered by the delivery role. The code's maximum must not exceed the contract's
  per-transfer maximum, or a directed deposit could fail its dry run without a named condition. The verifier checks
  this (`F-018`).
- **The reserve check** and its second reading (amendment 0028): a money pause needs two diverging evaluations at newer
  final points, and a blind check holds. A directed arrival is `OwedMint` from its finality, so it adds a term the
  check already itemises (`REQ-BK-2`). Nothing about the check changes.
- **The trust model.** bezent mints only against a final arrival it observed itself (`INV-2`, `INV-6`). For a directed
  deposit, the answer to "Can the requester send someone else's deposit to the wrong place?" becomes "No: there is no
  requester, and the funder named the destination in the transaction that moved its value." §4.3 gains that row.
- **Withdrawals.** Unchanged. The web app is a client of `redeem` and nothing more.

## New exposures, and a mitigation for each

A griefer loses almost nothing: each unit deposited can be redeemed back one for one. So each exposure below is priced
in what it costs **bezent**, not the attacker.

| Exposure | What it costs | Mitigation |
|---|---|---|
| **Spam at the minimum.** Many 1 USDC deposits | Each new mint reference takes about 0.0067 KSM of storage deposit from the Kreivo delivery account (`DEC-18`), plus its fee. A full window at 1 USDC is 120 000 mints, about 800 KSM a day. Each redemption back costs a Polygon payout | A **count cap** per window on directed mints (Open 6), the fee-position page (amendment 0025), and the option of a higher directed minimum. The count cap bounds the KSM drain at cap × 0.0067 a day |
| **Window exhaustion.** Directed deposits fill the route's 120 000 window and Swaps' deposits are refused `ERR-WindowExceeded` | Swaps' service, not value | A **directed share** of the window (Open 5): directed deposits may use at most part of it, and the rest stays for requesters. The same share bounds how much of the Kreivo mint window directed mints can take |
| **Return spam and the shared allowance.** Returns draw on the vault's delivery allowance, which withdrawals need too | Withdrawals wait in `CND-PayoutWindow` | Returns happen only for what can never mint (A5), which a well-behaved web app never sends. Returns count against the same count cap and raise an alert past a threshold |
| **Refund gas.** Each return is a Polygon transaction paid from the delivery lane | POL, against surplus | As above; a return of a directed deposit is rare by construction |
| **Bad recipients.** Zero; the Kreivo contract's own account; a declared ledger contract account (`DEC-15`, refused at intake today, and this path has no intake); a well-formed account nobody holds; an account with no provider | From nothing to the whole deposit | Zero reverts in the code. bezent's refused list leads to a return (A5). No provider leads to `CND-DestinationRefused`, and the web app checks first (below). An account nobody holds cannot be detected by anyone: the web app takes the recipient only from a connected Kreivo wallet, or from a pasted SS58 address with checksum and prefix 2 checked, and never from raw hex |
| **A recipient with no provider.** bUSDC is "not sufficient" (`DEC-7`), so a mint to an account holding no KSM, or with no other provider, is refused. A fresh account is the common case for a new user | The deposit waits and then needs review | **Open 7.** The web app reads the recipient's account before signing and refuses one that cannot receive. `REQ-IN-9` binds bezent, not the web app. The lasting fix is a governance choice: make bUSDC sufficient, or have the user's account created with a provider (the Virto spike) |
| **Permit front-running.** Anyone can submit a signed permit before the deposit does, so the deposit's own `permit` reverts | One failed transaction: the user's gas | The code calls `permit` inside `try`, then checks the allowance, and goes on if it is enough. It always pulls from `msg.sender`, never from an `owner` argument, so a front-run permit gives the front-runner nothing |
| **Stray tokens.** USDC sent straight to the directing code | Lost: the code has no owner and no way out | Stated plainly. The web app never does it. A permissionless sweep to the vault was considered and left out: it would make an arrival nobody can direct |
| **A compromised web app.** The page could name another recipient | The user's deposit | The wallet shows the call and the typed data, and the app shows the recipient as SS58 next to the connected account. The build is reproducible and its hash published, with a strict CSP, no third-party scripts and no backend secret. The contract's address is published with the reserve addresses (`REQ-AS-11` below) |
| **Cloned contracts and phishing pages.** A look-alike contract that does not go to the vault | The user's deposit, not bezent's | The endpoint watches only the declared code. The web app pins the address. The statement lists it |
| **USDC blacklisting.** Circle can blacklist a depositor after a deposit | A return or payout to it reverts | Its dry run fails, and the payout goes to review as today. Nothing new |

## Proposed text for `SPEC.md`

Guardrail 1 holds: no network, token, standard or product is named below.

- **§2, new term "Directed arrival":** an arrival made by directing code, which records in the same transaction where
  on the issuing network it is to be minted. Its funder is the account the value moved out of, which is the directing
  code's caller.
- **§2, new term "Directing code":** on-chain code on an origin network, declared for one reserve (`REQ-AS-11`), that
  takes an admitted token from its caller into the reserve address and records the destination, in one transaction,
  and holds no value.
- **§1.2, "Deposits":** "When an arrival is final on its backend and either a requester binds it with a deposit request,
  or it is a directed arrival, which names its own destination, bezent mints …".
- **§1.4:** "End users never call bezent. Value they bring in reaches a reserve address through the requester, or
  through directing code they call themselves on the origin network …".
- **§3, the funder's row:** "None over bezent. A funder may direct its own arrival through directing code
  (`REQ-IN-10`)."
- **§4.2, first row:** "Who a deposit is for; where a refund goes | Requester; for a directed arrival, its funder, on
  chain (`REQ-IN-10`)".
- **§4.3, new row:** "Can anyone redirect a directed arrival? | **No.** Its destination is recorded in the transaction
  that moved its value, and no request can bind it (`REQ-IN-10`)."
- **`REQ-AS-11` (new):** directing code MUST be declared for a reserve by an explicit, recorded declaration: its
  network, its address, the admitted token it moves, the reserve address it moves it to, and the limits it enforces. It
  MUST NOT hold value. Its address MUST be published with the reserve addresses (`REQ-BK-5`, §7.E). Code that can be
  upgraded is code that holds reserve for `REQ-GV-3`.
- **`REQ-IN-3`:** add, at the end, "A directed arrival is not bound by any request (`REQ-IN-10`)."
- **`REQ-IN-10` (new):** a final directed arrival of an admitted token at a reserve address MUST become a deposit
  without a request. It begins at `Source-Final` with a bridge reference derived deterministically from the coordinate.
  Its destination MUST be the one its directing code recorded, and its refund destination its funder. No one chooses
  either afterwards. A request naming its coordinate MUST be refused (`ERR-SourceNotFound`) and binds nothing.
- **`REQ-IN-11` (new):** a directed arrival that its route's limits, a pause, a hold or a bound on the issuing network
  keep from being minted MUST wait in the named condition that applies (§10), and resume by itself. One that can never
  be minted under the route's current declaration, or whose destination bezent refuses by rule, MUST be returned to its
  funder by a refund of an unassociated arrival (`REQ-RQ-6`) that bezent orders itself, with its funder as destination
  [and with a second approver, if Open 3 says so]. Only the issuing network's refusal of a destination waits for review
  instead (`REQ-IN-9`).
- **`REQ-IN-6`:** the release by refund also covers "or, for a directed arrival, its return to its funder
  (`REQ-IN-11`)".
- **`REQ-RF-2`:** "The refund destination of a deposit MUST be given in its request, or, for a directed deposit, be its
  funder (`REQ-IN-10`). bezent MUST NOT choose it afterwards."
- **`REQ-RQ-3`:** add "A coordinate holding a directed arrival is refused (`ERR-SourceNotFound`)."
- **`REQ-LM-1`:** add "A route MAY declare a share of its window, and a count per window, for directed arrivals."
  [Open 5, Open 6]
- **`REQ-ST-1`:** "Given only a request reference, or for a directed deposit the coordinate of its arrival, anyone
  MUST be able to find …".
- **`REQ-ST-8` (new, if Open 8 says yes):** the state, named conditions and coordinates of a directed deposit, and of
  a withdrawal, MUST be readable by anyone, without authentication, by the arrival's coordinate or the redemption's
  on-chain reference. The answer MUST NOT carry anything a requester supplied.
- **§10, new condition `CND-DepositWindow` (if Open 1 says park):** "A directed deposit over what its route's window,
  or its directed share, has left; resumes by itself as the window rolls over, in the order of arrival."
- **`US-A6` (new):** "As **anyone**, value I send to a reserve through its directing code is minted for the destination
  I named once it cannot be taken back, without any requester."
  - `AC-A6.1` Given a final directed arrival, Then a deposit exists at `Source-Final` with the recorded destination and
    my account as its refund destination, and is minted for exactly its amount (`REQ-IN-10`, `REQ-IN-4`).
  - `AC-A6.2` Given a deposit request, or a refund request, by any requester naming a directed arrival's coordinate,
    Then it is refused (`ERR-SourceNotFound`) and binds nothing; only bezent's own return to the funder may name it
    (`REQ-IN-10`, `REQ-IN-11`, `REQ-RQ-3`, `REQ-RQ-6`).
  - `AC-A6.3` Given a directed arrival held by a pause, a hold or the mint window, Then it waits and resumes by itself,
    and nothing is returned (`REQ-IN-11`, `REQ-LM-3`).
  - `AC-A6.4` Given a directed arrival that can never be minted, Then it is returned to its funder, once, and to no one
    else (`REQ-IN-11`, `REQ-RF-2`, `INV-2`).
  - `AC-A6.5` Given an amount the directing code's limits refuse, Then nothing moves (`REQ-LM-2`).
  - `AC-A6.6` Given the transfer into the reserve of a directed arrival, Then it is one observation, consumed once, and
    never a plain arrival (`INV-2`, `INV-10`).
- **`DEF-5`:** "Automatic return of unassociated arrivals to their sender, **except a directed arrival's return to its
  funder (`REQ-IN-11`)**."
- **§15:** Epic A gains `US-A6`; its principal requirements gain `REQ-IN-10`, `REQ-IN-11` and `REQ-AS-11`.

`REQ-PN-1`, `REQ-PN-3`, `REQ-RQ-7`, `REQ-AR-*`, `REQ-BK-*`, the invariants and the transition table are unchanged,
unless Open 2 adds an edge.

## Proposed text for `PLAN.md`

- **`DEC-21` (new), Directed deposits on Polygon.** One `DirectedDeposit` contract per origin network that admits
  directed arrivals; Polygon first, for bUSDC. It pulls USDC from `msg.sender` to the vault with `transferFrom` in the
  same call as the event; supports EIP-2612 `permit` (one signature), including the bytes-signature form for contract
  wallets (FiatToken 2.2); takes the recipient as `bytes32`; limits immutable and equal to the route's; no owner, no
  upgrade, no proxy; the pause of A6 if ruled. Deployed by a one-time deployment key that holds no role afterwards, as
  the vault's glue is (`DEC-10`, `F-008` NOTES, "Deployment"); `VerifyVault` grows a check of its runtime code
  (immutables included) against the repository's build for the configuration. *Serves:* `REQ-RN-5`, `REQ-AS-11`,
  `REQ-IN-10`, `REQ-LM-2`, `REQ-GV-3`. *Cost:* one more contract of ours in the path of value, which needs review
  before mainnet, and a new address per change of limits.
- **`DEC-8` and `DEC-9`:** add "A transfer into the vault paired with the declared `DirectedDeposit` event in the same
  transaction is a directed arrival, not an arrival; it becomes a deposit without Swaps (`DEC-21`)."
- **`DEC-15`:** add that the refused list of ledger contract accounts also applies to directed recipients, as a return.
- **`DEC-22` (new), the bridge web app.** A static, client-only web app (`apps/bridge/`) on Cloudflare Workers static
  assets, at the working name `bridge.bloque.network`: deposit with an EVM wallet (EIP-6963 discovery; MetaMask and
  similar), redeem with a Kreivo wallet (Talisman, SubWallet or Polkadot.js first; Virto's passkey accounts after a
  spike), status from public chain data, and the public status route if Open 8 is ruled yes. It holds no key and no
  secret. *Serves:* `US-A6`, `US-B5`, `REQ-ST-1`. *Cost:* a public front end to keep and to defend against phishing.
- **§4, new row `F-018`:** "Directed deposits and the bridge web app | `M-6` | directed arrivals, their returns and the
  public way in (`US-A6`, `REQ-IN-10`, `REQ-IN-11`, `REQ-AS-11`) | the EVM endpoint, the core and the contract (`F-009`,
  `F-003`, `F-006`)".
- **§5, new milestone `M-6` (Open 10):** "Permissionless deposits | Anyone bridges USDC on Polygon to bUSDC on Kreivo
  and back from a web page, with no requester | `F-018` | A round trip from the web app on mainnet starting with a
  1 USDC deposit (Open 14), a directed
  return exercised, the spam caps in force, and every new runbook exercised". After `M-4`.

## Open

Each is one question for @pandres95, with a recommendation.

1. **Window.** Should a directed deposit over what the route's window has left wait for the window to roll over, rather
   than be returned? *Recommended: wait* (`CND-DepositWindow`). Your brief said return. Waiting costs no gas, keeps
   `REQ-LM-3`'s stance for every bound that clears itself, and a griefer who fills the window only delays others. It
   does not make bezent pay a return for each of them.
2. **A recipient that can never receive.** Should a directed deposit in review for `CND-DestinationRefused`, with no
   mint ever signed, be able to resolve into a refund to its funder? Today `Source-Final → Refunding` is not an edge
   (`crates/core/src/state.rs`, `edge`), so such a deposit stays in review until its recipient can receive.
   *Recommended: yes*, as one new edge for directed deposits only, with a second approver, resting on the deposit being
   closed to minting (amendment 0014's reading).
3. **Second approver for a return.** May bezent return a never-mintable directed arrival to its funder without a
   second approver? *Recommended: yes*, because the destination is fixed by rule. Returns count against the directed
   count cap (Open 6), and an alert fires past a threshold.
4. **A pause on the directing code.** Should `DirectedDeposit` carry a pause switch (an immutable `pauser` alone, an
   immutable multi-signature `unpauser` to lift it)? *Recommended: yes.* It is the only thing that stops value flowing
   into a vault under attack. The `pauser` should be a key of its own on Polygon (`REQ-KY-4`), not the Kreivo pause
   key.
5. **Directed share of the window.** Should directed deposits be limited to a share of the route's window?
   *Recommended: yes, 20 000 USDC a day to start*, so that the other 100 000 stays for Swaps and directed mints take at
   most that much of the Kreivo mint window. Raise it from what the first weeks show.
6. **Count cap.** Should directed deposits also be capped in number per window? *Recommended: yes, 200 a day to start.*
   Each new mint reference costs the delivery account about 0.0067 KSM of storage deposit, so 200 cost at most about
   1.3 KSM a day. Without a cap, a full window at the 1 USDC minimum would cost about 800 KSM.
7. **bUSDC sufficiency.** Should governance make bUSDC a sufficient asset on Kreivo, so that a fresh account can
   receive it without KSM? *Recommended: not now.* Ship with the web app's check, which refuses a recipient that cannot
   receive and tells the user how to fix it. Decide after the Virto spike (Open 9) shows whether passkey accounts carry
   a provider. Sufficiency is a governance act with its own dust and spam trade-offs.
8. **Public status route.** Should the ingress serve an unauthenticated, read-only status route for directed deposits
   and withdrawals (`REQ-ST-8`)? *Recommended: yes*, as a separate listener, rate-limited at Cloudflare, answering by
   coordinate or redemption reference only, never by request reference, and never with a requester's fields. Chain data
   alone shows the happy path (deposit, then mint, by the derived reference), but not a condition (waiting for the
   window, a pause, a hold, review), which is exactly what a user needs to see. `/v0/transfers/by-coordinate` cannot
   serve this today: it sits behind a requester's credential.
9. **Which Kreivo wallet to lead with.** Should the first release lead with browser extensions (Talisman, SubWallet,
   Polkadot.js) and add Virto's passkey accounts once a spike proves them? *Recommended: yes.* The extensions can sign
   a contract call today, and both Talisman and SubWallet list Kreivo. But they need KSM for fees and for the
   provider. Virto's `KreivoPassSigner` (papi-signers) is the plain-user path: no seed phrase, fees from a membership's
   gas tank. It still has to show it can sign `Contracts.call` to `redeem`, that a pass account can hold bUSDC, and who
   grants the membership. A two-to-three-day spike settles all three.
10. **Milestone.** Is `F-018` a new milestone, `M-6`, after `M-4`? *Recommended: yes.* It needs bUSDC live, and nothing
    in `M-4` needs it. No `M-6` exists on GitHub yet, so the pull request carries none until this is ruled.
11. **Release class.** Is the release that opens directed deposits on mainnet a value-moving release under amendment
    0024? *Recommended: yes.* It adds no asset or network, but it opens a new way for value in, so it gets the full gate,
    with the mainnet round trip (Open 14) run inside its soak.
12. **Licence of the web app.** Which licence does `apps/bridge/` take? *Recommended: GPL-3.0-or-later*, by
    @pandres95's rule for `DEC-20` (the GPL wherever it is possible). It also ships smoldot, which is GPL with the
    Classpath exception. `docs/oss/LICENSING.md` gains the row.
13. **Gas for users with no POL.** Should the first release sponsor Polygon gas (a relayer of EIP-3009 authorisations,
    or an ERC-4337 paymaster)? *Recommended: no.* A relayer means pulling from an `owner` who is not the caller, which
    reopens front-running and the refund destination. Revisit after `M-6`.
14. **The 1 USDC mainnet test and the redemption minimum.** *Corrected: no longer a question.* The first
    draft read the redemption minimum from `config/bounds.example.toml` (`redeem_minimum = 10_000_000`, 10 bUSDC) and
    proposed depositing 9 more USDC to redeem 10. That file is an example. Production's bUSDC redemption minimum is
    **1 bUSDC**, set by governance referendum 74, and a 1 bUSDC redemption succeeded on mainnet on 2026-11-09. So the
    mainnet test deposits 1 USDC, mints 1 bUSDC and redeems it whole, with no top-up, and still proves a deposit at
    the route's 1 USDC minimum.

## Identifier ledger

| Action | Identifiers |
|---|---|
| Added, if ruled | §2 "Directed arrival", §2 "Directing code", `REQ-AS-11`, `REQ-IN-10`, `REQ-IN-11`, `US-A6`, `AC-A6.1`–`AC-A6.6`; `REQ-ST-8` (Open 8); `CND-DepositWindow` (Open 1); in the plan `DEC-21`, `DEC-22`, `F-018`, `M-6`, `T-018-*` |
| Revised, if ruled | §1.2, §1.4, §3 (funder), §4.2 (first row), §4.3 (new row), `REQ-IN-3`, `REQ-IN-6`, `REQ-RF-2`, `REQ-RQ-3`, `REQ-LM-1`, `REQ-ST-1`, `DEF-5`, §15; the §5.4 table if Open 2 says yes; in the plan `DEC-8`, `DEC-9`, `DEC-15`, §4, §5 |

None of the new identifiers was used before. `REQ-RQ-10` is avoided because amendment 0016 (pull request #66) proposes
it, and `AC-E2.3` is taken by amendment 0028 (pull request #112).
