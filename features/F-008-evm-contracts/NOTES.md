<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-008 — notes

Working notes for the EVM contracts in `contracts/evm/` (T-008-01 to T-008-06). Not normative: `SPEC.md` and
`PLAN.md` (`DEC-10`, `DEC-13`) decide; this file records what was verified and the choices made inside them.

## Pinned external addresses

Every address below was checked twice: against the project's own deployment list, and on the forks at the blocks
PR #19 pins (`e2e/evm/forks.env`: Polygon 95 400 000, Base 52 500 000, BNB Chain 127 300 000), where the runtime code
hash matched. `test/fork/Deploy.t.sol` (`test_DEC_10_pinnedAddressesCarryVerifiedCode`) re-checks the hashes on every
run.

| Contract | Address | Source | Runtime code hash (all three forks) |
|---|---|---|---|
| SafeL2 1.4.1 (singleton used) | `0x29fcB43b46531BcA003ddC8FCB67FFE91900C762` | safe-global/safe-deployments `src/assets/v1.4.1/safe_l2.json` (canonical on 137, 8453, 56) | `0xb1f92697…4f81ff` (= listed `codeHash`) |
| Safe 1.4.1 (L1 singleton, not used) | `0x41675C099F32341bf84BFc5382aF534df5C7461a` | `safe.json` | `0x1fe2df85…c5e1c4` (= listed) |
| SafeProxyFactory 1.4.1 | `0x4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67` | `safe_proxy_factory.json` | `0x50c3cdc4…0b3317` (= listed) |
| MultiSendCallOnly 1.4.1 (used) | `0x9641d764fc13c8B624c04430C7356C1C7C8102e2` | `multi_send_call_only.json` | `0xecd5bd14…6c9939` (= listed) |
| MultiSend 1.4.1 (not admitted) | `0x38869bf66a61cF6bDB996A6aE40D5853Fd43B526` | `multi_send.json` | `0x0e4f7fc6…0c4b77` (= listed) |
| CompatibilityFallbackHandler 1.4.1 | `0xfd0732Dc9E303f09fCEf3a7388Ad10A83459Ec99` | `compatibility_fallback_handler.json` | `0x7c6007a5…3c42f9` (= listed) |
| Zodiac Roles Modifier **2.1.1** mastercopy | `0xF2964CE6161ce0e75964Fe7927cE114cb0B283D5` | gnosisguild/zodiac `src/contracts.ts` (`ROLES["2.1.1"]`) and `mastercopies/roles/2.1.1/Roles/bytecode.json` | `0x471d8b3b…6c10b6` (= keccak of the artefact's runtime bytecode) |
| Zodiac ModuleProxyFactory 1.2.0 | `0x000000000000aDdB49795b0f9bA5BC298cDda236` | zodiac `src/contracts.ts` (`FACTORY["1.2.0"]`), `mastercopies/factory/1.2.0` | `0x01623cbc…c5f21e` (= artefact) |
| Uniswap v3 SwapRouter02, Polygon | `0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45` | Uniswap/docs `content/protocols/v3/deployments/v3-polygon-deployments.mdx`; `factory()` = `0x1F98431c…F984` (UniswapV3Factory, same page) | on fork |
| Uniswap v3 SwapRouter02, Base (not used) | `0x2626664c2603336E57B271c5C0b26F421741e481` | `v3-base-deployments.mdx`; `factory()` = `0x33128a8f…FDfD` | on fork |
| Aerodrome Slipstream SwapRouter, Base (used) | `0xBE6D8f0d05cC4be24d5167a3eF062215bE6D18a5` | aerodrome-finance/slipstream `README.md`, "Initial Deployment"; `factory()` = PoolFactory `0x5e7BB104…809A` (same table) | on fork |
| USDC, Polygon (native, Circle) | `0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359` | developers.circle.com/stablecoins/usdc-contract-addresses | `symbol()` USDC, 6 decimals on fork |
| USDT0, Polygon | `0xc2132D05D31c914a87C6611C10748AEb04B58e8F` | docs.usdt0.to/technical-documentation/deployments (Polygon PoS token; OFT `0x6BA10300…9e13`) | `symbol()` USDT0 on fork |
| USDC, Base (native, Circle) | `0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913` | Circle USDC addresses page | `symbol()` USDC on fork |
| EURC, Base (Circle) | `0x60a3E35Cc302bFA44Cb288Bc5a4F316Fdb1adb42` | developers.circle.com/stablecoins/eurc-contract-addresses | `symbol()` EURC, 6 decimals on fork |
| BTCB, BNB Chain | `0x7130d2A12B9BCbFAe4f2634d864A1Ee1Ce3Ead9c` | trustwallet/assets `blockchains/smartchain/assets/0x7130d2A1…/info.json` ("BNB pegged BTCB Token", tag `binance-peg`, issued by Binance, explorer link to BscScan); first taken from PR #19 (`e2e/evm/lib.mjs`). Binance's own list and BscScan could not be read automatically (client-rendered page; BscScan refuses scripted requests): confirm the BscScan "Binance-Peg" label by hand before `F-014` | `symbol()` BTCB, 18 decimals on fork |

Read on 2026-10-13. Sources: safe-deployments at `main`; zodiac at `89352c4` (2026-07-14); zodiac-modifier-roles
branch `v2` at `218a5164` (the "v2.1.1" release commit); Uniswap/docs and slipstream at `main`.

### Choices among them

- **Roles 2.1.1, not 2.1.0.** The zodiac registry lists `0x9646fDAD…D337` (2.1.0) under `FAULTY`. 2.1.1 fixes
  `ArraySome` (only the first element was evaluated), `Bitmask` on dynamic bytes (padding counted as data) and the
  `SignatureChecker`. The pinned submodule is the 2.1.1 source: its `Types.sol`, `adapters/Types.sol`, `Roles.sol`,
  `PermissionChecker.sol` and `Integrity.sol` are byte-identical to the verified sources in the mastercopy artefact.
  Only `Types.sol` and `adapters/Types.sol` are compiled here; the Roles code itself is the deployed mastercopy.
- **SafeL2, not Safe.** Same code apart from events on every execution, which makes the vault's executions
  observable from logs alone (useful to `F-009`). Both are 1.4.1 and verified.
- **MultiSendCallOnly only.** The delivery batch delegatecalls MultiSendCallOnly, whose entries cannot delegatecall.
  The general MultiSend is never a target of any role.
- **One Slipstream router.** Aerodrome has three Slipstream deployments, each router bound to its own pool factory.
  At the pinned block the deepest EURC/USDC pool is in the initial deployment (tick spacing 50: about 0.44M USDC and
  1.19M EURC), so its router is the declared venue (`REQ-VN-4`). Another router is another venue: governance adds it
  with a role change. Uniswap on Base is not configured: `DEC-13` names Aerodrome there.
- **Pinned pools (review of PR #28, should-fix 1).** On these routers the token pair and the fee tier or tick spacing
  name the pool, and the pool is where the vault's value goes. With that field left free, a conversion key could send
  a full window through a shallow pool: the review swapped 50 000 USDT0 through the 1 % USDT0/USDC pool on the Polygon
  fork and the vault received 0.13 USDC, the rest going to that pool's liquidity holders. That is value sent to a
  venue address nobody declared (`REQ-VN-4`). Each convertible in `config/<network>.json` now declares its pool (`fee`
  or `tickSpacing`, plus the pool's address for the record), and the role pins the field with `EqualTo`. At the
  pinned blocks the declared pools are the deepest of their pair (`test_REQ_VN_4_declaredPoolIsTheDeepest`, which
  checks every fee tier the factory enables and every tick spacing it lists):
  - Polygon USDT0→USDC: fee 100, pool `0x31083a78E11B18e450fd139F9ABEa98CD53181B7`, in-range liquidity 2.16e15
    (the 0.05 % pool has 1.09e13; the 1 % pool 1.3e6).
  - Base USDC→EURC: tick spacing 50, pool `0xE846373C1a92B167b4E9cd5d8E4d6B1Db9E90EC7`, in-range liquidity 2.11e14,
    deepest of tick spacings 1, 10, 50, 100, 200 and 2000.
  `REQ-CV-5` leaves price, slippage and fee to the requester; it still sets the minimum output and the price limit, and
  chooses when to convert. Which pool is a venue declaration, so moving to another pool is a role change by the
  owners, like adding a router. `test_REQ_VN_4_undeclaredPoolIsRefused` replays the review's scenario on each venue
  (a full window through every other pool, and through a parameter no pool has yet) and shows it refused.
- **Not used:** Roles' stock MultiSendUnwrapper (2.1.0 `0x93B7fCbc…27c0`, 2.1.1 `0xB4Cd4bb7…efD`; both addresses match
  CREATE2 of the published creation code). See "Why a custom unwrapper".

## The two roles

**Delivery** (bezent's signer key; `REQ-KY-3`, `REQ-RN-5`, `INV-14`, `AC-I1.1`). One call shape: a delegatecall to
MultiSendCallOnly whose batch is exactly `originToken.transfer(any, amount)` then `ReferenceEmitter.emitReference(ref)`,
both plain calls with no value. `amount` consumes the delivery allowance, which refills per period up to its maximum.
A bare `transfer`, a batch without the reference, any other token, value, or any other delegatecall is refused.

**Conversion** (Swaps' key; `REQ-CV-7`). Per admitted convertible: `approve(router, any)` on that token, and on the
router `exactInputSingle` with `tokenIn` that convertible, `tokenOut` the origin token, the fee tier (Uniswap) or tick
spacing (Slipstream) equal to the declared pool's (`REQ-VN-4`, see "Pinned pools"), `recipient` the vault
(`EqualToAvatar`), and `amountIn` within that convertible's own allowance. Deadline, minimum output and price limit
are Swaps' (`REQ-CV-5`). No other function, token, value or delegatecall. With several convertibles the condition is
an `Or` of one branch per token, each with its own pool.

Neither role can reach the Roles modifier, the Safe, or any other target, and the role keys are separate from each
other and from the owners (the script refuses otherwise). The approval in the conversion role is not bounded: it
moves nothing by itself, since a router pulls only from its caller and the vault calls the router only under the
bounded swap scope.

## Why a custom unwrapper

Roles v2 checks each call of an unwrapped batch on its own. With the stock MultiSendUnwrapper a batch holding only
the transfer would pass, and so would a direct `transfer`. `DEC-10` needs the pair. `ReferencedTransferUnwrapper`
(`src/`) admits exactly the 356-byte canonical `multiSend` of the two entries and returns both, so Roles still checks
the transfer against its scope and allowance. It is also registered at (origin token, `transfer`), so a bare
transfer under any role reaches it and is refused. Inside an unwrapped batch Roles does not look up unwrappers again,
so the paired transfer is unaffected. It never returns an empty array (Roles would treat that as authorised).

This is a small contract of ours in the authorisation path. It is the trade-off `DEC-10` states plainly: a role
configuration is code, and this is part of it. It needs review with the rest of the configuration before
mainnet (`F-014`).

## Deployment

`script/DeployVault.s.sol`, configured by `config/<network>.json` (network constants, allowances, venue,
convertibles and their pools) and by parameters (owners, threshold, delivery and conversion members, salt nonce). In
order:

1. Deploy `ReferenceEmitter`, `ReferencedTransferUnwrapper`, `VaultModuleSetup`.
2. Deploy the Roles proxy with the deployer as temporary owner. No Safe has it as a module.
3. Compute the Safe's address before it exists: CREATE2 by SafeProxyFactory 1.4.1 over the Safe's initializer
   (owners, threshold, and a delegatecall to `VaultModuleSetup` that enables the Roles proxy) and the salt.
4. Point Roles' avatar and target at that address, make every call of `DeployVault.permissionCalls` (scopes,
   allowances, unwrappers, members), and hand Roles' ownership to that address.
5. Create the Safe. It is born with the Roles modifier, already configured and already its own, as its only module.
   The script checks it landed at the computed address.

The deployer therefore never owns an enabled modifier (review of PR #28, should-fix 2;
`test_REQ_KY_4_deployerNeverOwnsAnEnabledModifier` checks the order of the handover and the enabling in the logs). A
run that stops midway leaves at most a modifier that no Safe has enabled, or one owned by a Safe that does not exist
yet and that only that initializer can create. The Safe never has the deployer as owner or module. The script refuses
a threshold below 2 (`REQ-KY-5`), a role key that is an owner or holds both roles (`REQ-KY-4`), a deployer that is an
owner or a role member, the origin token as a convertible (`REQ-CV-7`), and a convertible without a declared pool
(`REQ-VN-4`).

**What the deployer could still do** while it owned the modifier, between steps 2 and 4: add a member of its own, or
widen a role (for example `allowTarget(delivery, originToken, Both)`). Either survives the handover, and Roles keeps
its permissions in mappings that cannot be listed, so reading state does not show them. A one-shot factory contract
would make that impossible instead of detectable, at the cost of more of our own code in the deployment path; the
reorder plus the verifier was chosen because the verifier is needed anyway (it is also how anyone audits the vault
later).

**The verifier**, `script/VerifyVault.s.sol`, must pass before the vault is funded. It replays every event the Roles
modifier has emitted since its creation (`eth_getLogs` from the creation block, in ranges) and fails unless the
permission events are exactly the ones `permissionCalls` produces for the configuration, one for one (`AssignRoles`,
`EnabledModule`, `ScopeTarget`, `ScopeFunction` with its conditions, `AllowFunction`, `SetAllowance`,
`SetUnwrapAdapter`; any `AllowTarget`, `RevokeTarget`, `RevokeFunction`, `SetDefaultRole` or `DisabledModule` fails
it). An event the mastercopy does not emit fails it, and so does a log range that misses the modifier's setup event.
For the permission events the log data is the call's arguments byte for byte (no argument is indexed); the one
exception is `SetAllowance`, whose timestamp is the block's when the call passes 0. It also reads state: Roles is a
clone of the pinned mastercopy, owned by the Safe, with avatar and target the Safe, and its modules are exactly the
members; the Safe has the configured owners and threshold, the pinned singleton and fallback handler, no guard, and
the Roles modifier as its only module; the emitter and the unwrapper have the code this repository builds. It fails
closed: a permission granted and revoked again during deployment fails it too. After an owners' role change the
configuration has to change with it before the verifier passes again.

Fork tests: `test_REQ_KY_4_verifierAcceptsTheDeployment`, `test_REQ_KY_4_verifierDetectsAPlantedMember` and
`test_REQ_KY_4_verifierDetectsAWidenedRole` (a deployer that plants either before the handover), and
`test_REQ_KY_4_verifierRefusesPartialHistory`. The script itself (fetching logs over RPC) was run against an anvil
fork of Polygon at the pinned block: it passed on a fresh deployment and failed after a role was widened.

**Salt nonce.** The default is `keccak256("bezent.vault", network, chainId, deployer, deployer's nonce)`, so it is
derived per network and per deployment: a second run with the same parameters no longer collides with the first.
`VAULT_SALT_NONCE` still overrides it.

BNB Chain (`config/bsc.json`) gets the vault and the delivery role only: `DEC-13` names no venue there.

## How `REQ-GV-3` applies

Only the Safe owns the Roles modifier, so every role change, allowance change, new venue or unwrapper is a Safe
transaction at the owners' threshold (`test_REQ_GV_3_onlyOwnersAtThresholdChangeRoles`). Nothing upgrades the vault's
code otherwise: the Roles proxy is a minimal clone without an upgrade path, and the Safe's singleton changes only by
an owners' transaction.

What this does **not** give is a delay. A Safe transaction with enough signatures executes at once, so the delay
`REQ-GV-3` requires is, today, procedural: the owners publish the transaction hash and wait before signing. Enforcing
it on chain would mean putting a timelock (for example a Zodiac Delay modifier) between the owners and the Roles
modifier. Neither the plan nor a ruling says which, so it is not built here; see the report's spec gaps.

## Review of PR #28: notes and what stays open

Should-fix 1 and 2 are done (see "Pinned pools" and "Deployment"). The rest:

- **Should-fix 3, the owners can move reserve directly** (`Safe.execTransaction` at threshold, outside Roles; guardrail
  6, `REQ-GV-5`, `INV-14`, `REQ-GV-3`). Not implemented: it waits on @pandres95's ruling (a Safe guard limiting owner
  transactions to Roles administration and a surplus-withdrawal shape, a Delay or timelock, or an explicit statement
  that the owners are trusted). It belongs on the spec-gaps list with the missing `REQ-GV-3` delay below.
- **Self-transfers, zero transfers and bare emits still emit a reference.** The delivery recipient is `Pass` by
  design, so `deliver(vault, x, ref)` and `deliver(any, 0, ref)` pass and emit `Reference` (the self-transfer also
  uses allowance), and a bare `emitReference` from the vault passes too. **`F-009` must match each `Reference` to the
  vault's own `Transfer` of the origin token in the same transaction and the same batch, with recipient and amount
  taken from that transfer, and never accept a reference by itself.** A transfer to the vault itself, a zero
  transfer, or a reference with no transfer is not a delivery.
- **Approvals outlive the router's admission.** The unbounded `approve` to a router stays if the router is later
  removed as a venue, so the owners' change that removes a router (or a convertible) must also set that approval to 0.
- **BNB Chain allowance.** 1 BTCB a day is several times the 50 000 cap on the stablecoins. It is a placeholder;
  confirm it with `F-016`'s figures before funding.
- **CI actions** are pinned by commit SHA in `.github/workflows/evm-contracts.yml`, the release named in a comment
  (checkout v4.4.0, foundry-toolchain v1.9.1, cache v4.3.0). `ci.yml` and `contracts.yml` belong to other features and
  still pin by tag.
- **Salt nonces** are now derived per network and deployment (see "Deployment").

## Placeholders

- **Allowance figures** in `config/*.json` (50 000 USDC or EURC a day, 1 BTCB a day; 50 000 of each convertible a
  day) are test values. `DEC-17` takes the real figures from `F-016`'s measurements.
- **Admitted convertibles** (USDT0 for bUSDC on Polygon, USDC for bEURC on Base) are proposals: `REQ-AS-7` admits a
  convertible by a recorded declaration, and none is recorded yet. The plan names USDT on Polygon as an example
  (`DEC-9`); nothing names a convertible for bEURC.
- **Owners and members** are parameters only; the fork tests use Foundry's default test keys (2 of 3 owners).

## Smaller choices

- `emit` is a Solidity keyword, so the emitter's function is `emitReference(bytes32)`. The event is
  `Reference(bytes32 indexed ref)`: the signature `DEC-10` names, with the reference as a topic for filtering.
- Role keys are `bytes32("bezent.delivery")` and `bytes32("bezent.conversion")`. Allowance keys are
  `keccak256("bezent.delivery.allowance" ‖ token)` and `keccak256("bezent.conversion.allowance" ‖ token)`.
- Licence `Apache-2.0 OR MIT`, as `contracts/bezent`.

Removed when F-008 merges and its decisions are folded into the plan, or kept as the feature's record.
