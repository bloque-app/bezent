// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The bezent contract on Kreivo (`contracts/bezent`, F-006), as the app speaks to it: plain SCALE, written from the
// contract's source, as the Kreivo endpoint does in Rust (`services/kreivo-endpoint/src/kreivo.rs`). No contract
// metadata is fetched or shipped: the messages are named by their ink! 5 selectors (the first four bytes of the
// BLAKE2b-256 of the name) and the events by their signature topics.
//
// - Messages the app sends or dry-runs: `redeem(asset, amount, backend, payout, reference)`, and the reads
//   `bounds(asset)`, `backends(asset)`, `paused(scope)`, `redemption(redeemer, reference)` and
//   `mint_reference_used(reference)`.
// - The `Redeemed` event: `{ seq, reference (topic), redeemer (topic), asset, amount, backend, payout }`.
// - The contract's `Error`, by its SCALE index, with what each one means for a redeemer.

import { blake2b } from '@noble/hashes/blake2b';
import { concat, fromHex, toHex, type Hex } from '../lib/bytes';
import { Reader, u128, u32, u64, u8, vecU8 } from '../lib/scale';

const utf8 = (s: string) => new TextEncoder().encode(s);
export const blake2_256 = (b: Uint8Array) => blake2b(b, { dkLen: 32 });

/** An ink! 5 message selector. */
export const selector = (name: string) => blake2_256(utf8(name)).slice(0, 4);

/** `AssetId::Here(id)`: a local asset of Kreivo's `pallet-assets` (bUSDC is `Here(101)`). */
export const assetId = (id: number) => concat([u8(0), u32(id)]);

export type Backend = { kind: 'evm'; chainId: bigint } | { kind: 'attested'; id: number };
export type Payout = { kind: 'address'; address: Hex } | { kind: 'commitment'; commitment: Hex };

export function encodeBackend(b: Backend): Uint8Array {
  return b.kind === 'evm' ? concat([u8(0), u64(b.chainId)]) : concat([u8(1), u32(b.id)]);
}

function readBackend(r: Reader): Backend {
  const v = r.u8();
  if (v === 0) return { kind: 'evm', chainId: r.u64() };
  if (v === 1) return { kind: 'attested', id: r.u32() };
  throw new Error(`Backend: variant ${v}`);
}

function readPayout(r: Reader): Payout {
  const v = r.u8();
  if (v === 0) return { kind: 'address', address: toHex(r.take(20)) };
  if (v === 1) return { kind: 'commitment', commitment: toHex(r.take(32)) };
  throw new Error(`Payout: variant ${v}`);
}

function readAsset(r: Reader): number {
  const v = r.u8();
  if (v !== 0) throw new Error(`AssetId: variant ${v}`);
  return r.u32();
}

export const sameBackend = (a: Backend, b: Backend) =>
  a.kind === b.kind && (a.kind === 'evm' ? a.chainId === (b as typeof a).chainId : a.id === (b as typeof a).id);

export const backendText = (b: Backend) => (b.kind === 'evm' ? `Evm { chain_id: ${b.chainId} }` : `Attested { id: ${b.id} }`);

const ref32 = (h: Hex, what: string) => {
  const b = fromHex(h);
  if (b.length !== 32) throw new Error(`${what} is not 32 bytes`);
  return b;
};

// ---------------------------------------------------------------------------------------------------- messages

/** The input of `redeem(asset, amount, backend, payout, reference)`; `payout` is a `Vec<u8>`: for an EVM backend,
 * the 20-byte address (`REQ-RQ-9`). */
export function redeemData(asset: number, amount: bigint, backend: Backend, payout: Uint8Array, reference: Hex): Uint8Array {
  return concat([selector('redeem'), assetId(asset), u128(amount), encodeBackend(backend), vecU8(payout), ref32(reference, 'the reference')]);
}

export const boundsData = (asset: number) => concat([selector('bounds'), assetId(asset)]);
export const backendsData = (asset: number) => concat([selector('backends'), assetId(asset)]);
export const redemptionData = (redeemer: Hex, reference: Hex) =>
  concat([selector('redemption'), ref32(redeemer, 'the redeemer'), ref32(reference, 'the reference')]);
export const mintReferenceUsedData = (reference: Hex) => concat([selector('mint_reference_used'), ref32(reference, 'the mint reference')]);

/** What a pause covers; a redemption of an asset is stopped when any of the four scopes covering it is paused. */
export type PauseScope =
  | { kind: 'all' }
  | { kind: 'direction'; redeem: boolean }
  | { kind: 'asset'; asset: number }
  | { kind: 'asset-direction'; asset: number; redeem: boolean };

export function pausedData(s: PauseScope): Uint8Array {
  const dir = (redeem: boolean) => u8(redeem ? 1 : 0);
  const scope =
    s.kind === 'all'
      ? u8(0)
      : s.kind === 'direction'
        ? concat([u8(1), dir(s.redeem)])
        : s.kind === 'asset'
          ? concat([u8(2), assetId(s.asset)])
          : concat([u8(3), assetId(s.asset), dir(s.redeem)]);
  return concat([selector('paused'), scope]);
}

/** The scopes that stop a redemption of `asset`, each with how the page names it. */
export const redeemScopes = (asset: number): [string, PauseScope][] => [
  ['everything', { kind: 'all' }],
  ['every redemption', { kind: 'direction', redeem: true }],
  [`Here(${asset})`, { kind: 'asset', asset }],
  [`redemptions of Here(${asset})`, { kind: 'asset-direction', asset, redeem: true }],
];

// ---------------------------------------------------------------------------------------------------- answers

/** What `ContractsApi.call` returned, as the app needs it (polkadot-api's decoding of the runtime API's result). */
export interface DryRun {
  result: { success: true; value: { flags: number; data: Uint8Array } } | { success: false; value: unknown };
  gasRequired: { refTime: bigint; proofSize: bigint };
  /** The storage deposit the call would charge (zero for a refund). */
  storageCharge: bigint;
  debugMessage?: string;
}

/** A read-only message's value: the bytes after `Ok` of ink!'s `LangError`. A failed dispatch, a revert or a
 * `LangError` throws: never read as an answer. */
export function getterValue(d: DryRun, what: string): Reader {
  if (!d.result.success) throw new Error(`${what}: the call failed to dispatch (${describeDispatch(d.result.value)}).`);
  const { flags, data } = d.result.value;
  if (flags & 1) throw new Error(`${what}: the contract reverted.`);
  const r = new Reader(data);
  if (r.u8() !== 0) throw new Error(`${what}: the contract answered with a language error.`);
  return r;
}

export interface Bounds {
  supplyCeiling: bigint;
  windowAmount: bigint;
  windowBlocks: number;
  perTransferMax: bigint;
  redeemMinimum: bigint;
}

/** `bounds(asset)`: `Option<Bounds>`; `undefined` when the contract does not know the asset. */
export function decodeBounds(d: DryRun): Bounds | undefined {
  const r = getterValue(d, 'bounds');
  const some = r.u8();
  if (some === 0) return (r.end('bounds'), undefined);
  if (some !== 1) throw new Error('bounds: not an Option');
  const b = { supplyCeiling: r.u128(), windowAmount: r.u128(), windowBlocks: r.u32(), perTransferMax: r.u128(), redeemMinimum: r.u128() };
  r.end('bounds');
  return b;
}

export function decodeBackends(d: DryRun): Backend[] {
  const r = getterValue(d, 'backends');
  const n = Number(r.compact());
  const out: Backend[] = [];
  for (let i = 0; i < n; i++) out.push(readBackend(r));
  r.end('backends');
  return out;
}

export function decodeBool(d: DryRun, what: string): boolean {
  const r = getterValue(d, what);
  const v = r.bool();
  r.end(what);
  return v;
}

export interface Redemption {
  redeemer: Hex;
  asset: number;
  amount: bigint;
  backend: Backend;
  payout: Payout;
  refunded: boolean;
}

/** `redemption(redeemer, reference)`: `Option<Redemption>`. */
export function decodeRedemption(d: DryRun): Redemption | undefined {
  const r = getterValue(d, 'redemption');
  const some = r.u8();
  if (some === 0) return (r.end('redemption'), undefined);
  if (some !== 1) throw new Error('redemption: not an Option');
  const out = { redeemer: toHex(r.take(32)), asset: readAsset(r), amount: r.u128(), backend: readBackend(r), payout: readPayout(r), refunded: r.bool() };
  r.end('redemption');
  return out;
}

/** The contract's errors, by their SCALE index (`contracts/bezent/src/types.rs`, `Error`). */
export const CONTRACT_ERRORS = [
  'Unauthorised',
  'UnknownAsset',
  'Paused',
  'ReferenceUsed',
  'ZeroAmount',
  'AboveTransferMaximum',
  'AboveWindow',
  'AboveCeiling',
  'BelowMinimum',
  'PayoutDoesNotFitBackend',
  'BackendNotDeclared',
  'InvalidBackends',
  'SupplyOfAKnownAsset',
  'NoSuchRedemption',
  'AlreadyRefunded',
  'NotALowering',
  'InvalidBounds',
  'NoRaiseScheduled',
  'RaiseNotDue',
  'NoDelayScheduled',
  'DelayNotDue',
  'ZeroDelay',
  'DelayTooLong',
  'RolesNotDistinct',
  'InvalidRoleAccount',
  'GovernanceChangeIsDelayed',
  'NoGovernanceScheduled',
  'GovernanceNotDue',
  'Overflow',
  'Kreivo',
  'DepositMismatch',
  'MintMismatch',
  'BurnShort',
] as const;

const ASSETS_API_ERRORS = ['CannotDeposit', 'CannotTransfer', 'CannotMint', 'CannotBurn', 'NotAGroupContract', 'NotAuthorised', 'NotALocalAsset'];

/** What a redeemer is told for each refusal the contract can give `redeem`. */
const REDEEM_REFUSALS: Partial<Record<(typeof CONTRACT_ERRORS)[number], string>> = {
  UnknownAsset: 'the contract does not know this asset.',
  Paused: 'redemptions of this asset are paused.',
  ReferenceUsed: 'you used this reference for a redemption before. Pick a new one.',
  BelowMinimum: 'the amount is below the route’s redemption minimum.',
  PayoutDoesNotFitBackend: 'the payout address does not fit the backend (an EVM backend takes a non-zero 20-byte address).',
  BackendNotDeclared: 'the asset does not declare this backend.',
  DepositMismatch: 'the contract’s balance did not rise by the amount it pulled.',
  BurnShort: 'Kreivo burnt less than the amount.',
};

export type ContractError = { name: string; detail?: string };

/** Decodes a contract `Error` (the bytes after `Err`). */
export function readContractError(r: Reader): ContractError {
  const i = r.u8();
  const name = CONTRACT_ERRORS[i];
  if (!name) return { name: `Error #${i}` };
  if (name !== 'Kreivo') return { name };
  const k = r.u8();
  if (k === 0) return { name, detail: 'UnknownError' };
  if (k === 1) return { name, detail: 'ExtQueryError' };
  if (k === 2) return { name, detail: `Assets(${ASSETS_API_ERRORS[r.u8()] ?? 'unknown'})` };
  return { name, detail: `#${k}` };
}

export const errorText = (e: ContractError) => (e.detail ? `${e.name}(${e.detail})` : e.name);

/** What `redeem` would do, from its dry run: `Ok`, or the contract's refusal. */
export type RedeemVerdict = { ok: true } | { ok: false; error: ContractError; why: string };

export function decodeRedeemDryRun(d: DryRun): RedeemVerdict {
  if (!d.result.success) {
    return { ok: false, error: { name: 'Dispatch' }, why: `the call failed to dispatch (${describeDispatch(d.result.value)}).` };
  }
  const r = new Reader(d.result.value.data);
  if (r.u8() !== 0) return { ok: false, error: { name: 'LangError' }, why: 'the contract answered with a language error.' };
  const outcome = r.u8();
  if (outcome === 0) return { ok: true };
  const error = readContractError(r);
  return { ok: false, error, why: REDEEM_REFUSALS[error.name as keyof typeof REDEEM_REFUSALS] ?? `the contract refused it (${errorText(error)}).` };
}

/** Whether a dry-run refusal of `redeem` alone is only its pull: without the approval the batch adds first, the
 * contract falls back to a pull that keeps the account alive, which a whole balance cannot pass. The batch's own
 * dry run decides it. */
export const isPullRefusal = (v: RedeemVerdict) =>
  !v.ok && ((v.error.name === 'Kreivo' && v.error.detail === 'Assets(CannotDeposit)') || v.error.name === 'DepositMismatch');

/** A short text for a `DispatchError` as polkadot-api decodes it (`{ type, value }`, nested). */
export function describeDispatch(e: unknown): string {
  const parts: string[] = [];
  let v: unknown = e;
  for (let i = 0; i < 5 && v && typeof v === 'object' && 'type' in v; i++) {
    parts.push(String((v as { type: unknown }).type));
    v = (v as { value?: unknown }).value;
  }
  return parts.length ? parts.join('.') : JSON.stringify(e, (_, x) => (typeof x === 'bigint' ? x.toString() : x));
}

// ---------------------------------------------------------------------------------------------------- events

/** An ink! 5 event's signature topic: BLAKE2b-256 of `Name(type,…)` as the contract's source writes it. */
export const signatureTopic = (signature: string) => toHex(blake2_256(utf8(signature)));

export const REDEEMED_TOPIC = signatureTopic('Redeemed(EventSeq,Reference,AccountId,AssetId,Balance,Backend,Payout)');
export const MINTED_TOPIC = signatureTopic('Minted(EventSeq,Reference,AssetId,AccountId,Balance)');

export interface Redeemed {
  seq: bigint;
  reference: Hex;
  redeemer: Hex;
  asset: number;
  amount: bigint;
  backend: Backend;
  payout: Payout;
}

/** Decodes a `Redeemed` event from its topics and data, and refuses one whose topics are not its signature, its
 * reference and its redeemer, or whose bytes do not decode exactly. */
export function decodeRedeemed(topics: readonly Hex[], data: Uint8Array): Redeemed {
  if (topics[0] !== REDEEMED_TOPIC) throw new Error('not a Redeemed event');
  const r = new Reader(data);
  const e = {
    seq: r.u64(),
    reference: toHex(r.take(32)),
    redeemer: toHex(r.take(32)),
    asset: readAsset(r),
    amount: r.u128(),
    backend: readBackend(r),
    payout: readPayout(r),
  };
  r.end('Redeemed');
  if (topics.length !== 3 || topics[1] !== e.reference || topics[2] !== e.redeemer) {
    throw new Error('the Redeemed event’s topics are not its reference and its redeemer');
  }
  return e;
}
