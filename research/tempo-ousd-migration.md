<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Tempo and OUSD: a backend, and moving bUSDC's reserve to it

This is evidence, not doctrine. It is a read-only spike into how much work it would take to give bezent a network
backend on **Tempo** (chain id 4217; testnet "Moderato", 42431) with **OUSD** (Open USD, issued by Bridge, a TIP-20
token) as the origin token. It also asks how to **migrate** bUSDC's reserve there from native USDC on Polygon, keeping
one reserve and one issuer at any time.

- **Date read:** 2026-11-17.
- **Read from:** Tempo's documentation and repositories, Bridge's and Open Standard's documentation, safe-deployments,
  and Zodiac's registries.
- **Checked on chain:** the public RPCs `https://rpc.tempo.xyz` (mainnet) and `https://rpc.moderato.tempo.xyz`, plus
  dRPC's public endpoint.
- **Testnet transaction:** one, from a throwaway key held only in memory (§7).
- **Funds:** no mainnet value moved.
- **bezent version:** read from `origin/develop` at `1075402`.

Nothing here is ruled. Every change it names to `SPEC.md` or `PLAN.md` would go through an amendment first. The
"Recommendations" section lists the rulings it would need.

## Sources

**D:** `tempoxyz/docs` at `ec243ee`, published under `https://tempo.xyz/developers/docs/<path>` (docs.tempo.xyz
redirects there). Pages used:

- `protocol/transactions/spec-tempo-transaction`
- `protocol/fees/spec-fee`
- `protocol/fees/spec-fee-amm`
- `quickstart/evm-compatibility`
- `quickstart/predeployed-contracts`
- `quickstart/faucet`
- `protocol/tip20/spec`
- `protocol/tip403/spec`
- `protocol/exchange/spec`
- `protocol/blockspace/consensus`
- `partners/rpc-and-nodes`
- `sdk/rust`
- `sdk/foundry`
- `guide/ousd`

**T:** `tempoxyz/tempo` at `23f7113`, `https://github.com/tempoxyz/tempo`. Used:

- `tips/tip-1000.md`: state-creation gas.
- `tips/tip-1010.md`: mainnet gas parameters.
- `tips/tip-1016.md`: the planned storage-gas reservoir.
- `tips/tip-1022.md`: virtual addresses.
- `tips/tip-1028.md`: receive policies.
- `tips/tip-1115.md`: fallback fee tokens.
- `crates/evm/src/handler/config.rs`
- `crates/precompiles/src/tip20/`

**Safe:** `safe-global/safe-deployments` at `ca9cb0d` (`src/assets/v1.4.1/*.json`).

**Zodiac:** `gnosisguild/zodiac` at `b7988c0` (`src/networks.ts`) and `gnosisguild/zodiac-modifier-roles` at
`b4ae431` (`packages/sdk/src/main/chains.ts`).

**OUSD:**

- `https://ousd.fyi`
- `https://apidocs.bridge.xyz/overview.md`
- `https://apidocs.bridge.xyz/migrating-to-ousd.md`
- `https://apidocs.bridge.xyz/get-started/introduction/what-we-support/payment-routes.md`
- `https://apidocs.bridge.xyz/platform/issuance/rewards.md`
- `https://transparency.bridge.xyz/v0/stablecoins/ousd`
- `https://joinopenstandard.com` (and `/integrate`, `/legal/geo-restrictions-and-prohibited-use-cases`)
- `https://tokenlist.tempo.xyz/list/4217`

**Name collision:** searches for "OUSD" also return Origin Protocol's older Origin Dollar on Ethereum, an unrelated
token. Everything below is Open USD.

## Answers

### 1. Fees

**What a transaction looks like.** Tempo adds an EIP-2718 type, **`0x76`**, the "Tempo transaction" (D:
spec-tempo-transaction). Its fields, in RLP order:

```text
0x76 || rlp([chain_id, max_priority_fee_per_gas, max_fee_per_gas, gas_limit, calls, access_list,
             nonce_key, nonce, valid_before, valid_after, fee_token, fee_payer_signature,
             aa_authorization_list, key_authorization?, sender_signature])
```

- **`calls`** is a batch of one or more `[to, value, input]` entries.
- **`nonce_key`** selects a parallel nonce sequence. Key 0 is the protocol nonce; `uint256.max` is an expiring nonce.
- **`fee_token`** names the TIP-20 that pays.
- **`fee_payer_signature`** lets another account sponsor the fee. The sponsor signs a `0x78`-domain hash with secp256k1.
- **`sender_signature`** may be secp256k1, P256, WebAuthn or an access key.

**A plain EIP-1559 transaction is accepted.** Types 0, 1, 2 and 4 all work (T: `crates/evm/src/handler/config.rs`).
I sent a type-2 transaction on Moderato (§7), and a mainnet type-2 is visible in the explorer as well.

- **Any non-zero `value` is rejected.** The error is `-32003 "value transfer not allowed"`, which I reproduced with
  `eth_estimateGas`.
- **`BALANCE`, `SELFBALANCE` and `CALLVALUE` are always 0** (D: evm-compatibility).
- **`eth_getBalance` returns a constant** for every address: 4242…42 (76 digits, about 4.24 × 10⁷⁵). Its purpose is to
  keep wallets from refusing a transaction for "insufficient funds".

**How the fee token is chosen** (D: spec-fee; TIP-1115). The first rule that applies wins:

1. The transaction's `fee_token`. Only the `0x76` type has this field.
2. The payer's preference, `FeeManager.setUserToken(token)`. FeeManager and the Fee AMM are one precompile at
   `0xfeec000000000000000000000000000000000000`.
3. The token being called, when the top-level call is `transfer` or `transferWithMemo` on a USD TIP-20.
4. The DEX's input token, for a swap.
5. The fallback: pathUSD, then OUSD. OUSD applies only when the pathUSD balance cannot cover the maximum fee. TIP-1115
   is active since upgrade T13, and `tempo_forkSchedule` shows both networks at T13.

The chosen token must be a USD TIP-20 whose balance covers `gas_limit × max_fee`. The Fee AMM must also hold enough
liquidity to convert it into the validator's token, or the transaction is invalid. Fee swaps are charged at a fixed
0.9970, a 0.3 % spread that goes to the liquidity providers (D: spec-fee-amm).

**Gas pricing.** Prices are in **attodollars per gas** (10⁻¹⁸ USD).

- **Fee in token units:** `ceil(gas × price / 10¹²)`, since tokens have 6 decimals.
- **Base fee:** bounded and dynamic. Its floor is 6 × 10⁸ (D: spec-fee). TIP-1010 names 2 × 10¹⁰ as the launch value,
  but the live networks sit at the floor.
- **Live readings on both networks:**
  - `eth_gasPrice`: `0x23c34600` (6 × 10⁸) on mainnet, `0x23c34601` on Moderato.
  - `eth_maxPriorityFeePerGas`: 0 on mainnet, 1 on Moderato.
  - `eth_feeHistory`: a flat base fee.
- **Cost of a payout:** about 300 000 gas, which is about $0.0002.

**State creation is expensive in gas, cheap in dollars** (TIP-1000):

- **New storage slot:** 250 000 gas.
- **New account** (its first nonce write): 250 000 gas.
- **Contract code:** 1 000 gas per byte, plus a fixed 500 000.
- **Transaction gas cap:** 30 M (TIP-1010).
- **Coming change:** TIP-1016 plans 2 500 gas per byte, under a separate "reservoir".

**`eth_estimateGas` behaves as on Ethereum**, but includes those costs. Measured on Moderato:

| Transaction | Estimated gas |
|---|---|
| OUSD `transferWithMemo` to an address that holds none | 293 714 |
| Zero-value self-transfer from a **fresh** key (nonce 0) | 274 318 |
| The same self-transfer once the nonce is 1 | 21 000 |

**What changes in bezent's signer** (`services/signer`):

- **The format can stay as it is.** It signs Keccak-256 over `0x02 ‖ rlp(…)` (`delivery.rs`, `cancel.rs`).
  - Tempo's chain id is just another `chain_id`.
  - The vault plugin signs a secp256k1 hash whatever the chain.
  - The signer should **never** sign `0x76`. Its strict decoders already refuse any other type byte, and that is the
    right posture.
- **Fee token.** Every transaction bezent signs is a call to the Roles modifier or a self-transfer of nothing, so rule 5
  picks the fee token. A delivery key that holds only OUSD pays in OUSD with no setup transaction. A gift of pathUSD
  large enough to cover the maximum fee would be spent first, which harms nothing.
- **Cancel at nonce 0.** `CANCEL_GAS` is 21 000 (`services/evm-endpoint/src/submit.rs`), and the signer insists on
  exactly 21 000 (F-009 NOTES, SF2). That fails at a lane's nonce 0, which costs 274 318 for the account creation. A cancel could
  therefore never consume nonce 0. The lane's first transaction must be a harmless one before any payout, or the
  signer's cancel rule must allow the account-creation gas at nonce 0.
- **Fee bounds are in attodollars, not wei.** This covers `gas.max_fee_per_gas` and `cancel_max_fee_per_gas`. Their
  meaning, and the values in fixtures, change per network.
- **The payout call shape changes** if deliveries use `transferWithMemo`, as recommended in §4. The decoder
  `decode_evm_payout` gains one shape: `Roles.execTransactionWithRole(OUSD, 0, transferWithMemo(beneficiary, amount,
  reference), Call, roleKey, true)`, with no MultiSend.

**What changes in the fee position** (amendment 0025):

- **Today's reading cannot work on Tempo.** The EVM endpoint reads the lane's fee position with `eth_getBalance`
  (`src/fee.rs`). The constant 4.24 × 10⁷⁵ is above `u128`, so `fee::read` marks the reading unknown ("above the
  store's domain"), forever. Only the infrastructure alarm on `fee_position_age_seconds` would notice.
- **Preparation's balance check passes vacuously.** It uses the same native balance (`submit.rs`, "the lane's {balance}
  wei does not pay the fee"), and the constant always covers the fee.
- **On Tempo, the fee position is the lane's balance of its fee token(s):** OUSD, and pathUSD if it is ever held. It is
  in the token's minor units, and the needed fee is `ceil(gas × max_fee / 10¹²)` in those units.
- **The SPEC glossary needs one line.** It defines a fee position as "the native token an address needs to pay network
  costs", and Tempo has none. The wording should say "the token the network charges its costs in".
- **The floor in the core's configuration becomes microdollars** for this network.
- **Network costs go against surplus as today** (`REQ-BK-4`). They are now in the same unit as the reserve, which makes
  the book simpler.

**Fee sponsorship.** Tempo can sponsor fees natively, through `fee_payer_signature` on `0x76` (D: spec-tempo-transaction).
bezent does not need it for its own keys. It is the straightforward answer to amendment 0029's gas question
(`d-0029-gas`) for web-app users. On Tempo, those users pay fees in the stablecoin they already hold, so they need no
native gas token at all.

### 2. JSON-RPC compatibility and finality

**Finality.**

- **Consensus is Simplex BFT** (Commonware), with a BLS12-381 threshold key and blocks about every 600 ms (D: consensus).
- **Finality is deterministic.** There is no reorganisation after it.
- **`finalized` and `safe` both exist,** one or two blocks behind `latest`. Live:
  - Mainnet: `finalized` = `safe` = latest − 1.
  - Moderato: `finalized` = latest − 2, `safe` = latest − 1.
- **`REQ-RN-1` holds,** and the endpoint's rule (`DEC-8`: read up to `finalized`, re-check hashes) works unchanged.

**What the endpoint uses.**

- **`eth_getLogs`** works with ranges. The public RPC caps a query at 100 000 blocks ("query exceeds max block range
  100000") and 20 000 results ("query exceeds max results 20000, retry with the range A-B"). The endpoint's and the
  verifier's ranges (`VAULT_LOG_RANGE`, 10 000 by default) are inside both caps.
- **Receipts** add `feeToken` and `feePayer`.
- **Blocks** add `timestampMillis`, `timestampMillisPart`, `mainBlockGeneralGasLimit`, `sharedGasLimit` and
  `consensusContext`.
- **The parsers ignore extra fields.** They read JSON by hand (`services/evm-endpoint/src/rpc.rs`), so the extra fields
  are harmless.
- **`eth_getBlockByNumber(n, true)` is not harmless.** A `0x76` transaction carries `calls` and `signature`, not
  `input`, `r`, `s` and `v`. `parse_tx` then returns `None`, and `block_transactions` fails the **whole block** ("well-formed
  transaction"). Any block that holds one `0x76`, which is most Tempo blocks, leaves a non-execution proof `Pending`
  forever (`proof.rs`). **This must be fixed before payouts.**
- **Matching the lane's transaction must respect nonce keys.** A `0x76` transaction's `nonce` belongs to its
  `nonce_key`. Only key 0 is the protocol nonce that `eth_getTransactionCount` reports. The proof's lookup
  (`t.from == lane && t.nonce == n`) has to apply the same rule.

**Providers** (D: partners/rpc-and-nodes).

- **Listed:** Alchemy, QuickNode, dRPC, Chainstack, Conduit, Dwellir, Blockdaemon, Luganodes and SenseiNode.
- **Answered live:** dRPC's public `https://tempo.drpc.org` answered `0x1079` (4217). The others need keys, and I did
  not test them.
- **QuickNode:** its documentation still names the old testnet id 42429.
- **Enough for `DEC-12`:** two independent paid providers are available. Public submission nodes (`amendment 0023`)
  could be `rpc.tempo.xyz` and dRPC's public endpoint.

### 3. TIP-20 against ERC-20, and OUSD

**Addresses.**

- TIP-20 tokens are **precompiles at fixed addresses**, `0x20c0` followed by 18 more bytes. They are created by the
  TIP20Factory at `0x20fc…0000` (D: tip20/spec).
- `eth_getCode` at one returns one byte, `0xef`, so a token cannot be pinned by code hash.
- Transfers to the zero address, or to any `0x20c0…` address, are refused.

**The ERC-20 surface is identical.**

- `transfer`, `approve`, `transferFrom` and `allowance` behave as on ERC-20.
- `Transfer` and `Approval` have the standard topics (`Transfer` is `0xddf252ad…`).
- `decimals()` is always 6.
- **EIP-2612 `permit`** is supported since upgrade T2, with **ECDSA only** (v = 27 or 28). There is no EIP-1271 form.

**Memos.**

- Functions: `transferWithMemo(address,uint256,bytes32)`, `transferFromWithMemo(address,address,uint256,bytes32)`,
  `mintWithMemo`, `burnWithMemo`.
- Event: `TransferWithMemo(address indexed from, address indexed to, uint256 amount, bytes32 indexed memo)`, topic
  `0x57bc7354aa85aed339e000bccffabbc529466af35f0772c8f8ee1145927de7f0`.
- **The memo is exactly 32 bytes.**
- The token emits the plain `Transfer` first, then `TransferWithMemo`, adjacent. I saw them at log indexes 3 and 4 in
  §7's transaction.

**Issuer powers** (D: tip20/spec, tip403/spec).

- **Roles:**
  - `ISSUER_ROLE`: mints and burns.
  - `PAUSE_ROLE` and `UNPAUSE_ROLE`.
  - `BURN_BLOCKED_ROLE`: burns the balance of an address the token's policy blocks.
  - `BURN_AT_ROLE`: burns from any address, ignoring policy.
  - `DEFAULT_ADMIN_ROLE`: sets the policy, the quote token and the supply cap.
- **TIP-403 transfer policies** live in a registry at `0x403c…0000`. Policy 1 allows everything. Custom policies
  (WHITELIST, BLACKLIST, COMPOUND) start at 2, and each one's `admin` edits it.
- **OUSD's policy on mainnet:**
  - `transferPolicyId()` is 2, and `policyData(2)` is a **BLACKLIST**.
  - Its admin is `0x251d2711ebeB0a09fdB8992F5506f3D949175246`, an account with an EIP-7702 delegation.
  - **So the issuer can block the reserve vault or any recipient, and can then burn a blocked balance.** That is more
    than Circle can do to USDC on Polygon, where a blacklist freezes a balance but cannot burn it.
  - Who holds the burn roles: **unconfirmed**, as the roles were not enumerated.

**Receive policies** (TIP-1028, active since T6). This mechanism is new to bezent's threat model.

- **Any recipient may set a receive policy.** A transfer that the policy blocks **does not revert**. The token credits
  `ReceivePolicyGuard` instead, emits `Transfer(from, ReceivePolicyGuard, amount)` and `TransferBlocked(…)`, and keeps
  a receipt.
- **Only the receipt's recovery authority can claim it:** the originator, which for a payout is the vault, or an address
  the recipient chose.
- **This affects payouts:**
  - A payout to such a recipient executes, and value leaves the reserve.
  - The beneficiary is credited nothing, and the endpoint sees no transfer to the beneficiary.
  - If the recovery authority is the originator, the value sits in the guard until the vault claims it, and no vault
    role can do that today.
- **A recipient can change its policy at any time,** including between bezent's dry run and inclusion.

**TIP-1022 virtual addresses** (active since T3).

- An address of the form `[4-byte masterId][0xFDFD…FD][6-byte tag]` forwards any TIP-20 sent to it to a registered
  master. It emits two `Transfer` events: sender → virtual, then virtual → master. Registration needs a 32-bit
  proof of work.
- That gives deposit addresses without sweeps, which is relevant to evmhot and Swaps. The arrival bezent sees at the
  vault is the second `Transfer`, whose `from` is the virtual address.

**OUSD** (Bridge, `apidocs.bridge.xyz/overview.md`; D: guide/ousd; read on chain).

- **Address:** `0x20c0000000000000000000006a37da5c996874be`, the same on mainnet and Moderato. On Moderato it is a
  faucet test token with policy 1.
- **Mainnet reads:** `name()` "OpenUSD", `symbol()` "OUSD", `decimals()` 6, `currency()` "USD", `quoteToken()`
  pathUSD, `paused()` false, `supplyCap()` 1 000 000 001 OUSD.
- **Mainnet supply:** about 631.8 M OUSD on 2026-11-17.
- **Other chains:** OUSD also exists on Ethereum, Base and Solana. **It is not on Polygon.**

### 4. The vault on Tempo

**Safe is deployed canonically.**

- safe-deployments lists chain ids 4217 and 42431 as canonical for v1.3.0, v1.4.1 and v1.5.0.
- I compared runtime code hashes on both networks with the ones bezent pins (F-008 NOTES), and they are **identical**:

  | Contract | Code hash on both networks |
  |---|---|
  | SafeL2 1.4.1 `0x29fc…C762` | `0xb1f92697…` |
  | Safe 1.4.1 | `0x1fe2df85…` |
  | SafeProxyFactory 1.4.1 | `0x50c3cdc4…` |
  | MultiSendCallOnly 1.4.1 | `0xecd5bd14…` |

**Deterministic deployers exist.** Arachnid's CREATE2 proxy `0x4e59…956c` and the Safe singleton factory `0x914d…43d7`
are present, with identical code. So are CreateX `0xba5E…a5Ed`, Multicall3 and Permit2 (D: predeployed-contracts).
**ERC-2470's `0xce00…cf9f` has no code.**

**Zodiac is not deployed.**

- There is no code at the Roles 2.1.1 mastercopy `0xF296…83D5`, the Delay 1.1.1 mastercopy `0x8241…6F01` or the
  ModuleProxyFactory `0x0000…a236`, on either network.
- Neither Zodiac registry lists Tempo.
- **bezent would deploy the three itself,** through Arachnid's proxy, at addresses of its own, pinned in a
  `config/tempo.json`. The canonical addresses are out of reach: a keyless deployment transaction signed for Ethereum's
  gas costs runs out of gas under TIP-1000. That is an inference; I did not try it.
- **Gas is not a problem.** No contract can exceed EIP-170's 24 576 bytes, which still applies (TIP-1010). The largest,
  Roles, therefore costs at most about 25.1 M gas, inside the 30 M cap, and cents in fees.

**Stablecoin fees break nothing the vault relies on.**

- **Safe's refund path.** `execTransaction` with `gasPrice > 0` and `gasToken = 0` would send native value, which is
  always 0 and so reverts (GS011). `VaultGuard` already refuses any owner transaction with a refund (F-008 NOTES, "The
  guard is an allowlist"), so that path is closed.
- **Value.** Every transaction bezent builds carries no value, and the roles admit no value.
- **The guard is an allowlist by target.** TIP-20's extra selectors (`transferWithMemo`, `transferFromWithMemo`), the
  TIP-403 registry, the receive-policy calls and the virtual-address registry are all "anything else", which it refuses.
- **The missing fallback handler still matters.** TIP-20 `permit` is ECDSA-only, so an EIP-1271 permit is impossible
  anyway.
- **Owners and the guardian need stablecoin for fees** (pathUSD or OUSD) instead of POL.
- **Deployment.** Upstream Foundry supports Tempo in `forge`, `cast` and `anvil` (D: sdk/foundry), including an anvil
  Tempo mode for fork tests.
- **Safe tooling.** Whether Safe{Wallet}'s interface and transaction service support Tempo is **unconfirmed**. bezent's
  owners already act through scripts, so this is not a blocker.

**`VerifyVault` still works.** It compares runtime code hashes with the repository's artefacts and replays the Roles
events with `eth_getLogs` in 10 000-block ranges (`script/VerifyVault.s.sol`). Both work on Tempo. Two differences:

- The pinned addresses for Zodiac become bezent's own, with the hashes unchanged.
- The origin token cannot be checked by code hash. It is checked by address, with `currency()`, `decimals()`,
  `symbol()` and `transferPolicyId()`.

**A simpler delivery role.** TIP-20 carries the reference in the token itself (`REQ-RN-5`: "by the token itself"). The
delivery role can therefore be one scoped function: `OUSD.transferWithMemo(any, amount ≤ allowance, any)`, and nothing
else on OUSD.

- **What goes away:** MultiSendCallOnly, `ReferencedTransferUnwrapper` and `ReferenceEmitter`, so less of bezent's own
  code sits in the authorisation path (F-008 NOTES, "Why a custom unwrapper").
- **Payout pairing** becomes "the vault's `TransferWithMemo`, right after its `Transfer`".
- **Proofs** look for the memo in place of the `Reference` event.

### 5. Directed deposits: could a memo replace `DirectedDeposit`?

**Not on its own.** The memo is 32 bytes. Amendment 0029 needs two things recorded with the value:

- the Kreivo recipient, a 32-byte AccountId;
- the refund address, the original depositor, named explicitly (`d-refund-dest-swaps`).

Both do not fit in one memo. A raw memo is also not a signal of intent: Bridge, exchanges and payment processors send
memos of their own. If any `TransferWithMemo` into the vault counted as directed, bezent would mint to whatever 32
bytes a third party used as its reference.

There are three ways to do it. Each is compared with amendment 0029's design:

| | (a) Port `DirectedDeposit` | (b) Memo to a "directed" virtual address of the vault | (c) Memo to the vault address |
|---|---|---|---|
| **How** | Same contract with `token` = OUSD. It does `transferFrom(msg.sender, vault)`, then emits `Directed`, as on Polygon | The vault registers a TIP-1022 master id kept for directed deposits. The user calls `OUSD.transferWithMemo(<that virtual address>, amount, recipient)` | `transferWithMemo(vault, amount, recipient)` |
| **Pairing** (A4) | Unchanged: the `Transfer` right before `Directed`. Use plain `transferFrom`: `transferFromWithMemo` would put a `TransferWithMemo` between them | `Transfer(from → virtual)`, `TransferWithMemo`, `Transfer(virtual → vault)`, all from the token. The virtual `to` marks the intent | Any memo |
| **Refund** | Named in the call, as ruled | Only `from`. That loses `d-refund-dest-swaps`, and `DEF-5`'s concern about exchanges' shared addresses comes back. The sweep loop behind the ruling goes away, because virtual addresses need no sweeps | Only `from` |
| **Limits** (`REQ-LM-2`: refuse before value moves) | In the contract | **None on chain.** An amount outside the route's limits arrives anyway and can only be returned | None |
| **Front-running** | Permit is ECDSA-only, so the bytes-signature form for contract wallets (FiatToken 2.2) does not exist. Keep `try permit`, then check the allowance, then pull from `msg.sender`. A Tempo account can instead batch `approve` and `deposit` in one `0x76` transaction, with no permit and no race | Nothing to front-run: the funder's own transfer | Same |
| **Our code in the path of value** | One contract, as today | None. The vault must register the master: a Safe call to the registry, through the Delay, which `VaultGuard` refuses today | None |
| **Verdict** | **Do this first.** Smallest change, and every ruling of 0029 stands | Worth a later spike, with a ruling on refunds to `from` and on limits | Rejected |

**What would change in amendment 0029 for (a):**

- `DEC-21` names Tempo and OUSD.
- The EIP-1271 permit overload is dropped, or kept knowing that it always falls through to the allowance check.
- The web app adds the batched path for Tempo accounts.
- A4 and A5 are unchanged.

**A new exposure to record:** TIP-1028 receive policies on the **refund** address. A return to a depositor that has set
a policy can be redirected to `ReceivePolicyGuard`, as with any payout (§3).

### 6. OUSD operations

**Converting USDC to OUSD, 1:1.**

- **Bridge's API is the documented route** (`migrating-to-ousd.md`, `payment-routes.md`):
  - A `POST /v0/transfers` with source `{currency: "usdc", payment_rail: <chain>}` and destination `{currency:
    "ousd", payment_rail: "tempo", to_address}`.
  - Liquidation addresses and static templates can also point at OUSD.
  - **USDC on Polygon to OUSD on Tempo is a listed route.** The reverse route reaches about 12 chains.
  - The minimum is $1 per route.
- **Open Standard's partners** (Stripe and Bridge, Coinbase, Mastercard, Visa) mint and redeem 1:1 for their customers
  (`joinopenstandard.com/integrate`). Coinbase supports OUSD on Base, Ethereum, Solana and Tempo. Its conversion terms
  are **unconfirmed**.
- **Tempo's stablecoin DEX** (a precompile at `0xdec0…`) has a direct USDC.e/OUSD book. Quoted live:
  - USDC.e → OUSD at 0.9996 (about 4 bp), up to about $3.97 M.
  - OUSD → USDC.e at 1.0001, up to about $0.99 M.
  - The DEX documents no taker fee.
  - USDC.e is Stargate-bridged USDC, `0x20c0…b9537d11c60e8b50`. **No native Circle USDC on Tempo was found.**

**Fees and limits.**

- Open Standard says there are "no fees to mint or redeem, even at scale" and no volume limits.
- Bridge's Tempo examples show a zero exchange fee and a zero gas fee. A developer fee is optional, set by the
  integrator.
- Daily limits and settlement time for OUSD are **unconfirmed**. For its custom stablecoins in general, Bridge states
  redemption within one business day.

**From a Safe or contract address: unconfirmed, probably yes.**

- The transfer API takes any `to_address`.
- Bridge's travel-rule schema has an `external` wallet type, defined as an "exchange-owned wallet or smart contract".
- On chain, OUSD's blacklist policy lets any contract that is not listed hold and move it.
- **Ask Bridge directly before relying on it.**

**Reserves transparency.**

- **ousd.fyi** is built by Tempo from Bridge's Transparency API. It calls itself "informational, not an independent
  reserve attestation".
- **The API on 2026-11-17:**
  - Supply on all chains: 963.7 M.
  - Reserves: 965.7 M, of which 848.7 M is Treasuries and 115.1 M is cash.
  - Ratio: 1.0021.
- **Custody:** BNY, BlackRock and Lead Bank.
- **Attestation:** cadence and auditor for OUSD itself are **unconfirmed**. Bridge's issuance docs say quarterly
  attestations plus monthly reports.
- **Reserve yield** goes to Open Standard partners, less a management fee (`rewards.md`). Holders get nothing on chain:
  TIP-20 opt-in rewards are unused, with `optedInSupply` = 0.

**KYC and KYB.**

- **Holding and moving OUSD on chain needs none,** subject to the blacklist.
- **Minting and redeeming directly needs a KYB'd Bridge customer,** or a partner's onboarding.
- **Restricted jurisdictions:** Open Standard's list covers the EEA and sanctioned or conflict areas.
- **Whether Bridge onboards the operator's jurisdiction for OUSD is unconfirmed.** It is the longest lead item.
- **Bridge's charter** is a conditional OCC national-trust-bank approval of 2026-02-12. Whether it is now final is
  unconfirmed.

### 7. The Moderato check

- **The faucet needs no captcha.** It is an RPC method, `tempo_fundAddress <address>` on
  `https://rpc.moderato.tempo.xyz` (D: faucet). It funded 10⁶ tokens each of pathUSD, AlphaUSD, BetaUSD, ThetaUSD and OUSD to a
  fresh address.
- **The key existed only in a shell variable** for one command and was discarded. A first key was funded and discarded
  without sending, because the memo literal was the wrong length.

**One transaction** from the second key: `OUSD.transferWithMemo(0x…bEEF, 1, "bezent-tempo-research")`, sent by `cast
send` with no Tempo options.

- **Transaction** `0x32b2e69fcefde3c9a2787266a3fe68015b1480fcb951e1d83e8b90c8e9ade912`, block 44 642 144, explorable
  at `explore.tempo.xyz`.
- **Type `0x2`:** chain id `0xa5bf`, nonce 0, `value` 0, `maxFeePerGas` 1 200 000 001, `maxPriorityFeePerGas` 1.
- **Receipt:** status 1, `gasUsed` 283 443, `effectiveGasPrice` 600 000 001, `feeToken` = OUSD, `feePayer` = the sender.
  Precedence rule 3 chose OUSD, because the top-level call was a TIP-20 `transferWithMemo`.
- **Logs:**
  - `Transfer(sender → 0x…bEEF, 1)`.
  - `TransferWithMemo(sender, 0x…bEEF, 1, memo)`, with the memo as the third indexed topic.
  - The fee: `Transfer(sender → 0xfeec…, 171)`. That is 283 443 × 600 000 001 / 10¹² = 170.07, rounded up.
- **After it:** OUSD fell by 172 (1 sent + 171 in fees), pathUSD was untouched, the nonce was 1, and `eth_getBalance`
  still returned 4242…42.
- **Self-transfer estimates:** a zero-value self-transfer estimated 274 318 gas at nonce 0 (the first key) and 21 000
  at nonce 1. A value-1 self-transfer is refused: "value transfer not allowed".

## Gaps against bezent's components

| Component | Gap | Size |
|---|---|---|
| **EVM endpoint** (`services/evm-endpoint`) | `parse_tx` must read `0x76` transactions, with `from`, `nonce_key` and `nonce`. Without it, any block holding one fails `block_transactions`, and proofs stay `Pending` (§2) | M |
| | The proof's lane lookup counts only nonce key 0 | S |
| | Fee position and the preparation balance check read TIP-20 balances, not `eth_getBalance`. Fee units are attodollars, and the needed fee is `ceil(gas × fee / 10¹²)` in the fee token (§1) | S |
| | Payout pairing and proofs use `TransferWithMemo`, if the delivery role uses it (§4) | M |
| | TIP-1028: classify a vault transfer to `ReceivePolicyGuard` paired with `TransferBlocked`, a new observation kind, never a `Payout`. Read `validateReceivePolicy(OUSD, vault, beneficiary)` at the dry run and refuse to prepare a blocked payout (§3) | M |
| | TIP-1022: an arrival through a virtual address (two hops) is still keyed at the hop into the vault. Decide what `from` the store records | S |
| | The lane alarm (`SF1`, amendment 0022) reads only the protocol nonce and code. A thief with the delivery key can use other nonce keys, expiring nonces, or a sponsor, and an access key (`key_authorization`) can be added to the account. Watch the lane's fee `Transfer`s to `0xfeec…` and its keychain events as well | M |
| | Configuration, fixtures and fork tests under anvil's Tempo mode | M |
| **Signer** (`services/signer`) | A decoder for the `transferWithMemo` payout shape (§1) | S |
| | Cancel gas at nonce 0 (274 318, not 21 000), or a rule that the lane is warmed first | S |
| | Fee bounds in attodollars | S |
| | Tests. No new engine: secp256k1 over Keccak-256 is unchanged | S |
| **Vault and contracts** (`contracts/evm`) | Deploy Roles 2.1.1, Delay 1.1.1 and ModuleProxyFactory 1.2.0 ourselves through Arachnid, and pin them with code hashes in `config/tempo.json` | S |
| | The `transferWithMemo` delivery scope, and `VerifyVault`'s replay for it | M |
| | Port `DirectedDeposit` to OUSD (§5) | S |
| | A **migration role** on the Polygon vault (below) | M |
| | Fork tests on anvil's Tempo mode | M |
| | An external review of the changed role configuration before funds, as `F-014` requires | M |
| **Kreivo contract** (`contracts/bezent`) | No code change. `Backend::Evm { chain_id }` admits 4217, and changing bUSDC's backends is a scheduled governance change with the raise delay | — |
| **Store** (`crates/store`) | A Tempo network row, with `address_format` `evm` and `chain_id` 4217 | S |
| | Observation kinds for blocked transfers and for the migration's legs, and the fee position's unit | S |
| | The migration's movement kind in the reserve book (`REQ-BK-5`, `REQ-BK-7`) | M |
| **Core** (`services/core`) | The reserve migration: tranches, an itemised `InTransit` per Bridge transfer, the cut-over of the asset's reserve backend, and floors in microdollars | M–L |
| | Rules for a payout blocked by a receive policy: a named condition or review, and how a vault claim is ordered | M |
| **Web app** (`apps/bridge`) | Tempo network configuration. Wallets need a chain entry with a native currency, and the balance they show is meaningless | S |
| | Deposits through `DirectedDeposit` on Tempo: permit (ECDSA) or a batched `0x76` approve and deposit | M |
| | Fee-token wording, and status reading Tempo | S |
| **Spec and plan** | Amendments for: `REQ-AS-3` (one issuer at any time, with a bounded migration); `REQ-BK-5` (a migration movement); the fee-position glossary line; blocked payouts (TIP-1028); and `DEC-8`/`9`/`10`/`11`/`12`/`21`/`22` naming Tempo | M |

S is under three days, M one to two weeks, and L three weeks or more, for one engineer with agents.

## Migration sketch

**The goal.** bUSDC's one reserve moves from native USDC in the Polygon vault to OUSD in a Tempo vault. Holders do
nothing. Supply never exceeds backing, and only one issuer backs bUSDC once the move is done.

**It cannot be atomic.** The value crosses issuers through Bridge, so for the hours of the move part of the reserve is
USDC, part is OUSD and part is in transit. `REQ-AS-3` and `REQ-AS-8` forbid exactly that today. The amendment should
allow it only while every bUSDC route is closed. Each step below states the reserve equation it holds:

`Σ Reserve + InTransit = Supply + OwedMint + OwedPayout + Surplus`

0. **Rulings first.** An amendment rewording `REQ-AS-3` to "one issuer at any time, outside a migration with every route
   closed", and adding a migration movement to `REQ-BK-5`.
   - **The Bridge leg.** Both of its ends are public on chain. Only the link between them rests on Bridge's transfer
     id: an attested pairing of two public legs, itemised in `InTransit` (`REQ-BK-2`).
   - **The asset's name.** bUSDC backed by OUSD is no longer USDC (CLAUDE.md, guardrail 9, in spirit). `REQ-AS-1` keeps
     the identity, but the metadata and the product name are the owner's call.
1. **Build and prove on testnet.** Run the Moderato vault, endpoint and signer end to end, with payouts, cancels,
   proofs, the directed deposit and a blocked-recipient case.
   - **Invariant:** nothing on mainnet changes.
2. **Stand up mainnet Tempo, unfunded.** Deploy the vault, pass `VerifyVault`, and run the endpoint and signer for 4217,
   observing only. No route is declared, so any arrival is unassociated.
   - **Invariant:** `Reserve(Tempo)` is not part of bUSDC's equation.
   - **Parallel work:** start Bridge KYB, and register the liquidation address whose destination is OUSD on Tempo, to
     the Tempo vault.
3. **Queue the migration role on the Polygon vault.** It is a widening change, so it goes through the Delay: a 24-hour
   cooldown in public, and the guardian can veto it (`DEC-10`). It allows `USDC.transfer` **only** to that one Bridge
   deposit address, paired with a reference, within an allowance equal to the reserve.
   - **Invariant:** queueing moves nothing.
4. **Close bUSDC's routes.**
   - The Kreivo contract's pause, per direction (§7.F), and the routes' pause in the core.
   - In-flight transfers finish, or wait named.
   - `OwedMint` and `OwedPayout` drain to zero, or stay itemised.
   - **Invariant:** `Reserve(Polygon) = Supply + Surplus`, plus any itemised owed amounts.
5. **Move in tranches.** Start with a small probe, then the rest within the allowance.
   - **Each tranche:** a Polygon outflow with reference *rᵢ*, final. Then `InTransit += aᵢ`, itemised by *rᵢ* and
     Bridge's transfer id. Then the OUSD arrival at the Tempo vault, final, named by that id. Then `InTransit −= aᵢ` and
     `Reserve(Tempo) += aᵢ`.
   - **A difference in a tranche** (a fee, which should be zero) goes against surplus (`REQ-BK-4`). The operator tops
     up the surplus first if the floor would be crossed (`REQ-BK-10`).
   - **Invariant:** `Reserve(Polygon) + InTransit + Reserve(Tempo) = Supply + Surplus` at every final point. A tranche
     Bridge never delivers is a divergence: routes stay closed and a person is paged.
6. **Cut over.**
   - `Reserve(Polygon)` is zero: dust, and the surplus held on Polygon, leave as a recorded surplus withdrawal.
     `InTransit` is zero.
   - Declare OUSD on Tempo as bUSDC's origin token (`REQ-AS-2`) and its one reserve (`REQ-AS-8`).
   - Schedule the contract's backends change from `Evm { 137 }` to `Evm { 4217 }`. The raise delay applies, so
     schedule it during step 3.
   - Revoke the migration role. That is immediate, since narrowing is a pause.
   - Publish the new statement, naming the backend and the issuer (`REQ-BE-3`).
   - Reopen the routes.
   - **Invariant:** `Reserve(Tempo) = Supply + Surplus`, with one issuer.
7. **Retire Polygon.** Keep observing the Polygon vault for a declared period.
   - **Directed deposits keep arriving.** `DirectedDeposit` on Polygon has no pause (A6), so late users still send
     to it. Each such deposit is a directed arrival for a route that no longer exists, so A5's "can never be minted"
     returns it to its refund address. The design already handles this.
   - **Plain transfers** are unassociated and refundable as today.
   - **Swaps** moves its funding to Tempo: plain transfers, or Bridge delivering straight to the vault.
   - **Convertibles:** none are declared on Tempo at first. USDC.e through the Tempo DEX is a later venue (`DEC-13`),
     and USDC.e is not USDC.

**Downtime is step 4 to step 6.** That is hours if Bridge settles in minutes. Bridge's settlement time is unconfirmed,
so the first tranche measures it.

**An alternative with no downtime** keeps both reserves live and lets withdrawals run Polygon down. It needs two
reserves for one asset for weeks, and so a much larger amendment. I do not recommend it.

## Effort

| Component | Estimate |
|---|---|
| EVM endpoint | 3–4 weeks |
| Signer | 1–1.5 weeks |
| Vault and contracts, without external review | 3 weeks |
| Store | 1 week |
| Core, mostly the migration flow | 2–3 weeks |
| Web app | 1–1.5 weeks |
| Amendments and plan | 1 week, plus rulings |
| Kreivo contract | none: governance actions only |
| **Total** | **about 12–15 engineer-weeks** |

Add the role configuration's external review and the 48-hour mainnet soak (`DEC-19`). Bridge onboarding runs in
parallel and probably sets the calendar: about **2–3 months**.

## Open risks

1. **Issuer powers.**
   - OUSD's blacklist can block the vault.
   - `BURN_BLOCKED_ROLE` can then burn its balance, and `PAUSE_ROLE` stops every transfer.
   - The policy admin is a single account with an EIP-7702 delegation.
   - Circle can freeze USDC but not burn it, so this is more than bUSDC's holders accept today.
2. **Receive policies (TIP-1028).**
   - Any payout or return can execute without reaching its beneficiary.
   - A recipient can turn a policy on between the dry run and inclusion.
   - What counts as "delivered" then is a spec question, not an implementation one.
3. **Protocol churn.**
   - Thirteen protocol upgrades so far (T1 to T13), with T14 next.
   - Each changed gas, fees, nonces or token behaviour: TIP-1016 plans 2 500 gas per byte, and T14 adds native
     multisig accounts.
   - bezent should watch `tempo_forkSchedule` and re-run its fork tests before each upgrade.
4. **Fee AMM liquidity.** A delivery key paying in OUSD needs OUSD-to-validator-token liquidity. If it runs dry, every
   transaction is invalid: payouts stall as under `CND-FeeShort`.
5. **The lane's new surfaces.** Two-dimensional nonces, expiring nonces, sponsorship and access keys widen what a
   stolen delivery key can do unseen. What it can move is still bounded by the Roles allowance (`REQ-KY-3`).
6. **The validator set and chain maturity.** Finality rests on a BFT validator set of a young network. That is the same
   class of trust as Polygon's, with a shorter track record.
7. **Bridge.**
   - KYB of the operator is unconfirmed.
   - Accepting a Safe as source or destination is unconfirmed.
   - Settlement time is unconfirmed.
   - The OCC charter is conditional.
   - During step 5 the reserve in transit is Bridge's to deliver.
8. **The name.** bUSDC would no longer hold USDC.
9. **Tooling that reads native balances,** dashboards and alerts included, reads 4.24 × 10⁷⁵.
10. **Zodiac outside its registries.** Self-deployed mastercopies are trusted by code hash, but Zodiac's own tooling
    will not know them.

## Recommendations

1. **The protocol has no blocker.**
   - Tempo meets `REQ-RN-1` to `REQ-RN-6`.
   - `REQ-RN-5` is met more simply than on Polygon, by the token's memo.
   - Safe is canonical, and the vault's design holds.
   - What blocks are rulings (the amendments above) and Bridge onboarding. **Start Bridge KYB now.**
2. **Sign EIP-1559 only** (type 2). Let the OUSD fallback pay fees, keep the lane free of other fee tokens, and never
   sign `0x76`.
3. **Fix the endpoint's `0x76` parsing and nonce-key handling before anything else.** Without it no proof completes on
   Tempo.
4. **Deliver with `transferWithMemo`** in a single-function Roles scope, and drop MultiSend, the unwrapper and the
   emitter on Tempo.
5. **Port `DirectedDeposit`** rather than relying on memos. Spike TIP-1022 virtual addresses later, with Swaps, as the
   way to retire evmhot's sweeps.
6. **Rule on blocked payouts** (TIP-1028) before mainnet, and pre-check every payout's receive policy.
7. **Migrate as a paused cut-over in tranches,** through a Delay-queued, single-destination migration role on the
   Polygon vault, with the Bridge leg itemised in `InTransit`.
8. **Ask the owner** whether bUSDC keeps its name, and whether OUSD's burn power over the vault is acceptable. That is
   a trust change for holders, and the statement must say so (`REQ-BE-3`).
