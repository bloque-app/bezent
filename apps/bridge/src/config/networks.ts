// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The networks the app deposits on, pinned in the build (F-018 plan part 4, "Defence": the contract's address
// is pinned in the build and shown). Each origin network names its DirectedDeposit contract, the admitted
// token and the Kreivo it mints on. An address that is not known yet is `UNSET`, and `resolveNetwork` refuses
// such a network: the app never falls back to another address, another network or a default.

import { getAddress, isAddress, type Address } from 'viem';

/** A configuration value that is deliberately not set yet (for example, a contract not yet deployed). */
export const UNSET = null;
export type Pinned<T> = T | typeof UNSET;

/** Which build this is. A build carries the networks of one deployment only. */
export type Deployment = 'testbed' | 'test' | 'production';

export interface KreivoNetwork {
  /** bezent's id of the issuing network. */
  network: string;
  label: string;
  ss58Prefix: number;
  /** The bridged asset the deposit mints: `Here(101)`, bUSDC (PLAN DEC-7). */
  asset: { id: number; symbol: string; decimals: number };
  /**
   * How the app reads Kreivo. `light-client`: smoldot, with the relay chain's and Kreivo's chain
   * specifications, trusting no RPC (plan part 4). `loopback-rpc`: a node on this machine, for the local
   * zombienet test bed only, whose chain specification changes with every spawn; refused for anything but a
   * loopback URL.
   */
  connection: { kind: 'light-client'; relay: 'kusama' } | { kind: 'loopback-rpc'; url: string };
  /** 32-byte accounts the core refuses as a directed deposit's recipient (the Kreivo contract's account, the
   * DEC-15 ledger accounts), as hex. A deposit to one would be returned; the app refuses it before signing. */
  refusedRecipients: readonly string[];
}

export interface OriginNetwork {
  key: string;
  label: string;
  /** bezent's id of the origin network: the `network` of an arrival's coordinate, which the references are
   * derived from. Not the chain id. */
  network: string;
  chainId: number;
  /** For `wallet_addEthereumChain`, when the wallet does not know the chain. The app itself never calls
   * these: every EVM read goes through the connected wallet. */
  chain: {
    name: string;
    rpcUrls: readonly string[];
    blockExplorerUrls: readonly string[];
    nativeCurrency: { name: string; symbol: string; decimals: number };
  };
  /** The admitted origin token (native USDC). */
  token: Pinned<Address>;
  /** The DirectedDeposit contract (T-018-01). */
  directedDeposit: Pinned<Address>;
  kreivo: KreivoNetwork;
}

/** Kreivo, read through the light client (its chain specification is `services/kreivo-endpoint/chain-specs/
 * kreivo.json`, shipped beside the app). */
export const KREIVO: KreivoNetwork = {
  network: 'kreivo',
  label: 'Kreivo',
  ss58Prefix: 2,
  asset: { id: 101, symbol: 'bUSDC', decimals: 6 },
  connection: { kind: 'light-client', relay: 'kusama' },
  refusedRecipients: [],
};

/** The zombienet test bed's Kreivo (`e2e/network`), through its collator's RPC on this machine. */
export const KREIVO_TESTBED: KreivoNetwork = {
  network: 'kreivo-local',
  label: 'Kreivo (local test bed)',
  ss58Prefix: 2,
  asset: { id: 101, symbol: 'bUSDC', decimals: 6 },
  connection: { kind: 'loopback-rpc', url: 'ws://127.0.0.1:9944' },
  refusedRecipients: [],
};

const POLYGON_CHAIN = {
  name: 'Polygon',
  rpcUrls: ['https://polygon-rpc.com'],
  blockExplorerUrls: ['https://polygonscan.com'],
  nativeCurrency: { name: 'POL', symbol: 'POL', decimals: 18 },
} as const;

export const POLYGON: OriginNetwork = {
  key: 'polygon',
  label: 'Polygon',
  network: 'polygon',
  chainId: 137,
  chain: POLYGON_CHAIN,
  token: '0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359',
  // Not deployed yet: T-018-18 deploys it and pins its address here.
  directedDeposit: UNSET,
  kreivo: KREIVO,
};

export const POLYGON_AMOY: OriginNetwork = {
  key: 'polygon-amoy',
  label: 'Polygon Amoy (test)',
  network: 'polygon-amoy',
  chainId: 80002,
  chain: {
    name: 'Polygon Amoy',
    rpcUrls: ['https://rpc-amoy.polygon.technology'],
    blockExplorerUrls: ['https://amoy.polygonscan.com'],
    nativeCurrency: { name: 'POL', symbol: 'POL', decimals: 18 },
  },
  // Circle's test USDC on Amoy (domain "USDC"/"2"; features/F-018-directed-deposits/NOTES.md).
  token: '0x41E94Eb019C0762f9Bfcf9Fb1E58725BfB0e7582',
  // Not deployed yet: T-018-04 deploys it, pins its address here, and confirms the Kreivo it mints on.
  directedDeposit: UNSET,
  kreivo: KREIVO,
};

/**
 * The local test bed: the anvil fork of Polygon (`e2e/evm`, chain id 137 on 127.0.0.1:8545), under bezent's
 * production id as the harness declares it (`crates/store/src/environment.rs`), and the zombienet Kreivo.
 * The bed deploys DirectedDeposit afresh on each run, so its address comes from the build's environment
 * (`BRIDGE_TESTBED_DIRECTED_DEPOSIT`) and is pinned into that build; left out, the network is refused.
 */
export function testbed(directedDeposit: Pinned<Address>): OriginNetwork {
  return {
    key: 'testbed',
    label: 'Local test bed (anvil fork of Polygon)',
    network: 'polygon',
    chainId: 137,
    chain: {
      name: 'Polygon (local fork)',
      rpcUrls: ['http://127.0.0.1:8545'],
      blockExplorerUrls: [],
      nativeCurrency: { name: 'POL', symbol: 'POL', decimals: 18 },
    },
    token: POLYGON.token,
    directedDeposit,
    kreivo: KREIVO_TESTBED,
  };
}

/** The networks a deployment's build carries. */
export function networksFor(deployment: Deployment, testbedDirectedDeposit: Pinned<Address> = UNSET): OriginNetwork[] {
  switch (deployment) {
    case 'production':
      return [POLYGON];
    case 'test':
      return [POLYGON_AMOY];
    case 'testbed':
      return [testbed(testbedDirectedDeposit)];
  }
}

/** A network whose every address is set and well formed. */
export interface ResolvedNetwork extends OriginNetwork {
  token: Address;
  directedDeposit: Address;
}

export class UnsetConfiguration extends Error {}

const LOOPBACK_WS = /^wss?:\/\/(127\.0\.0\.1|localhost|\[::1\])(:\d+)?(\/|$)/;
const HEX32 = /^0x[0-9a-f]{64}$/;

/** Checks `n` before anything is read or signed on it, and refuses it when an address is unset or malformed,
 * or when its Kreivo connection is not one the app allows. Never substitutes a value. */
export function resolveNetwork(n: OriginNetwork): ResolvedNetwork {
  const pinned = (what: string, value: Pinned<string>): Address => {
    if (value === UNSET || value === undefined || value === '') {
      throw new UnsetConfiguration(`${n.label}: the ${what} address is not set in this build, so the app refuses this network.`);
    }
    if (!isAddress(value, { strict: false })) throw new UnsetConfiguration(`${n.label}: the ${what} address ${value} is malformed.`);
    if (/^0x0{40}$/i.test(value)) throw new UnsetConfiguration(`${n.label}: the ${what} address is the zero address.`);
    return getAddress(value);
  };
  const token = pinned('token', n.token);
  const directedDeposit = pinned('DirectedDeposit', n.directedDeposit);
  if (token === directedDeposit) throw new UnsetConfiguration(`${n.label}: the token and the DirectedDeposit contract are one address.`);
  if (!Number.isInteger(n.chainId) || n.chainId <= 0) throw new UnsetConfiguration(`${n.label}: no chain id.`);
  if (n.network === '') throw new UnsetConfiguration(`${n.label}: no bezent network id.`);
  const c = n.kreivo.connection;
  if (c.kind === 'loopback-rpc' && !LOOPBACK_WS.test(c.url)) {
    throw new UnsetConfiguration(`${n.kreivo.label}: an RPC connection is allowed only to a node on this machine, not ${c.url}.`);
  }
  for (const r of n.kreivo.refusedRecipients) {
    if (!HEX32.test(r)) throw new UnsetConfiguration(`${n.kreivo.label}: refused recipient ${r} is not 32 bytes of lowercase hex.`);
  }
  return { ...n, token, directedDeposit };
}
