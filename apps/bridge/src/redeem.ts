// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The locked redeem (F-018, T-018-15; US-B5, REQ-RQ-9, REQ-OT-2), as the deposit's locked review (review.ts) does it.
// At the Redeem click the page takes a frozen snapshot of what the user reviewed: the redeemer, the asset, the
// amount, the backend, the payout address and the reference. Every input stays locked until the redemption ends.
// The flow then reads the contract again on the snapshot (bounds, backends, pauses, the balance, the reference),
// dry-runs `redeem` through the contract and the whole batch through Kreivo, signs and submits the snapshot, and
// after finality decodes the contract's `Redeemed` event and compares it with the snapshot, failing loudly on any
// difference. Kept apart from the DOM, so the tests drive it.

import type { Address } from 'viem';
import { getAddress, isAddress } from 'viem';
import {
  backendText,
  decodeRedeemDryRun,
  decodeRedeemed,
  isPullRefusal,
  redeemData,
  REDEEMED_TOPIC,
  sameBackend,
  type Backend,
  type Redeemed,
  type Redemption,
} from './kreivo/bezent';
import { REDEEM_GAS_FLOOR, REDEEM_STORAGE_FLOOR, type Finalized, type Prepared, type RedeemChain, type RedeemFacts } from './kreivo/redeem';
import type { PolkadotSigner } from 'polkadot-api/signer';
import { formatAmount } from './lib/amount';
import { encodeSs58 } from './lib/ss58';
import { fromHex, toHex, type Hex } from './lib/bytes';
import { withdrawalReference } from './lib/refs';

/** What the user reviewed and the redemption sends, frozen at the click. */
export interface RedeemSnapshot {
  readonly redeemer: { readonly account: Hex; readonly address: string };
  readonly asset: { readonly id: number; readonly symbol: string; readonly decimals: number };
  readonly amount: bigint;
  /** Whether the user asked for the whole balance: the amount is the balance read at the click, and the redemption
   * is refused if the balance moved since. */
  readonly whole: boolean;
  readonly backend: Backend;
  /** The payout address on the backend's network, checksummed. */
  readonly payout: Address;
  readonly reference: Hex;
}

/** A fresh, random 32-byte reference (`REQ-PV-2`: never sequential). */
export function freshReference(random: (b: Uint8Array) => Uint8Array = (b) => crypto.getRandomValues(b)): Hex {
  const b = random(new Uint8Array(32));
  if (b.every((x) => x === 0)) throw new Error('The random source gave zeros.');
  return toHex(b);
}

/** One of bezent's own contracts on the payout's network, pinned in the build. */
export interface OwnContract {
  what: string;
  address: Address;
}

/** bezent's own contracts on `n` that the build pins: the token, the reserve, the DirectedDeposit contract and the
 * vault's reference emitter. USDC paid to one of them never reaches the redeemer, so a payout address equal to one
 * is refused. An unset value is left out. */
export function ownContracts(n: {
  token: Address | null | undefined;
  reserve: Address | null | undefined;
  directedDeposit: Address | null | undefined;
  emitter: Address | null | undefined;
}): OwnContract[] {
  const named: [string, Address | null | undefined][] = [
    ['the USDC token contract', n.token],
    ['bezent’s reserve', n.reserve],
    ['bezent’s DirectedDeposit contract', n.directedDeposit],
    ['bezent’s reference emitter', n.emitter],
  ];
  return named.flatMap(([what, a]) => (a && isAddress(a, { strict: false }) ? [{ what, address: getAddress(a) }] : []));
}

/** Parses a payout address on an EVM backend: 20 bytes, not zero, any case, and none of bezent's own contracts
 * (`own`, compared case-insensitively); returned checksummed. */
export function parsePayout(input: string, own: readonly OwnContract[] = []): { ok: true; address: Address } | { ok: false; reason: string } {
  const t = input.trim();
  if (t === '') return { ok: false, reason: 'Enter the Polygon address the USDC is paid to.' };
  if (!/^0x[0-9a-fA-F]{40}$/.test(t)) return { ok: false, reason: 'A Polygon address is 0x and 40 hex digits.' };
  // All lower or all upper case carries no checksum; mixed case is an EIP-55 checksum, and must be right.
  const body = t.slice(2);
  if (body !== body.toLowerCase() && body !== body.toUpperCase() && !isAddress(t, { strict: true })) {
    return { ok: false, reason: 'This address’s mixed-case checksum is wrong; paste it again.' };
  }
  if (/^0x0{40}$/i.test(t)) return { ok: false, reason: 'The zero address cannot be paid.' };
  const mine = own.find((o) => o.address.toLowerCase() === t.toLowerCase());
  if (mine) {
    return {
      ok: false,
      reason: `${getAddress(t)} is ${mine.what}, not yours: USDC paid to it would not reach you. Enter an address you control.`,
    };
  }
  return { ok: true, address: getAddress(t) };
}

export function takeRedeemSnapshot(live: {
  redeemer?: { account: Hex };
  asset?: { id: number; symbol: string; decimals: number };
  amount?: bigint;
  whole?: boolean;
  backend?: Backend;
  payout?: Address;
  reference?: Hex;
}, ss58Prefix: number): RedeemSnapshot | undefined {
  const { redeemer, asset, amount, backend, payout, reference } = live;
  if (!redeemer || !asset || amount === undefined || !backend || !payout || !reference) return undefined;
  return Object.freeze({
    redeemer: Object.freeze({ account: redeemer.account, address: encodeSs58(fromHex(redeemer.account), ss58Prefix) }),
    asset: Object.freeze({ ...asset }),
    amount,
    whole: !!live.whole,
    backend: Object.freeze({ ...backend }),
    payout: getAddress(payout),
    reference,
  });
}

/** The review's rows for a snapshot. */
export function redeemRows(s: RedeemSnapshot): [string, string][] {
  return [
    ['Redeemer (Kreivo)', s.redeemer.address],
    ['Amount', `${formatAmount(s.amount, s.asset.decimals)} ${s.asset.symbol} (Here(${s.asset.id}))${s.whole ? ', the whole balance' : ''}`],
    ['Backend', backendText(s.backend)],
    ['Paid to', s.payout],
    ['Reference', s.reference],
    ['Bridge reference', withdrawalReference(s.redeemer.account, s.reference)],
  ];
}

/** Checks a redemption against the contract's facts. Returns the reason it would be refused, or `undefined`. */
export function refusal(f: RedeemFacts, a: { amount: bigint; backend: Backend; whole: boolean; decimals: number; symbol: string }): string | undefined {
  const fmt = (u: bigint) => `${formatAmount(u, a.decimals)} ${a.symbol}`;
  if (!f.bounds) return 'The contract does not know this asset, so it cannot be redeemed.';
  const paused = f.pauses.filter((p) => p.paused).map((p) => p.scope);
  if (paused.length) return `Redemptions are paused (${paused.join(', ')}).`;
  if (!f.backends.some((b) => sameBackend(b, a.backend))) {
    return `The contract does not declare the backend ${backendText(a.backend)} for this asset (it declares ${f.backends.map(backendText).join(', ') || 'none'}).`;
  }
  if (f.assetStatus && f.assetStatus !== 'Liquid') return `This account’s ${a.symbol} is ${f.assetStatus.toLowerCase()}, so it cannot move.`;
  if (a.amount <= 0n) return 'Nothing to redeem.';
  if (a.amount < f.bounds.redeemMinimum) return `The route’s redemption minimum is ${fmt(f.bounds.redeemMinimum)}.`;
  if (a.amount > f.balance) return `This account holds ${fmt(f.balance)}.`;
  if (a.whole && a.amount !== f.balance) return `The balance moved since the review (it is now ${fmt(f.balance)}); review it again.`;
  return undefined;
}

export type RedeemedComparison = { ok: true; redeemed: Redeemed } | { ok: false; reason: string; redeemed: Redeemed | undefined };

/** Compares the `Redeemed` events a transaction emitted with the snapshot: exactly one, and its reference,
 * redeemer, asset, amount, backend and payout must be the snapshot's. */
export function compareRedeemed(events: readonly { topics: Hex[]; data: Uint8Array }[], s: RedeemSnapshot): RedeemedComparison {
  const decoded: Redeemed[] = [];
  for (const e of events) {
    try {
      decoded.push(decodeRedeemed(e.topics, e.data));
    } catch {
      // Another of the contract's events, or one that does not decode: not this redemption.
    }
  }
  const seen = decoded[0];
  if (!seen) return { ok: false, reason: 'The transaction emitted no Redeemed event of the contract.', redeemed: undefined };
  if (decoded.length > 1) return { ok: false, reason: `The transaction emitted ${decoded.length} Redeemed events, not one.`, redeemed: seen };
  const diffs: string[] = [];
  if (seen.reference !== s.reference) diffs.push(`reference ${seen.reference}, not ${s.reference}`);
  if (seen.redeemer !== s.redeemer.account) diffs.push(`redeemer ${seen.redeemer}, not ${s.redeemer.account}`);
  if (seen.asset !== s.asset.id) diffs.push(`asset Here(${seen.asset}), not Here(${s.asset.id})`);
  if (seen.amount !== s.amount) diffs.push(`amount ${seen.amount}, not ${s.amount} (minor units)`);
  if (!sameBackend(seen.backend, s.backend)) diffs.push(`backend ${backendText(seen.backend)}, not ${backendText(s.backend)}`);
  const payout = seen.payout.kind === 'address' ? seen.payout.address : seen.payout.commitment;
  if (seen.payout.kind !== 'address' || payout !== s.payout.toLowerCase()) diffs.push(`payout ${payout}, not ${s.payout}`);
  if (diffs.length) return { ok: false, reason: `The Redeemed event differs from what you reviewed: ${diffs.join('; ')}.`, redeemed: seen };
  return { ok: true, redeemed: seen };
}

/** A dry run's events are bare (`RuntimeEvent`, no record): its `ContractEmitted` carries no topics, so the
 * comparison stands on the decoded fields alone. `seq` is 8 bytes, then the reference and the redeemer, which are
 * the event's two topics. */
const withOwnTopics = (data: Uint8Array) =>
  data.length < 72 ? { topics: [] as Hex[], data } : { topics: [REDEEMED_TOPIC, toHex(data.slice(8, 40)), toHex(data.slice(40, 72))], data };

export interface RedeemOutcome {
  prepared: Prepared;
  finalized: Finalized;
  comparison: RedeemedComparison;
}

export type Progress = (step: string, kind?: 'muted' | 'ok' | 'error') => void;

/** Twice what the dry run needed, and never under the proven floors. */
export function limits(d: { gasRequired: { refTime: bigint; proofSize: bigint }; storageCharge: bigint }) {
  const max = (a: bigint, b: bigint) => (a > b ? a : b);
  return {
    gas: { refTime: max(2n * d.gasRequired.refTime, REDEEM_GAS_FLOOR.refTime), proofSize: max(2n * d.gasRequired.proofSize, REDEEM_GAS_FLOOR.proofSize) },
    storageLimit: max(2n * d.storageCharge, REDEEM_STORAGE_FLOOR),
  };
}

/** The KSM a redemption may need before it is signed: the estimated fee, the storage deposit limit, and the
 * approval's deposit when the batch's `approve_transfer` opens a new approval (an existing one already holds its
 * deposit, and the batch adds to it). */
export function ksmNeeded(f: Pick<RedeemFacts, 'approval' | 'approvalDeposit'>, p: Pick<Prepared, 'fee' | 'storageLimit'>) {
  const fee = p.fee ?? 0n;
  const approval = f.approval ? 0n : f.approvalDeposit;
  return { fee, storage: p.storageLimit, approval, total: fee + p.storageLimit + approval, feeKnown: p.fee !== undefined };
}

/** Why the redeemer's free KSM is short of what the redemption may need, or `undefined`. */
export function ksmShortfall(f: Pick<RedeemFacts, 'ksm' | 'approval' | 'approvalDeposit'>, p: Pick<Prepared, 'fee' | 'storageLimit'>): string | undefined {
  const n = ksmNeeded(f, p);
  if (f.ksm >= n.total) return undefined;
  const ksm = (u: bigint) => `${formatAmount(u, 12)} KSM`;
  const parts = [
    n.feeKnown ? `the fee (about ${ksm(n.fee)})` : 'the fee (which could not be estimated, so it is not counted)',
    `the storage deposit limit (${ksm(n.storage)})`,
    ...(n.approval > 0n ? [`the approval’s deposit (${ksm(n.approval)}, returned when the redemption uses the approval up)`] : []),
  ];
  return `This account holds ${ksm(f.ksm)} free; the redemption may need up to ${ksm(n.total)}: ${parts.join(', ')}. Add KSM to it, then try again.`;
}

/** A redemption whose outcome is unclear: it was broadcast, but following it failed, so it may still be included
 * until its mortality runs out. Its reference stays locked until the contract records the redemption or the
 * transaction can no longer be included. */
export interface Unsettled {
  redeemer: Hex;
  reference: Hex;
  txHash: Hex;
  /** The last block that may include the transaction (its birth plus its 64-block mortality); `undefined` when
   * unknown, and then only the contract's record settles it. */
  validUntil: number | undefined;
}

export type Settled = { kind: 'recorded'; redemption: Redemption; at: number } | { kind: 'expired'; at: number };

/** What one turn of `settle` read: the finalized head it checked, or why the read failed. Reported for display only;
 * it decides nothing. */
export type SettleRead = { ok: true; head: number } | { ok: false; error: string };

/** Waits until an unclear redemption settles: the contract records it (read at the finalized block), or the
 * finalized head is past its mortality and no record exists. A failed read is tried again. `chain` may answer
 * `undefined` (the page moved to another network), which skips a turn; `stopped` ends the wait unsettled. `onRead`
 * hears each read's outcome, so a watcher that keeps failing is not silent; it changes nothing `settle` returns. */
export async function settle(
  u: Unsettled,
  chain: () => RedeemChain | undefined,
  o: { sleep: (ms: number) => Promise<void>; intervalMs?: number; stopped?: () => boolean; onRead?: (r: SettleRead) => void },
): Promise<Settled | undefined> {
  const heard = (r: SettleRead) => {
    try {
      o.onRead?.(r);
    } catch {
      // A display that fails must not stop the wait.
    }
  };
  for (;;) {
    if (o.stopped?.()) return undefined;
    const c = chain();
    if (c) {
      let head: number | undefined;
      let red: Redemption | undefined;
      try {
        // The head first: a redemption final at or before it shows in the read that follows.
        head = await c.finalized();
        red = await c.redemption(u.redeemer, u.reference);
      } catch (e) {
        // Read again next turn.
        heard({ ok: false, error: e instanceof Error ? e.message : String(e) });
        head = undefined;
      }
      if (head !== undefined) {
        heard({ ok: true, head });
        if (red) return { kind: 'recorded', redemption: red, at: head };
        if (u.validUntil !== undefined && head > u.validUntil) return { kind: 'expired', at: head };
      }
    }
    await o.sleep(o.intervalMs ?? 12_000);
  }
}

/** An unclear redemption as the page keeps it: the lock, and what is needed to compare the record with what was
 * reviewed. */
export interface Pending extends Unsettled {
  /** The origin network the redemption's backend is on (`key`) and its label. */
  network: string;
  networkLabel: string;
  /** The Kreivo network it was sent on. */
  kreivo: string;
  snapshot: RedeemSnapshot;
}

/** The storage key prefix, versioned: a record of another version is not read. */
export const PENDING_PREFIX = 'bezent.bridge.pending/v1/';
/** One key per Kreivo network and redeemer. */
export const pendingKey = (kreivo: string, redeemer: Hex) => `${PENDING_PREFIX}${kreivo}/${redeemer}`;

/** The record as text for storage (`bigint`s as decimal strings). */
export function encodePending(p: Pending): string {
  const s = p.snapshot;
  return JSON.stringify({
    v: 1,
    network: p.network,
    networkLabel: p.networkLabel,
    kreivo: p.kreivo,
    redeemer: p.redeemer,
    reference: p.reference,
    txHash: p.txHash,
    validUntil: p.validUntil ?? null,
    snapshot: {
      redeemer: s.redeemer,
      asset: s.asset,
      amount: s.amount.toString(),
      whole: s.whole,
      backend: s.backend.kind === 'evm' ? { kind: 'evm', chainId: s.backend.chainId.toString() } : s.backend,
      payout: s.payout,
      reference: s.reference,
    },
  });
}

const U32 = 2 ** 32 - 1;

/** What a stored record is checked against: an origin network of this build, and the issuing network it mints on. */
export interface PendingConfig {
  key: string;
  label: string;
  chainId: number;
  kreivo?: { network: string; ss58Prefix: number; asset: { id: number; symbol: string; decimals: number } };
}

/** Why a record that decodes is still not one this build could have written: it names a network the build does not
 * carry, or an address, asset, backend or payout the network's configuration contradicts. `undefined` when it holds.
 * A record that fails is as unreadable as a malformed one (it locks, and is cleared only on confirmation). */
export function pendingProblem(p: Pending, networks: readonly PendingConfig[]): string | undefined {
  const n = networks.find((x) => x.key === p.network);
  if (!n?.kreivo) return `the network ${p.network} is not one this build carries`;
  const k = n.kreivo;
  const s = p.snapshot;
  if (p.networkLabel !== n.label) return 'its network label does not match';
  if (p.kreivo !== k.network) return `it was sent on ${p.kreivo}, not ${k.network}`;
  if (s.redeemer.address !== encodeSs58(fromHex(p.redeemer), k.ss58Prefix)) return 'its redeemer address does not match its account';
  if (s.asset.id !== k.asset.id || s.asset.decimals !== k.asset.decimals || s.asset.symbol !== k.asset.symbol) return 'its asset is not the network’s';
  if (s.backend.kind !== 'evm' || s.backend.chainId !== BigInt(n.chainId)) return 'its backend is not the network’s';
  return undefined;
}

/** Reads a stored record back; `undefined` when it is not one this version wrote, whole and well formed. */
export function decodePending(text: string): Pending | undefined {
  try {
    const j = JSON.parse(text);
    const hex = (x: unknown, bytes: number): x is Hex => typeof x === 'string' && new RegExp(`^0x[0-9a-f]{${bytes * 2}}$`).test(x);
    const str = (x: unknown): x is string => typeof x === 'string' && x.length > 0;
    const uint = (x: unknown): x is string => typeof x === 'string' && /^[0-9]{1,40}$/.test(x);
    const s = j?.snapshot;
    if (j?.v !== 1 || !str(j.network) || !str(j.networkLabel) || !str(j.kreivo) || !hex(j.redeemer, 32) || !hex(j.reference, 32) || !hex(j.txHash, 32)) return undefined;
    // A block number is a u32. Absent or 0 means no expiry is known, and then only the contract's record settles it;
    // anything else must be a positive integer, so a tampered value cannot be read as one.
    if (j.validUntil !== null && !(Number.isSafeInteger(j.validUntil) && j.validUntil >= 0 && j.validUntil <= U32)) return undefined;
    if (!s || !hex(s.redeemer?.account, 32) || !str(s.redeemer?.address) || !Number.isSafeInteger(s.asset?.id) || s.asset.id < 0 || s.asset.id > U32 || !str(s.asset?.symbol)) return undefined;
    if (!Number.isSafeInteger(s.asset?.decimals) || s.asset.decimals < 0 || s.asset.decimals > 36) return undefined;
    if (!uint(s.amount) || typeof s.whole !== 'boolean' || !isAddress(s.payout, { strict: false }) || !hex(s.reference, 32)) return undefined;
    if (s.redeemer.account !== j.redeemer || s.reference !== j.reference) return undefined;
    let backend: Backend;
    if (s.backend?.kind === 'evm' && uint(s.backend.chainId)) backend = { kind: 'evm', chainId: BigInt(s.backend.chainId) };
    else if (s.backend?.kind === 'attested' && Number.isSafeInteger(s.backend.id)) backend = { kind: 'attested', id: s.backend.id };
    else return undefined;
    const snapshot: RedeemSnapshot = Object.freeze({
      redeemer: Object.freeze({ account: s.redeemer.account as Hex, address: s.redeemer.address as string }),
      asset: Object.freeze({ id: s.asset.id as number, symbol: s.asset.symbol as string, decimals: s.asset.decimals as number }),
      amount: BigInt(s.amount),
      whole: s.whole as boolean,
      backend: Object.freeze(backend),
      payout: getAddress(s.payout),
      reference: s.reference as Hex,
    });
    return {
      redeemer: j.redeemer,
      reference: j.reference,
      txHash: j.txHash,
      validUntil: j.validUntil === null || j.validUntil === 0 ? undefined : j.validUntil,
      network: j.network,
      networkLabel: j.networkLabel,
      kreivo: j.kreivo,
      snapshot,
    };
  } catch {
    return undefined;
  }
}

/** Whether the redemption Kreivo recorded is the one that was reviewed. */
export function sameRedemption(red: Redemption, s: RedeemSnapshot): boolean {
  return red.amount === s.amount && red.asset === s.asset.id && sameBackend(red.backend, s.backend) && red.payout.kind === 'address' && red.payout.address === s.payout.toLowerCase();
}

/** Runs the redemption of a snapshot. Throws, before anything is signed, when the contract or the dry runs refuse
 * it; after signing, the comparison says whether what Kreivo recorded is what was reviewed. */
export async function runRedeem(
  s: RedeemSnapshot,
  chain: RedeemChain,
  signer: PolkadotSigner,
  progress: Progress,
  confirm: (p: Prepared) => void = () => undefined,
): Promise<RedeemOutcome> {
  if (toHex(signer.publicKey) !== s.redeemer.account) throw new Error('The wallet’s account changed after the review; nothing was signed.');
  progress('Reading the contract again: bounds, backends, pauses, your balance and the reference.');
  const [facts, used] = await Promise.all([chain.facts(s.redeemer.account), chain.redemption(s.redeemer.account, s.reference)]);
  const why = refusal(facts, { amount: s.amount, backend: s.backend, whole: s.whole, decimals: s.asset.decimals, symbol: s.asset.symbol });
  if (why) throw new Error(`${why} Nothing was signed.`);
  if (used) throw new Error('You used this reference for a redemption before; pick a new one. Nothing was signed.');

  progress('Dry-running redeem through the contract.');
  const data = redeemData(s.asset.id, s.amount, s.backend, fromHex(s.payout.toLowerCase()), s.reference);
  const dry = await chain.dryRunMessage(s.redeemer.account, data);
  const verdict = decodeRedeemDryRun(dry);
  if (!verdict.ok && !isPullRefusal(verdict)) throw new Error(`The contract refuses this redemption: ${verdict.why} Nothing was signed.`);

  progress('Dry-running the whole batch (approve_transfer, then redeem) through Kreivo.');
  const { gas, storageLimit } = limits(dry);
  const prepared = await chain.prepare({ redeemer: s.redeemer.account, asset: s.asset.id, amount: s.amount, data }, gas, storageLimit);
  // The clearer reason first: a batch short of KSM also fails its dry run.
  const short = ksmShortfall(facts, prepared);
  if (short) throw new Error(`${short} Nothing was signed.`);
  if (!prepared.dryRun.ok) {
    const hint = /InsufficientBalance|Payment|Funds/i.test(prepared.dryRun.error)
      ? ' The account needs a little KSM for the fee and the storage deposit.'
      : !verdict.ok
        ? ` The contract said: ${verdict.why}`
        : '';
    throw new Error(`Kreivo’s dry run of the batch fails (${prepared.dryRun.error}).${hint} Nothing was signed.`);
  }
  const planned = compareRedeemed(prepared.dryRun.emitted.map((e) => withOwnTopics(e.data)), s);
  if (!planned.ok) throw new Error(`The batch’s dry run would not record what you reviewed: ${planned.reason} Nothing was signed.`);
  confirm(prepared);

  progress('Sign the redemption in your Kreivo wallet.');
  const finalized = await prepared.submit(signer, (t) => progress(t));
  if (!finalized.ok) {
    throw new Error(`The redemption ${finalized.txHash} is final in block ${finalized.block.number} but failed (${finalized.dispatchError}); nothing was redeemed.`);
  }
  return { prepared, finalized, comparison: compareRedeemed(finalized.events, s) };
}
