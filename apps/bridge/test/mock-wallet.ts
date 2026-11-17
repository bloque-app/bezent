// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// A fake EIP-1193 wallet on a fake chain for the deposit flow's tests: it answers the contract's and the token's
// reads, signs typed data with a published test key (or refuses to, as some wallets do), and records every
// transaction it is asked to send, giving each deposit the `Transfer` then `Directed` logs the contract emits
// (or, to test the post-receipt check, logs that say something else). It can also switch chain on its own
// right after signing, as a user or a hostile wallet might.

import {
  decodeFunctionData,
  encodeAbiParameters,
  encodeEventTopics,
  encodeFunctionResult,
  getAddress,
  keccak256,
  numberToHex,
  type Address,
  type EIP1193Provider,
  type Hex,
} from 'viem';
import { privateKeyToAccount } from 'viem/accounts';
import { directedDepositAbi, tokenAbi } from '../src/evm/abi';
import { KREIVO_TESTBED, POLYGON_AMOY, resolveNetwork } from '../src/config/networks';

// anvil's first published test key: a test value, never a real account's.
export const OWNER = privateKeyToAccount('0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80');
export const CONTRACT: Address = '0x5FbDB2315678afecb367f032d93F642f64180aa3';
export const TOKEN: Address = '0x41E94Eb019C0762f9Bfcf9Fb1E58725BfB0e7582';
export const RESERVE: Address = '0x000000000000000000000000000000000000Ba5e';
export const BLOCK_HASH: Hex = `0x${'ab'.repeat(32)}`;
/** The contract's runtime code in the mock, and its hash, as a network pins it. */
export const CODE: Hex = '0x6080';
export const CODE_HASH: Hex = keccak256(CODE);

/** Amoy as the tests deploy on it: the mock's contract, reserve and code pinned, and a test Kreivo. */
export const network = resolveNetwork({
  ...POLYGON_AMOY,
  directedDeposit: CONTRACT,
  reserve: RESERVE,
  codeHash: CODE_HASH,
  kreivo: { ...KREIVO_TESTBED, network: 'kreivo-test', deployment: 'test' },
});

export interface Sent {
  to: Address;
  functionName: string;
  args: readonly unknown[];
}

export interface MockOptions {
  chainId?: number;
  /** `unsupported`: 4200; `invalid-params`: -32602, the wallet refusing these parameters; `reject`: 4001. */
  typedData?: 'sign' | 'unsupported' | 'invalid-params' | 'reject';
  knowsChain?: boolean;
  tokenOfContract?: Address;
  reserveOfContract?: Address;
  contractCode?: Hex;
  allowance?: bigint;
  /** The chain the wallet moves to on its own right after it signs typed data. */
  switchChainAfterSign?: number;
  /** What the deposit's logs record instead of what was sent. */
  record?: { recipient?: Hex; refund?: Address; amount?: bigint };
}

export function mockWallet(o: MockOptions = {}) {
  let chainId = o.chainId ?? 80002;
  const sent: Sent[] = [];
  const requests: string[] = [];
  const receipts = new Map<Hex, unknown>();
  let allowance = o.allowance ?? 0n;
  const typed = o.typedData ?? 'sign';
  const fail = (code: number, message: string) => Object.assign(new Error(message), { code });

  const call = (to: Address, data: Hex): Hex => {
    if (getAddress(to) === CONTRACT) {
      const { functionName } = decodeFunctionData({ abi: directedDepositAbi, data });
      const values: Record<string, unknown> = {
        token: o.tokenOfContract ?? TOKEN,
        reserve: o.reserveOfContract ?? RESERVE,
        minimum: 1_000_000n,
        maximum: 50_000_000_000n,
      };
      return encodeFunctionResult({ abi: directedDepositAbi, functionName: functionName as 'token', result: values[functionName] as Address });
    }
    if (getAddress(to) === TOKEN) {
      const { functionName } = decodeFunctionData({ abi: tokenAbi, data });
      const values: Record<string, unknown> = {
        name: 'USDC',
        version: '2',
        symbol: 'USDC',
        decimals: 6,
        nonces: 7n,
        balanceOf: 100_000_000n,
        allowance,
      };
      return encodeFunctionResult({ abi: tokenAbi, functionName: functionName as 'name', result: values[functionName] as string });
    }
    throw fail(-32000, `no contract at ${to}`);
  };

  const receipt = (hash: Hex, from: Address, to: Address, logs: unknown[]) => ({
    blockHash: BLOCK_HASH,
    blockNumber: numberToHex(100),
    contractAddress: null,
    cumulativeGasUsed: '0x5208',
    effectiveGasPrice: '0x1',
    from,
    gasUsed: '0x5208',
    logs,
    logsBloom: `0x${'00'.repeat(256)}`,
    status: '0x1',
    to,
    transactionHash: hash,
    transactionIndex: '0x0',
    type: '0x2',
  });

  const log = (address: Address, topics: Hex[], data: Hex, index: number, hash: Hex) => ({
    address,
    topics,
    data,
    logIndex: numberToHex(index),
    blockHash: BLOCK_HASH,
    blockNumber: numberToHex(100),
    transactionHash: hash,
    transactionIndex: '0x0',
    removed: false,
  });

  const provider = {
    on: () => undefined,
    removeListener: () => undefined,
    request: async ({ method, params }: { method: string; params?: unknown[] }) => {
      requests.push(method);
      const arg = <T>(i: number) => (params ?? [])[i] as T;
      switch (method) {
        case 'eth_requestAccounts':
        case 'eth_accounts':
          return [OWNER.address];
        case 'eth_chainId':
          return numberToHex(chainId);
        case 'wallet_switchEthereumChain': {
          const want = Number(arg<{ chainId: Hex }>(0).chainId);
          if (!o.knowsChain && o.knowsChain !== undefined) throw fail(4902, 'unknown chain');
          chainId = want;
          return null;
        }
        case 'wallet_addEthereumChain':
          chainId = Number(arg<{ chainId: Hex }>(0).chainId);
          return null;
        case 'eth_blockNumber':
          return numberToHex(101);
        case 'eth_getCode':
          return getAddress(arg<Address>(0)) === CONTRACT ? (o.contractCode ?? CODE) : '0x';
        case 'eth_call': {
          const { to, data } = arg<{ to: Address; data: Hex }>(0);
          return call(to, data);
        }
        case 'eth_signTypedData_v4': {
          if (typed === 'unsupported') throw fail(4200, 'eth_signTypedData_v4 is not supported');
          if (typed === 'invalid-params') throw fail(-32602, 'Invalid parameters');
          if (typed === 'reject') throw fail(4001, 'User rejected the request.');
          const t = JSON.parse(arg<string>(1));
          const m = t.message;
          const signature = await OWNER.signTypedData({
            domain: t.domain,
            types: { Permit: t.types.Permit },
            primaryType: 'Permit',
            message: { ...m, value: BigInt(m.value), nonce: BigInt(m.nonce), deadline: BigInt(m.deadline) },
          });
          if (o.switchChainAfterSign !== undefined) chainId = o.switchChainAfterSign;
          return signature;
        }
        case 'eth_sendTransaction': {
          const tx = arg<{ from: Address; to: Address; data: Hex }>(0);
          const abi = getAddress(tx.to) === CONTRACT ? directedDepositAbi : tokenAbi;
          const { functionName, args = [] } = decodeFunctionData({ abi, data: tx.data });
          sent.push({ to: getAddress(tx.to), functionName, args });
          const hash = keccak256(numberToHex(sent.length));
          const logs: unknown[] = [];
          if (functionName === 'approve') allowance = args[1] as bigint;
          if (functionName === 'deposit' || functionName === 'depositWithPermit') {
            const [sentAmount, sentRecipient, sentRefund] = args as [bigint, Hex, Address];
            const amount = o.record?.amount ?? sentAmount;
            const recipient = o.record?.recipient ?? sentRecipient;
            const refund = o.record?.refund ?? sentRefund;
            logs.push(
              log(TOKEN, encodeEventTopics({ abi: tokenAbi, eventName: 'Transfer', args: { from: tx.from, to: RESERVE } }) as Hex[],
                encodeAbiParameters([{ type: 'uint256' }], [amount]), 41, hash),
              log(CONTRACT, encodeEventTopics({ abi: directedDepositAbi, eventName: 'Directed', args: { sender: tx.from, recipient } }) as Hex[],
                encodeAbiParameters([{ type: 'uint256' }, { type: 'address' }], [amount, refund]), 42, hash),
            );
          }
          receipts.set(hash, receipt(hash, tx.from, tx.to, logs));
          return hash;
        }
        case 'eth_getTransactionReceipt':
          return receipts.get(arg<Hex>(0)) ?? null;
        case 'eth_getTransactionByHash':
          return null;
        default:
          throw fail(-32601, `${method} is not supported by the mock`);
      }
    },
  } as unknown as EIP1193Provider;

  return {
    provider,
    sent,
    requests,
    chainId: () => chainId,
    /** Moves the wallet to another chain, as the user might at any moment. */
    switchTo: (id: number) => {
      chainId = id;
    },
  };
}
