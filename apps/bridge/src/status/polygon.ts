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
//   page looks for that log, then reads its transaction and takes the transfer from it. The emitter's
//   `emitReference` is open to anyone, so a `Reference` log proves nothing by itself: the page collects every one
//   carrying the reference in the range searched, and only one whose transaction moves exactly one transfer from
//   the reserve is a payout. The others (no transfer from the reserve, or several) are not payouts (F-008 NOTES),
//   are listed as such, and never decide a state.

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
  | { found: false; reason: string; /** The Transfer indexes to pick from, when the transaction holds several deposits. */ pick?: number[] }
  | {
      found: true;
      txHash: Hex;
      arrival: DirectedArrival;
      /** The `Directed` log's index: right after the `Transfer`'s. */
      directedLog: number;
      finality: Finality;
      /** Other `Directed` arrivals in the same transaction, if any. */
      others: number;
      /** The Transfer's index was not given: it was found from the receipt, as the transaction's only directed
       * arrival. */
      fromReceipt: boolean;
    };

/** The directed deposit at (`txHash`, `transferLog`): the pair, or why there is none. Without `transferLog`, the
 * transaction's own directed arrival: the one pair there is, never a guess among several (their indexes are named
 * for the user to pick one). */
export async function depositOnPolygon(pc: Reads, n: ResolvedNetwork, txHash: Hex, transferLog?: number): Promise<DepositOnPolygon> {
  let receipt;
  try {
    receipt = await pc.getTransactionReceipt({ hash: txHash });
  } catch (e) {
    return { found: false, reason: `No receipt for ${txHash} on ${n.label} (${(e as Error).message.split('\n')[0]}).` };
  }
  if (receipt.status !== 'success') return { found: false, reason: `The transaction ${txHash} reverted: it moved nothing.` };
  const arrivals = directedArrivals(receipt.logs, { network: n.network, contract: n.directedDeposit, token: n.token, reserve: n.reserve });
  const fromReceipt = transferLog === undefined;
  if (fromReceipt && arrivals.length !== 1) {
    const at = arrivals.map((a) => a.coordinate.positionIndex);
    return {
      found: false,
      ...(at.length ? { pick: at } : {}),
      reason: at.length
        ? `${txHash} holds ${at.length} directed deposits, with Transfer logs at ${at.join(', ')}: enter the index of the one to look up.`
        : `no directed deposit in this transaction: ${txHash} holds no Transfer of ${n.token} to the reserve paired with a Directed log of ${n.directedDeposit}.`,
    };
  }
  const arrival = fromReceipt ? arrivals[0] : arrivals.find((a) => a.coordinate.positionIndex === transferLog);
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
    directedLog: arrival.coordinate.positionIndex + 1,
    finality: await finality(pc, arrival.coordinate.blockNumber, arrival.coordinate.blockHash),
    others: arrivals.length - 1,
    fromReceipt,
  };
}

/** A `Reference` log carrying the reference, and its transaction's transfers from the reserve. */
export interface ReferenceLog {
  txHash: Hex;
  referenceLog: number;
  blockNumber: bigint;
  blockHash: Hex;
  /** The token's transfers from the reserve in the same transaction. */
  transfers: { to: Address; amount: bigint; logIndex: number }[];
}

/** A payout: a `Reference` log whose transaction moves exactly one transfer from the reserve, with its finality. */
export interface Payout extends ReferenceLog {
  finality: Finality;
}

export interface PayoutSearch {
  /** The payout carrying the reference: the newest `Reference` log whose transaction moves exactly one transfer
   * from the reserve. */
  found: Payout | undefined;
  /** Other payouts carrying the same reference, older than `found`: one reference is paid once, so any is an
   * incident. */
  others: Payout[];
  /** `Reference` logs carrying the reference whose transaction moves nothing, or more than one transfer, from the
   * reserve. Anyone can emit these: they are no payout, and decide nothing. */
  unpaid: ReferenceLog[];
  /** `Reference` logs found but not examined, past `maxHits` (those whose transaction moves nothing from the
   * reserve go last). When there are any and nothing was found, whether a payout exists is not determined. */
  unexamined: number;
  /** The blocks searched, newest first; `complete` when they reach `from`. */
  searched: { from: bigint; to: bigint };
  complete: boolean;
}

export interface SearchOptions {
  /** Blocks per `eth_getLogs` to begin with; halved when a provider refuses the range. */
  chunk?: bigint;
  /** At most this many `eth_getLogs` calls. */
  maxRequests?: number;
  /** At most this many `Reference` logs' transactions are read (anyone can emit them). */
  maxHits?: number;
}

/** Looks for the payout from the reserve carrying `reference` on `n`, from its newest block back to `from`. Every
 * `Reference` log carrying it in the range is collected, and each one's transaction is read: only one that moves
 * exactly one transfer from the reserve is a payout. */
export async function findPayout(pc: Reads, n: ResolvedNetwork, emitter: Address, from: bigint, reference: Hex, o: SearchOptions = {}): Promise<PayoutSearch> {
  let chunk = o.chunk ?? 50_000n;
  const maxRequests = o.maxRequests ?? 60;
  const maxHits = o.maxHits ?? 100;
  const latest = await pc.getBlockNumber();
  let to = latest;
  let requests = 0;
  let complete = false;
  // Newest first; one per transaction.
  const hits: { txHash: Hex; logIndex: number; blockNumber: bigint; blockHash: Hex }[] = [];
  const seen = new Set<string>();
  // Transactions holding a transfer of the token from the reserve in the blocks searched. Only the reserve moves
  // these, so spam cannot add to them: a `Reference` hit in one of them is read before any other.
  const fromReserve = new Set<string>();
  // Transactions of ranges whose reserve `Transfer` query failed: nothing is known of them, so they are not ranked
  // down (see `rank`).
  const unranked = new Set<string>();
  while (to >= from && requests < maxRequests) {
    const lo = to - chunk + 1n > from ? to - chunk + 1n : from;
    requests++;
    let logs;
    let reserved: { transactionHash: Hex | null }[] | undefined;
    try {
      [logs, reserved] = await Promise.all([
        pc.getLogs({ address: emitter, event: referenceEmitterAbi[0], args: { ref: reference }, fromBlock: lo, toBlock: to, strict: true }),
        // Only a ranking aid: when this query alone fails for the range (the reserve moved too many transfers for
        // the provider to return), the range's Reference hits are examined unranked rather than the search lost.
        pc
          .getLogs({ address: n.token, event: tokenAbi.find((a) => a.type === 'event' && a.name === 'Transfer')!, args: { from: n.reserve }, fromBlock: lo, toBlock: to, strict: true })
          .catch(() => undefined),
      ]);
    } catch (e) {
      if (chunk > 1_000n) {
        chunk /= 2n;
        continue;
      }
      throw new Error(`${n.label} refused the log search (${(e as Error).message.split('\n')[0]}).`);
    }
    for (const t of reserved ?? []) if (t.transactionHash) fromReserve.add(t.transactionHash.toLowerCase());
    const inChunk = logs
      .filter((l) => l.args.ref?.toLowerCase() === reference.toLowerCase() && l.transactionHash && l.blockHash && l.logIndex !== null && l.blockNumber !== null)
      .sort((a, b) => (a.blockNumber === b.blockNumber ? b.logIndex! - a.logIndex! : a.blockNumber! > b.blockNumber! ? -1 : 1));
    for (const l of inChunk) {
      const key = l.transactionHash!.toLowerCase();
      if (seen.has(key)) continue;
      seen.add(key);
      if (!reserved) unranked.add(key);
      hits.push({ txHash: l.transactionHash!, logIndex: l.logIndex!, blockNumber: l.blockNumber!, blockHash: l.blockHash! });
    }
    if (lo === from) {
      complete = true;
      to = from - 1n;
      break;
    }
    to = lo - 1n;
  }
  // Those whose transaction transfers from the reserve first (newest first within each), so that Reference logs
  // anyone emitted cannot push the real payout past `maxHits`; shown newest first.
  //
  // Two assumptions make this ranking sound, and both are about the token and the reserve, not this code: the
  // reserve has given no allowance or permit that a third party could use with `transferFrom` (so a transfer from
  // the reserve is the reserve's own act), and the token has no recipient hook (so a payout's recipient cannot make
  // the reserve emit a transfer in someone else's transaction). If either stops holding, a spammer could place a
  // transfer from the reserve beside a Reference log of their own, and rank 0 would no longer mean "the reserve's".
  // Three tiers: a transaction that transfers from the reserve (0); one in a range whose reserve query failed, so
  // nothing is known of it (1); one known not to (2). A reserve hit therefore always outranks an unranked one.
  const rank = (h: { txHash: Hex }) => {
    const k = h.txHash.toLowerCase();
    return fromReserve.has(k) ? 0 : unranked.has(k) ? 1 : 2;
  };
  const examined = hits
    .map((h, i) => ({ h, i }))
    .sort((a, b) => rank(a.h) - rank(b.h) || a.i - b.i)
    .slice(0, maxHits)
    .sort((a, b) => a.i - b.i)
    .map((x) => x.h);
  const logs = await Promise.all(examined.map((h) => referenceAt(pc, n, h.txHash, h.logIndex, h.blockNumber, h.blockHash)));
  const payouts = await Promise.all(
    logs.filter((l) => l.transfers.length === 1).map(async (l) => ({ ...l, finality: await finality(pc, l.blockNumber, l.blockHash) })),
  );
  return {
    found: payouts[0],
    others: payouts.slice(1),
    unpaid: logs.filter((l) => l.transfers.length !== 1),
    unexamined: hits.length - examined.length,
    searched: { from: to + 1n, to: latest },
    complete,
  };
}

/** The `Reference` log at (`txHash`, `referenceLog`) and its transaction's transfers from the reserve. */
export async function referenceAt(pc: Reads, n: ResolvedNetwork, txHash: Hex, referenceLog: number, blockNumber: bigint, blockHash: Hex): Promise<ReferenceLog> {
  const receipt = await pc.getTransactionReceipt({ hash: txHash });
  const transfers: ReferenceLog['transfers'] = [];
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
  return { txHash, referenceLog, blockNumber, blockHash, transfers };
}

/** The payout whose `Reference` log is at (`txHash`, `referenceLog`): its transaction's transfers from the
 * reserve, and its finality. */
export async function payoutAt(pc: Reads, n: ResolvedNetwork, txHash: Hex, referenceLog: number, blockNumber: bigint, blockHash: Hex): Promise<Payout> {
  const l = await referenceAt(pc, n, txHash, referenceLog, blockNumber, blockHash);
  return { ...l, finality: await finality(pc, blockNumber, blockHash) };
}

/** Whether a payout pays exactly `amount` to `to`, in one transfer from the reserve (no fee comes out of a
 * delivery, REQ-TR-5). Names the difference otherwise. */
export function checkPayout(p: ReferenceLog, to: Address, amount: bigint): { ok: true } | { ok: false; reason: string } {
  if (p.transfers.length === 0) return { ok: false, reason: 'Its transaction moves nothing from the reserve: a reference with no transfer is not a payout.' };
  if (p.transfers.length > 1) return { ok: false, reason: `Its transaction holds ${p.transfers.length} transfers from the reserve, not one.` };
  const t = p.transfers[0]!;
  const diffs: string[] = [];
  if (t.to !== getAddress(to)) diffs.push(`to ${t.to}, not ${getAddress(to)}`);
  if (t.amount !== amount) diffs.push(`${t.amount}, not ${amount} (minor units)`);
  return diffs.length ? { ok: false, reason: `The payout differs: ${diffs.join('; ')}.` } : { ok: true };
}
