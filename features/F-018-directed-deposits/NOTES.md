<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-018 — notes

**Status: Proposed.** Research for amendment 0029, which is not ruled.

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
`Transfer` log's `from` is the funder (as amendment 0026 reads a sender), and a front-run permit only sets an
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
| **Polkadot.js extension** | Any chain, once the app supplies metadata | Yes, as above | KSM | As above |
| **Virto (`<virto-connect>`, papi-signers' `KreivoPassSigner`)** | Built for Kreivo: accounts of `pallet-pass`, with passkeys (WebAuthn) as devices | `KreivoPassSigner` implements polkadot-api's `PolkadotSigner`, so in principle any transaction. Not yet shown for `Contracts.call` | None from the user: memberships of a community carry a gas tank (Virto's forum post, 2025-04-08) | **Unverified**: whether a pass account has a provider and can hold a non-sufficient asset |

**The usability question** is not signing. It is that bUSDC is "not sufficient" (`DEC-7`), so a mint to an account with
no provider is refused (`research/03`, `UnavailableConsumer`), and every extension user also needs KSM for fees. A
plain user with only USDC on Polygon cannot get bUSDC on Kreivo, and cannot redeem it, without first getting some KSM.
Virto removes both needs if its pass accounts carry a provider and its memberships pay fees. That is the spike
(T-018-16, amendment 0029 Open 9). Until it passes, the web app leads with the extensions, checks the recipient before a
deposit, and says plainly that a little KSM is needed.

Sources: [Virto's forum post](https://forum.polkadot.network/t/try-out-virto-connect-transact-without-wallets-or-tokens/12440),
[papi-signers' `KreivoPassSigner`](https://virto-network.github.io/papi-signers/api/signer/src/classes/KreivoPassSigner.html),
[Talisman's network support](https://docs.talisman.xyz/talisman/start/network-support/substrate).

## Hosting

Cloudflare Workers with static assets, not Pages. Both serve a static app. Cloudflare's own migration guide (updated
2026-10-12) says Workers has "a distinctly broader set of features" than Pages and does not deprecate Pages. Workers
leaves room for a rate-limiting front to the public status route (amendment 0029, Open 8) in the same deployment.
`bridge.bloque.network` is the working name.

## Smaller choices

- `Directed(address indexed sender, uint256 amount, bytes32 indexed recipient)`: the sender and recipient are topics, so
  a wallet or the status page can filter by either. The amount stays in data.
- Pairing at the `Transfer` log's coordinate, not the `Directed` log's: the arrival is the token's movement, as for
  every other arrival (`REQ-RN-3`). The reserve book and the existing bridge reference derivation then need no change.
- The recipient is public on Polygon, next to the depositor's address. That is the user's own choice, and an account id
  is not personal data (`REQ-PV-1`). The web app says so.
- The reserve Safe for bUSDC on Polygon is the operator's published vault address. It is a parameter of the deployment
  script, not written into this repository.
