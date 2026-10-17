<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

---
name: One core, many backends
summary: Can bezent's mechanism be generalised to every backed asset (bUSDC, bEURC, bBTC, bCOP, bMXN, bBRL), with Swaps or the ramp driving the operations? Yes, with one honest change, an assurance class per backend. Answer to @pandres95's question of 2026-10-07.
status: informative design note; nothing here is ruled
date: 2026-10-07
---

# One core, many backends

Basis: bridge repo at `474dbb0` (`SPEC.md` v0.2.0); aledgent `PLAN.md` at `a3cb28b` (`DEC-20`). Claims are
tagged **[Spec]** (in bridge `SPEC.md`), **[Plan]** (aledgent's plan), **[Ruled]** (a ruling by @pandres95),
**[Knowledge]** (general facts about a network or rail, to confirm before relying on them) or
**[Inference]** (my reasoning).

## 1. Verdict

**Yes, it generalises, and it is not a different monster.** The part that makes a bridge safe is the same for
every asset: one way in, one lifecycle per transfer, references and exactly-once, a reserve book, a Kreivo
endpoint that mints and burns, a custody-isolated signer with its guards, statements, review and pause. What
changes per asset is the *backend*: how arrivals are observed, what "final" means, how a payout is made and
proven, and who can check the reserve. **[Inference]**

The one change that is not cosmetic: today's spec promises that *anyone* can check reserves and
non-execution from public data (`REQ-BK-6`, `REQ-NX-2`). That holds for chains. It cannot hold for a bank.
So each backend must **declare its assurance class**, and every statement must say which class backs each
asset. **[Spec] [Inference]**

## 2. What is shared, and what varies

| Part | Shared by every asset | Varies per backend |
|---|---|---|
| Ingress, queue, transfer lifecycle, states, conditions | yes | — |
| References, idempotency, exactly-once | yes | how a reference is carried (event field, `OP_RETURN`, bank end-to-end id) |
| Reserve book and reserve equation | yes, one book, many positions | how each position's balance is read |
| Kreivo endpoint (mint, escrow, destroy, return) | yes, identical for every b-asset | — |
| Signer and its four guards | yes | key type and outflow bounds (contract, multisig, bank dual control) |
| Observation and finality | the rules (`REQ-OB-*`) | the finality signal |
| Proof of non-execution | the rule (`REQ-NX-1`) | the evidence (nonce, spent input, bank rejection) |
| Who can verify | — | public chain data, or the institution's word |

## 3. The backend contract

Each backend implements one interface for the core. The core never trusts a backend's verdict; it records
observations and derives outcomes, as today (`REQ-AR-2`). **[Spec]**

| Capability | EVM (bUSDC, bEURC) | Bitcoin (bBTC) | Bank rails (bCOP, bMXN, bBRL) |
|---|---|---|---|
| Observe an arrival, with a stable id | log at a coordinate | output at a coordinate | statement line or rail event with its end-to-end id (PIX `endToEndId`, SPEI *clave de rastreo*, Bre-B id) **[Knowledge]** |
| Finality | the `finalized` tag **[Spec]** | none native: a **declared depth** (e.g. 6 blocks), a stated trade-off against `REQ-RN-1` **[Knowledge]** | the rail's settlement rule; instant rails settle finally, but **returns and reversals** can still arrive later **[Knowledge]** |
| Deliver, idempotently | signed tx, bytes stored first | signed tx, bytes stored first | payout call with an idempotency key |
| Prove non-execution | nonce consumed by another final tx | an input spent by another tx at depth | the bank's final rejection; **not third-party checkable** |
| Bound the key on chain | vault contract caps | multisig or MPC; no native caps | bank-side limits and dual approval |
| Read the reserve | public balance | public UTXO set at published addresses | bank API balance; **attested, not public** |
| **Assurance class** | **public** | **public** (with a declared-depth caveat) | **attested** |

## 4. Non-chain-backed assets: what is different

- **Several reserves per asset.** bCOP may sit in two or three banks. The book already handles that: one
  position per (asset, bank), exactly like the per-network reserves of the first design. Moving value
  between banks is a rebalance over the national rail. **[Inference]**
- **Instant rails help.** PIX (BRL), SPEI (MXN) and Bre-B (COP) are real-time, carry an end-to-end id, and
  settle in seconds. That gives each arrival a stable id and a short wait before mint. **[Knowledge]**
- **Reversals after "final".** A return or chargeback after mint becomes a loss against surplus, never a
  change in supply. The backend must declare its reversal window, and the mint waits past whatever part of
  it @pandres95 rules. **[Inference]**
- **Independence of evidence.** The core should read bank evidence itself, with read-only API access. If it
  only hears it from the ramp, the ramp effectively asserts arrivals, which the spec forbids
  (`REQ-RQ-7`). **[Spec] [Inference]**
- **Who pays out.** For chains, the core signs payouts. For banks, the equivalent is the core holding payout
  credentials. The alternative is the ramp paying from its own accounts, with bezent only burning
  against its report. The first keeps the trust model; the second is simpler but makes the ramp a custodian
  of the reserve. **[Inference]**
- **Regulation.** Issuing tokens backed by COP, MXN or BRL may need a licence in each country. That is for
  legal, before any value. **[Inference]**

## 5. Your question: a small aledgent instance for the sums?

**Recommendation: no separate aledgent deployment.** Keep the reserve book inside bezent core, and
reconcile it against sums that already exist.

- **The supply side already has a ledger.** aledgent registers COPM, MXNB, BRLB and USDB as book-only assets
  issued by Swaps (`DEC-20`). Its issuance counterparty is, by construction, the negative of all value held,
  per asset. Once an asset is bound, Kreivo's total issuance is the supply. **[Plan]**
- **The reserve side is small.** Per asset: a handful of positions (banks, networks, vaults), value in
  transit, surplus. A double-entry, append-only book of a few accounts does it, with the reserve equation
  checked every minute. **[Inference]**
- **A second aledgent costs more than it gives.** It brings identities, credentials, custody modes and its
  own settlement adapter, built for customers' accounts and not for reserve positions. It also couples the
  bridge's releases to aledgent's. **[Inference]**
- **When to revisit:** if reserves must be disclosed to third parties with proofs (an auditor or a
  regulator), aledgent's disclosure and anchoring features become the reason to use it. **[Inference]**

## 6. How Swaps or the ramp fits

Unchanged from the ruling of 2026-10-01: Swaps or the ramp decides and triggers, and the core verifies and
signs. **[Ruled]** For banks that means:

- the ramp's provider adapters can be the backend's code;
- the core keeps its own read-only view of each reserve account;
- the core signs or authorises every movement of a reserve. **[Inference]**

## 7. What it changes in the spec

One amendment, after @pandres95 rules:

1. §4.4(b) "origin networks" becomes a **backend contract** of capabilities.
2. A per-backend **assurance class** (public or attested) is declared in the statement, like aledgent's
   assurance level.
3. Finality may be a **declared rule** (depth, or settlement rule plus reversal window) where a network has
   no signal of its own.
4. Non-execution evidence may be **attested** for attested backends.
5. One asset may have **many reserve positions**: one per bank or network.

`DEF-3` (other kinds of origin network) moves into scope. **[Spec] [Inference]**

## 8. Since 2026-10-07: a contract on Kreivo, and BNB Chain for bBTC

@pandres95, 2026-10-11, after kreivo#512 and a conversation with a Kreivo collaborator.

### 8.1 The Kreivo end is always one contract (kreivo#512)

kreivo#512 (runtime 0.17.1, open on 2026-10-11) adds `assets__mint(asset, amount, beneficiary)` and
`assets__burn(asset, amount, who)` to Kreivo's contract API. The PR description, read on 2026-10-11, says:
**[Knowledge]**

- they work only for **local** assets (`Here(_)`), and only from a **ContractsStore** instance;
- authorisation is by the contract's **community account**: it must be the asset's **Issuer** to mint and its
  **Admin** to burn;
- burn takes from **any** `who`, best effort, and returns the amount actually burnt;
- weight is charged before the call, so refused calls pay too.

**Verdict: agreed.** The Kreivo end becomes the same contract for every b-asset, and the backends vary behind
it. This also brings back what the "relayer account as issuer" ruling (`br-issuer`, 2026-10-04) gave up: caps
per window, one-time references, escrow and pause can again be **enforced on chain** (`REQ-AR-4`, `INV-2`,
`INV-3`, `INV-4`). The four off-chain guards then become defence in depth, not the only line. **[Inference]**

Three things the contract must handle:

1. **Burn only its own holdings.** The API can burn from any account; the contract's code must only ever pass
   its own address as `who` (`REQ-HN-8`), and check that the returned amount equals what it meant to burn.
   **[Inference]**
2. **The roles sit with the community, not the contract.** Any other contract of the same community, and the
   community's own governance acting as its account, can also mint. So bezent needs a **dedicated
   community** with one contract, and its governance (membership, track, delay) is the upgrade authority to
   name (`REQ-GV-3`). **[Inference]**
3. **Reusing `Here(101)` and `Here(102)`.** On mainnet today they carry MXNB and BRLB metadata (2 decimals, not
   sufficient, supply 0; research 07). bUSDc and bEURc need new metadata (6 decimals) and the community account
   as Issuer and Admin, both through Root. **[Inference]**

### 8.2 BNB Chain for bBTC

Reading the collaborator's suggestion as: back bBTC with **BTCB**, Binance's BTC token on BNB Chain. **[Inference]**

- **It needs no Bitcoin backend.** BNB Chain is an EVM chain, so the EVM backend covers it: deposit
  forwarders, a vault, and finality by the `finalized` tag. BNB Chain has fast finality, in seconds.
  **[Knowledge]**
- **The backing changes issuer.** bBTC would be a claim on BTCB, and BTCB is a claim on Binance's custody of
  BTC. By `REQ-AS-3`, one bridged asset has one issuer, so bBTC would be "Binance's BTC", not Bitcoin itself.
  **[Spec] [Inference]**
- **The same shape fits other wrapped BTC:** cbBTC (Coinbase) on Base, where bezent already is, or WBTC
  (BitGo) on Polygon. Native BTC through a Bitcoin backend gives the strongest backing, and costs the most to
  build. **[Knowledge] [Inference]**
- **Moving in and out of Binance is cheap with BTCB**, which suits Swaps' existing Binance flows.
  **[Inference]**

## 9. The Kreivo contract's two operations (@pandres95, 2026-10-11)

@pandres95's design: fiat-backed assets also live on Kreivo, so that DAOs and other accounts can use them, and
the Kreivo end has two operations:

1. **Mint**, then transfer to the destination named in the order on the counterpart backend.
2. **Redeem**: `deposit_assets` from the caller, then burn, with a **reference** that says where the counterpart
   releases the funds. For USDC on Polygon that can be an account id; for fiat, a continuation on Swaps. The
   reference can carry the instructions, or the instructions can be relayed to bezent afterwards.

**Verdict: yes, and it works the same for every backend.** The contract never needs to know backends. It
records `(asset, amount, backend, instructions or commitment, reference)` and emits it in order. Each
backend's endpoint reads those events at finality and pays out. Redeem is permissionless, which is what lets
any Kreivo account or DAO exit. **[Inference]**

Four refinements:

- **Escrow, then burn at delivery.** Burning at once means a failed payout can only be refunded by minting
  again. Holding the deposit in escrow, burning it when the payout is final, and returning it on a proven
  non-execution keeps the mint authority out of refunds. That is today's `REQ-OT-6` and `INV-8`. Burning at
  once also works, with refunds as one-time mints tied to the original reference. **[Spec] [Inference]**
- **Instructions on chain only where they are already public.** An EVM address is fine inline: it is public on
  the destination chain anyway. Bank details never go on chain (`REQ-PV-1`). For fiat, the contract stores
  a **hash commitment** of the instructions, and Swaps relays the instructions later. The core accepts them
  only if they match the hash, so nobody can redirect a redemption by relaying different instructions.
  **[Spec] [Inference]**
- **Recipients must be able to receive.** The asset parameters you chose (`br-usdb-params`) say "not
  sufficient". Then a mint to an account with no KSM fails (`UnavailableConsumer`), and with DAOs and arbitrary
  accounts as destinations, that will happen. Either create the assets sufficient, or refuse such
  destinations before any value moves (`REQ-LM-2`). **[Knowledge] [Inference]**
- **Fiat on Kreivo changes aledgent's plan.** On 2026-10-07 COPM, MXNB and BRLB were to stay book-only in
  aledgent. Living on Kreivo means aledgent binds them (`DEC-20`) like the b-assets. The aledgent session
  needs to know. **[Plan] [Inference]**

`deposit_assets` pulls from the caller "keep alive", so a caller can't redeem its last unit below the asset's
minimum balance. That is minor, but redemption screens should say so. **[Knowledge]**
