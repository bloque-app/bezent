// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The status page's reads on the origin network (F-018, T-018-17; REQ-ST-1, REQ-ST-8), every one through the
// connected wallet's provider, as the deposit's: the app runs no EVM RPC.
//
// - A directed deposit, by its coordinate (the transaction and the `Transfer` log's index): the `Transfer`/`Directed`
//   pair exactly as the EVM endpoint pairs it (evm/coordinate.ts), the block's finality, and the references it
//   derives (lib/refs.ts).
// - A payout from the reserve, by the on-chain reference it carries: the vault's delivery is one transaction holding
//   the token's `Transfer` from the reserve and the emitter's `Reference(bytes32 indexed)` (F-008, DEC-10), so the
//   page looks for that log, then reads its transaction and takes the transfer from it. A `Reference` with no
//   transfer from the reserve in its transaction is not a payout (F-008 NOTES), and is shown as such.

import { decodeEventLog, getAddress, type Address, type Hex, type PublicClient } from 'viem';
import { directedArrivals, type DirectedArrival } from '../evm/coordinate';
import { tokenAbi } from '../evm/abi';
import type { ResolvedNetwork } from '../config/networks';

export const referenceEmitterAbi = [
  { type: 'event', name: 'Reference', inputs: [{ name: 'ref', type: 'bytes32', indexed: true }] },
] as const;

export type Reads = Pick<PublicClient, 'getTransactionReceipt' | 'getBlock' | 'getBlockNumber' | 'getLogs'>;

export interface Finality {
  blockNumber: bigint;
  blockHash: Hex;
  /** The network's finalized head when read. */
  finalized: bigint;
  final: boolean;
  /** Whether the block at that height is still this one (a reorganisation would drop the coordinate). */
  canonical: boolean;
}

async function finality(pc: Reads, blockNumber: bigint, blockHash: Hex): Promise<Finality> {
  const [head, at] = await Promise.all([pc.getBlock({ blockTag: 'finalized' }), pc.getBlock({ blockNumber })]);
  return { blockNumber, blockHash, finalized: head.number, final: blockNumber <= head.number, canonical: at.hash === blockHash };
}

export type DepositOnPolygon =
  | { found: false; reason: string }
  | {
      found: true;
      txHash: Hex;
      arrival: DirectedArrival;
      /** The `Directed` log's index: right after the `Transfer`'s. */
      directedLog: number;
      finality: Finality;
      /** Other `Directed` arrivals in the same transaction, if any. */
      others: number;
    };

/** The directed deposit at (`txHash`, `transferLog`): the pair, or why there is none. */
export async function depositOnPolygon(pc: Reads, n: ResolvedNetwork, txHash: Hex, transferLog: number): Promise<DepositOnPolygon> {
  let receipt;
  try {
    receipt = await pc.getTransactionReceipt({ hash: txHash });
  } catch (e) {
    return { found: false, reason: `No receipt for ${txHash} on ${n.label} (${(e as Error).message.split('\n')[0]}).` };
  }
  if (receipt.status !== 'success') return { found: false, reason: `The transaction ${txHash} reverted: it moved nothing.` };
  const arrivals = directedArrivals(receipt.logs, { network: n.network, contract: n.directedDeposit, token: n.token, reserve: n.reserve });
  const arrival = arrivals.find((a) => a.coordinate.positionIndex === transferLog);
  if (!arrival) {
    const at = arrivals.map((a) => a.coordinate.positionIndex);
    return {
      found: false,
      reason: at.length
        ? `Log ${transferLog} of ${txHash} is not a directed arrival's Transfer; this transaction's are at ${at.join(', ')}.`
        : `${txHash} holds no Transfer to the reserve paired with a Directed log of ${n.directedDeposit}: it is not a directed deposit.`,
    };
  }
  return {
    found: true,
    txHash,
    arrival,
    directedLog: transferLog + 1,
    finality: await finality(pc, arrival.coordinate.blockNumber, arrival.coordinate.blockHash),
    others: arrivals.length - 1,
  };
}

export interface Payout {
  txHash: Hex;
  referenceLog: number;
  finality: Finality;
  /** The token's transfers from the reserve in the same transaction. */
  transfers: { to: Address; amount: bigint; logIndex: number }[];
}

export interface PayoutSearch {
  found: Payout | undefined;
  /** The blocks searched, newest first; `complete` when they reach `from`. */
  searched: { from: bigint; to: bigint };
  complete: boolean;
}

export interface SearchOptions {
  /** Blocks per `eth_getLogs` to begin with; halved when a provider refuses the range. */
  chunk?: bigint;
  /** At most this many `eth_getLogs` calls. */
  maxRequests?: number;
}

/** Looks for the payout from the reserve carrying `reference` on `n`, from its newest block back to `from`. */
export async function findPayout(pc: Reads, n: ResolvedNetwork, emitter: Address, from: bigint, reference: Hex, o: SearchOptions = {}): Promise<PayoutSearch> {
  let chunk = o.chunk ?? 50_000n;
  const maxRequests = o.maxRequests ?? 60;
  const latest = await pc.getBlockNumber();
  let to = latest;
  let requests = 0;
  while (to >= from && requests < maxRequests) {
    const lo = to - chunk + 1n > from ? to - chunk + 1n : from;
    requests++;
    let logs;
    try {
      logs = await pc.getLogs({ address: emitter, event: referenceEmitterAbi[0], args: { ref: reference }, fromBlock: lo, toBlock: to, strict: true });
    } catch (e) {
      if (chunk > 1_000n) {
        chunk /= 2n;
        continue;
      }
      throw new Error(`${n.label} refused the log search (${(e as Error).message.split('\n')[0]}).`);
    }
    const hit = logs.find((l) => l.args.ref?.toLowerCase() === reference.toLowerCase());
    if (hit && hit.transactionHash && hit.blockHash && hit.logIndex !== null) {
      return { found: await payoutAt(pc, n, hit.transactionHash, hit.logIndex, hit.blockNumber, hit.blockHash), searched: { from: lo, to: latest }, complete: true };
    }
    if (lo === from) return { found: undefined, searched: { from, to: latest }, complete: true };
    to = lo - 1n;
  }
  return { found: undefined, searched: { from: to + 1n, to: latest }, complete: false };
}

/** The payout whose `Reference` log is at (`txHash`, `referenceLog`): its transaction's transfers from the
 * reserve, and its finality. */
export async function payoutAt(pc: Reads, n: ResolvedNetwork, txHash: Hex, referenceLog: number, blockNumber: bigint, blockHash: Hex): Promise<Payout> {
  const receipt = await pc.getTransactionReceipt({ hash: txHash });
  const transfers: Payout['transfers'] = [];
  for (const l of receipt.logs) {
    if (getAddress(l.address) !== n.token || l.logIndex === null) continue;
    try {
      const t = decodeEventLog({ abi: tokenAbi, eventName: 'Transfer', topics: l.topics as [Hex], data: l.data });
      const a = t.args as { from: Address; to: Address; value: bigint };
      if (getAddress(a.from) === n.reserve) transfers.push({ to: getAddress(a.to), amount: a.value, logIndex: l.logIndex });
    } catch {
      // Not a Transfer.
    }
  }
  return { txHash, referenceLog, finality: await finality(pc, blockNumber, blockHash), transfers };
}

/** Whether a payout pays exactly `amount` to `to`, in one transfer from the reserve (no fee comes out of a
 * delivery, REQ-TR-5). Names the difference otherwise. */
export function checkPayout(p: Payout, to: Address, amount: bigint): { ok: true } | { ok: false; reason: string } {
  if (p.transfers.length === 0) return { ok: false, reason: 'Its transaction moves nothing from the reserve: a reference with no transfer is not a payout.' };
  if (p.transfers.length > 1) return { ok: false, reason: `Its transaction holds ${p.transfers.length} transfers from the reserve, not one.` };
  const t = p.transfers[0]!;
  const diffs: string[] = [];
  if (t.to !== getAddress(to)) diffs.push(`to ${t.to}, not ${getAddress(to)}`);
  if (t.amount !== amount) diffs.push(`${t.amount}, not ${amount} (minor units)`);
  return diffs.length ? { ok: false, reason: `The payout differs: ${diffs.join('; ')}.` } : { ok: true };
}
