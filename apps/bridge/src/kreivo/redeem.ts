// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The redeem on Kreivo through polkadot-api (F-018, T-018-15; plan part 4, "Redeem (Kreivo)"): what src/redeem.ts
// needs from the chain, behind `RedeemChain`, so the flow is tested against a model and this file stays a thin
// adapter.
//
// - Reads, at the finalized block: the contract's `bounds`, `backends` and the four pause scopes covering a
//   redemption of the asset, through `ContractsApi.call` (the contract's own answer, `contracts/bezent`); the
//   redeemer's bUSDC (`Assets.Account`), KSM (`System.Account`) and any approval it already gave the contract
//   (`Assets.Approvals`), with the approval deposit (`Assets.ApprovalDeposit`); whether the redeemer used a
//   reference before (`redemption(redeemer, reference)`).
// - The call: `Utility.batch_all([Assets.approve_transfer(Here(101), bezent, amount), Contracts.call(bezent, 0,
//   gas, storage limit, redeem(asset, amount, backend, payout, reference))])`, as Swaps' signer builds it. The
//   approval lets the contract pull the whole balance (`transfer_approved`); its deposit is returned when the pull
//   uses the approval up, in the same batch.
// - Two dry runs before anything is signed: `ContractsApi.call` of `redeem` alone, for the contract's own refusal and
//   the gas it needs; then `DryRunApi.dry_run_call` of the exact batch, as the redeemer, for the dispatch's result
//   and the `Redeemed` event it would emit.
// - The submission: signed by the extension wallet through kreivo/extensions.ts, with Kreivo's custom signed
//   extensions given explicitly, mortal (64 blocks), and followed to finality. polkadot-api says `signed` before it
//   validates the transaction, and `broadcasted` only once it sends it: a failure before that sent nothing (a
//   BadProof among them, `InvalidTxError`), and only a failure after it leaves the outcome unclear (`UnclearOutcome`,
//   with the last block that may still include it).

import type { Kreivo } from './client';
import type { KreivoNetwork } from '../config/networks';
import type { PolkadotSigner } from 'polkadot-api/signer';
import {
  backendsData,
  boundsData,
  decodeBackends,
  decodeBool,
  decodeBounds,
  decodeRedemption,
  describeDispatch,
  pausedData,
  redeemScopes,
  redemptionData,
  mintReferenceUsedData,
  type Backend,
  type Bounds,
  type DryRun,
  type Redemption,
} from './bezent';
import { KREIVO_CUSTOM_VALUES, WalletSignatureMismatch, checkExtensions, kreivoMetadataDef, metadataExtensions, type MetadataDef } from './extensions';
import { encodeSs58, decodeSs58 } from '../lib/ss58';
import { fromHex, toHex, type Hex } from '../lib/bytes';
import { withTimeout } from '../lib/timeout';

/** How long a read may take before it is given up (a light client still syncing fails closed). */
export const READ_TIMEOUT_MS = 45_000;

/** Upper bounds proven on Kreivo mainnet for `Contracts.call(redeem)` (Swaps' signer, 2026-11-08): a gas limit is a
 * ceiling, the unused part is refunded. The app uses twice the dry run's figure when that is higher. */
export const REDEEM_GAS_FLOOR = { refTime: 10_000_000_000n, proofSize: 500_000n } as const;
/** The transaction's mortality, in blocks: it can be included only in the 64 blocks from the one it was built at. */
export const MORTALITY = 64;

/** 0.01 KSM: the storage deposit a redemption may take at most, unless the dry run asks more (2026-11-13's
 * mainnet redeem charged 0.006695 KSM). */
export const REDEEM_STORAGE_FLOOR = 10_000_000_000n;

export interface RedeemFacts {
  bounds: Bounds | undefined;
  backends: Backend[];
  /** Each pause scope covering a redemption of the asset, and whether it is paused. */
  pauses: { scope: string; paused: boolean }[];
  /** The redeemer's balance of the asset, and its asset account's status (`Liquid`, `Frozen`, `Blocked`). */
  balance: bigint;
  assetStatus: string | undefined;
  /** The redeemer's free KSM. */
  ksm: bigint;
  /** An approval the redeemer already gave the contract for the asset (`Assets.Approvals`): the batch's
   * `approve_transfer` adds to it, and its deposit is already held. */
  approval: { amount: bigint; deposit: bigint } | undefined;
  /** What a new approval holds as its deposit, in KSM's planck (`Assets.ApprovalDeposit`). */
  approvalDeposit: bigint;
  /** The block the facts were read at. */
  at: { number: number; hash: Hex };
}

export interface Plan {
  redeemer: Hex;
  asset: number;
  amount: bigint;
  /** The contract's `redeem` input. */
  data: Uint8Array;
}

export interface Prepared {
  gas: { refTime: bigint; proofSize: bigint };
  storageLimit: bigint;
  /** The estimated fee, in KSM's planck. */
  fee: bigint | undefined;
  /** The batch's dry run: whether it dispatches, and the contract's events it would emit. */
  dryRun: { ok: true; emitted: { contract: Hex; data: Uint8Array }[] } | { ok: false; error: string };
  /** The call data that is signed. */
  callData: Hex;
  submit(signer: PolkadotSigner, progress: (step: string) => void): Promise<Finalized>;
}

export interface ContractEvent {
  contract: Hex;
  topics: Hex[];
  data: Uint8Array;
  /** The event's index among the block's events. */
  index: number;
}

export interface Finalized {
  txHash: Hex;
  block: { hash: Hex; number: number; index: number };
  ok: boolean;
  dispatchError?: string;
  events: ContractEvent[];
}

/** What the redeem flow needs from Kreivo. */
export interface RedeemChain {
  contract: Hex;
  facts(redeemer: Hex): Promise<RedeemFacts>;
  redemption(redeemer: Hex, reference: Hex): Promise<Redemption | undefined>;
  /** The finalized head's number. */
  finalized(): Promise<number>;
  dryRunMessage(redeemer: Hex, data: Uint8Array): Promise<DryRun>;
  prepare(plan: Plan, gas: { refTime: bigint; proofSize: bigint }, storageLimit: bigint): Promise<Prepared>;
}

// ---------------------------------------------------------------------------------------------------- polkadot-api

interface RawDryRun {
  gas_required: { ref_time: bigint; proof_size: bigint };
  storage_deposit: { type: string; value: bigint };
  debug_message?: unknown;
  result: { success: true; value: { flags: number; data: unknown } } | { success: false; value: unknown };
}

interface Tx {
  decodedCall: unknown;
  getEncodedData(): Promise<unknown>;
  getEstimatedFees(from: string, opts?: unknown): Promise<bigint>;
  signSubmitAndWatch(signer: PolkadotSigner, opts?: unknown): { subscribe(o: { next(e: TxEvent): void; error(e: unknown): void }): { unsubscribe(): void } };
}

type TxEvent =
  | { type: 'signed' | 'broadcasted'; txHash: string }
  | { type: 'txBestBlocksState'; txHash: string; found: boolean; block?: { number: number } }
  | {
      type: 'finalized';
      txHash: string;
      ok: boolean;
      block: { hash: string; number: number; index: number };
      dispatchError?: unknown;
      events: RawEvent[];
    };

interface RawEvent {
  type: string;
  value: { type: string; value: { contract?: unknown; data?: unknown } };
  topics?: unknown[];
}

interface Api {
  apis: {
    ContractsApi: { call(...args: unknown[]): Promise<RawDryRun> };
    DryRunApi: { dry_run_call(...args: unknown[]): Promise<{ success: boolean; value: unknown }> };
    Metadata: { metadata_at_version(v: number): Promise<unknown> };
  };
  query: {
    System: { Account: { getValue(...a: unknown[]): Promise<{ data: { free: bigint } }> } };
    Assets: {
      Account: { getValue(...a: unknown[]): Promise<{ balance: bigint; status: { type: string } } | undefined> };
      Approvals: { getValue(...a: unknown[]): Promise<{ amount: bigint; deposit: bigint } | undefined> };
    };
  };
  constants: { System: { Version(): Promise<{ spec_version: number }> }; Assets: { ApprovalDeposit(): Promise<bigint> } };
  tx: {
    Utility: { batch_all(a: unknown): Tx };
    Assets: { approve_transfer(a: unknown): Tx };
    Contracts: { call(a: unknown): Tx };
  };
}

/** Bytes as polkadot-api hands them: a `Uint8Array`, `0x` hex, or a value with `asBytes()`. */
export function bytesOf(x: unknown): Uint8Array {
  if (x instanceof Uint8Array) return x;
  if (typeof x === 'string' && x.startsWith('0x')) return fromHex(x);
  if (x && typeof (x as { asBytes?: unknown }).asBytes === 'function') return (x as { asBytes(): Uint8Array }).asBytes();
  throw new Error('not bytes');
}

/** An account as polkadot-api hands it (SS58 in the chain's prefix) as its 32 bytes. */
export function accountOf(x: unknown): Hex {
  if (typeof x === 'string' && !x.startsWith('0x')) {
    const d = decodeSs58(x);
    if (!d) throw new Error(`not an account: ${x}`);
    return d.account;
  }
  return toHex(bytesOf(x));
}

function toDryRun(raw: RawDryRun): DryRun {
  const debug = raw.debug_message === undefined ? undefined : new TextDecoder().decode(bytesOf(raw.debug_message));
  return {
    result: raw.result.success ? { success: true, value: { flags: raw.result.value.flags, data: bytesOf(raw.result.value.data) } } : raw.result,
    gasRequired: { refTime: raw.gas_required.ref_time, proofSize: raw.gas_required.proof_size },
    storageCharge: raw.storage_deposit.type === 'Charge' ? raw.storage_deposit.value : 0n,
    ...(debug ? { debugMessage: debug } : {}),
  };
}

/** The contract's events among a transaction's or a dry run's events. */
export function contractEvents(events: readonly RawEvent[], contract: Hex): ContractEvent[] {
  const out: ContractEvent[] = [];
  events.forEach((e, index) => {
    if (e.type !== 'Contracts' || e.value.type !== 'ContractEmitted') return;
    const v = e.value.value;
    const from = accountOf(v.contract);
    if (from !== contract) return;
    out.push({ contract: from, topics: (e.topics ?? []).map((t) => toHex(bytesOf(t))), data: bytesOf(v.data), index });
  });
  return out;
}

/** Kreivo's redeem surface on `k`, for the bezent contract `contract`. */
export function redeemChain(k: Kreivo, n: KreivoNetwork, contract: Hex, timeoutMs = READ_TIMEOUT_MS): RedeemChain {
  const api = k.api as Api;
  const ss58 = (account: Hex) => encodeSs58(fromHex(account), n.ss58Prefix);
  const dest = ss58(contract);
  const asset = { type: 'Here', value: n.asset.id };
  const late = `${n.label} did not answer within ${Math.round(timeoutMs / 1000)} s (the light client may still be syncing). Nothing was signed; try again in a moment.`;
  const read = <T>(f: () => Promise<T>) => withTimeout(timeoutMs, late, f);
  const call = async (origin: Hex, data: Uint8Array) => toDryRun(await api.apis.ContractsApi.call(ss58(origin), dest, 0n, undefined, undefined, data));

  return {
    contract,
    async facts(redeemer) {
      return read(async () => {
        const at = await k.client.getFinalizedBlock();
        const scopes = redeemScopes(n.asset.id);
        const [bounds, backends, pauses, account, system, approval, approvalDeposit] = await Promise.all([
          call(redeemer, boundsData(n.asset.id)).then(decodeBounds),
          call(redeemer, backendsData(n.asset.id)).then(decodeBackends),
          Promise.all(scopes.map(async ([scope, s]) => ({ scope, paused: decodeBool(await call(redeemer, pausedData(s)), 'paused') }))),
          api.query.Assets.Account.getValue(asset, ss58(redeemer)),
          api.query.System.Account.getValue(ss58(redeemer)),
          api.query.Assets.Approvals.getValue(asset, ss58(redeemer), dest),
          api.constants.Assets.ApprovalDeposit(),
        ]);
        return {
          bounds,
          backends,
          pauses,
          balance: account?.balance ?? 0n,
          assetStatus: account?.status.type,
          ksm: system.data.free,
          approval: approval ? { amount: approval.amount, deposit: approval.deposit } : undefined,
          approvalDeposit,
          at: { number: at.number, hash: at.hash as Hex },
        };
      });
    },
    redemption: (redeemer, reference) => read(async () => decodeRedemption(await call(redeemer, redemptionData(redeemer, reference)))),
    finalized: () => read(async () => (await k.client.getFinalizedBlock()).number),
    dryRunMessage: (redeemer, data) => read(() => call(redeemer, data)),
    async prepare(plan, gas, storageLimit) {
      const approve = api.tx.Assets.approve_transfer({ id: { type: 'Here', value: plan.asset }, delegate: { type: 'Id', value: dest }, amount: plan.amount });
      const redeem = api.tx.Contracts.call({
        dest: { type: 'Id', value: dest },
        value: 0n,
        gas_limit: { ref_time: gas.refTime, proof_size: gas.proofSize },
        storage_deposit_limit: storageLimit,
        data: plan.data,
      });
      const batch = api.tx.Utility.batch_all({ calls: [approve.decodedCall, redeem.decodedCall] });
      const options = { customSignedExtensions: KREIVO_CUSTOM_VALUES, mortality: { mortal: true, period: 64 } };
      const origin = { type: 'system', value: { type: 'Signed', value: ss58(plan.redeemer) } };
      const [callData, fee, dry] = await read(() =>
        Promise.all([
          batch.getEncodedData().then((d) => toHex(bytesOf(d))),
          batch.getEstimatedFees(ss58(plan.redeemer), options).catch(() => undefined),
          (async () => {
            // The result's XCM version does not matter here (the batch sends none); Kreivo takes 5, older runtimes 4.
            const v5 = await api.apis.DryRunApi.dry_run_call(origin, batch.decodedCall, 5);
            return v5.success ? v5 : api.apis.DryRunApi.dry_run_call(origin, batch.decodedCall, 4);
          })(),
        ]),
      );
      let dryRun: Prepared['dryRun'];
      if (!dry.success) {
        dryRun = { ok: false, error: `Kreivo could not dry-run the batch (${describeDispatch(dry.value)}).` };
      } else {
        const v = dry.value as { execution_result: { success: boolean; value: { error?: unknown } }; emitted_events: RawEvent[] };
        dryRun = v.execution_result.success
          ? { ok: true, emitted: contractEvents(v.emitted_events, contract) }
          : { ok: false, error: describeDispatch(v.execution_result.value.error) };
      }
      return {
        gas,
        storageLimit,
        fee,
        dryRun,
        callData,
        submit: (signer, progress) =>
          new Promise<Finalized>((resolve, reject) => {
            let hash: string | undefined;
            let broadcast = false;
            let birth: number | undefined;
            // The block the transaction is built at, which its mortality counts from.
            const watched: PolkadotSigner = {
              ...signer,
              signTx: (...a: Parameters<PolkadotSigner['signTx']>) => {
                birth = a[3];
                return signer.signTx(...a);
              },
            };
            // The observable completes once the transaction is final.
            batch.signSubmitAndWatch(watched, options).subscribe({
              next(e) {
                if (e.type === 'signed') {
                  hash = e.txHash;
                  progress(`Signed: ${e.txHash}. Kreivo checks it before it is broadcast.`);
                } else if (e.type === 'broadcasted') {
                  broadcast = true;
                  progress('Broadcast; waiting for a block to include it.');
                } else if (e.type === 'txBestBlocksState' && e.found) {
                  progress(`Included in block ${e.block?.number ?? '?'}, not final yet; waiting for finality.`);
                } else if (e.type === 'finalized') {
                  resolve({
                    txHash: e.txHash as Hex,
                    block: { hash: e.block.hash as Hex, number: e.block.number, index: e.block.index },
                    ok: e.ok,
                    ...(e.ok ? {} : { dispatchError: describeDispatch(e.dispatchError) }),
                    events: contractEvents(e.events, contract),
                  });
                }
              },
              error(err) {
                reject(submitError(err, { hash, broadcast, birth }));
              },
            });
          }),
      };
    },
  };
}

/** A redemption broadcast to Kreivo whose outcome the page could not follow: it may still be included until
 * `validUntil`, so its reference stays locked. */
export class UnclearOutcome extends Error {
  constructor(
    message: string,
    readonly txHash: Hex,
    readonly validUntil: number | undefined,
  ) {
    super(message);
    this.name = 'UnclearOutcome';
  }
}

/** What a failed submission means. polkadot-api's `InvalidTxError` (a BadProof among them) is Kreivo's refusal to
 * take the transaction at all, and anything before `broadcasted` sent nothing: only a failure after the broadcast
 * leaves the transaction possibly on chain. */
export function submitError(err: unknown, at: { hash: string | undefined; broadcast: boolean; birth: number | undefined }): Error {
  if (err instanceof WalletSignatureMismatch) return err;
  const m = err instanceof Error ? err.message : String(err);
  const invalid = (err as { name?: unknown } | null)?.name === 'InvalidTxError';
  if (invalid || !at.broadcast || !at.hash) {
    const text = explainSigningError(m.replace(/\s+/g, ' ').trim());
    return /BadProof|bad signature|Invalid signature/i.test(m) ? new WalletSignatureMismatch(text) : new Error(text);
  }
  const until = at.birth === undefined ? undefined : at.birth + MORTALITY;
  return new UnclearOutcome(
    `The redemption ${at.hash} was broadcast, but following it failed: ${m}. It may still be included${until === undefined ? '' : ` until block ${until}`}; its reference stays locked until Kreivo records it or it can no longer be included. Do not reload or close this page until it settles.`,
    at.hash as Hex,
    until,
  );
}

/** Says what a failed signature or submission most likely means. */
export function explainSigningError(message: string): string {
  if (/BadProof|bad signature|Invalid signature/i.test(message)) {
    return `Kreivo rejected the signature (${message}): the wallet signed another payload than the one built, most likely because it does not know Kreivo's PassAuthenticate signed extension. Offer the wallet Kreivo's metadata again (the button above), accept it, then try again. Nothing was executed.`;
  }
  if (/Cancelled|Rejected|denied/i.test(message)) return `The wallet did not sign (${message}). Nothing was sent.`;
  return `${message}. Nothing was sent.`;
}

/** Whether the contract recorded the mint reference `reference` (`mint_reference_used`): a `Minted` event with it
 * was emitted, since only a mint adds one (`INV-2`). */
export async function mintReferenceUsed(k: Kreivo, n: KreivoNetwork, contract: Hex, reference: Hex, timeoutMs = READ_TIMEOUT_MS): Promise<boolean> {
  const api = k.api as Api;
  const contractSs58 = encodeSs58(fromHex(contract), n.ss58Prefix);
  return withTimeout(timeoutMs, `${n.label} did not answer within ${Math.round(timeoutMs / 1000)} s.`, async () =>
    decodeBool(toDryRun(await api.apis.ContractsApi.call(contractSs58, contractSs58, 0n, undefined, undefined, mintReferenceUsedData(reference))), 'mint_reference_used'),
  );
}

/** The redemption (`redeemer`, `reference`) as the contract records it, read by anyone. */
export async function readRedemption(k: Kreivo, n: KreivoNetwork, contract: Hex, redeemer: Hex, reference: Hex, timeoutMs = READ_TIMEOUT_MS) {
  return redeemChain(k, n, contract, timeoutMs).redemption(redeemer, reference);
}

/** Kreivo's runtime as the wallet needs it: its signed extensions (checked: every one known to polkadot.js or
 * registered by the app) and the initialisation metadata to give the wallet. */
export async function kreivoRuntime(k: Kreivo, n: KreivoNetwork): Promise<{ extensions: string[]; def: MetadataDef }> {
  const api = k.api as Api;
  return withTimeout(READ_TIMEOUT_MS, `${n.label} did not answer within ${READ_TIMEOUT_MS / 1000} s.`, async () => {
    const at = await k.client.getFinalizedBlock();
    const [metadata, spec, version, v15] = await Promise.all([
      k.client.getMetadata(at.hash),
      k.client.getChainSpecData(),
      api.constants.System.Version(),
      api.apis.Metadata.metadata_at_version(15).catch(() => undefined),
    ]);
    const { identifiers } = metadataExtensions(metadata);
    checkExtensions(identifiers);
    const props = (spec.properties ?? {}) as { tokenDecimals?: number | number[]; tokenSymbol?: string | string[] };
    const first = <T>(v: T | T[] | undefined) => (Array.isArray(v) ? v[0] : v);
    const def = kreivoMetadataDef({
      label: n.label,
      genesisHash: spec.genesisHash as Hex,
      ss58Format: n.ss58Prefix,
      specVersion: version.spec_version,
      tokenDecimals: first(props.tokenDecimals) ?? 12,
      tokenSymbol: first(props.tokenSymbol) ?? 'KSM',
      ...(v15 ? { rawMetadata: bytesOf(v15) } : {}),
    });
    return { extensions: identifiers, def };
  });
}
