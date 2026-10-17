<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Amendment 0001 — one core with backends, the Kreivo contract, redeem, and the owner's rulings to 2026-10-11

**Status:** ruled by the owner on the decision form ("USDB Bridge Decisions") and in chat, 2026-10-04 to
2026-10-11; to be applied to `SPEC.md` v0.3.0
**Target:** `SPEC.md` v0.2.0
**Date:** 2026-10-11
**Rationale in one line:** the bridge becomes one core with a backend per asset class, each bridged asset has
one reserve, the issuing-network end is a contract in a dedicated community that mints to an order's
destination and burns on redemption, and fiat-backed assets are bridged through attested backends.

Every item cites the decision-form items that ruled it (the form is kept privately). Identifiers are never renumbered: a
withdrawn identifier stays withdrawn.

## A1 — One core, a backend per asset class (`gen-core`)

- §0 "Networks" becomes **"Networks and backends"**. The bridge connects one issuing network with
  **backends**. A backend is the counterpart system for one class of origin: a network, or a financial rail
  reached through a named party. Each backend declares an assurance class (A2).
- §4.4(b) is retitled **"Backends"**. `REQ-RN-1`–`REQ-RN-6` keep their text and bind **network** backends.
  Add:
  - **`REQ-BE-1`** Each backend MUST implement the backend contract. It MUST observe an arrival with a stable
    identifier and its finality, deliver idempotently, give evidence of non-execution, and report the reserve
    it holds.
  - **`REQ-BE-2`** Each backend MUST declare its **assurance class**. It is **public** when every observation,
    reserve balance and proof of non-execution can be checked from public data. It is **attested** when they
    are taken from a named party's reports.
  - **`REQ-BE-3`** The statement (`REQ-PR-1`) MUST name each bridged asset's backend and its assurance class.
  - **`REQ-BE-4`** `REQ-BK-6`, `REQ-NX-2` and `REQ-ST-1` bind public backends. For an attested backend, the
    bridge MUST record each report of the attesting party with that party's own identifiers, and the
    statement MUST say that the asset's reserve is attested.
- Glossary: add **Backend** and **Assurance class**. "Origin network" is kept, as "a network backend".
- `DEF-3` (other kinds of origin network) is **withdrawn**: those kinds are now in scope as backends.

## A2 — Attested backends take the ramp's reports (`gen-bank-evidence`, `br-asset-names`)

- Add **`REQ-BE-5`**. For an attested backend whose attesting party is a requester, that requester's report
  of an arrival, a payout or a reserve balance **is** the observation. `REQ-RQ-7` and `REQ-AR-2` ("no caller
  asserts an outcome") bind public backends only.
- Add a "Trade-off, stated plainly" callout after `REQ-BE-5`: for an asset with an attested backend, the
  attesting requester can cause a mint. What bounds it is the on-chain component's mint window and supply
  ceiling (§7.F), and the statement, which names the party whose word backs the asset.
- Notes (not `SPEC.md`): COPM, MXNB and BRLB are backed through Swaps, an attested backend.

## A3 — One reserve per bridged asset (`br-one-reserve`, `br-asset-names`)

- Add **`REQ-AS-8`**. Each bridged asset MUST have exactly one reserve backend. For a network backend, the
  reserve sits on exactly one network. An attested backend MAY hold it in several positions (accounts), each
  itemised (`REQ-BK-5`).
- §1.7 assumption 3 becomes: "**One bridged asset per origin token family, reserved on one backend.**
  Deliveries to any other network go through a protocol venue, straight to the beneficiary (A10). Tokens of
  different issuers are never merged (`REQ-AS-3`)."
- `REQ-OT-4` becomes: "A withdrawal MUST be paid from the asset's reserve, directly on the reserve network,
  or through a protocol venue that delivers straight to the beneficiary on another network."
- `REQ-OT-5` keeps its text, with "that network's treasury" replaced by "the asset's reserve".
- §5.7 is retitled **"Venues"**, and `REQ-RB-1`–`REQ-RB-10` are **withdrawn**: with one reserve per asset
  there is nothing to rebalance. Add **`REQ-VN-6`**. A delivery through a protocol venue is a payout like any
  other. The bridge signs the leg out of the reserve. The venue's delivery to the beneficiary is its
  destination leg, observed at finality. A step that anyone may perform MAY be driven by the requester.
- `US-F1` and `US-F2` and their criteria are **withdrawn**. Epic F becomes "Venues", with **`US-F3`**:
  "As a requester, I pay a withdrawal on a network other than the asset's reserve network."
  - `AC-F3.1` Given a withdrawal to another network, Then the bridge pays it through a protocol venue,
    straight to the beneficiary (`REQ-OT-4`, `REQ-VN-6`).
  - `AC-F3.2` Given the venue's delivery is final, Then the withdrawal is `Completed`.
- `OQ-1` is closed by this item.
- Notes: bUSDC is reserved on Polygon, bEURC on Base, and bBTC on BNB Chain (as BTCB, `br-bbtc-backing`).

## A4 — Mint only on the backend's finality signal (`gen-reversal`, the owner's note)

- Add **`REQ-OB-7`**. A mint MUST rest on the backend's own finality signal. For an attested backend, that is
  the party's report that the payment is final under its rail's rules. The party MUST NOT report a payment
  final before its rail guarantees it.
- Add **`OQ-14`**: "What happens when a payment an attested backend reported final is reversed afterwards?"
  It blocks `REQ-OB-7` and §5.3. `gen-reversal` stays open for discussion.

## A5 — The issuing-network end is a contract in a dedicated community (`br-contract-512`, superseding `br-issuer`)

- `REQ-AR-4` keeps its text: mint and destroy authority stays chain-enforced. The relayer account as issuer
  (`br-issuer`, 2026-10-04) is **not** applied.
- Add **`REQ-HN-10`**. The on-chain component's mint and destroy authority MUST be held through a group
  dedicated to it. No other on-chain program, and no other holder, may exercise that authority. That
  group's governance is the upgrade authority of `REQ-GV-3`.
- `OQ-5` is closed by this item (who governs: the dedicated group's governance).

## A6 — Mint to the order's destination; redeem by anyone, burnt at once (`br-redeem-burn`, the owner's design of 2026-10-11)

- §7.F's table is replaced by:

  | Capability | Who may call | Refuses when |
  |---|---|---|
  | **Mint** an amount of a bridged asset to the address named in the order, with an on-chain reference | The delivery role | The reference was used before; the amount exceeds the per-transfer bound, the mint window or the supply ceiling; the asset or direction is paused |
  | **Redeem**: take an amount of a bridged asset from the caller and destroy it in the same call, recording the backend, the payout instructions or their commitment (A7) and a reference | Anyone | The asset or direction is paused; the amount is below the route's minimum; the instructions do not fit the backend |
  | **Refund mint**: mint, once, to the redeemer of a redemption whose payout is proven not executed, tied to that redemption's reference | The delivery role | That redemption was refunded before; the mint window would be exceeded |
  | **Pause / unpause** per asset, per direction, or all | The pause role / governance | — |
  | **Set bounds**: supply ceiling, mint window, per-transfer maximum | Governance (raising), the pause role (lowering) | A raise without the delay of `REQ-GV-3` |

  The rows "Mint into a ledger", "Escrow call", "Record escrow", "Destroy" and "Return" are **withdrawn**.
- `REQ-HN-8` becomes: "The on-chain component MUST NOT destroy value held by any account but its own. A
  redemption takes the value into the component and destroys it there."
- `REQ-OT-1` becomes: "A withdrawal is funded by a **redemption** on the issuing network, and only by one."
  `REQ-OT-2` becomes: "A withdrawal MUST be bound to its redemption by the redemption's on-chain reference.
  It MUST NOT be matched by amount, sender or time." `REQ-OT-7` and `REQ-OT-8` are **withdrawn**: a
  redemption is the withdrawal's request.
- `REQ-OT-3` becomes: "The bridge MUST NOT submit a payout before the redemption is final."
- `REQ-OT-6` becomes: "The redeemed value is destroyed at redemption. If every payout attempt is proven not
  executed at finality, the redeemer is refunded by one refund mint tied to that redemption."
- `REQ-RF-4` becomes: "A withdrawal's refund is a refund mint to the redeemer, tied to the redemption's
  reference. It counts against the mint window."
- `INV-7` becomes: "Nothing is paid out before its redemption is final." (Service)
- `INV-8` becomes: "No refund mint is made unless every payout attempt for the redemption is proven not
  executed at finality, and at most one is made per redemption." (Service + chain)
- §5.3: the term **Undestroyed** is replaced by **OwedPayout**, the sum of final redemptions whose payout is
  not yet final. The equation becomes:

      Σ Reserve(A) + InTransit(A) = Supply(A) + OwedMint(A) + OwedPayout(A) + Surplus(A)

  `REQ-BK-2` lists OwedPayout in place of Undestroyed. InTransit is now value moving through a protocol
  venue.
- Add a "Trade-off, stated plainly" callout in §5.6: burning at once means a failed payout is made whole by
  the mint authority. A refund mint is bounded by the mint window and is one per redemption, so a stolen
  delivery key cannot use refunds to exceed the window.
- `US-B1`'s criteria: `AC-B1.1` becomes "Given a redemption, Then nothing is paid out before it is final
  (`REQ-OT-3`, `INV-7`)". `AC-B1.2` becomes "Given the payout is final and executed in full, Then the
  withdrawal is `Completed`". `AC-B1.3` becomes "Given the payout is proven never to execute, Then the
  redeemer gets one refund mint, or a new attempt is made (`REQ-RF-1`, `REQ-RF-5`, `INV-8`)". `US-B2` is
  replaced by **`US-B5`**: "As anyone holding a bridged asset on the issuing network, I redeem it to the
  backend I name."
  - `AC-B5.1` Given a redemption, Then its value is destroyed in the same call, and its reference and
    instructions (or their commitment) are recorded in order (`REQ-OT-1`, `REQ-HN-7`).
  - `AC-B5.2` Given the same reference again, Then it is refused (`INV-2`).
- `CND-ReserveShort` keeps its text with "the origin network's treasury" replaced by "the asset's reserve".

## A7 — Payout instructions: inline for network backends, a commitment for attested ones (`br-redeem-instructions`)

- Add **`REQ-RQ-9`**. A redemption MUST carry its payout instructions inline when the backend is public (a
  destination address), and only a commitment (a hash of the instructions) when the backend is attested. The
  bridge MUST accept instructions relayed for a commitment only when they hash to it.
- `REQ-PV-1` applies unchanged: no personal data on any network.

## A8 — Recipients are responsible for receiving (`br-recipients`, chat, 2026-10-11)

- `REQ-HN-4` is **withdrawn**.
- Add **`REQ-IN-9`**. The bridge MUST NOT check whether a destination on the issuing network can receive the
  asset. A mint the network refuses because of its destination holds `CND-DestinationRefused` and mints
  nothing. Past the route's deadline it goes to `Needs-Review`.
- `CND-DestinationRefused` becomes: "The issuing network refused a mint because of its destination; nothing
  was minted."

## A9 — No direct delivery into a ledger in V0 (`br-kreivo-hop`)

- `REQ-IN-5` becomes: "A deposit's destination is an address on the issuing network. Delivering into a ledger
  is the requester's, from its own account."
- `REQ-HN-6` is **withdrawn**. `REQ-LG-1` becomes: "The bridge MUST NOT call a ledger's contract, hold a
  ledger's keys, accounts or authority, or report outcomes to it. Value reaches a ledger through the
  requester, and leaves it through the requester's redemption." `REQ-LG-2` is **withdrawn**.
- `US-A3` and its criteria are **withdrawn**.
- `OQ-10` and `OQ-13` are closed by this item.

## A10 — Conversion on a protocol venue; no custodial venue in V0 (`br-dex`)

- `REQ-CV-2` becomes: "A conversion on a deposit MUST be executed by the bridge at a protocol venue, as one
  transaction whose output lands in the reserve. The deposit is then minted against **that** output, at its
  observed amount, under every rule of §5.5. The deposit records the convertible arrival and the converted
  output, with both amounts."
- `REQ-CV-3` becomes: "A withdrawal MAY name a convertible token. The bridge converts the reserve token at a
  protocol venue, in the payout transaction or just before it, and pays the beneficiary the output. The
  delivered amount is reported with both amounts."
- Add **`DEF-8`**: "Custodial venues, such as an exchange, for conversion or for delivery to another network.
  V0 uses protocol venues only (`br-dex`)."
- `REQ-RB-6`'s custodial bound goes with the withdrawn `REQ-RB-*` (A3). `REQ-VN-3` keeps only its first
  sentence. `REQ-VN-5` is **withdrawn**.
- `OQ-9` is closed by this item.

## A11 — No spec change

| Item | Ruling | Lands in |
|---|---|---|
| `br-split` | The core/operator split | §4.2 already says it |
| `gen-reserve-book` | Reserve book in the core | The plan (`REQ-BK-*` stand) |
| `br-fiat-on-kreivo` | Fiat-backed assets live on the issuing network | A deployment decision (the owner's ruling); A1–A2 cover it |
| `br-usdb-params` | 6 decimals, not sufficient | The plan, per asset; A8 covers "not sufficient" |
| `br-dex` (Binance out) | No Binance in V0 | A10 |
| `gen-legal` | Licences already covered | Nothing |
| `br-asset-names` | bUSDC, bEURC, bBTC; COPM, MXNB, BRLB through Swaps | Notes and the plan; `OQ-4` closed |

## A12 — Consequential edits (follow from A3, A6, A9 and A10; no new choice)

Text that contradicts the rulings above, found while applying them. Each change only aligns it.

- **`REQ-IN-8`** becomes: "A deposit whose mint the issuing network refuses holds `CND-DestinationRefused`,
  with nothing minted (`REQ-IN-9`)." Its retry interval, deadline and refund-destination clauses are
  removed (A8, A9: there is no ledger destination).
- **`REQ-RQ-4`** becomes: "**Request a withdrawal:** a redemption is the request. The requester MAY add, by the
  redemption's reference, the refund destination and an expiry, and for an attested backend the relayed
  instructions (`REQ-RQ-9`)." (A6)
- **`REQ-RQ-5`** is **withdrawn** (A3: no rebalancing). **`REQ-RQ-6`** keeps its text.
- **`REQ-HN-7`** becomes: "Every mint, redemption and refund mint MUST emit an event carrying its on-chain
  reference, in an order anyone can read." (A6)
- **`AC-B4.1`** becomes: "Given a withdrawal naming a convertible token, Then the bridge converts at a protocol
  venue and pays the beneficiary the output (`REQ-CV-3`)." (A10)
- **`REQ-PN-2`** becomes: "Requester authority MUST be scoped by kind: issuing deposit addresses, requesting
  deposits, and attesting for an attested backend are separate grants." (A2, A3)
- **`REQ-BK-2`**, **`REQ-BK-4`** and **`REQ-BK-5`**: replace "rebalance" with "venue delivery". Replace
  "rebalances" with "venue deliveries". (A3)
- **`REQ-VN-2`** and **`REQ-VN-4`**: replace "a rebalance" or "rebalance routes" with "a venue delivery" or
  "venue routes". (A3)
- **`REQ-KY-5`**: drop "every rebalance through a custodial venue". (A10)
- **`REQ-GV-3`**: "on-chain code that holds escrow or reserve" becomes "on-chain code that mints, destroys or
  holds reserve". (A6)
- **`INV-14`**: "transfer, sweep, rebalance or recorded surplus withdrawal" becomes "transfer, sweep, venue
  delivery or recorded surplus withdrawal". (A3)
- **`DEF-6`** is **withdrawn** (rebalances no longer exist). (A3)
- **`REQ-RF-2`**: drop the sentence "A refund into a ledger is a deposit with the account reference supplied."
  (A9)
- **`REQ-PV-4`**: drop "and MUST deliver it to the ledger contract unchanged". The bridge treats any account
  reference it is given as opaque. (A9)
- **§4.4(e) "The ledger contract"** and **`REQ-LC-1`**/**`REQ-LC-2`** are **withdrawn**. The bridge no longer
  calls a ledger contract (A9). "Exactly five external capabilities" becomes "four".
- **Prose**, rewritten to match without adding requirements. The front-matter summary, §1.1(4), §1.2,
  §1.5, the §3 parties table, the §4.1 diagram, the §4.2 table and §4.6 say:
  - one reserve per asset;
  - minting to the order's destination;
  - redemption by anyone, destroyed at once;
  - payouts through protocol venues;
  - no ledger contract calls;
  - backends with an assurance class.
- **Glossary**: remove **Escrow** and **Rebalance**, which are no longer used. Add **Redemption**: "A call
  to the on-chain component that takes a bridged asset from the caller, destroys it, and records where the
  backend pays it out." **Refund mint** is defined in §7.F.

- **§1.1(5)**: the requester cannot cause a mint on a public backend; on an attested backend its report is the
  evidence, bounded by the mint window and supply ceiling. (A2)
- **Glossary "Conversion"** and **§13** "not an exchange": the bridge executes conversions at a protocol venue,
  within the requester's limits. (A10)

## Identifier ledger

| Action | Identifiers |
|---|---|
| New | `REQ-BE-1`–`REQ-BE-5`, `REQ-AS-8`, `REQ-OB-7`, `REQ-HN-10`, `REQ-RQ-9`, `REQ-IN-9`, `REQ-VN-6`, `US-F3`, `AC-F3.1`, `AC-F3.2`, `US-B5`, `AC-B5.1`, `AC-B5.2`, `OQ-14`, `DEF-8` |
| Withdrawn | `REQ-RB-1`–`REQ-RB-10`, `REQ-HN-4`, `REQ-HN-6`, `REQ-LG-2`, `REQ-OT-7`, `REQ-OT-8`, `REQ-VN-5`, `REQ-RQ-5`, `REQ-LC-1`, `REQ-LC-2`, `DEF-6`, `US-A3`, `AC-A3.1`, `AC-A3.2`, `US-B2`, `AC-B2.1`, `AC-B2.2`, `US-F1`, `US-F2`, `AC-F1.1`–`AC-F1.7`, `AC-F2.1`, `DEF-3` |
| Revised | `REQ-IN-8`, `REQ-RQ-4`, `REQ-HN-7`, `AC-B4.1`, `REQ-PN-2`, `REQ-BK-2`, `REQ-BK-4`, `REQ-BK-5`, `REQ-VN-2`, `REQ-VN-4`, `REQ-KY-5`, `REQ-GV-3`, `INV-14`, `REQ-RF-2`, `REQ-PV-4`, `REQ-OT-1`–`REQ-OT-6`, `REQ-RF-4`, `REQ-HN-8`, `REQ-IN-5`, `REQ-LG-1`, `REQ-CV-2`, `REQ-CV-3`, `REQ-VN-3`, `INV-7`, `INV-8`, `CND-ReserveShort`, `CND-DestinationRefused`, `AC-B1.1`–`AC-B1.3`, §0, §1.7(3), §4.4(b), §5.3, §5.7, §7.F |
| Closed | `OQ-1`, `OQ-4`, `OQ-5`, `OQ-9`, `OQ-10`, `OQ-13` |

## What this costs, stated plainly

- **Attested assets rest on a requester's word.** For COPM, MXNB and BRLB, the ramp's reports are the
  observations. The chain bounds how much can be minted. It cannot tell a real payment from a reported one.
- **Burning at once uses the mint authority for refunds.** That is one refund per redemption, within the
  window. Escrow would have avoided it; the owner chose simplicity.
- **No direct delivery into aledgent in V0.** Every deposit for aledgent passes through the requester's
  account: one more hop, and one more party holding value briefly.

## Carried to the plan

- One contract per bridge, in a dedicated ContractsStore community. kreivo#512 gives mint and burn, and the
  community account is Issuer and Admin.
- Asset table: bUSDC (Polygon, native USDC), bEURC (Base, EURC), bBTC (BNB Chain, BTCB). COPM, MXNB and BRLB
  are attested through Swaps. Locations and decimals are open; see the form.
- DEX venues: Uniswap v3 (Polygon) and Aerodrome (Base); CCTP for USDC deliveries to other networks.
- The reserve book is a double-entry store inside the core.
