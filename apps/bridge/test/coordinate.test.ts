// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The directed arrival's coordinate from a receipt: the Transfer right before Directed, as the endpoint pairs
// them (amendment 0029, A4); an unpaired Directed log is not an arrival.

import { describe, expect, it } from 'vitest';
import { encodeAbiParameters, encodeEventTopics, type Address, type Hex } from 'viem';
import { directedArrivals } from '../src/evm/coordinate';
import { directedDepositAbi, tokenAbi } from '../src/evm/abi';
import { BLOCK_HASH, CONTRACT, RESERVE, TOKEN } from './mock-wallet';

const SENDER: Address = '0x70997970C51812dc3A010C7d01b50e0d17dc79C8';
const RECIPIENT: Hex = `0x${'42'.repeat(32)}`;
const opts = { network: 'polygon', contract: CONTRACT, token: TOKEN, reserve: RESERVE };

const transfer = (index: number, from: Address = SENDER, to: Address = RESERVE, value = 5n, address: Address = TOKEN) => ({
  address,
  topics: encodeEventTopics({ abi: tokenAbi, eventName: 'Transfer', args: { from, to } }) as [Hex, ...Hex[]],
  data: encodeAbiParameters([{ type: 'uint256' }], [value]),
  logIndex: index,
  blockNumber: 9n,
  blockHash: BLOCK_HASH,
});
const directed = (index: number, amount = 5n) => ({
  address: CONTRACT,
  topics: encodeEventTopics({ abi: directedDepositAbi, eventName: 'Directed', args: { sender: SENDER, recipient: RECIPIENT } }) as [Hex, ...Hex[]],
  data: encodeAbiParameters([{ type: 'uint256' }, { type: 'address' }], [amount, SENDER]),
  logIndex: index,
  blockNumber: 9n,
  blockHash: BLOCK_HASH,
});

describe('directedArrivals', () => {
  it('pairs Directed with the Transfer right before it, at the Transfer’s coordinate', () => {
    const [a, ...rest] = directedArrivals([transfer(3), directed(4)], opts);
    expect(rest).toHaveLength(0);
    expect(a).toEqual({
      coordinate: { network: 'polygon', blockNumber: 9n, blockHash: BLOCK_HASH, positionKind: 'log', positionIndex: 3 },
      sender: SENDER,
      amount: 5n,
      recipient: RECIPIENT,
      refund: SENDER,
    });
  });

  it('gives two adjacent pairs for two deposits in one transaction', () => {
    const got = directedArrivals([transfer(0), directed(1), transfer(2), directed(3)], opts);
    expect(got.map((a) => a.coordinate.positionIndex)).toEqual([0, 2]);
  });

  it('leaves out a Directed log that is not paired', () => {
    expect(directedArrivals([directed(4)], opts)).toEqual([]);
    expect(directedArrivals([transfer(2), directed(4)], opts)).toEqual([]);
    expect(directedArrivals([transfer(3, SENDER, SENDER), directed(4)], opts)).toEqual([]);
    expect(directedArrivals([transfer(3, RESERVE), directed(4)], opts)).toEqual([]);
    expect(directedArrivals([transfer(3, SENDER, RESERVE, 6n), directed(4)], opts)).toEqual([]);
    expect(directedArrivals([transfer(3, SENDER, RESERVE, 5n, CONTRACT), directed(4)], opts)).toEqual([]);
  });

  it('ignores a Directed log from another contract', () => {
    expect(directedArrivals([transfer(3), { ...directed(4), address: TOKEN }], opts)).toEqual([]);
  });
});
