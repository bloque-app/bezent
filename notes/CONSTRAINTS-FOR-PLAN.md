<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Constraints and facts for the plan

Informative companion to `SPEC.md` and binding input to `PLAN.md` (not yet written). Not part of the
specification. Everything here is an intended binding, a technical fact or a measured bound: the
material a plan needs and a specification must not carry. Sources are the research notes in
`research/`, cited by number. Facts read on 2026-09-30; asset table updated 2026-10-11. Sections below that speak of USDB, per-network reserves, rebalancing or Binance predate amendment 0001 and are kept as research context.

## @pandres95 rulings

| Date | Ruling | Where it lands |
|---|---|---|
| 2026-10-01 | **Rely on Swaps** to keep the design simple. A plan-level decision: wherever Swaps already does a job, the plan uses Swaps instead of building it into bezent | "Relying on Swaps" below; `SPEC.md` `REQ-RB-3` loosened in v0.1.1 so the plan has room |
| 2026-10-01 | **Internal shape: one ingress, three components.** A single ingress takes bridging requests in and enqueues them. (1) **External network endpoints**: evmhot-shaped, per chain; receive deposits, make withdrawals; sweeps go to a Bloque treasury, not to Binance. (2) **Asset unification / disaggregation**, relying on Swaps: USDT ⇄ USDC conversion, Polygon ⇄ Base through Circle. (3) **Kreivo endpoint**: a light client that mints and burns, nothing more | "Internal shape" below; `SPEC.md` changes pending @pandres95's answer on how conversion is modelled |
| 2026-10-01 | **USDT is never reserve.** The reserve is USDC. Swaps converts USDT to USDC (deposits) and back (withdrawals); bezent mints against the USDC that arrives. The bridged asset on Kreivo is **USDB** | `SPEC.md` v0.2.0 §5.10 (`REQ-CV-*`), `REQ-AS-7`, `OQ-4` partly closed |
| 2026-10-01 | **CCTP is for netting, and batching Polygon → Base is key.** A Base withdrawal larger than Base's reserve takes only the remainder from Polygon through CCTP (or the reverse) | `REQ-RB-9` (batching), `REQ-RB-10` (netting first); "CCTP in practice" below |
| 2026-10-01 | **Asset Hub is out of scope as an origin network.** XCM is already trustless and works well; bezent must not put a custodian on that path | `SPEC.md` v0.1.1 §1.6 and §13; below |

## Assets and backends (@pandres95, to 2026-10-11; amendment 0001)

| Asset on Kreivo | Backend | Assurance | Reserve | Notes |
|---|---|---|---|---|
| **bUSDC** | EVM | public | native USDC on **Polygon** | Deliveries to Base by CCTP, straight to the beneficiary |
| **bEURC** | EVM | public | EURC on **Base** | |
| **bBTC** | EVM | public | **BTCB** on **BNB Chain** (18 decimals, measured 2026-10-11; finalized head ~2 blocks behind) | Issuer is Binance (`REQ-AS-3`); decimals on the form (`br-bbtc-decimals`) |
| **COPM**, **MXNB**, **BRLB** | Swaps (bank rails) | attested | Swaps' accounts | Swaps' reports are the observations (`REQ-BE-5`); fiat payouts continue on Swaps |

- The Kreivo end is **one ink! contract per bridge** in a **dedicated ContractsStore community**. kreivo#512
  (runtime 0.17.1) gives `assets__mint`/`assets__burn` to contracts whose community account is the local
  asset's Issuer and Admin. Burn takes from any account, so the contract burns only its own holdings
  (`REQ-HN-8`, `REQ-HN-10`).
- Mint goes to the address named in the order; for aledgent, that is Swaps' Kreivo account, and Swaps deposits
  (`br-kreivo-hop`). Redeem is permissionless: `deposit_assets` and burn in one call, with inline payout
  addresses for EVM and a hash commitment for fiat.
- Assets are "not sufficient", 6 decimals for bUSDC and bEURC (`br-usdb-params`, `br-recipients`); bBTC 8
  decimals with a dust rule (`br-bbtc-decimals`; rule proposed in amendment 0004). **Locations** (`br-asset-locations`,
  2026-10-13): bUSDC and bEURC get **new ids** at the Root referendum; MXNB `Here(101)` and BRLB `Here(102)` stay for
  now, and are recreated later once bezent is live. `Here(103)` is free for bBTC.
- No custodial venue in V0: conversions on DEXs (Uniswap v3 on Polygon, Aerodrome on Base); no Binance
  (`br-dex`).

## Intended realisation

| SPEC capability | Intended binding | Research |
|---|---|---|
| The issuing network (§4.4(a), `REQ-HN-*`) | **Kreivo**, mainnet runtime 0.17.0 (spec 133, `polkadot-stable2606-2`, pallet-contracts 49, pallet-assets 53) | 03 |
| The on-chain component (§7.F, `CTR-HOME`) | An **ink! contract** on Kreivo in a dedicated ContractsStore community, minting and burning through kreivo#512's API | 03 |
| Backends (§4.4(b), `REQ-BE-*`, `REQ-RN-*`) | EVM: **Polygon** (bUSDC), **Base** (bEURC), **BNB Chain** (bBTC). Attested: **Swaps** (COPM, MXNB, BRLB). **Never Asset Hub** (XCM serves it) | 01, 05, 07 |
| Deposit addresses, treasury, sweeps (§5.2) | **evmhot**, hardened and moved out of substrate-rail (see "evmhot" below) | 01 |
| The requester (§3) | **Swaps** (`the Swaps platform's swap service`) today; the ramp (`swaps-vos`) when it ships | 02 |
| Protocol venues (§5.7, §5.10) | **Circle CCTP** for USDC deliveries to other networks; **Uniswap v3** (Polygon) and **Aerodrome** (Base) for conversion | 05, 07 |
| Custodial venue | **None in V0** (`br-dex`, `DEF-8`) | 02, 07 |
| The ledger contract (§4.4(e), §7.G) | **aledgent's settlement contract** on Kreivo | 03 |
| Custody (§4.4(c)) | A KMS. Open: which one signs for Kreivo (see "Custody") | — |
| Test network (§15 release scope) | **zombienet**, reusing aledgent's `e2e/network`; EVM testnets (Polygon Amoy, Base Sepolia) or local forks for origin networks | 03 |

## Internal shape (ruling 2026-10-01)

    Swaps / requesters ──> Ingress (durable, enqueues) ──> internal queue
                                                         ├─> (1) External network endpoints   per chain: deposit addresses, observation, treasury, deliveries
                                                         ├─> (2) Unification / disaggregation  on Swaps: USDT ⇄ USDC, Polygon ⇄ Base via CCTP
                                                         └─> (3) Kreivo endpoint              light client: mint, escrow/destroy, return

Readings and consequences, to confirm with @pandres95:

- **Swaps runs component (2) as graphs.** The ingress and its queue are bezent's own and hold transfer
  state and the reserve equation (`REQ-BK-1`), so that nothing the trust model rests on sits in Swaps.
- **The treasury is a contract**, not an account: `REQ-KY-3` and `REQ-RN-4` need on-chain outflow bounds,
  which an EOA cannot give. Candidate: a Safe with a Zodiac Roles (v2) modifier, whose allowances refill
  per period and whose roles can be scoped to "transfer USDC to any address up to N per day" and
  "`depositForBurn` only with `mintRecipient` = our other vault". The Safe's owners are the governance role.
  The alternative is a small custom vault, which needs its own audit.
- **Conversion is new scope.** `SPEC.md` v0.1.1 forbids one bridged asset backed by two issuers' tokens
  (`REQ-AS-3`) and says bezent never converts (§13). Unification fits if USDT is never reserve: a USDT
  arrival is held until component (2) converts it, and the mint is made against the **USDC that the
  conversion actually delivered to the treasury**, at that observed amount. Price and slippage stay with
  Swaps. That needs a spec change.
- **The Kreivo endpoint as a light client** verifies finality itself, and **can dry-run contract calls**.
  Tested on Kreivo mainnet on 2026-10-02 with PAPI (`polkadot-api` 2.1.8, smoldot 3.3.1), with no node RPC
  involved. Every runtime call succeeded, and each output was byte-identical to the archive node's
  `state_call` at the same block. The calls were metadata, `AccountNonceApi`, `DryRunApi.dry_run_call`
  (events), and `ContractsApi.instantiate` dry runs of two minimal contracts whose constructor emits an
  event with zero and with one topic, uploaded from the Treasury account as dry-run origin (nothing touched
  the chain). The gas figures matched exactly. Scripts are in
  `research/06-scripts/`; research 06 has the detail.
  - aledgent's "smoldot cannot dry-run `execute`" (F-027 decision 36) rested on a hypothesis that the node's
    source contradicts: `sp-state-machine` 0.53 reads the existing value on the first append
    (`changeset.rs:409`), so the read is recorded in the proof. What differed there was smoldot 0.19.4
    embedded in subxt, a zombienet network with one peer, and the `execute` message itself.
  - **Chain specs.** `boot-1`'s peer id `12D3KooWEht3…` is correct: smoldot synced Kreivo through
    `/dns/boot-1.kreivo.bloque.network/tcp/443/wss/p2p/12D3KooWEht3GDE23nGEhpyNgAJ6B2GiGT9hquBDko6PtWuFRRB8`
    alone (the form on aledgent's canary branch). Only its tcp/30335 form did not accept a connection from
    here. The test reached Kreivo through
    `/ip4/104.206.91.18/tcp/30333/p2p/12D3KooWP1mhED5zhk5ghkdYH1TSEVTVX2s2hSADz67oLfiYeeXF` (the peer id of
    Bloque's public RPC node). The stale **Kusama** checkpoint (#34.27M) was blosketools' `known-chains`
    0.11.7, which the test imported. It is *not* aledgent's canary pin (0.12.4, #35 434 907).
  - Still to prove for bezent: bezent contract's own mint and escrow dry runs, on zombienet, before
    dropping the dry-run node from the plan.

## CCTP in practice

Facts (research 05; Circle's documentation, 2026-10-01):

- **Permissionless.** No contract with Circle, no account, no API key. Circle Mint (the business account) is
  only for USDC ⇄ fiat and is not needed.
- **Attestation API:** public, **40 requests per second**; above that, every request is refused for five
  minutes (HTTP 429). One poller for the whole bridge, with back-off, is enough when rebalances are batched.
- **Cost:** standard transfers carry **no Circle fee**, only gas on both chains (cents on Polygon and
  Base). Fast transfers cost **1.3 bps from Base** (about 1.30 USD per 10 000 USD).
- **Time** (standard): **Polygon → Base ≈ 8 s** plus attestation and the destination call; **Base → Polygon ≈
  15–19 min**, because the attestation waits for Base's finality.

What that means for the design, per @pandres95's ruling:

- **Netting first** (`REQ-RB-10`). A Base withdrawal is paid from Base's reserve. Only the part Base lacks
  moves, and only from Polygon.
- **Batch Polygon → Base** (`REQ-RB-9`). Withdrawals short on Base wait in `CND-ReserveShort`; one CCTP burn
  refills Base for all of them, plus enough to restore Base's low-water mark. One burn means one attestation
  poll, one `receiveMessage`, and one on-chain reference, whatever the number of withdrawals served.
- **Keep Base topped up from Polygon, not the reverse.** Polygon → Base is the fast direction. A Polygon
  shortfall refilled from Base costs ~15–19 min (or 1.3 bps for Fast). Low-water marks should be set so
  that Base → Polygon is rare.
- **Swaps drives the permissionless steps** (fetching the attestation, `receiveMessage` into the Base vault);
  bezent signs the burn out of the Polygon vault and observes both legs (`REQ-RB-3`).
- **Conversions** (USDT ⇄ USDC): CCTP does not convert. Binance Convert (Swaps holds it) has a 10 USD withdrawal
  minimum that strands small deposits. A DEX swap from the vault (Uniswap v3 0.01% on Polygon ≈ 0.027% all-in;
  Aerodrome on Base) has no minimum and settles atomically (research 07).
- **Settled (2026-10-03):** USDB gets its own identity, `Here(103)`. See "This is R0 of the Kreivo Bridge".

## Relying on Swaps

@pandres95's ruling of 2026-10-01: to simplify the design, the plan relies on Swaps. What Swaps already
does, bezent does not rebuild:

| Job | Swaps today (research 02) | Bridge's part |
|---|---|---|
| Learning that a deposit arrived and deciding where it goes | evmhot `deposit_swept` → mediums → `swapClient.take` | Verify the arrival itself at finality, then mint for the deposit request Swaps sends (`REQ-RQ-3`). Standing instructions (`REQ-DA-2`, a MAY) can wait |
| Pricing, quotes, fees to customers, eligibility | Swaps | None |
| Driving a custodial venue (Binance deposit, convert, withdraw) | `binance-*` modules, deposit-history watcher | Sign the leg out of the treasury; verify the arrival Swaps points at (`REQ-RB-4`) |
| CCTP's permissionless steps (fetch Circle's attestation, call `receiveMessage` to the destination vault) | New Swaps module | Sign `depositForBurn` out of the vault; observe both legs (`REQ-RB-3` as loosened) |
| Batching, lanes, retries toward customers, customer webhooks | bezent subsystem, graph engine | One outcome per transfer (`REQ-EO-5`); the sequenced log (`REQ-ST-4`) |
| Deciding when to rebalance | Swaps (it sees demand) | Publish reserve per network and low-water marks (`REQ-RB-8`) |

**Where relying on Swaps stops.** These stay in bezent however the plan is cut, because the spec's
trust model rests on them (`SPEC.md` §4.3):

- Swaps never mints, never declares an outcome, never states that something arrived. It points at
  coordinates; bezent verifies (`REQ-RQ-7`, `INV-6`).
- The signer for reserve addresses and the contract's delivery role is not in Swaps' process
  (`REQ-AR-3`, `REQ-KY-1`). Swaps' current signing (`swap.maker.urn` through substrate-rail with an `Any`
  proxy) is exactly the pattern bezent must not reuse.
- The reserve equation is computed by bezent from chain data, not from Swaps' Binance pool shadow
  balance (`REQ-BK-1`).

If the plan wants bezent's coordinator to run *inside* Swaps (as a set of graphs), these three
lines still hold, and the requester's authority over it must still be scoped (`REQ-PN-2`). That is a plan
choice to make explicitly, not by drift.

## Asset Hub is out

Ruling of 2026-10-01. Asset Hub ⇄ Kreivo moves over XCM reserve transfers, which are trustless and work
well. bezent never treats Asset Hub as an origin network (`SPEC.md` §13). Consequences:

- The requirements note (`notes/BRIDGE-REQUIREMENTS.md` §1) lists Kusama Asset Hub among remote networks;
  this bridge does not serve it (`notes/TRACEABILITY.md`).
- Today's DUSD path (Brale on Asset Hub, then a reserve transfer to Kreivo) is not this bridge's to replace.
- The Kreivo Bridge's R1 (a Polkadot Asset Hub escrow, over XCM) stays the trustless successor for tokens
  native to Asset Hub.
- The bridged-asset identities `Sibling{1000, 50, 1337|1984}` are only *labels* located under Asset Hub;
  `NotBridgeBacked` already stops them from moving over XCM, so minting them here does not touch Asset Hub.

## This is R0 of the Kreivo Bridge

@pandres95's message-backed **Kreivo Bridge** spec (the Kreivo repository's `specs/kreivo-bridge/SPEC.md`
v0.2.0, 2026-09-19) defines a release **R0 — interim issuance**: Bloque mints a bridged asset against
reserves it holds and publishes (`REQ-R0-1`, `REQ-R0-2`), until a message-backed binding (R1: Polkadot
Asset Hub escrow; R2: EVM escrows with a messaging provider) takes over, at which point Bloque's issuer
right ends (`REQ-R0-3`). This bridge is that R0, specified as a system. Research 04.

Consequences the plan must carry:

- **USDB is a new local asset, `Here(103)`** (@pandres95, 2026-10-03), not one of the runtime's reserved
  `BridgeBackedAssets` (`Sibling{1000, 50, 1337|1984}`). `Here(103)` is free on mainnet (research 07). The
  hand-over to a later binding still works without holders acting: the Kreivo Bridge spec lets an existing
  asset acquire a binding with its identity kept (its `REQ-AS-7`). Because `Here(103)` is not in
  `BridgeBackedAssets`, Kreivo's XCM filter lets it be reserve-transferred out; backing is unaffected (supply
  counts the sovereign account's holding). Blocking that needs a runtime change (form item `br-xcm-usdb`).
- **Creation:** a Root referendum creates `Here(103)`; @pandres95 is sending it on Monday 2026-10-05. Its parameters
  (metadata decimals, sufficient, issuer/admin) should be settled before then: form items `br-usdb-params`, `br-issuer`.
- **Issuer and admin (@pandres95, 2026-10-03): the relayer's account**, minting and burning locally; no XCM is
  involved. Mint plus aledgent deposit stays atomic through `utility.batch_all`. This conflicts with
  `REQ-AR-4`, `INV-2` and `INV-3` (caps and one-time references enforced on chain) unless the account sits behind
  a capped proxy; form item `br-issuer` asks which.
- **Decimals.** Set in the asset metadata, which aledgent registers from (@pandres95). `REQ-AS-4` requires USDB's precision to equal USDC's (6). COPM, MXNB and BRLB use 2. A 2-decimal
  USDB would need an amendment and a rule for the sub-cent remainder (form item `br-usdb-params`).
- **Fungibility across origins (`OQ-1`) is R0-safe, but not free later.** The Kreivo Bridge spec defers
  "fungibility across bindings" (`DEF-9`): merging backings merges their trust. Under R0 every origin
  is backed by the same custodian, so one fungible USDC adds no trust. At the hand-over, though, all
  of the bridged USDC's supply must move under one binding, or `DEF-9` must be decided. Rebalancing
  (§5.7) is the tool for that: it can consolidate R0 reserves wherever the successor escrow lives.
- **Kreivo Bridge `OQ-10`** gave the direction "COPM on Polygon, then USDC on Base". This bridge's
  `OQ-4` asks again, because netting through CCTP is a USDC property.

## The issuing network: Kreivo

Research 03 has every reference.

- **Mint and destroy from a contract.** Kreivo's contract API (chain extension 0) offers assets
  balance, deposit and transfer only; **no mint, burn or total issuance**. The `CallFilter` lets a
  contract `call_runtime` any `Assets` call (`config/contracts/mod.rs:19-27`). So the contract mints with
  `Assets::mint` (pallet 13, call 6) and burns with `Assets::burn` (call 7), hand-encoded as aledgent
  does for call 13/8 (`contracts/settlement/src/kreivo.rs:46-91`).
- **Roles.** Mint needs **Issuer**; burn needs **Admin**. Admin can burn from **any** account
  ("best effort", up to the amount), so the contract must only ever burn from its own account
  (`REQ-HN-8`, `INV-4`). `call_runtime` reports success or failure only, not the amount burned: pull to
  the contract first, check the balance, burn from itself.
- **Creating the asset.** `CreateOrigin` is `NeverEnsureOrigin`; only Root (`ForceOrigin`) creates
  assets. Root is the Kreivo collective's referendum track 0, or the relay chain. The collective is
  Virto and its partners, so this is within Bloque's reach. Steps: `force_create(id, owner, is_sufficient = true,
  min_balance)`, `force_set_metadata`, then `force_asset_status` (or the owner's `set_team`) to make the
  contract Issuer and Admin. Create it **sufficient**, so a beneficiary needs no KSM (`REQ-HN-4`;
  otherwise `UnavailableConsumer`).
- **Deploying.** Through the contracts store: a community registers, is funded, publishes, requests a
  licence, instantiates (`InstantiateOrigin = EnsureNever`; uploads need a community origin). Upgrade
  authority is that community's membership governance, as for aledgent (`F-026` `T-026-07`). Costs on
  mainnet (2026-09-30): community 0.5 KSM, publish ~0.02, instantiate ~0.1 KSM.
- **Expiring transactions.** `CheckEra` is in Kreivo's extensions; `BlockHashCount` is 4096. aledgent's
  adapter builds eras born at the finalized head with `max_era` 64 and proves non-execution as "no inbox
  entry plus an unspent nonce" (`aledgent/features/F-027-settlement-extension/NOTES.md:365-390`). Reuse
  it for `REQ-NX-1`.
- **Finality.** The relay-chain-finalised head. Kreivo has reorganised up to six blocks deep in testing.

## The ledger: aledgent's settlement contract

- `deposit(account_ref: [u8; 32], asset, amount) -> InboxSeq`, selector `0x00000101`. **The payer is the
  direct caller**, keep-alive. So "mint into a ledger" (§7.F) is: bezent contract mints to itself,
  then calls `deposit` in the same call; the ledger pulls from bezent contract's balance. A refusal
  (`DepositRefused`) records nothing; bezent contract must then revert the mint (`AC-A3.2`). Keep the
  asset's `min_balance` in the contract as surplus so keep-alive never blocks a deposit.
- `credit` accepts only other aledgent deployments. bezent must never use it (`REQ-LG-1`).
- Withdrawals: `Call::Withdraw { to: Destination::Account(..), asset, amount }` under an
  `operation_ref`. Paid by the extension's `transfer`, or by `Assets::transfer` through `call_runtime`
  when it empties the holding. A withdrawal to bezent contract's account is the "ledger withdrawal"
  funding of `REQ-OT-1`; its inbox entry `Withdrawn{seq, operation_ref, to, asset, amount}` is the
  coordinate the requester names (`REQ-OT-2`).
- aledgent's per-asset withdrawal bound (`set_withdrawal_bound`) must be set for the bridged asset
  before any withdrawal can leave aledgent toward bezent.
- aledgent's latency budget (19 s target, 30 s ceiling, amendment 0007) ends at the hand-off
  (`REQ-LG-3`, requirements note Q2).

## Origin networks: Polygon and Base

Research 05. Numbers to be measured by the plan; these are the published orders of magnitude.

- **Finality signal** (`REQ-OB-1`): the `finalized` block tag on both.
  - **Polygon PoS**: milestone-based finality; `finalized` trails the head by seconds (Circle counts 2–3
    blocks, about 8 s, for CCTP standard transfers).
  - **Base** (OP Stack): `finalized` means the L2 block's batch is in a finalised Ethereum block:
    about **15 to 20 minutes** (Base's documentation estimates about 20; Circle about 15–19). That bounds every Base deposit's completion, and is the main
    argument for or against a fast path (`OQ-2`, `DEF-1`).
- **Proof of non-execution** (`REQ-NX-1`, `REQ-NX-4`): nonce consumed at a final block by a different
  transaction. Cancelling means a zero-value self-transfer at the same nonce with a higher fee.
- **Tokens and issuers** (`REQ-AS-3`): native USDC on Polygon (`0x3c499c54…3359`) and on Base
  (`0x833589fC…2913`) are both Circle's and both 6 decimals: one issuer, one precision. **USDC.e on Polygon
  (`0x2791bca1…84174`) is a different issuer's token** (the PoS bridge) and must not back the same bridged
  asset. USDT on Polygon is 6 decimals; Swaps calls it `usdt0`.
- **Reserve addresses** (`REQ-RN-4`, `REQ-DA-3`, `REQ-KY-3`): EOAs cannot satisfy these. The plan needs:
  - a **treasury vault contract** per network: deliveries only by the delivery role, an outflow bound per
    token and window, rebalance legs only to declared venue addresses, an event carrying the on-chain
    reference on every outflow (`REQ-RN-5`);
  - **deposit addresses as counterfactual forwarders** (CREATE2): an address that can only flush to its
    network's treasury vault, deployed on first sweep. This removes per-address keys, the gas faucet and
    the risk that a leaked seed drains unswept deposits.

## evmhot

Research 01. Production runs the copy vendored in substrate-rail (`crates/evm-hot-wallet`, from evmhot
`feat/postgres-storage-driver-v2`, `23be9d9`), served at substrate-rail `/evm/*`. The standalone repo's
checked-out branch is older, and `origin/feat/evm-hot-v2` is a parallel multi-chain line.

What it already does that bezent needs: per-requester address registration with an external id;
per-chain configuration with token allow-lists and per-token treasuries; batched `getLogs` catch-up with
range bisection; a webhook outbox (v2).

What it does not do, against this spec:

| Requirement | evmhot today |
|---|---|
| `REQ-OB-1`, `REQ-OB-2` finality by the network's signal, block hash kept | A fixed 20-block offset (none at all in WebSocket mode). No block hash stored; no reorg handling |
| `REQ-OB-3` contiguous ranges, visible gaps | Five silent-skip paths, including an unauthenticated `POST /block_number` that moves the cursor |
| `REQ-IN-1`, `INV-10` coordinate as key | `tx_hash:log_index`; no block number or hash recorded |
| `REQ-EO-1`, `INV-9` exact bytes stored before submit | Nothing stored until the receipt; receipt status never checked; a reverted sweep is marked swept |
| `REQ-KY-1` keys in custody | Mnemonics in environment variables |
| `REQ-DA-3`, `INV-13` deposit value movable only to treasury | HD-derived EOAs; any holder of the seed can move it anywhere |
| `REQ-DA-6` sweeps' costs from a fee position | A faucet funds each address; faucet funds leak into the treasury on re-deposit |
| `REQ-OT-*` deliveries from the treasury | None. evmhot has no outbound path at all |
| `REQ-RQ-1` authenticated requests | `/register` and `/block_number` are unauthenticated |

So "use evmhot" means: keep its registration and monitoring shape, replace its custody model (forwarders
and a vault), its finality rule, its cursor and its signing path. aledgent replaces substrate-rail
(aledgent `notes/MOTIVATION.md`), so this work must happen in bezent's own code, not in the vendored
crate.

## The requester: Swaps

Research 02.

- Swaps already has an in-process **bridge subsystem**: `POST /api/swap/_internal/bridge/requests`, lanes
  (`polygon`, `asset-hub`, `link-card`), `bridge_batch_member` unique on `(lane, caller_ref)`, a cron
  `runLane` that batches members into an accumulator graph, a Binance "pool" shadow balance, and a
  callback with `{status, result: {amount_delivered, fees_deducted, tx_refs}}`. bezent replaces this
  subsystem's EVM role. Two of its habits violate this spec and must not carry over: pro-rata splitting of
  a batch delivery (`REQ-EO-5` needs one outcome per transfer), and `fees_deducted` (`REQ-TR-5`).
- Today's routes: Polygon/Base **USDC → Brale → DUSD on Asset Hub → reserve transfer → Kreivo**; Polygon/Base
  **USDT0 → Binance → convert → USDC on Solana → Brale → DUSD**. **No registered graph pays USDC out to a
  user's Polygon or Base address**; `wayback-bridge.ts` is an unregistered draft.
- Deposits reach Swaps through **mediums**: evmhot `deposit_swept` → mediums → `swapClient.take(...)` with
  `idempotency_key = evmEvent.id`, then `check-evm-deposit` calls `/evm/verify_transfer`.
- Integration shape: a new instruction module that pauses with a callback token and hands bezent the
  callback URL only in `afterPersist()`; bezent's notifications resolve it. It must also be
  added to the stall alarm's module list. A sequenced-log reader (`REQ-ST-4`) is the safety net for lost
  callbacks.
- **A product consequence, stated plainly.** bezent mints **bridged USDC** on Kreivo. Kreivo's dollar
  today is **DUSD** (Brale, via Asset Hub). Unless the two are converted somewhere, Kreivo will carry two
  dollars. Which one aledgent and the products hold is @pandres95's call (`OQ-4`).

## Venues

Research 02 and 05.

- **Circle CCTP** (protocol venue, `REQ-RB-3`): burn native USDC on one chain (`depositForBurn`), fetch
  Circle's attestation, mint on the other (`receiveMessage`, callable by anyone, `mintRecipient` = the
  destination vault). Every step is on chain. Standard transfers wait for source finality: about 8 s from
  Polygon, 15–19 min from Base. Fast transfers (8–20 s) carry a fee and Circle's own credit; they are a
  venue choice, not a fast path for bezent (`DEF-1` is about delivering before *our* source is final). Nothing in the Swaps platform or substrate-rail uses CCTP today.
- **Binance** (custodial venue, `REQ-RB-4`): Swaps already holds the client (`withdraw`, deposit history,
  Convert). Its deposit and withdrawal legs are on chain; the interval between them is not. Binance
  withdrawals carry no reference, so the requester supplies the withdrawal's coordinate (from Binance's
  withdrawal history) and bezent verifies it. Swaps reads no per-network `withdrawMin`; incident I-15
  came from exactly that. Two known defects in Swaps' Binance module are listed in research 02.

## Custody

- EVM signing on a KMS with secp256k1 is standard.
- Kreivo accounts can be ECDSA (secp256k1) as well as sr25519/ed25519, so one KMS key type can serve both.
  The plan must confirm the account type for each role and that the contracts store and the asset roles
  accept it.
- aledgent's own signer still sits in its process (aledgent `F-017`, "KMS signer before production"); the
  bridge must not copy that (`REQ-AR-3`).

## The test network

- aledgent's `e2e/network` (zombienet-sdk 0.4.15) starts `rococo-local` plus Kreivo (para 2281,
  `--authoring slot-based`) from the released runtime. Test assets are a genesis override
  (`kreivo_genesis_overrides`); fork it to add the bridged USDC identity, with Alice as owner, then make the
  contract Issuer and Admin after instantiation. CI uses `polkadot-stable2606-2` with Kreivo 0.17.0.
- Contracts are deployed there exactly as on mainnet (`drive.mjs:308-360`).
- Origin networks for the test run: Polygon Amoy and Base Sepolia (both have Circle test USDC and CCTP), or
  local forks for fault injection.

## Known costs

- Kreivo: one asset transfer ≈ 0.002 KSM; in a batch of 32 ≈ 0.0012 each. No gas tanks exist today.
- Base and Polygon: cents per transfer; a forwarder deployment on first sweep costs more than a sweep.
- Every cost is recorded against surplus (`REQ-BK-4`).
