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

/** Parses a payout address on an EVM backend: 20 bytes, not zero, any case; returned checksummed. */
export function parsePayout(input: string): { ok: true; address: Address } | { ok: false; reason: string } {
  const t = input.trim();
  if (t === '') return { ok: false, reason: 'Enter the Polygon address the USDC is paid to.' };
  if (!/^0x[0-9a-fA-F]{40}$/.test(t)) return { ok: false, reason: 'A Polygon address is 0x and 40 hex digits.' };
  // All lower or all upper case carries no checksum; mixed case is an EIP-55 checksum, and must be right.
  const body = t.slice(2);
  if (body !== body.toLowerCase() && body !== body.toUpperCase() && !isAddress(t, { strict: true })) {
    return { ok: false, reason: 'This address’s mixed-case checksum is wrong; paste it again.' };
  }
  if (/^0x0{40}$/i.test(t)) return { ok: false, reason: 'The zero address cannot be paid.' };
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
