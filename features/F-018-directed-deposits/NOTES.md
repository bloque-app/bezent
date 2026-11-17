<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-018 — notes

**Status:** research for amendment 0029, accepted 2026-11-17 and applied in `SPEC.md` v0.5.13.

The facts this design rests on, as checked on 2026-11-08, and the choices made among standards and wallets. Not
normative: amendment 0029 proposes, `SPEC.md` and `PLAN.md` decide. Removed when F-018 is built and its decisions are
folded into the plan, or kept as the feature's record.

## Native USDC on Polygon supports permit

Read from two public providers (`polygon.drpc.org`, `1rpc.io/matic`), which agreed, at Polygon's finalized head of
about block 96 983 980:

| Read | Value |
|---|---|
| Token (proxy) | `0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359` |
| Implementation (EIP-1967 slot) | `0x235ae97b28466db30469b89a9fe4cff0659f82cb` |
| `name()` | `"USD Coin"` |
| `version()` | `"2"` |
| Chain id | `137` |
| `DOMAIN_SEPARATOR()` | `0xcaa2ce1a5703ccbe253a34eb3166df60a705c561b44b192061e28f2a985be2ca` |
| Recomputed: `keccak256(abi.encode(EIP712Domain typehash, keccak256("USD Coin"), keccak256("2"), 137, token))` | the same, `0xcaa2…e2ca` |

So the **EIP-712 domain** of a permit is `{ name: "USD Coin", version: "2", chainId: 137, verifyingContract:
0x3c49…3359 }`, with the standard EIP-2612 `Permit(address owner,address spender,uint256 value,uint256 nonce,uint256
deadline)` type. The implementation's code contains the selectors of:

- `permit(address,address,uint256,uint256,uint8,bytes32,bytes32)` (`0xd505accf`), EIP-2612;
- `permit(address,address,uint256,uint256,bytes)` (`0x9fd5a6cf`), the bytes-signature form FiatToken 2.2 added, which
  also accepts EIP-1271 signatures from contract wallets;
- `transferWithAuthorization` (`0xe3ee160e`) and `receiveWithAuthorization` (`0xef55bec6`), EIP-3009.

A selector's presence in the bytecode is evidence, not proof. The fork tests (T-018-02) prove the permit path.

**Polygon Amoy is different.** Its test USDC `0x41E94Eb019C0762f9Bfcf9Fb1E58725BfB0e7582` answers `name()` `"USDC"`,
`version()` `"2"`, chain id 80002, `DOMAIN_SEPARATOR()`
`0x5cddc98319864e897e4469bd1d13c2288c677047b54b8cca12bd342adb6be9eb` (read through `polygon-amoy.drpc.org`). The web
app therefore reads the domain from the token, and never hard-codes it across networks.

**Why permit, not EIP-3009.** `transferWithAuthorization` to the vault can be submitted by anyone who sees the
signature, straight to the token. That makes a plain arrival with no direction, which waits unassociated. The funds
are safe, but the deposit is lost to the user until the operator releases it. `receiveWithAuthorization` requires the
payee to be the caller, so the funds would pass through the contract, and the vault's `Transfer` would come from the
contract instead of the user. With permit, the token moves from the user to the vault in one `transferFrom`, the
`Transfer` log's `from` is the funder (the funder, as the endpoint reads a sender), and a front-run permit only sets an
allowance the contract then uses.

## Recipient format: `bytes32`, not ERC-7930

Read from `ethereum/ERCs` at `master` and `ChainAgnostic/namespaces` at `main`:

| Standard | Status (front matter) | Last change | What it is | Fit |
|---|---|---|---|---|
| ERC-7930 Interoperable Addresses | **Review** | 2026-05-24 ("define Chain Identifier") | A versioned binary envelope: chain type, chain reference, address. Each chain type's serialisation is defined by a **CAIP-350 profile** | **Not yet.** CASA's `polkadot` namespace has `caip2.md` and `caip10.md` but **no `caip350.md`**, so there is no defined way to put a Kreivo account in a 7930 address. `eip155` and `solana` have profiles |
| ERC-7786 Cross-Chain Messaging Gateway | **Final** (moved 2026-03-06) | — | `sendMessage` from a source gateway to a receiver on another chain, with 7930 addresses | **Wrong layer.** It is a messaging transport whose receivers execute messages. bezent relays no message: it observes a transfer and mints. Adopting its interface would promise semantics bezent does not have |
| ERC-7683 Cross Chain Intents | **Draft** | 2026-05-13 ("redesign around resolvers") | Orders filled by solvers or fillers, now through resolvers | **Wrong model.** There is no filler: the operator is the custodian and mints against its own observation. Still a moving draft |

**Recommendation: `bytes32`.** A Kreivo account is exactly 32 bytes, the event stays fixed-size and cheap, and the
contract serves one issuing network only. Versioning is by address: a later version that takes a 7930 address (once a
`polkadot` CAIP-350 profile exists and 7930 is final) is a new contract with a new event, declared beside this one,
and needs no upgrade path.

## Kreivo wallets

| Wallet | Kreivo | Signs a contract call | Fees | Account with a provider |
|---|---|---|---|---|
| **Talisman** | Listed (`TalismanSociety/chaindata`, `pub/v3/chains/byId/kreivo.json`) | Yes, any extrinsic through the injected-extension interface | KSM | Needs KSM at or above the existential deposit |
| **SubWallet** | Listed (`Koniverse/SubWallet-ChainList`, slug `kreivo`, address prefix 2, KSM, status `ACTIVE`, providers `wss://kreivo.io/` and `wss://kreivo.kippu.rocks/`) | Yes, as above | KSM | As above |
| **Polkadot.js extension** | Any chain, once the app supplies metadata; Kreivo's custom signed extensions must be registered in the polkadot.js initialisation metadata (Open 9) | Yes, as above | KSM | As above |
| **Virto (`<virto-connect>`, papi-signers' `KreivoPassSigner`)** (out for now: Open 9, ruled) | Built for Kreivo: accounts of `pallet-pass`, with passkeys (WebAuthn) as devices | `KreivoPassSigner` implements polkadot-api's `PolkadotSigner`, so in principle any transaction. Not yet shown for `Contracts.call` | None from the user: memberships of a community carry a gas tank (Virto's forum post, 2025-04-08) | **Unverified**: whether a pass account has a provider and can hold a non-sufficient asset |

**The usability question** is not signing. It is that bUSDC is "not sufficient" (`DEC-7`), so a mint to an account with
no provider is refused (`research/03`, `UnavailableConsumer`), and every extension user also needs KSM for fees. A
plain user with only USDC on Polygon cannot get bUSDC on Kreivo, and cannot redeem it, without first getting some KSM.
Virto would remove both needs if its pass accounts carry a provider and its memberships pay fees; that stays
unverified. @pandres95 ruled (amendment 0029, Open 9, 2026-11-09) that passkeys and Virto are out for now and the spike
(T-018-16) is dropped. The web app uses the extensions, checks the recipient before a deposit, and says plainly that a
little KSM is needed. His note: for these wallets to work with the polkadot.js interface, Kreivo's custom signed
extensions must be registered in the polkadot.js initialisation metadata (plan part 4, `T-018-15`).

Sources: [Virto's forum post](https://forum.polkadot.network/t/try-out-virto-connect-transact-without-wallets-or-tokens/12440),
[papi-signers' `KreivoPassSigner`](https://virto-network.github.io/papi-signers/api/signer/src/classes/KreivoPassSigner.html),
[Talisman's network support](https://docs.talisman.xyz/talisman/start/network-support/substrate).

## Kreivo's signed extensions (T-018-15)

Read from Kreivo mainnet's metadata on 2026-11-16 (spec 136, transaction version 14; the endpoint's fixture
`services/kreivo-endpoint/fixtures/kreivo-spec-136.metadata` has the same list), in order: `PassAuthenticate`,
`CheckNonZeroSender`, `CheckSpecVersion`, `CheckTxVersion`, `CheckGenesis`, `CheckMortality`, `CheckNonce`,
`CheckWeight`, `ChargeAssetTxPayment`, `DynamicMaxBlockWeight`.

- **Custom to Kreivo**, unknown to polkadot.js: `PassAuthenticate` (Virto's pass pallet,
  `Option<{ device_id, credential }>`, one byte `0x00` when the account signs for itself) and `DynamicMaxBlockWeight` (a
  wrapper, no data). `ChargeAssetTxPayment`'s asset is Kreivo's own location type, but the app pays in KSM, so it is
  `None` and polkadot.js's definition encodes it the same.
- **Without registration it fails.** A polkadot.js wallet builds the payload it signs from the page's JSON with its own
  registry and treats an extension it does not know as "no effect", so it leaves out `PassAuthenticate`'s byte and signs
  another payload; Kreivo rejects the signature. polkadot-api's own polkadot.js bridge refuses to sign at all (`PJS does
  not support this signed-extension: PassAuthenticate`).
- **The registration** is the `MetadataDef`'s `userExtensions` the page hands the wallet through
  `injected.metadata.provide` before signing, the shape polkadot.js's `ApiPromise.create({ signedExtensions })` takes:
  `PassAuthenticate: { extrinsic: { passAuthenticate: 'Option<u8>' }, payload: {} }` (only `None` is ever signed, so the
  option's inner type is never encoded) and `DynamicMaxBlockWeight: { extrinsic: {}, payload: {} }`. The definition also
  carries the runtime's V15 metadata, so the wallet can show the call. A wallet with no `metadata` interface signs only
  if it knows the extensions itself; the page says so.
- **Kreivo has `DryRunApi`**, so the exact batch is dry-run as the redeemer before signing, besides the contract's own
  `ContractsApi.call` dry run of `redeem`, which alone cannot see the approval the batch adds first.

## Hosting

Cloudflare Workers with static assets, not Pages. Both serve a static app. Cloudflare's own migration guide (updated
2026-10-12) says Workers has "a distinctly broader set of features" than Pages and does not deprecate Pages. Workers
leaves room for a rate-limiting front to the public status route (amendment 0029, Open 8, ruled yes) in the same deployment.
`bridge.bloque.network` is the working name.

## The public status route, the statement and the alerts (T-018-10, T-018-11)

**The route** (`services/ingress/src/public.rs`, migration `0146_public_status`). `GET /v0/public/deposits/by-coordinate`
(`network`, `block_number`, `block_hash`, `position_kind`, `position_index`) and `GET /v0/public/withdrawals/by-redemption`
(`redeemer`, SS58 of any prefix or `0x` and 64 hex digits, and `reference`, 32 bytes), on the ingress's `[public]` listener,
apart from the authenticated one, with no credential. The answer is `kind`, `bridge_reference`, `state`, `conditions`,
`coordinates` (network, role, block, hash, position where bezent holds one, and whether it observed it final),
`transfers` (each transfer's role, bridge reference, state and condition) and `updated_at`; the web app reads the first
five it knows (`apps/bridge/src/status/route.ts`, which now sends `block_number`). The block number is required: it is
part of the coordinate a bridge reference is derived from, and the lookup goes by that reference.

- **A role that reads one view.** The route's login is a member of `bezent_public` only, which holds SELECT on
  `public_status` and on no table; the view reads as its owner. It has no column a requester supplied, and no row a request
  made or reached but a withdrawal (whose extras it does not show), so a requester's deposit answers "not found" with the
  body an unknown coordinate gets (`REQ-ID-4`), whatever the route's code does.
- **Index reads only.** The coordinate derives `bz-d-` and `bz-u-`, the redemption `bz-w-` (the core's derivation, pinned
  by `apps/bridge/test/vectors/core-refs.json`), and every branch of the view starts at `transfers`' unique bridge
  reference; a test holds the plan to that on a few thousand rows. At most `max_in_flight` requests at once (64), on 4
  connections with a 2 s statement bound; beyond, `ERR-Overloaded` at once (`/health` too). The rate limit itself is
  Cloudflare's, in front.
- **Bounded before any work** (review M1, L1 to L3 of PR #139). A redeemer longer than an account can be (49 base58
  characters, 66 as `0x` hex) and a reference longer than 66 are refused before they are decoded (base58 decoding is
  quadratic: 64 thousand characters took two seconds of a worker). The listener is HTTP/1 only, reads a head of at most
  8 KiB in 32 headers (431 beyond) and a URI of at most 2 KiB (414), closes a connection whose head takes longer than
  `header_read_timeout_ms` (5 s), holds at most `max_http_connections` (256) open, and answers a request that takes
  longer than `request_timeout_ms` (5 s) with 503, retryable. `public_status` is a security-barrier view; the route's
  keys are leakproof, so its plan still starts at the unique index. **The origin should accept connections from
  Cloudflare only** (security group or firewall): these are the listener's own bounds, not a rate limit.
- **Public data**: `Access-Control-Allow-Origin: *`, no credentials, `Cache-Control: public, max-age=5` on an answer and on
  "not found".

**The statement and published addresses.** The directing code is declared in the store (`directing_code`, migration
`0147`, written by the migration owner as every declaration is, append-only): network, address, token, the reserve
(asset, network) it feeds, its limits and first block, with a reference. The statement lists it in `published_addresses`
beside the reserve (`holds: "nothing"`, `feeds`: the reserve address) and in `directing_code` with its limits; a
declaration made after the evaluation a statement states is left out of it, so a re-derived statement keeps its bytes.
The EVM endpoint still watches the contracts its own `[[directing]]` names; the deployment keeps the two equal. Since
review M3 of PR #139 the store holds the declaration to account (`REQ-AS-11` is a MUST; migration `0148`): the core's
deposit of a directed arrival and its return are refused, whoever writes, unless the arrival's `detail.directing` is
declared for its network and token from a block at or before its own (without case on an EVM network), and the core
does not try one that is not (it logs `directing-undeclared` once an hour). Such an arrival waits, final and unbound,
an unassociated arrival an operator sees, until it is declared or released by hand. So an arrival of a token admitted
for no asset, which no declaration can name, now waits too, where review L3 of PR #136 returned it. The endpoint's
login does not read `directing_code`, so it does not check its `[[directing]]` against it at start; it refuses a
`[[directing]]` address that is an admitted token, the vault or one of its contracts. A declaration's address is kept
in lower case on an EVM network and is none of the network's reserve addresses or admitted tokens, and `declared_at` is
the clock's at the write, not the transaction's start, so a declaration committing while an evaluation is stated cannot
make a re-derived statement differ (reviews L4, L5).

**The alerts** (`services/core/src/directed_alerts.rs`; `deploy/runbooks/pages/directed-*.md`, `unpaired-direction.md`,
`token-upgraded.md`). Per deposit route, each minute: directed deposits minted (`Delivered`) in the trailing 24 hours,
returns and refunds of directed deposits made in it, and the share of the window directed deposits hold (counted as
`route_usage_locked()` counts them, waiting ones included). Thresholds in `[directed_alerts]`, defaults 200 mints, 3
returns and 50 % (the first is the count Open 6 put as a cap; returns are rare by construction). Above a threshold pages
once on crossing and daily after; back at or below logs `directed-alert-cleared`. An `UnpairedDirection` pages once per
observation. Alerts only: nothing is capped, refused or held.

**An origin token's upgrade.** The EVM endpoint now also reads every admitted origin token's `Upgraded(address)` (one
query per range, the declared tokens' addresses and that topic) and records it as `token_upgrade`, which binds nothing; the
core pages `origin-token-upgraded` once per observation and pauses nothing. It reads the event in both forms (review M2 of
PR #139): Circle's `FiatTokenProxy`, native USDC's proxy, emits `Upgraded(address implementation)` with the address **not**
indexed (one topic, the address as a data word); an OpenZeppelin proxy indexes it (two topics, no data). A page fires on an
observation that is not final yet, so a reorganised log can page twice (the runbooks say how to tell). Whether directed intake should hold on it is
the owner's open ruling `d-f018-usdc-upgrade`; if it rules a hold, `page_observation` in `directed_alerts.rs` is where it
goes (a hold of the token's asset, which the engine honours), and `token_upgrades_since` already reads what it would.

## Deployment order (PR #136)

Deploy migration `0145_directed_core` first, then the **Kreivo endpoint, before the core**. The endpoint records two
things the core and the store now read from a refused dry run: the block time of the head it ran at
(`prepared_transactions.dry_run_block_time`), and, for a refusal to mint (`Kreivo(Assets(CannotMint))`), the asset's
status and supply at that head (`dry_run.asset`). Kreivo's `mint` answers `CannotMint` for every error of `mint_into`,
the whole asset's too (frozen, being destroyed, gone), so only a refusal recorded with the asset `Live` is the
destination's. A core deployed before the endpoint is safe but blind: every refusal to mint reads as one that names
nothing, pages, and goes to review at its deadline without `CND-DestinationRefused`. A directed deposit still in
`Source-Final` can take the review exit once the endpoint is updated and its mint is prepared again; one that already
entered review without the condition never can, since the store reads the condition the deposit held when it entered
review (whether such a deposit may take the exit later is the open ruling `d-f018-review-exit-later`). Supply
overflow (`mint_into` past `u128::MAX`) is not checked: at six decimals it is about 3.4 × 10^32 tokens, not a plausible issuance. On a `Live`
asset, `mint_into` may also refuse with `BelowMinimum` (a fresh account credited less than the asset's `min_balance`),
which reaches the core as the same `CannotMint` and reads as a refusal of the destination. The refund it opens still
needs two approvers (`REQ-GV-5`).

## Smaller choices

- `Directed(address indexed sender, uint256 amount, bytes32 indexed recipient, address refund)`: the sender and recipient
  are topics, so a wallet or the status page can filter by either. The amount and the refund address stay in data.
- The refund destination is named by the caller, not taken from `msg.sender` (`d-refund-dest-swaps`, owner, 2026-11-09:
  "The original depositing address. That's a contract flaw. The depositing contract should pass on the information about
  the original depositor. If not doing it, that's a flaw."). evmhot's sweeps are called from its HD deposit addresses, so
  `msg.sender` would loop a refund into another sweep.
- The original sender may be an exchange's shared hot wallet (SPEC `DEF-5` risk: returning there can lose the value),
  and the owner has ruled that the refund goes to the original depositor all the same (`d-refund-dest-swaps`).
- **Ruled (amendment 0029, Open 15, `d-mixed-sweeps-2`: one sweep per transfer):** a single sweep can mix funds from several
  senders, because evmhot sweeps full balances and sub-minimum dust accumulates. A deposit has one refund address, so
  without a rule the original depositor would be undefined. The rule: one directed call per inbound transfer, with
  refund = that transfer's sender. Sub-minimum transfers are never combined across senders, and dust from different
  senders stays in the deposit address. It binds evmhot (plan, part 5).
- Pairing at the `Transfer` log's coordinate, not the `Directed` log's: the arrival is the token's movement, as for
  every other arrival (`REQ-RN-3`). The reserve book and the existing bridge reference derivation then need no change.
- The recipient is public on Polygon, next to the depositor's address. That is the user's own choice, and an account id
  is not personal data (`REQ-PV-1`). The web app says so.
- The reserve Safe for bUSDC on Polygon is the operator's published vault address. It is a parameter of the deployment
  script, not written into this repository.
- Besides the core, the migration owner may write the same row (`NFR-7`: drills and restores). It is a third writer, as for
  every other store-guarded row, and the store's guards apply to it as to the core.
