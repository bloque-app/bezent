<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-018 — Directed deposits and the bridge web app

**Status: Proposed.** Nothing here is built. It is the feature that would carry out amendment 0029, which is itself
proposed and not applied; its Open 1 to 13 are ruled (2026-11-09). Folded into milestone `M-4` (amendment 0029, Open 10,
ruled: no `M-6`). Would serve `US-A6`, `AC-A6.1`–`AC-A6.6`,
`REQ-AS-11`, `REQ-IN-10`, `REQ-IN-11`, `REQ-RN-5`, `REQ-LM-2`, `REQ-ST-1`, `REQ-ST-8`, `US-B5`, and would
keep `INV-2`, `INV-6`, `INV-10` and `INV-14` (plan `DEC-21` and `DEC-22`, proposed; `DEC-8`, `DEC-9`, `DEC-10`,
`DEC-12`, `DEC-15`). Read amendment 0029 first, then `F-008` NOTES ("Deployment", "The two roles") and `F-009` NOTES
(decisions 2, 4, 5, 7). `NOTES.md` here records the facts checked for this design.

## What it builds

Five parts, in this order. Each is a milestone of the feature (sizes in `tasks.md`).

### 1. The contract: `contracts/evm/src/DirectedDeposit.sol`

One contract per origin network that admits directed arrivals. Polygon first, for bUSDC.

```solidity
// Sketch of the interface, not the code.
contract DirectedDeposit {
    IERC20 public immutable token;      // native USDC, 0x3c49…3359
    address public immutable reserve;   // the vault (the Safe of DEC-10)
    uint256 public immutable minimum;   // the route's minimum, 1 USDC = 1_000_000
    uint256 public immutable maximum;   // the route's per-transfer maximum, 50 000 USDC; ≤ the Kreivo per-transfer maximum

    event Directed(address indexed sender, uint256 amount, bytes32 indexed recipient, address refund);

    function deposit(uint256 amount, bytes32 recipient, address refund) external;
    function depositWithPermit(uint256 amount, bytes32 recipient, address refund, uint256 deadline, uint8 v, bytes32 r, bytes32 s) external;
    function depositWithPermit(uint256 amount, bytes32 recipient, address refund, uint256 deadline, bytes calldata signature) external; // EIP-1271 wallets
    // No pause, no owner, no admin function (Open 4, ruled).
}
```

- **Order inside a call:** check `minimum ≤ amount ≤ maximum`, `recipient != 0`, `refund != 0`, `refund != reserve`, `refund != address(this)`. For the permit forms, call
  `try token.permit(msg.sender, address(this), amount, deadline, …)`, then require
  `token.allowance(msg.sender, address(this)) ≥ amount` whether or not the `try` succeeded, so a front-run permit costs
  nothing. Then `token.transferFrom(msg.sender, reserve, amount)` (checked return), then `emit Directed`. Nothing comes
  between the transfer and the event, so the `Transfer` log is always the one right before `Directed`.
- **Never holds value.** The token moves from the caller to the vault, so no balance passes through the contract. It has
  no `receive` and no `fallback`, and no sweep (amendment 0029, "Stray tokens").
- **Value always from `msg.sender`; the refund address is named.** The permit's owner, the `from` of the transfer and the
  event's `sender` are all the caller. No function takes an owner. Every function takes the `refund` address, the original
  depositor, which is where a return goes (the owner's ruling `d-refund-dest-swaps`, 2026-11-09: "The original depositing
  address. That's a contract flaw. The depositing contract should pass on the information about the original
  depositor."). The funder chooses it, so bezent never chooses a refund destination (`REQ-RF-2`). It reverts on a zero
  refund, the vault and the contract itself, each with its own custom error.
- **No owner, no upgrade, no proxy.** Changing any limit, the token, the vault or a key means deploying a new contract
  at a new address, then a new declaration (`REQ-AS-11`) and an endpoint configuration that watches both until the old
  one is paused.
- **Deployment** follows `F-008`. A one-time deployment key deploys it with plain `CREATE` and holds no role
  afterwards. `script/DeployDirected.s.sol` reads `config/polygon.json` (token, vault, limits) and refuses: a `maximum`
  above the route's or the Kreivo per-transfer maximum; a `minimum` below the route's. There is no
  role to give: the contract has no pause and no owner (Open 4, ruled). `VerifyVault` grows `checkDirected`: runtime code equal to the repository's build with these immutables,
  `token` and `reserve` equal to the vault's configuration, and the limits equal to the route's.
- **Licence:** `Apache-2.0 OR MIT`, as the rest of `contracts/evm`.

### 2. The EVM endpoint (`services/evm-endpoint`)

- **Configuration:** `directing = [{ address, from_block }]` per network, empty by default. An empty list means
  today's behaviour exactly.
- **Observation** (`src/observe.rs`). The logs filter adds the declared addresses and the `Directed` topic. A
  `Directed` log pairs with the log right before it when that log is the admitted token's `Transfer` with `from` equal
  to the event's `sender`, `to` the vault, and `value` equal to its `amount`, above zero, in the same transaction. The
  pair is one observation, at the `Transfer` log's coordinate, of a new kind `directed_arrival`, with
  `detail = { directing: <address>, direction_log: <index>, recipient: <0x…32 bytes>, refund: <address> }`. The `Transfer` is not also
  recorded as an `Arrival`. A `Directed` log that does not pair is `UnpairedDirection` (recorded, never an arrival,
  never bindable). Every other rule of decision 4 stands.
- **Finality and dedupe:** unchanged. Only final blocks are recorded (decision 2); the two providers must agree on the
  pair and its block hash (`DEC-12`), or the range is a gap; the coordinate is applied once (`INV-10`).
- **Payouts:** the return of a directed arrival is an ordinary payout of the delivery role, with the refund's on-chain
  reference. Nothing new in preparation or proofs.

### 3. The store and the core

- **Migration** (the next free number when built): `observation_kind` gains `directed_arrival` and
  `unpaired_direction`. The ingress's own lookup (`intake::source`) never offers either kind, so a requester's deposit
  or refund naming one is `ERR-SourceNotFound` (`AC-A6.2`). The store's guard (`source_bindable()`, fired by
  `transfers_source_bindable` for **every** writer, migration 0034) gets one carve-out per purpose, and no other:
  - purpose `deposit`: a `directed_arrival` is bindable only by a transfer that `directed_deposit()` writes, with no
    intake answer, its destination equal to the observation's `detail.recipient` and its refund destination equal to
    its `detail.refund`;
  - purpose `refund` (an unassociated arrival's, `refund_of` empty): a `directed_arrival` is bindable only by the
    core's return, with no intake answer, its destination equal to the observation's `detail.refund`;
  - `unpaired_direction`: bindable by nothing.
  A transfer that names a `directed_arrival` and has an intake answer (that is, any requester's) is refused, whoever
  writes.
- **`directed_deposit(coordinate)`**, a `SECURITY DEFINER` function the core's login calls. It creates the deposit at
  `Source-Final`: bridge reference `bz-d-…` from the coordinate (the same derivation as `refs::deposit`), destination
  from the observation's `detail.recipient`, refund destination from its `detail.refund`, and no intake row. The store refuses a refund of the zero address, the vault or the directing contract, as the destination guard does. The schema allows
  that: `intake_answers` points at `transfers` (migration 0010), not the other way, and no sender guard applies to a deposit
  with no request (amendment 0026 is withdrawn). What has to be checked before building is every reader that assumes a deposit has a
  request: webhooks, `by-request` queries, the sequenced log's requester field, the core's lookup of a refund
  destination. Each must treat "no answer" as a directed deposit and never as an error. A trigger refuses,
  whoever writes, a transfer bound to a `directed_arrival` whose destination or refund destination differs from the
  observation's. A second call for the same coordinate is a no-op (`INV-2`).
- **The core's directed intake**, in the cycle where it reads new final observations. For each `directed_arrival`, in
  coordinate order:
  1. Is it mintable under the route as declared now (minimum, maximum; no share and no count cap: Open 5 and 6, ruled), and
     is the recipient off the refused list (the Kreivo contract's account, the `DEC-15` ledger accounts)?
  2. If so, call `directed_deposit`. From there it is an ordinary deposit (mint order, dry run, conditions, deadline,
     review). Over the window: it waits in `CND-DepositWindow` and resumes as the window rolls over (Open 1, ruled: wait); it is not returned.
  3. If not, record the arrival as unassociated with its owner, close it to minting, and order its return. That is a
     refund of an unassociated arrival (`bz-u-…`, `refs::unassociated_refund`) to the observed `detail.refund`, recorded by
     the core, with no second approver (Open 3, ruled 2026-11-09).
- **Review** (Open 2, ruled yes, 2026-11-09): one new edge for a directed deposit only, a resolution from `Needs-Review` held at
  `Source-Final` into `Refunding` with a second approver, when no mint attempt of it was ever signed.
- **The public status route** (Open 8, ruled yes). A second listener in `services/ingress`, with no credential, serving
  `GET /v0/public/deposits/by-coordinate` and `GET /v0/public/withdrawals/by-redemption`: state, named conditions,
  coordinates, finality and times, for directed deposits and for withdrawals only. Never a request reference, a
  requester, a callback or an extra. Rate-limited in front, at Cloudflare.
- **Statement and published addresses:** the directing contract is listed beside the vault (`REQ-AS-11`, §7.E).
- **Alerts** (`F-014`): directed mints and returns per day against thresholds, and how much of the route's window
  directed deposits take (alerts only: there is no count cap and no share, Open 5 and 6, ruled); the KSM float
  (amendment 0025);
  an `UnpairedDirection` (which should never happen with the declared code) pages.

### 4. The web app: `apps/bridge/`

A static, client-only TypeScript app on **Cloudflare Workers static assets**, the working name `bridge.bloque.network`.
It holds no key and no secret, and has no server code except the static asset handler (`NOTES.md`, "Hosting").
UI copy says **bezent**.

- **Deposit (EVM).** Discover wallets with EIP-6963 (MetaMask, Rabby and others); switch to chain 137. Read `minimum`
  and `maximum` from the contract, and the route's state from the public route (Open 8). Take the recipient from the connected Kreivo wallet, or from a pasted SS58 address with checksum and prefix 2
  checked. Never take raw hex. Before signing, read the recipient's account on Kreivo through the light client, and
  refuse one that cannot receive a non-sufficient asset (no provider), saying what to do (Open 7, ruled: bUSDC stays not sufficient). Sign an EIP-2612
  permit for **exactly** the amount, with the domain read from the token (`name()`, `version()`, chain id, the token's
  address; `NOTES.md`). Send `depositWithPermit`. Fallback, for a wallet that cannot sign typed data: `approve`
  exactly the amount, then `deposit`.
- **Redeem (Kreivo).** Connect Talisman, SubWallet or Polkadot.js through the injected-extension interface. Passkey
  accounts (Virto) are out for now and there is no spike (Open 9, ruled). **Requirement (Open 9):** to make these wallets
  work with the polkadot.js interface, Kreivo's custom signed extensions are registered in the polkadot.js
  initialisation metadata (the API's initialisation, before any signing), so that the extensions can build and sign
  Kreivo's extrinsics. The test bed shows it for each wallet (`T-018-15`). Show the bUSDC (`Here(101)`) balance and the contract's redemption minimum and pause
  state. The payout address defaults to the connected EVM account. Pick a random 32-byte reference (`REQ-PV-2`: never
  sequential). Call the contract's `redeem(asset, amount, backend, payout, reference)`. To redeem a whole balance,
  `utility.batch_all` of `assets.approve_transfer` and `redeem`, since the plain pull keeps the account alive
  (`contracts/bezent/src/lib.rs`, `redeem`). The user pays the Kreivo fee in KSM.
- **Status.** From chain data: the Polygon receipt gives the coordinate, then the bridge reference (`bz-d-…`), the
  mint's on-chain reference, and the contract's `mint_reference_used(reference)` and `Minted` event on Kreivo. Kreivo
  is read through smoldot with Kreivo's chain specification, which needs trust in no RPC. A withdrawal is followed
  from its `Redeemed` event to the vault's `Reference` log on Polygon. Conditions and review come from the public
  route (Open 8). The references are derived in TypeScript and checked against test vectors generated from the Rust
  derivations, so the two never drift.
- **Defence.** A reproducible build whose hash is published with each release; a strict CSP; no third-party script or
  analytics; the contract's address pinned in the build and shown; the recipient shown as SS58 beside the connected
  account.
- **Licence:** `Apache-2.0 OR MIT` (Open 12, ruled 2026-11-09; the row for `docs/oss/LICENSING.md` is in amendment
  0029). The app ships smoldot, GPL-3.0-or-later with the Classpath exception: check that this is compatible, and
  record it, before the first release.

### 5. evmhot sweeps through the directing contract

Ruled by @pandres95, 2026-11-08: Bloque's evmhot hot-wallet service sweeps each user's deposit address by calling the
directing contract's `deposit` from that address (`depositWithPermit`, or `approve` and then `deposit`), instead of a
plain transfer into the vault. Each sweep then names its Kreivo recipient on chain, in the same transaction as the
value: a Bloque Kreivo account set per chain and token in evmhot's configuration (which one is @pandres95's call), with
Swaps attributing each sweep to its user from the deposit address. A sweep is a directed arrival: it becomes a
deposit by itself (part 3) and needs no requester to bind it.

- **Until then**, a sweep stays a plain transfer, and Swaps binds it by coordinate as today (amendment 0029, A3).
- **The work is evmhot's**, outside this repository. bezent needs nothing new for it beyond parts 1 to 3: the sweep's
  `Transfer` pairs with its `Directed` log like any other.
- **What follows from the rule.** The funder is the deposit address, but the refund is the original depositor that evmhot
  names in the call (`d-refund-dest-swaps`), so a sweep that can never be minted is returned there (A5), and never back
  to a deposit address that would sweep it again. A sweep that mixes several senders has no single original depositor
  (amendment 0029, Open 15, to the owner, not ruled). A sweep must lie within the contract's limits (1 to 50 000 USDC), so a
  larger balance is swept in several calls and a smaller one waits. There is no share and no count cap (Open 5 and 6,
  ruled), so sweeps are bound by the route's window and the contract's limits alone, and count toward the alerts on
  directed mints.

## Not now

Gas sponsorship (Open 13, ruled: no); passkey and Virto accounts (Open 9, ruled: out for now); a pause on the directing
code (Open 4, ruled: none); ERC-7930 recipients (`NOTES.md`, "Recipient format"); other networks or assets (Base and
BNB Chain would each need their own directing contract and route); any custodial way in; making bUSDC
sufficient (Open 7, ruled: not now).

## Done when

- **Contract:** Foundry fork tests on Polygon at the pinned block show that a deposit moves exactly `amount` from the
  caller to the vault with `Directed` right after the `Transfer` (`AC-A6.6`, `REQ-RN-5`); below the minimum, above the
  maximum, a zero recipient, a zero refund, a refund equal to the vault and a refund equal to the contract revert and move nothing (`AC-A6.5`); `Directed` carries the refund;  a front-run permit does not stop
  the deposit; the bytes-signature permit works from a Safe as depositor; the contract never holds a balance
  and has no pause, owner or admin function; the verifier accepts the deployment and refuses a
  changed immutable.
- **Endpoint and core:** a directed arrival is one observation at the `Transfer`'s coordinate, never also an `Arrival`
  (`AC-A6.6`); a requester's deposit or refund naming it is `ERR-SourceNotFound` (`AC-A6.2`); it becomes a deposit
  minted for the recorded recipient, with the refund address its caller named as refund destination (`AC-A6.1`); a pause, a hold and the mint
  window park it (`AC-A6.3`); a never-mintable one is returned once, to its refund address (`AC-A6.4`); an unpaired `Directed`
  log is never an arrival; the reserve check stays at zero divergence through all of it (`INV-1`).
- **Web app:** on Amoy and the zombienet test bed, a user with only a browser wallet deposits, sees the mint, redeems
  and sees the payout; a recipient that cannot receive is refused before signing.
- **Mainnet (`M-4`):** a value-moving release under amendment 0024 (Open 11, ruled), inside the 48-hour soak of
  `DEC-19`. A round trip from the web app (deposit, mint, redeem, payout) of 1 USDC; production's redemption minimum
  is 1 bUSDC, so the 1 bUSDC minted is redeemed whole (amendment 0029, Open 14); one return exercised, with the alerts
  in force and the new runbooks exercised (`NFR-9`).
- **evmhot sweeps:** a sweep from a deposit address through the directing contract is minted for the recipient it
  names, with no deposit request from Swaps.
