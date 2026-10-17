<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# 03 — Kreivo as the issuing network, and aledgent's settlement contract

Evidence, not doctrine. Read 2026-09-30.
**K** = `a checkout of Kreivo's kreivo-srtool` (identical to `origin/master` `3e0a5c3`, tag
`0.17.0`). **A** = a checkout of aledgent.

## Versions

`K/runtime/kreivo/Cargo.toml:8` 0.17.0; spec 133 (`K/runtime/kreivo/src/lib.rs:174`); pallet-contracts 49.0.0,
pallet-assets 53.0.0 (root `Cargo.toml:113, 119`); `polkadot-stable2606-2`.

## Contracts and assets

- `CallFilter` allows `Balances::transfer_allow_death` and any `Assets` call
  (`K/runtime/kreivo/src/config/contracts/mod.rs:19-27`). Chain extension `KreivoChainExtensions` (`:109`).
- Contract API (extension 0): assets balance `0x0000`, deposit `0x0001`, transfer `0x0002`
  (`K/apis/src/contract/chain_extension.rs:11-33`, `K/apis/src/runtime/types/assets.rs:35-55`). No mint, burn,
  total issuance. `deposit` pulls from the direct caller; both keep-alive
  (`K/apis/src/runtime/api_impls/assets.rs:25-43`).
- `Assets` = pallet 13, `pallet_assets<Instance1>` (`K/runtime/kreivo/src/lib.rs:229-230`); asset id
  `FungibleAssetLocation` (`config/currency.rs:97-98`); lookup `CommunityLookup` (`config/system.rs:82`).
  pallet-assets 53 call indices: `force_create` 1, `mint` 6, `burn` 7, `transfer` 8, `set_team` 16,
  `force_asset_status` 21.
- Mint needs Issuer (`functions.rs:465`); burn needs Admin (`functions.rs:550`), best-effort, any account.
- `CreateOrigin = NeverEnsureOrigin`, `ForceOrigin = EnsureRoot` (`config/currency.rs:88-106`). Root: collective
  referendum track 0 (`config/collective/tracks.rs:41, 147`) or relay via XCM (`xcm_config.rs:154`).
- Non-sufficient assets need a provider on the recipient (`UnavailableConsumer`, `functions.rs:80-90`).
- Deploying: contracts store; `InstantiateOrigin = EnsureNever`; uploads need a community origin
  (`config/contracts/mod.rs:133-140`). Example flow: `A/contracts/settlement/scripts/drive.mjs:308-360`.
- Hand-encoded runtime call pattern: `A/contracts/settlement/src/kreivo.rs:46-91`.

## Bridge groundwork already in the runtime

- `BridgeBackedAssets`: `Sibling{1000, 50, 1337}` and `Sibling{1000, 50, 1984}`, "USDC and USDT, as minted by
  Phase 0 and the bridge" (`K/runtime/kreivo/src/xcm_config.rs:349-353`).
- `NotBridgeBacked` refuses reserve transfers of them (`:358`, used at `:415`). Asset Hub is the only trusted
  reserve (`:296-300`). Commit `c6ccd46` (#486), "XCM groundwork for message-backed bridging (0.17)".

## aledgent's settlement contract

- `deposit(account_ref: [u8;32], asset, amount) -> Result<InboxSeq, Error>`, selector `0x00000101`
  (`A/contracts/settlement/src/lib.rs:286-311`). Payer is the direct caller
  (`A/features/F-026-settlement-contract/NOTES.md:29-33, 164-182`). Emits `Deposited{seq, account_ref, asset,
  amount}` (`lib.rs:104-115`); inbox `InboxEvent::Deposited` (`types.rs:383-395`). `AssetId` mirrors Kreivo's id.
- `credit` only from contracts running the same code (`lib.rs:316-336`).
- `execute(Vec<Envelope>)`, `Envelope{operation_ref, call}`, `Call::Withdraw{to: Destination::Account | Deployment,
  asset, amount}` (`types.rs:165-219`); events `Withdrawn` / `Refused` (`lib.rs:117-160, 614-701`); payout in
  `try_withdraw` (`lib.rs:765-862`).
- `set_withdrawal_bound(asset, Option<WithdrawalBound{amount, period}>)`, admin only; no bound means no
  withdrawals (`lib.rs:365-391, 796-806`).

## Expiring transactions

`CheckEra` (`K/runtime/kreivo/src/lib.rs:106`); `BlockHashCount` 4096. aledgent builds eras in
`A/services/settle/src/chain/mod.rs:342-375`, encodes in `chain/kreivo.rs:923-935`; rules in
`A/features/F-027-settlement-extension/NOTES.md:365-390`, `A/features/F-016-settlement-adapter/NOTES.md:105-121`.

## zombienet

`A/e2e/network/src/lib.rs:92-145` (zombienet-sdk 0.4.15): `rococo-local` + Kreivo para 2281 (`--authoring
slot-based`). Test assets via genesis override (`lib.rs:57-84`). Binaries: `A/e2e/scripts/prepare.sh`; CI uses
`polkadot-stable2606-2` + Kreivo 0.17.0 (`A/e2e/README.md:88-94`). Asset Hub deliberately absent (`lib.rs:6-8`).

## Not relevant

VOS `clerk-bridge` is a voucher bridge between clerk-ledger instances, not an asset bridge.
