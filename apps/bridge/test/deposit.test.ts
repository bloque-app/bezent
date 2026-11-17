// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// T-018-14: the deposit flow against a fake wallet. The permit is for exactly the amount, signed by the
// depositor; the deposit names the recipient and the depositor as refund; a wallet that cannot sign typed data
// approves exactly the amount, then deposits; a declined signature sends nothing; a contract that does not
// direct the pinned token is refused; and the coordinate is the Transfer's, from which the references follow.

import { describe, expect, it } from 'vitest';
import { recoverTypedDataAddress, type Hex } from 'viem';
import { connect, deposit, readContract } from '../src/evm/deposit';
import { permitTypedData } from '../src/evm/permit';
import { POLYGON_AMOY, resolveNetwork } from '../src/config/networks';
import { depositReference } from '../src/lib/refs';
import { BLOCK_HASH, CONTRACT, mockWallet, OWNER, RESERVE, TOKEN } from './mock-wallet';

const network = resolveNetwork({ ...POLYGON_AMOY, directedDeposit: CONTRACT });
const RECIPIENT: Hex = '0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d';
const quiet = () => undefined;

async function session(o: Parameters<typeof mockWallet>[0] = {}) {
  const w = mockWallet(o);
  const s = await connect(w.provider, network, 5);
  return { w, s, facts: await readContract(s) };
}

describe('connect', () => {
  it('switches the wallet to the network’s chain', async () => {
    const w = mockWallet({ chainId: 1 });
    await connect(w.provider, network, 5);
    expect(w.chainId()).toBe(80002);
    expect(w.requests).toContain('wallet_switchEthereumChain');
  });

  it('adds the chain when the wallet does not know it', async () => {
    const w = mockWallet({ chainId: 1, knowsChain: false });
    await connect(w.provider, network, 5);
    expect(w.chainId()).toBe(80002);
    expect(w.requests).toContain('wallet_addEthereumChain');
  });
});

describe('readContract', () => {
  it('reads the immutable limits and the permit domain', async () => {
    const { facts } = await session();
    expect(facts).toMatchObject({ token: TOKEN, reserve: RESERVE, minimum: 1_000_000n, maximum: 50_000_000_000n, tokenName: 'USDC', tokenVersion: '2', decimals: 6 });
  });

  it('refuses a contract that directs another token', async () => {
    const w = mockWallet({ tokenOfContract: '0x000000000000000000000000000000000000dEaD' });
    const s = await connect(w.provider, network, 5);
    await expect(readContract(s)).rejects.toThrow(/not the pinned token/);
  });

  it('refuses an address with no code', async () => {
    const w = mockWallet({ contractCode: '0x' });
    const s = await connect(w.provider, network, 5);
    await expect(readContract(s)).rejects.toThrow(/no contract/);
  });
});

describe('deposit', () => {
  it('signs an exact-amount permit and calls depositWithPermit with the depositor as refund', async () => {
    const { w, s, facts } = await session();
    const res = await deposit(s, facts, { amount: 2_500_000n, recipient: RECIPIENT }, quiet);
    expect(res.method).toBe('permit');
    expect(w.sent).toHaveLength(1);
    const [tx] = w.sent;
    expect(tx!.to).toBe(CONTRACT);
    expect(tx!.functionName).toBe('depositWithPermit');
    const [amount, recipient, refund, deadline, v, r, sv] = tx!.args as [bigint, Hex, string, bigint, number, Hex, Hex];
    expect(amount).toBe(2_500_000n);
    expect(recipient).toBe(RECIPIENT);
    expect(refund).toBe(OWNER.address);
    // The signature is the depositor's, over a permit for exactly this amount to the contract.
    const typed = permitTypedData({
      tokenName: 'USDC', tokenVersion: '2', chainId: 80002, token: TOKEN,
      owner: OWNER.address, spender: CONTRACT, value: 2_500_000n, nonce: 7n, deadline,
    });
    const signature = `${r}${sv.slice(2)}${v.toString(16).padStart(2, '0')}` as Hex;
    expect(await recoverTypedDataAddress({ ...typed, signature })).toBe(OWNER.address);
    // The arrival is at the Transfer's coordinate, the log right before Directed.
    expect(res.arrival?.coordinate).toEqual({ network: 'polygon-amoy', blockNumber: 100n, blockHash: BLOCK_HASH, positionKind: 'log', positionIndex: 41 });
    expect(depositReference(res.arrival!.coordinate)).toMatch(/^bz-d-[0-9a-f]{40}$/);
  });

  it('falls back to approve exactly the amount, then deposit, when the wallet cannot sign typed data', async () => {
    const { w, s, facts } = await session({ typedData: 'unsupported' });
    const res = await deposit(s, facts, { amount: 3_000_000n, recipient: RECIPIENT }, quiet);
    expect(res.method).toBe('approve');
    expect(w.sent.map((t) => [t.to, t.functionName, t.args])).toEqual([
      [TOKEN, 'approve', [CONTRACT, 3_000_000n]],
      [CONTRACT, 'deposit', [3_000_000n, RECIPIENT, OWNER.address]],
    ]);
  });

  it('uses an allowance already in place rather than approving again', async () => {
    const { w, s, facts } = await session({ typedData: 'unsupported', allowance: 3_000_000n });
    await deposit(s, facts, { amount: 3_000_000n, recipient: RECIPIENT }, quiet);
    expect(w.sent.map((t) => t.functionName)).toEqual(['deposit']);
  });

  it('sends nothing when the user declines the permit', async () => {
    const { w, s, facts } = await session({ typedData: 'reject' });
    await expect(deposit(s, facts, { amount: 3_000_000n, recipient: RECIPIENT }, quiet)).rejects.toThrow();
    expect(w.sent).toHaveLength(0);
  });

  it('refuses an amount outside the contract’s limits, and no recipient, before anything is signed', async () => {
    const { w, s, facts } = await session();
    await expect(deposit(s, facts, { amount: 999_999n, recipient: RECIPIENT }, quiet)).rejects.toThrow(/limits/);
    await expect(deposit(s, facts, { amount: 50_000_000_001n, recipient: RECIPIENT }, quiet)).rejects.toThrow(/limits/);
    await expect(deposit(s, facts, { amount: 1_000_000n, recipient: `0x${'0'.repeat(64)}` }, quiet)).rejects.toThrow(/recipient/);
    expect(w.requests).not.toContain('eth_signTypedData_v4');
    expect(w.sent).toHaveLength(0);
  });
});
