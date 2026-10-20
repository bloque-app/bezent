<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-008 — notes

Working notes for the EVM contracts in `contracts/evm/` (T-008-01 to T-008-08). Not normative: `SPEC.md` and
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
| CompatibilityFallbackHandler 1.4.1 (not used: the vault has no fallback handler) | `0xfd0732Dc9E303f09fCEf3a7388Ad10A83459Ec99` | `compatibility_fallback_handler.json` | `0x7c6007a5…3c42f9` (= listed) |
| Zodiac Roles Modifier **2.1.1** mastercopy | `0xF2964CE6161ce0e75964Fe7927cE114cb0B283D5` | gnosisguild/zodiac `src/contracts.ts` (`ROLES["2.1.1"]`) and `mastercopies/roles/2.1.1/Roles/bytecode.json` | `0x471d8b3b…6c10b6` (= keccak of the artefact's runtime bytecode) |
| Zodiac Delay Modifier **1.1.1** mastercopy | `0x824175b945838d127C1ca83CbcE11D8e44F6dF01` | gnosisguild/zodiac `src/contracts.ts` (`DELAY[\"1.1.1\"]`) and `mastercopies/delay/1.1.1/Delay/` (`sourcecode.json`, `bytecode.json`) | `0xdd928edd…c577b8` (= keccak of the artefact's runtime bytecode; the code on all three forks equals it byte for byte) |
| Zodiac ModuleProxyFactory 1.2.0 | `0x000000000000aDdB49795b0f9bA5BC298cDda236` | zodiac `src/contracts.ts` (`FACTORY["1.2.0"]`), `mastercopies/factory/1.2.0` | `0x01623cbc…c5f21e` (= artefact) |
| Uniswap v3 SwapRouter02, Polygon | `0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45` | Uniswap/docs `content/protocols/v3/deployments/v3-polygon-deployments.mdx`; `factory()` = `0x1F98431c…F984` (UniswapV3Factory, same page) | on fork |
| USDC, Polygon (native, Circle) | `0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359` | developers.circle.com/stablecoins/usdc-contract-addresses | `symbol()` USDC, 6 decimals on fork |
| USDT0, Polygon | `0xc2132D05D31c914a87C6611C10748AEb04B58e8F` | docs.usdt0.to/technical-documentation/deployments (Polygon PoS token; OFT `0x6BA10300…9e13`) | `symbol()` USDT0 on fork |
| EURC, Base (Circle) | `0x60a3E35Cc302bFA44Cb288Bc5a4F316Fdb1adb42` | developers.circle.com/stablecoins/eurc-contract-addresses | `symbol()` EURC, 6 decimals on fork |
| BTCB, BNB Chain | `0x7130d2A12B9BCbFAe4f2634d864A1Ee1Ce3Ead9c` | trustwallet/assets `blockchains/smartchain/assets/0x7130d2A1…/info.json` ("BNB pegged BTCB Token", tag `binance-peg`, issued by Binance, explorer link to BscScan); first taken from PR #19 (`e2e/evm/lib.mjs`). Binance's own list and BscScan could not be read automatically (client-rendered page; BscScan refuses scripted requests): confirm the BscScan "Binance-Peg" label by hand before `F-014` | `symbol()` BTCB, 18 decimals on fork |

Read on 2026-10-13. Sources: safe-deployments at `main`; zodiac at `89352c4` (2026-07-14); zodiac-modifier-roles
branch `v2` at `218a5164` (the "v2.1.1" release commit); Uniswap/docs at `main`. The Delay mastercopy was read on
2026-10-20 from the zodiac registry and artefacts at `master`.

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
- **Delay 1.1.1, not 1.1.0.** The zodiac registry lists `0x01F8…339b` (1.1.0) under `FAULTY`; 1.1.1 is the release
  after it. Its source (`contracts/Delay.sol` in the artefact's `sourcecode.json`) was read in full. `executeNextTx` is
  callable by anyone; the cooldown counts from the moment of queueing, and the expiration from the end of the cooldown
  (`queued + cooldown + expiration >= now` still runs); the queue is first in, first out; `setTxNonce` skips queued
  transactions; and `moduleOnly` also accepts a module's EIP-712 or EIP-1271 signature appended to the calldata (the
  reason the Safe has no fallback handler, see "The guard and the Delay"). Only the interface is compiled here: the
  code is the deployed mastercopy.
- **Uniswap v3 only, on Polygon only.** `DEC-13` admits USDT0 for bUSDC on Polygon and nothing else (`pl-convertibles`,
  ruled 2026-10-13, applied by issue #22). The Base vault, like BNB Chain's, has the delivery role only: its
  conversion role (EURC through Aerodrome Slipstream), its config and its tests are gone, and with them the venue
  type, the layout and the router interfaces for Slipstream.
- **Pinned pools (review of PR #28, should-fix 1).** On this router the token pair and the fee tier
  name the pool, and the pool is where the vault's value goes. With that field left free, a conversion key could send
  a full window through a shallow pool: the review swapped 50 000 USDT0 through the 1 % USDT0/USDC pool on the Polygon
  fork and the vault received 0.13 USDC, the rest going to that pool's liquidity holders. That is value sent to a
  venue address nobody declared (`REQ-VN-4`). Each convertible in `config/<network>.json` now declares its pool (`fee`,
  plus the pool's address for the record), and the role pins the field with `EqualTo`. At the pinned block the
  declared pool is the deepest of its pair (`test_REQ_VN_4_declaredPoolIsTheDeepest`, which checks every fee tier the
  factory enables): Polygon USDT0→USDC, fee 100, pool `0x31083a78E11B18e450fd139F9ABEa98CD53181B7`, in-range
  liquidity 2.16e15 (the 0.05 % pool has 1.09e13; the 1 % pool 1.3e6).
  `REQ-CV-5` leaves price, slippage and fee to the requester; it still sets the minimum output and the price limit, and
  chooses when to convert. Which pool is a venue declaration, so moving to another pool is a role change by the
  owners, like adding a router. `test_REQ_VN_4_undeclaredPoolIsRefused` replays the review's scenario (a full window
  through every other pool, and through a parameter no pool has yet) and shows it refused.
- **Not used:** Roles' stock MultiSendUnwrapper (2.1.0 `0x93B7fCbc…27c0`, 2.1.1 `0xB4Cd4bb7…efD`; both addresses match
  CREATE2 of the published creation code). See "Why a custom unwrapper".

## The two roles

**Delivery** (bezent's signer key; `REQ-KY-3`, `REQ-RN-5`, `INV-14`, `AC-I1.1`). One call shape: a delegatecall to
MultiSendCallOnly whose batch is exactly `originToken.transfer(any, amount)` then `ReferenceEmitter.emitReference(ref)`,
both plain calls with no value. `amount` consumes the delivery allowance, which refills per period up to its maximum.
A bare `transfer`, a batch without the reference, any other token, value, or any other delegatecall is refused.

**Conversion** (Swaps' key; `REQ-CV-7`; Polygon only, `DEC-13`). Per admitted convertible: `approve(router, any)` on
that token, and on the router `exactInputSingle` with `tokenIn` that convertible, `tokenOut` the origin token, the fee
tier equal to the declared pool's (`REQ-VN-4`, see "Pinned pools"), `recipient` the vault
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
convertibles and their pools, pinned addresses) and by parameters: owners, threshold, delivery and conversion members,
the guardian, the Delay's cooldown and expiration, and a salt nonce. `config/vault.example.env` lists every parameter with the
recommended values. In order:

1. Deploy `ReferenceEmitter`, `ReferencedTransferUnwrapper`, `VaultModuleSetup`.
2. Deploy the Roles proxy, the Delay proxy and the guardian's Roles proxy (clones of the pinned mastercopies, by the
   module proxy factory; the guardian's has a salt of its own), with the deployer as temporary owner of each, and
   deploy `VaultGuard` for the three. No Safe has any of them yet.
3. Compute the Safe's address before it exists: CREATE2 by SafeProxyFactory 1.4.1 over the Safe's initializer
   (owners, threshold, no fallback handler, and a delegatecall to `VaultModuleSetup` that enables the three proxies as
   modules and sets the guard) and the salt.
4. Point Roles' avatar and target, and the Delay's, at that address; make every call of
`DeployVault.permissionCalls` (scopes, allowances, unwrappers, members) and of `DeployVault.guardianCalls` (the
   guardian's scope, below); enable the Safe as the Delay's only module; hand the three modifiers' ownership to that
   address.
5. Create the Safe. It is born with the three modifiers, already configured and already its own, as its only modules,
   and with the guard set. The script checks it landed at the computed address.

The deployer therefore never owns an enabled modifier (review of PR #28, should-fix 2;
`test_REQ_KY_4_deployerNeverOwnsAnEnabledModifier` checks the order of the handovers and the enabling in the logs). A
run that stops midway leaves at most modifiers that no Safe has enabled, or owned by a Safe that does not exist
yet and that only that initializer can create. The Safe never has the deployer as owner or module. The script refuses
a threshold below 2 (`REQ-KY-5`), a role key that is an owner or holds both roles (`REQ-KY-4`), a deployer that is an
owner or a role member, the origin token as a convertible (`REQ-CV-7`), a convertible without a declared pool
(`REQ-VN-4`), a guardian that is missing or is the deployer, an owner or a role member (`REQ-KY-4`), and a cooldown
or an expiration outside the bounds of "Cooldown and expiration" (`REQ-GV-3`).

**A Safe created early.** The Safe's address follows from its initializer and the salt, which anyone can compute as
soon as the modifiers exist, and `createProxyWithNonce` is open to everyone. Someone can therefore create the Safe
at its address while the deployer is still configuring the modifiers. It is the same Safe (the address commits to the
initializer, so its owners, modules and guard are the configured ones), but it means the modifiers can be live while
the deployer still owns them, which the order of steps is meant to prevent. The script refuses to go on if a Safe
already exists at the address, when it computes the address and again just before it creates the Safe
(`test_REQ_KY_4_scriptRefusesAPreCreatedSafe`), and the run is abandoned and started again with a new salt. Those checks
run while forge simulates the script; a Safe created between the simulation and the broadcast is stopped only by the
CREATE2 collision when the script creates it, and then by the verifier. The
configuration cannot be made atomic with the Safe's creation without a one-shot factory contract (see below), so
what proves that nothing else happened in that window is the verifier's replay, which a vault must pass before it is
funded. Treat a vault whose verifier has not passed as not deployed.

**What the deployer could still do** while it owned the modifiers, between steps 2 and 4. On Roles: add a member of
its own, or widen a role (for example `allowTarget(delivery, originToken, Both)`); either survives the handover, and
Roles keeps its permissions in mappings that cannot be listed, so reading state does not show them. On the Delay:
enable a module of its own, or queue a transaction (enabling itself, queueing, disabling itself again leaves the
module list clean). A one-shot factory contract would make all of that impossible instead of detectable, at the cost
of more of our own code in the deployment path; the reorder plus the verifier was chosen because the verifier is
needed anyway (it is also how anyone audits the vault later).

**The verifier**, `script/VerifyVault.s.sol`, must pass before the vault is funded. For Roles it replays every event
the modifier has emitted since its creation (`eth_getLogs` from the creation block, in ranges) and fails unless the
permission events are exactly the ones `permissionCalls` produces for the configuration, one for one (`AssignRoles`,
`EnabledModule`, `ScopeTarget`, `ScopeFunction` with its conditions, `AllowFunction`, `SetAllowance`,
`SetUnwrapAdapter`; any `AllowTarget`, `RevokeTarget`, `RevokeFunction`, `SetDefaultRole` or `DisabledModule` fails
it). An event the mastercopy does not emit fails it, and so does a log range that misses the modifier's setup event.
For the permission events the log data is the call's arguments byte for byte (no argument is indexed); the one
exception is `SetAllowance`, whose timestamp is the block's when the call passes 0. It also reads state: Roles is a
clone of the pinned mastercopy, owned by the Safe, with avatar and target the Safe, and its modules are exactly the
members. **The guardian's Roles modifier** is replayed the same way against `guardianCalls` (so its scope is exactly
`setTxNonce` on the Delay and `disableModule(any, the delivery Roles modifier)` on the Safe, and its one member the
guardian; widening its scope or adding a member fails the check), is a clone of the same mastercopy, owned by the Safe,
with avatar and target the Safe, and is enabled as a Safe module (`VAULT_GUARDIAN_ROLES` names it). **Key separation** is asserted too: the guardian is set and differs from every Safe owner (the configured ones and the ones read from the Safe), from the delivery key and from the conversion key, and the verifier fails otherwise (`test_REQ_GV_2_verifierChecksTheGuardianIsAKeyOfItsOwn`). **The Delay** (T-008-07) has no permission mapping to replay, so state is enough: a clone of the pinned
Delay mastercopy, owned by the Safe, avatar and target the Safe, the Safe its only module, the configured cooldown and
expiration, and an empty queue (`queueNonce` 0, which is what catches a transaction queued by the deployer). **The
Safe** has the configured owners and threshold, the pinned singleton, the guard slot holding `d.guard`, no fallback
handler, an empty module-guard slot, and exactly the three modules. (Safe 1.4.1, which this vault pins, never reads the
module-guard slot: module guards arrived in 1.5.0. The check keeps a future Safe from being deployed with one, which
would stop every module transaction, the guardian's included.) The cooldown and expiration must lie in the bounds. The
emitter, the unwrapper and the guard have the code this repository builds (the guard's three immutables are part of
its code). It fails closed: a permission granted and revoked again during
deployment fails it too. After an owners' role change the configuration has to change with it before the verifier
passes again, and a vault whose Delay has queued anything is no longer in the state the verifier checks: it is for
the vault's first state, before funding.

Fork tests: `test_REQ_KY_4_verifierAcceptsTheDeployment`, `test_REQ_KY_4_verifierDetectsAPlantedMember` and
`test_REQ_KY_4_verifierDetectsAWidenedRole` (a deployer that plants either before the handover),
`test_REQ_KY_4_verifierRefusesPartialHistory`, `test_REQ_GV_3_verifierDetectsAPlantedDelayModule`,
`test_REQ_GV_3_verifierDetectsAQueuedTransaction`, `test_REQ_GV_5_verifierChecksDelayGuardModulesAndHandler`,
`test_REQ_GV_2_verifierDetectsAPlantedGuardianMember`, `test_REQ_GV_2_verifierDetectsAWidenedGuardianScope` and
`test_REQ_GV_2_verifierChecksTheGuardianModule`. The
scripts themselves (the deployment with `--broadcast`, the verifier fetching logs over RPC) were run against a local
anvil fork of Polygon at the pinned block: the verifier passed on a fresh deployment (T-008-03, again with the guard
and the Delay for T-008-07) and, in T-008-03, failed after a role was widened. That run predates the guardian: the
guardian's `VAULT_GUARDIAN_ROLES` input and second log fetch of `run()` are covered by `check`, which the tests call
with recorded logs, and not yet by a script run against an anvil fork. Nothing was sent to a real network.

**Salt nonce.** The default is `keccak256("bezent.vault", network, chainId, deployer, deployer's nonce)`, so it is
derived per network and per deployment: a second run with the same parameters no longer collides with the first.
`VAULT_SALT_NONCE` still overrides it.

Base (`config/base.json`) and BNB Chain (`config/bsc.json`) get the vault, the guard, the Delay and the delivery role
only: `DEC-13` names no venue on either.

## The guard and the Delay

`REQ-GV-3`, `REQ-GV-5`, `REQ-KY-3`; `DEC-10` (`pl-vault-delay`, ruled 2026-10-13); T-008-07. Three pieces, all owned by
the Safe: `VaultGuard` (`src/VaultGuard.sol`, the Safe's guard), a Zodiac Delay modifier (a clone of the pinned 1.1.1
mastercopy, a module of the Safe, with the Safe as its only module), and the Roles modifier. A fourth piece, the
guardian's Roles modifier ("The guardian"), is the independent defender. Owners no longer act on the vault directly
except to pause it; everything that widens what the vault can do waits out the Delay's cooldown.

**The guard is an allowlist.** Safe 1.4.1 runs a guard on `execTransaction` only, never on a module's transaction.
So the guard decides what the owners may do at once and what they may queue, and a queued call, once it runs, is
a module transaction that no guard sees. Every owner transaction must be a plain call (no delegatecall) with no value
and no gas refund. A refund (`gasPrice` above 0) makes the Safe itself pay out of any token it holds, which is a
transfer by another name (`test_REQ_GV_5_ownersCannotMoveTheVaultsTokens` shows it moving tokens with the guard
removed, and refused with it). Then, by target:

| Direct owner transaction to | At once (narrowing, a pause) | Only through the Delay (widening) |
|---|---|---|
| Roles (the delivery one) | revoke a member (`assignRoles` with every flag false, for a module already enabled), `disableModule`, `revokeTarget`, `revokeFunction`, lower an allowance | add a member, scope or allow a target or function, set an unwrapper or default role, enable a module, raise an allowance, change owner or avatar |
| Guardian's Roles | nothing, not even a narrowing call | any change (rotate, rescope, remove): queued only |
| Safe | `disableModule(prev, roles)`, which stops every outflow | enable a module, change owners or threshold, set the guard or the fallback handler, remove the guardian (never: the Delay) |
| Delay | queue a call (`execTransactionFromModule`), skip queued calls (`setTxNonce`, the veto) | keep or raise the cooldown, change the expiration, within the bounds (lower the cooldown, or touch the rest of the Delay's configuration: never) |
| anything else (a token, a router, MultiSend) | nothing | nothing: it cannot even be queued |

**What can be queued** is a plain call, with no value, to the Delay, the two Roles modifiers or the Safe itself. A call
to the Delay may be only `setTxCooldown` or `setTxExpiration`, within the bounds below. The review of PR #41 (finding
2) showed why the rest of the Delay's configuration is not queueable: a queued `Delay.setAvatar` makes the guard
refuse every owner transaction from then on (it checks that the Delay's avatar is the Safe), pause and veto included,
with no recovery, and `setTarget`, `transferOwnership` and `Delay.disableModule(prev, safe)` break governance the
same way. A queued `Delay.enableModule(x)` makes `x` a module that queues without the guard seeing it, and a queued
queue call (`test_REQ_GV_5_theQueueCannotQueueAgain`) would run as the Safe, a module of the Delay, and enter the queue
unseen. The simplest fix that the tests prove (`test_REQ_GV_3_queuedCallsThatLockTheOwnersOutAreRefused`) is the
refusal at queueing time, rather than a recovery path a guardian could abuse. For the same reason a queued
`Safe.disableModule(prev, delay)` is refused: with the Delay gone nothing can be queued, so a paused vault could never
be unpaused. The guard compares the module as an address, not as a word: Safe 1.4.1 is compiled with solc 0.7.6 (ABI
coder v1), which masks the high bits of an address argument instead of rejecting them, so a word with dirty high bits
still names the Delay to the Safe (`test_REQ_GV_3_removingTheDelayWithDirtyAddressBitsIsRefused` shows the Safe
disabling the Delay on such a call, with the guard off). The cost is that the Delay can never be replaced or removed (a new Delay needs a new vault), which is the
safe direction. A call to a
token, a router or MultiSend, and any delegatecall, is refused when the owners try to queue it. So the Delay is not a
way to move the vault's tokens: moving them takes a widening of the vault's configuration first (a role, a module, the
guard), which sits in the queue in public for a whole cooldown. That configuration is what the cooldown is for.

**A queued cooldown may keep or raise the review window, never shorten it** (re-review of PR #41, finding A). The
Delay checks the cooldown that is current when an entry runs, not the one when it was queued. If the queue accepted a
lower cooldown, a quorum could queue `setTxCooldown(1 h)` (it looks harmless) and, 23 hours later, a hostile entry:
when the first runs at 24 hours the second runs right after it, and every later change has a 1-hour window. So the
guard reads `Delay.txCooldown()` when a `setTxCooldown` is queued and refuses a value below it (the 1-hour floor and
the 30-day cap still apply); entries queued before keep their original readiness
(`test_REQ_GV_3_aQueuedCooldownCannotBeLowered`, `test_REQ_GV_3_noQueuedEntryBecomesReadySoonerThanItsOwnCooldown`). Lowering the cooldown is therefore not a queue
operation at all. It takes a deliberate recovery path: redeploy the vault with the new cooldown and move the
reserve, which is what a vault whose review window must change is. This is the same stance as for the rest of the
Delay's configuration (it can never be replaced or removed): no path an honest quorum can use to weaken the window is
one a compromised quorum cannot use.

**The queue-time checks are a guard rail, not the boundary.** The guard sees a queued call's target and selector, not
what a wrapped call will do when it runs. `Safe.execTransactionFromModule(...)` or a nested `Safe.execTransaction(...)`
queued to the Safe itself passes the queue check, and so can `enableModule(safe)` followed by a module transaction that
moves tokens, in one window. The refusals above remove the foot-guns (the lock-outs and the unseen queue); what stops a
wrapped call that is hostile is the guardian's veto, within the cooldown. A guardian's monitoring therefore has to
decode nested payloads, not only the outer selector (`test_REQ_GV_3_wrappedCallsStillQueueAndTheVetoStopsThem`).

**Lowering an allowance** is monotone in every field, so the new allowance can never release more than the old one
would have, at any later time: the balance is at most the stored balance, the cap (`maxRefill`) is at most the old
cap and is not 0 (Roles reads 0 as "no cap"), a refill is no larger and no less frequent than the old one (an
allowance that does not refill now may not start to), and the refill clock starts now (a `timestamp` of 0; an old
one would refill at once). Comparing rates instead (`refill / period`) is not enough: refills arrive in whole
periods, so a lower average rate can still release more at one instant. Unknown allowance keys cannot be set.

**Canonical calldata.** The guard decodes each admitted call, encodes it again and compares: the target and the guard
then read the same arguments (`test_REQ_GV_5_nonCanonicalCalldataIsRefused`). It keeps no state, has no owner, and
answers only the Safe that is the Delay's avatar.

**Why the Safe has no fallback handler.** Two paths would otherwise let the owners' signatures stand in for a Safe
transaction, with no `execTransaction` and so no guard. (1) A token that accepts EIP-1271 signatures (a permit or an
authorised transfer) would let the owners, at the threshold, sign a message that anyone submits to move the vault's
tokens. (2) Delay 1.1.1 accepts a module's signature appended to the calldata of a queue call; the Safe is the
Delay's module, and with `CompatibilityFallbackHandler` it answers EIP-1271 for its owners, so anyone could queue
a token transfer that the guard's queue check never saw. Both go through the Safe's `isValidSignature`, which the
Safe answers only through a fallback handler. The vault therefore has none (`fallbackHandler` of `setup` is 0, the
verifier checks the slot), and `test_REQ_GV_5_theSafeSignsNothingOffChain` and `test_REQ_GV_3_signedQueueCallIsRefused`
show each path open with the handler and closed without it. Setting a handler later is a Safe change, so it
is delayed. The cost: the Safe cannot receive ERC-721 or ERC-1155 tokens, which a reserve vault does not hold.

**The guardian** (review of PR #41, finding 1; `REQ-GV-2`, `REQ-KY-4`). Without it the veto and the pause belong to
the same quorum that attacks: a compromised quorum queues `setGuard(0)`, waits a cooldown, and nobody else can skip it
or pause the vault. The guardian is one key, supplied as the `GUARDIAN` parameter, that is neither an owner nor the
delivery or conversion key (the script refuses the same address; whether the custody is different is for whoever picks
it). It is a member of a role in a *second* Roles modifier, enabled as a Safe module and owned by the Safe, scoped to
exactly two things (`DeployVault.guardianCalls`, `VaultRoles.pauseConditions`): `Delay.setTxNonce` (veto a queued
change) and `Safe.disableModule(prev, rolesMain)` with `module` equal to the delivery Roles modifier (pause). It has no
other target or function, no value, no delegatecall and no unwrapper, so it cannot move value, queue, widen, unpause,
or change a Roles modifier (`test_REQ_GV_2_guardianCannotMoveValueOrWiden`). Pausing by the guardian alone satisfies
`REQ-GV-2` ("one party suffices"); unpausing stays the owners' quorum plus the delay.

- *Why a second instance.* The guardian's role is not in the delivery Roles modifier: the owners can revoke a member
  there at once, and pausing disables the module that carries it. A second instance is not in the guard's set of
  immediate changes, so the owners reach it only through the queue, where the guardian can veto the entry
  (`test_REQ_GV_2_ownersCannotRemoveTheGuardianAtOnce`, `test_REQ_GV_2_removingTheGuardianIsDelayedAndVetoable`).
  Rotating the guardian, rescoping it and removing it are delayed changes, which the guard classifies as widening
  (`test_REQ_GV_2_rotatingAndRescopingTheGuardianIsDelayed`). A direct owner transaction to the guardian's Roles
  modifier is refused whatever it says.
- *Cost, stated plainly.* The guardian can veto everything, including its own rotation and every unpause, and the
  owners have no path out: a paused vault stays paused as long as the guardian keeps pausing or vetoing. It is a
  defender with a veto, not a co-owner, and the price is that a hostile or lost guardian freezes governance (never
  value). Make it a hardware key or a 2-of-N account, and write down how it is rotated, before the vault is funded.
- *A lost guardian.* A lost or unreachable guardian vetoes nothing, so queued changes simply run after the cooldown,
  and it pauses nothing, so only the owners can stop deliveries (by lowering the allowance, which is immediate). If
  the vault was paused by the guardian before it was lost, the pause cannot be lifted by anyone but the owners:
  re-enabling the delivery Roles modifier takes the owners' quorum plus the Delay's cooldown, as for any widening,
  and nothing else, because the guardian never had that power. Replacing a lost guardian is a rotation, which is also
  a queued change. Hold the guardian by a party independent of the owners and of the delivery key, as a 2-of-N
  account or a hardware key where possible, so that a lost guardian and a hostile one are both unlikely.
- *Still open.* A queued `Safe.setGuard(x)` with an `x` that refuses everything is a lock-out the guard does not
  classify away (removing the guard is the point of that call being queueable); it is stopped by the veto.

**Cooldown and expiration.** 24 hours and 7 days are the recommended values (`config/vault.example.env`, which the
fork tests deploy with): the brief for this task set them. `PLAN.md` `DEC-10` says only "its cooldown" and gives no
figure, and no ruling does. They are parameters (`VAULT_DELAY_COOLDOWN`, `VAULT_DELAY_EXPIRATION`, seconds), not
constants. Expiration counts from the end of the cooldown; a queued change nobody ran in that window is dead and has to
be queued again, so a stale change cannot be run months later. Both are bounded, at deployment and whenever a change of
them is queued (the same constants in `src/VaultGuard.sol`): a cooldown from 1 hour to 30 days, an expiration from 1
day to 90 days. Below the floor the delay is not one; above the cap a queued change would sit out of reach of a
pause or a veto for longer than anyone watches; an expiration of 0 would make queued changes never expire (review of
PR #41, finding 3). The bounds are this change's choice, not a ruling.

**The queue** is first in, first out. A queued change nobody wants blocks the ones behind it until it is run, skipped
by the owners (`setTxNonce`, at once and at the threshold, the veto) or expired (`skipExpired`, by anyone).

**What this does not do, stated plainly.**

- *The residual of `DEC-10`.* After the cooldown, a queued `setGuard(0)`, an `enableModule(x)` on the Safe or on the
  Delay (a Delay module queues without passing the guard), or an `allowTarget` on the origin token runs, and the owners
  are then no longer limited by the guard, or a key can move tokens the guard would have refused. The guard removes every path that moves value at once; it does not make the owners unable ever
  to change the vault. What the owners cannot do is do it unseen: the change sits in the queue for the whole
  cooldown, in public, and can be vetoed. `test_REQ_GV_3_removingTheGuardIsDelayed` ends by showing the owners
  moving tokens after the guard is gone. The monitoring that turns the cooldown into a veto is not built here.
- *Pausing by the owners needs the threshold.* Pausing the vault (lowering an allowance, revoking a member, disabling
  the Roles module) is immediate but is still a Safe transaction at the threshold. The guardian is the one party who
  can pause alone. Unpausing is delayed, which is the intended direction.
- *Disabling the Roles module on the Safe* is allowed at once (a full stop). Re-enabling it is a delayed change.
- *The Delay cannot be removed.* A queued `Safe.disableModule(prev, delay)` is refused (see "What can be queued").
  Removing the guard is a delayed change; the Delay stays.

Tests: `test/fork/Governance.t.sol` and `test/fork/Guardian.t.sol` on Polygon, Base and BNB Chain, and
`test/fork/Adversarial.t.sol` (the review's scenarios and a fuzz run on allowance lowering) on Polygon. The Delay modifier itself (cooldown, expiry, order,
anyone may run) is exercised as deployed; nothing of it is mocked.

## Review of PR #28: notes and what stays open

Should-fix 1 and 2 are done (see "Pinned pools" and "Deployment"). The rest:

- **Should-fix 3, the owners can move reserve directly** (`Safe.execTransaction` at threshold, outside Roles; guardrail
  6, `REQ-GV-5`, `INV-14`, `REQ-GV-3`). Done in T-008-07 on @pandres95's ruling (`pl-vault-delay`): the guard and the
  Delay of "The guard and the Delay". A surplus-withdrawal shape was not part of the ruling and is not built.
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
- **Admitted convertible** (USDT0 for bUSDC on Polygon, the only one: `DEC-13`, `pl-convertibles`) is a proposal until
  `REQ-AS-7`'s recorded declaration exists; none is recorded yet.
- **Delay cooldown and expiration** (24 hours, 7 days) are recommended values, not a ruling: see "The guard and the
  Delay". Governance sets the real ones at deployment.
- **Owners and members** are parameters only; the fork tests use Foundry's default test keys (2 of 3 owners).

## Smaller choices

- `emit` is a Solidity keyword, so the emitter's function is `emitReference(bytes32)`. The event is
  `Reference(bytes32 indexed ref)`: the signature `DEC-10` names, with the reference as a topic for filtering.
- Role keys are `bytes32("bezent.delivery")` and `bytes32("bezent.conversion")`. Allowance keys are
  `keccak256("bezent.delivery.allowance" ‖ token)` and `keccak256("bezent.conversion.allowance" ‖ token)`.
- Licence `Apache-2.0 OR MIT`, as `contracts/bezent`.

Removed when F-008 merges and its decisions are folded into the plan, or kept as the feature's record.
