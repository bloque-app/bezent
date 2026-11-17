// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The coordinate of a directed arrival from its transaction's receipt (F-018 plan part 4, "Status"; amendment
// 0029, A4): the EVM endpoint pairs a `Directed` log with the log right before it, the token's `Transfer` from
// the event's sender to the reserve, and the arrival sits at the `Transfer`'s coordinate (REQ-RN-3). The
// references (lib/refs.ts) are derived from that coordinate.

import { decodeEventLog, getAddress, type Address, type Hex as ViemHex, type Log } from 'viem';
import { directedDepositAbi, tokenAbi } from './abi';
import type { Coordinate } from '../lib/refs';

export interface DirectedArrival {
  coordinate: Coordinate;
  sender: Address;
  amount: bigint;
  recipient: ViemHex;
  refund: Address;
}

type ReceiptLog = Pick<Log, 'address' | 'topics' | 'data' | 'logIndex' | 'blockNumber' | 'blockHash'>;

/** Finds, in a receipt's logs, each `Directed` log of `contract` paired with the token's `Transfer` right
 * before it, as the endpoint pairs them. A `Directed` log without its `Transfer` is not an arrival and is
 * left out. */
export function directedArrivals(
  logs: readonly ReceiptLog[],
  opts: { network: string; contract: Address; token: Address; reserve: Address },
): DirectedArrival[] {
  const byIndex = new Map<number, ReceiptLog>();
  for (const l of logs) if (l.logIndex !== null) byIndex.set(l.logIndex, l);
  const out: DirectedArrival[] = [];
  for (const l of logs) {
    if (l.logIndex === null || l.blockNumber === null || l.blockHash === null) continue;
    if (getAddress(l.address) !== getAddress(opts.contract)) continue;
    let directed;
    try {
      directed = decodeEventLog({ abi: directedDepositAbi, eventName: 'Directed', topics: l.topics as [ViemHex], data: l.data });
    } catch {
      continue;
    }
    const prev = byIndex.get(l.logIndex - 1);
    if (!prev || getAddress(prev.address) !== getAddress(opts.token)) continue;
    let transfer;
    try {
      transfer = decodeEventLog({ abi: tokenAbi, eventName: 'Transfer', topics: prev.topics as [ViemHex], data: prev.data });
    } catch {
      continue;
    }
    const d = directed.args as { sender: Address; amount: bigint; recipient: ViemHex; refund: Address };
    const t = transfer.args as { from: Address; to: Address; value: bigint };
    if (getAddress(t.from) !== getAddress(d.sender) || getAddress(t.to) !== getAddress(opts.reserve) || t.value !== d.amount) continue;
    out.push({
      coordinate: {
        network: opts.network,
        blockNumber: l.blockNumber,
        blockHash: l.blockHash,
        positionKind: 'log',
        positionIndex: prev.logIndex!,
      },
      sender: getAddress(d.sender),
      amount: d.amount,
      recipient: d.recipient,
      refund: getAddress(d.refund),
    });
  }
  return out;
}
