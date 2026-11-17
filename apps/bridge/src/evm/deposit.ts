// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The deposit on the origin network (F-018, T-018-14; plan part 4, "Deposit (EVM)"). Every read goes through the
// connected wallet's provider: the app runs no RPC and holds no key.
//
// 1. Switch the wallet to the network's chain (adding it when the wallet does not know it).
// 2. Check the pinned contract: its runtime code hashes to the pinned hash, its `token()` is the pinned token and
//    its `reserve()` the pinned reserve, and read its immutable `minimum` and `maximum`.
// 3. Sign an EIP-2612 permit for exactly the amount, with the domain read from the token, and send
//    `depositWithPermit(amount, recipient, refund, deadline, v, r, s)`, `refund` being the connected account,
//    the original depositor (`d-refund-dest-swaps`).
// 4. A wallet that says it cannot sign typed data: `approve` exactly the amount, then `deposit`.
//
// The wallet can change chain at any moment, so the chain is checked again right before every send and every
// receipt wait, and every send carries the network's viem chain, so viem checks `eth_chainId` itself too.

import {
  createPublicClient,
  createWalletClient,
  custom,
  defineChain,
  getAddress,
  keccak256,
  parseSignature,
  UserRejectedRequestError,
  type Address,
  type Chain,
  type EIP1193Provider,
  type Hex,
  type PublicClient,
  type TransactionReceipt,
  type WalletClient,
} from 'viem';
import { directedDepositAbi, tokenAbi } from './abi';
import { permitDeadline, permitTypedData } from './permit';
import { directedArrivals, type DirectedArrival } from './coordinate';
import type { ResolvedNetwork } from '../config/networks';

export interface Session {
  network: ResolvedNetwork;
  /** The network as a viem chain: every send names it, so viem refuses to send on another chain. */
  chain: Chain;
  account: Address;
  wallet: WalletClient;
  public: PublicClient;
}

export interface ContractFacts {
  token: Address;
  reserve: Address;
  minimum: bigint;
  maximum: bigint;
  tokenName: string;
  tokenVersion: string;
  symbol: string;
  decimals: number;
}

const hexChain = (id: number) => `0x${id.toString(16)}` as const;

/** `n` as a viem chain. Its RPC URLs are never called: every read and write goes through the wallet. */
export function viemChain(n: ResolvedNetwork): Chain {
  return defineChain({
    id: n.chainId,
    name: n.chain.name,
    nativeCurrency: n.chain.nativeCurrency,
    rpcUrls: { default: { http: [...n.chain.rpcUrls] } },
  });
}

/** Asks the wallet for an account and switches it to `network`'s chain. */
export async function connect(provider: EIP1193Provider, network: ResolvedNetwork, pollingInterval = 2_000): Promise<Session> {
  const accounts = (await provider.request({ method: 'eth_requestAccounts' })) as string[];
  const first = accounts[0];
  if (!first) throw new Error('The wallet gave no account.');
  await switchChain(provider, network);
  const transport = custom(provider);
  const chain = viemChain(network);
  // No CCIP-Read: a contract's revert never sends the page to fetch a URL (vite.config.ts also leaves the code out).
  return {
    network,
    chain,
    account: getAddress(first),
    wallet: createWalletClient({ chain, transport, pollingInterval, ccipRead: false }),
    public: createPublicClient({ chain, transport, pollingInterval, ccipRead: false }),
  };
}

export async function switchChain(provider: EIP1193Provider, n: ResolvedNetwork): Promise<void> {
  const want = hexChain(n.chainId);
  if ((await provider.request({ method: 'eth_chainId' })) === want) return;
  try {
    await provider.request({ method: 'wallet_switchEthereumChain', params: [{ chainId: want }] });
  } catch (e) {
    // 4902: the wallet does not know the chain.
    if ((e as { code?: number }).code !== 4902) throw e;
    await provider.request({
      method: 'wallet_addEthereumChain',
      params: [
        {
          chainId: want,
          chainName: n.chain.name,
          nativeCurrency: n.chain.nativeCurrency,
          rpcUrls: [...n.chain.rpcUrls],
          blockExplorerUrls: n.chain.blockExplorerUrls.length ? [...n.chain.blockExplorerUrls] : undefined,
        },
      ],
    });
  }
  if ((await provider.request({ method: 'eth_chainId' })) !== want) {
    throw new Error(`The wallet did not switch to ${n.label} (chain ${n.chainId}).`);
  }
}

/** Reads the pinned contract and its token through the wallet, and refuses a contract that is not the one
 * configured: no code at the address, code whose hash is not the pinned one, or a `token()` or `reserve()`
 * other than the pinned ones. */
export async function readContract(s: Session): Promise<ContractFacts> {
  const { directedDeposit: address, token: pinnedToken, reserve: pinnedReserve, codeHash } = s.network;
  await assertChain(s);
  const code = await s.public.getCode({ address });
  if (!code || code === '0x') throw new Error(`There is no contract at ${address} on ${s.network.label}.`);
  if (keccak256(code) !== codeHash) {
    throw new Error(`The code at ${address} is not the DirectedDeposit this build pins (code hash ${keccak256(code)}, not ${codeHash}); refusing it.`);
  }
  const c = { address, abi: directedDepositAbi } as const;
  const [token, reserve, minimum, maximum] = await Promise.all([
    s.public.readContract({ ...c, functionName: 'token' }),
    s.public.readContract({ ...c, functionName: 'reserve' }),
    s.public.readContract({ ...c, functionName: 'minimum' }),
    s.public.readContract({ ...c, functionName: 'maximum' }),
  ]);
  if (getAddress(token) !== pinnedToken) {
    throw new Error(`The contract at ${address} directs ${token}, not the pinned token ${pinnedToken}; refusing it.`);
  }
  if (getAddress(reserve) !== pinnedReserve) {
    throw new Error(`The contract at ${address} forwards to ${reserve}, not the pinned reserve ${pinnedReserve}; refusing it.`);
  }
  const t = { address: pinnedToken, abi: tokenAbi } as const;
  const [tokenName, tokenVersion, symbol, decimals] = await Promise.all([
    s.public.readContract({ ...t, functionName: 'name' }),
    s.public.readContract({ ...t, functionName: 'version' }),
    s.public.readContract({ ...t, functionName: 'symbol' }),
    s.public.readContract({ ...t, functionName: 'decimals' }),
  ]);
  return { token: pinnedToken, reserve: pinnedReserve, minimum, maximum, tokenName, tokenVersion, symbol, decimals };
}

export async function balanceOf(s: Session): Promise<bigint> {
  return s.public.readContract({ address: s.network.token, abi: tokenAbi, functionName: 'balanceOf', args: [s.account] });
}

/** Refuses to go on when the wallet is no longer on the network's chain. */
export async function assertChain(s: Session): Promise<void> {
  const id = await s.public.getChainId();
  if (id !== s.network.chainId) {
    throw new Error(`The wallet is on chain ${id}, not ${s.network.label} (${s.network.chainId}); nothing more is sent. Switch it back and start again.`);
  }
}

export interface DepositRequest {
  amount: bigint;
  /** The Kreivo account, 32 bytes. */
  recipient: Hex;
}

export type Progress = (step: string) => void;

export interface DepositResult {
  hash: Hex;
  receipt: TransactionReceipt;
  /** The arrival whose `Directed` log names exactly this deposit: this sender, recipient, amount and refund. */
  arrival: DirectedArrival | undefined;
  /** Every arrival of the contract in the receipt, as decoded, to show what the chain recorded when it is not
   * what was asked. */
  arrivals: DirectedArrival[];
  method: 'permit' | 'approve';
  /** With `approve`: whether an allowance already in place was used rather than a new approval. */
  reusedAllowance: boolean;
}

/** A wallet's answer that says it does not implement typed-data signing at all (as opposed to the user
 * declining, or the wallet refusing these parameters, -32602, which is not a reason to fall back). */
function cannotSignTypedData(e: unknown): boolean {
  const code = (e as { code?: number; cause?: { code?: number } }).code ?? (e as { cause?: { code?: number } }).cause?.code;
  return code === 4200 || code === -32601;
}

/** Deposits `amount` for `recipient` with the connected account as depositor and refund address. */
export async function deposit(s: Session, facts: ContractFacts, req: DepositRequest, progress: Progress): Promise<DepositResult> {
  if (req.amount < facts.minimum || req.amount > facts.maximum) throw new Error('The amount is outside the contract’s limits.');
  if (!/^0x[0-9a-f]{64}$/.test(req.recipient) || /^0x0{64}$/.test(req.recipient)) throw new Error('No recipient.');
  await assertChain(s);
  const contract = s.network.directedDeposit;
  const refund = s.account;
  const chain = s.chain;
  let reusedAllowance = false;

  let hash: Hex | undefined;
  let method: DepositResult['method'] = 'permit';
  progress('Reading the permit nonce.');
  const nonce = await s.public.readContract({ address: facts.token, abi: tokenAbi, functionName: 'nonces', args: [s.account] });
  const deadline = permitDeadline(BigInt(Math.floor(Date.now() / 1000)));
  const typed = permitTypedData({
    tokenName: facts.tokenName,
    tokenVersion: facts.tokenVersion,
    chainId: s.network.chainId,
    token: facts.token,
    owner: s.account,
    spender: contract,
    value: req.amount,
    nonce,
    deadline,
  });
  let signature: Hex | undefined;
  try {
    progress('Sign the permit in your wallet: it allows the contract to move exactly this amount once.');
    signature = await s.wallet.signTypedData({ account: s.account, ...typed });
  } catch (e) {
    if (e instanceof UserRejectedRequestError || (e as { code?: number }).code === 4001) throw e;
    if (!cannotSignTypedData(e)) throw e;
    method = 'approve';
  }
  if (signature) {
    const { r, s: sv, v, yParity } = parseSignature(signature);
    const vv = Number(v ?? BigInt(yParity + 27));
    progress('Confirm the deposit in your wallet.');
    await assertChain(s);
    hash = await s.wallet.writeContract({
      account: s.account,
      chain,
      address: contract,
      abi: directedDepositAbi,
      functionName: 'depositWithPermit',
      args: [req.amount, req.recipient, refund, deadline, vv, r, sv],
    });
  } else {
    const allowance = await s.public.readContract({
      address: facts.token,
      abi: tokenAbi,
      functionName: 'allowance',
      args: [s.account, contract],
    });
    if (allowance < req.amount) {
      progress('Your wallet cannot sign a permit. Approve exactly this amount for the contract.');
      await assertChain(s);
      const approval = await s.wallet.writeContract({
        account: s.account,
        chain,
        address: facts.token,
        abi: tokenAbi,
        functionName: 'approve',
        args: [contract, req.amount],
      });
      progress('Waiting for the approval to be included.');
      await assertChain(s);
      const r = await s.public.waitForTransactionReceipt({ hash: approval });
      if (r.status !== 'success') throw new Error(`The approval ${approval} failed.`);
    } else {
      reusedAllowance = true;
      progress(
        `Your wallet cannot sign a permit. The contract already holds an allowance of at least this amount from your account, so it is used: no new approval.`,
      );
    }
    progress('Confirm the deposit in your wallet.');
    await assertChain(s);
    hash = await s.wallet.writeContract({
      account: s.account,
      chain,
      address: contract,
      abi: directedDepositAbi,
      functionName: 'deposit',
      args: [req.amount, req.recipient, refund],
    });
  }
  progress('Waiting for the deposit to be included.');
  const sent = hash;
  const onChain = () =>
    assertChain(s).catch((e: unknown) => {
      throw new Error(`The deposit ${sent} was sent, but its receipt cannot be read: ${(e as Error).message}`);
    });
  await onChain();
  const receipt = await s.public.waitForTransactionReceipt({ hash });
  await onChain();
  if (receipt.status !== 'success') throw new Error(`The deposit ${hash} reverted; nothing moved.`);
  const arrivals = directedArrivals(receipt.logs, {
    network: s.network.network,
    contract,
    token: facts.token,
    reserve: facts.reserve,
  });
  const arrival = arrivals.find(
    (a) => a.sender === s.account && a.recipient === req.recipient && a.amount === req.amount && a.refund === s.account,
  );
  return { hash, receipt, arrival, arrivals, method, reusedAllowance };
}
