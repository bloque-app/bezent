// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// A fake EIP-1193 wallet on a fake chain for the deposit flow's tests: it answers the contract's and the token's
// reads, signs typed data with a published test key (or refuses to, as some wallets do), and records every
// transaction it is asked to send, giving each deposit the `Transfer` then `Directed` logs the contract emits
// (or, to test the post-receipt check, logs that say something else). It can also switch chain on its own
// right after signing or while the page waits for a receipt, as a user or a hostile wallet might, fail the receipt
// wait, and serve the vault's payouts (a token Transfer from the reserve and the emitter's Reference log, in one
// transaction) and blocks, for the status page, beside Reference logs anyone could emit through the emitter (its
// `emitReference` is open), in transactions that move nothing, or more than one transfer, from the reserve.

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
import { referenceEmitterAbi } from '../src/status/polygon';
import { KREIVO_TESTBED, POLYGON_AMOY, resolveNetwork } from '../src/config/networks';

// anvil's first published test key: a test value, never a real account's.
export const OWNER = privateKeyToAccount('0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80');
export const CONTRACT: Address = '0x5FbDB2315678afecb367f032d93F642f64180aa3';
export const TOKEN: Address = '0x41E94Eb019C0762f9Bfcf9Fb1E58725BfB0e7582';
export const RESERVE: Address = '0x000000000000000000000000000000000000Ba5e';
export const EMITTER: Address = '0x00000000000000000000000000000000000E0111';
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
  kreivo: { ...KREIVO_TESTBED, network: 'kreivo-test', deployment: 'test', bezent: `0x${'c0'.repeat(32)}` },
  emitter: EMITTER,
  payoutsFrom: 10n,
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
  /** A second `Directed` log in the deposit's receipt: with its own `Transfer` (`paired`) or alone (`unpaired`). */
  extraDirected?: 'paired' | 'unpaired';
  /** The chain the wallet moves to while the page waits for a receipt. */
  switchChainOnReceipt?: number;
  /** The provider fails every receipt read, as a node that drops the connection. */
  receiptFails?: boolean;
  /** Payouts from the reserve already on chain: each one transaction with the Transfer and the Reference log. */
  payouts?: { reference: Hex; to: Address; amount: bigint; block: number }[];
  /** Reference logs someone else emitted, each in its own transaction: with a token transfer that is not from the
   * reserve, and `reserveTransfers` transfers from the reserve (none by default). */
  bogusReferences?: { reference: Hex; block: number; reserveTransfers?: number }[];
  /** The finalized head (the latest block is 101). */
  finalized?: number;
  /** The largest block range `eth_getLogs` takes. */
  maxLogRange?: number;
  /** The latest block (101 by default). */
  latest?: number;
  /** The reserve's `Transfer` log query fails (the emitter's does not), as when the reserve moved too many. */
  /** The reserve's Transfer query fails: always, or for the block ranges the function names. */
  reserveLogsFail?: boolean | ((from: number, to: number) => boolean);
}

export function mockWallet(o: MockOptions = {}) {
  let chainId = o.chainId ?? 80002;
  const sent: Sent[] = [];
  const requests: string[] = [];
  const receipts = new Map<Hex, unknown>();
  const transactions = new Map<Hex, unknown>();
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

  const blockHash = (n: number): Hex => (n === 100 ? BLOCK_HASH : keccak256(numberToHex(n, { size: 32 })));
  const receipt = (hash: Hex, from: Address, to: Address, logs: unknown[], block = 100) => ({
    blockHash: blockHash(block),
    blockNumber: numberToHex(block),
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

  const log = (address: Address, topics: Hex[], data: Hex, index: number, hash: Hex, block = 100) => ({
    address,
    topics,
    data,
    logIndex: numberToHex(index),
    blockHash: blockHash(block),
    blockNumber: numberToHex(block),
    transactionHash: hash,
    transactionIndex: '0x0',
    removed: false,
  });

  // The payouts: a Transfer from the reserve, then the emitter's Reference, in one transaction each.
  const payoutLogs: ReturnType<typeof log>[] = [];
  // The token's Transfer logs from the reserve (a search reads these to find which transactions to read first).
  const reserveLogs: ReturnType<typeof log>[] = [];
  (o.payouts ?? []).forEach((p, i) => {
    const hash = keccak256(numberToHex(10_000 + i));
    const transfer = log(TOKEN, encodeEventTopics({ abi: tokenAbi, eventName: 'Transfer', args: { from: RESERVE, to: p.to } }) as Hex[],
      encodeAbiParameters([{ type: 'uint256' }], [p.amount]), 7, hash, p.block);
    const reference = log(EMITTER, encodeEventTopics({ abi: referenceEmitterAbi, eventName: 'Reference', args: { ref: p.reference } }) as Hex[], '0x', 8, hash, p.block);
    payoutLogs.push(reference);
    reserveLogs.push(transfer);
    receipts.set(hash, receipt(hash, RESERVE, RESERVE, [transfer, reference], p.block));
  });
  (o.bogusReferences ?? []).forEach((b, i) => {
    const hash = keccak256(numberToHex(20_000 + i));
    const transfer = (from: Address, index: number) =>
      log(TOKEN, encodeEventTopics({ abi: tokenAbi, eventName: 'Transfer', args: { from, to: OWNER.address } }) as Hex[],
        encodeAbiParameters([{ type: 'uint256' }], [1_000_000n]), index, hash, b.block);
    const logs = [transfer(OWNER.address, 3), ...Array.from({ length: b.reserveTransfers ?? 0 }, (_, k) => transfer(RESERVE, 4 + k))];
    const reference = log(EMITTER, encodeEventTopics({ abi: referenceEmitterAbi, eventName: 'Reference', args: { ref: b.reference } }) as Hex[], '0x', 9, hash, b.block);
    payoutLogs.push(reference);
    reserveLogs.push(...logs.filter((l) => l.topics[1] === encodeEventTopics({ abi: tokenAbi, eventName: 'Transfer', args: { from: RESERVE } })[1]));
    receipts.set(hash, receipt(hash, OWNER.address, EMITTER, [...logs, reference], b.block));
  });
  const block = (n: number) => ({
    number: numberToHex(n),
    hash: blockHash(n),
    parentHash: blockHash(n - 1),
    timestamp: numberToHex(1_700_000_000 + n),
    nonce: '0x0000000000000000',
    difficulty: '0x0',
    gasLimit: '0x1c9c380',
    gasUsed: '0x0',
    miner: RESERVE,
    extraData: '0x',
    logsBloom: `0x${'00'.repeat(256)}`,
    transactionsRoot: BLOCK_HASH,
    stateRoot: BLOCK_HASH,
    receiptsRoot: BLOCK_HASH,
    sha3Uncles: BLOCK_HASH,
    mixHash: BLOCK_HASH,
    size: '0x0',
    totalDifficulty: '0x0',
    baseFeePerGas: '0x1',
    transactions: [],
    uncles: [],
  });
  const logRequests: { from: number; to: number }[] = [];

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
          return numberToHex(o.latest ?? 101);
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
            const transfer = (i: number) =>
              log(TOKEN, encodeEventTopics({ abi: tokenAbi, eventName: 'Transfer', args: { from: tx.from, to: RESERVE } }) as Hex[],
                encodeAbiParameters([{ type: 'uint256' }], [amount]), i, hash);
            const directed = (i: number) =>
              log(CONTRACT, encodeEventTopics({ abi: directedDepositAbi, eventName: 'Directed', args: { sender: tx.from, recipient } }) as Hex[],
                encodeAbiParameters([{ type: 'uint256' }, { type: 'address' }], [amount, refund]), i, hash);
            logs.push(transfer(41), directed(42));
            if (o.extraDirected === 'paired') logs.push(transfer(43), directed(44));
            if (o.extraDirected === 'unpaired') logs.push(directed(45));
          }
          receipts.set(hash, receipt(hash, tx.from, tx.to, logs));
          transactions.set(hash, {
            hash,
            from: tx.from,
            to: tx.to,
            input: tx.data,
            nonce: numberToHex(sent.length),
            value: '0x0',
            gas: '0x5208',
            maxFeePerGas: '0x1',
            maxPriorityFeePerGas: '0x1',
            blockHash: BLOCK_HASH,
            blockNumber: numberToHex(100),
            transactionIndex: '0x0',
            type: '0x2',
            chainId: numberToHex(chainId),
            v: '0x0',
            r: BLOCK_HASH,
            s: BLOCK_HASH,
            yParity: '0x0',
            accessList: [],
          });
          return hash;
        }
        case 'eth_getTransactionReceipt': {
          if (o.receiptFails) throw fail(-32000, 'header not found');
          const r = receipts.get(arg<Hex>(0)) ?? null;
          if (r && o.switchChainOnReceipt !== undefined) chainId = o.switchChainOnReceipt;
          return r;
        }
        case 'eth_getBlockByNumber': {
          const tag = arg<string>(0);
          const n = tag === 'finalized' ? (o.finalized ?? 90) : tag === 'latest' ? (o.latest ?? 101) : Number(tag);
          return block(n);
        }
        case 'eth_getLogs': {
          const f = arg<{ address: Address; topics: (Hex | null)[]; fromBlock: Hex; toBlock: Hex }>(0);
          const [from, to] = [Number(f.fromBlock), Number(f.toBlock)];
          const ofReserve = getAddress(f.address) === TOKEN;
          if (!ofReserve) logRequests.push({ from, to }); // The emitter's searches only.
          if (ofReserve && (typeof o.reserveLogsFail === 'function' ? o.reserveLogsFail(from, to) : o.reserveLogsFail)) throw fail(-32000, 'query returned more than 10000 results');
          if (o.maxLogRange !== undefined && to - from + 1 > o.maxLogRange) throw fail(-32000, 'query returned more than 10000 results');
          return (ofReserve ? reserveLogs : payoutLogs).filter(
            (l) => getAddress(l.address) === getAddress(f.address) && l.topics[1] === f.topics[1] && Number(l.blockNumber) >= from && Number(l.blockNumber) <= to,
          );
        }
        case 'eth_getTransactionByHash':
          return transactions.get(arg<Hex>(0)) ?? null;
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
    logRequests,
    /** Moves the wallet to another chain, as the user might at any moment. */
    switchTo: (id: number) => {
      chainId = id;
    },
  };
}
