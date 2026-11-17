// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The networks the app deposits on, pinned in the build (F-018 plan part 4, "Defence": the contract's address
// is pinned in the build and shown). Each origin network names its DirectedDeposit contract, the contract's
// reserve and runtime code hash, the admitted token and the Kreivo it mints on. A value that is not known yet is
// `UNSET`, and `resolveNetwork` refuses such a network: the app never falls back to another address, another
// network or a default.

import { getAddress, isAddress, type Address, type Hex } from 'viem';

/** A configuration value that is deliberately not set yet (for example, a contract not yet deployed). */
export const UNSET = null;
export type Pinned<T> = T | typeof UNSET;

/** Which build this is. A build carries the networks of one deployment only. */
export type Deployment = 'testbed' | 'test' | 'production';

export interface KreivoNetwork {
  /** bezent's id of the issuing network. */
  network: string;
  label: string;
  /** Which deployment this issuing network belongs to; an origin network pairs only with its own (no test
   * deposit ever mints on live Kreivo). */
  deployment: Deployment;
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
   * DEC-15 ledger accounts), as hex. A deposit to one would be returned; the app refuses it before signing.
   * `resolveNetwork` refuses a production issuing network whose list is empty. */
  refusedRecipients: readonly string[];
  /** frame_system's `MaxConsumers`: an account at this many consumer references cannot take the one a first
   * mint of a non-sufficient asset adds (`TooManyConsumers`). It is a `Config` type, not a pallet constant, so
   * the metadata does not carry it and it is pinned here. */
  maxConsumers: number;
}

export interface OriginNetwork {
  key: string;
  label: string;
  /** Which deployment this origin network belongs to. */
  deployment: Deployment;
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
  /** The contract's immutable `reserve()`, the vault it forwards deposits to. The app refuses a contract whose
   * `reserve()` differs. */
  reserve: Pinned<Address>;
  /** keccak256 of the contract's runtime code (`eth_getCode`), immutables included. The app refuses a contract
   * whose code hashes otherwise. */
  codeHash: Pinned<Hex>;
  /** The issuing network its deposits mint on; `UNSET` until one is named for it. */
  kreivo: Pinned<KreivoNetwork>;
}

/** Kreivo, read through the light client (its chain specification is `services/kreivo-endpoint/chain-specs/
 * kreivo.json`, shipped beside the app). */
export const KREIVO: KreivoNetwork = {
  network: 'kreivo',
  label: 'Kreivo',
  deployment: 'production',
  ss58Prefix: 2,
  asset: { id: 101, symbol: 'bUSDC', decimals: 6 },
  connection: { kind: 'light-client', relay: 'kusama' },
  // Deliberately empty until the mainnet deploy (T-018-18) pins the Kreivo contract's account and the DEC-15
  // ledger accounts here; until then `resolveNetwork` refuses every network that mints on live Kreivo.
  refusedRecipients: [],
  // Kreivo's runtime: `type MaxConsumers = ConstU32<16>` (runtime/kreivo/src/config/system.rs).
  maxConsumers: 16,
};

/** The zombienet test bed's Kreivo (`e2e/network`), through its collator's RPC on this machine. */
export const KREIVO_TESTBED: KreivoNetwork = {
  network: 'kreivo-local',
  label: 'Kreivo (local test bed)',
  deployment: 'testbed',
  ss58Prefix: 2,
  asset: { id: 101, symbol: 'bUSDC', decimals: 6 },
  connection: { kind: 'loopback-rpc', url: 'ws://127.0.0.1:9944' },
  refusedRecipients: [],
  maxConsumers: 16,
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
  deployment: 'production',
  network: 'polygon',
  chainId: 137,
  chain: POLYGON_CHAIN,
  token: '0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359',
  // Not deployed yet: T-018-18 deploys it and pins its address, its reserve and its code hash here.
  directedDeposit: UNSET,
  reserve: UNSET,
  codeHash: UNSET,
  kreivo: KREIVO,
};

export const POLYGON_AMOY: OriginNetwork = {
  key: 'polygon-amoy',
  label: 'Polygon Amoy (test)',
  deployment: 'test',
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
  // T-018-04, deployed at block 53060139 (contracts/evm/deployments/polygon-amoy.json).
  directedDeposit: '0xB8F75BeAE2723637313072fC89C69B7C15EdAec7',
  reserve: '0x3530dBB6986Dc584bf3D83a0ed4cbE027238204a',
  codeHash: '0x6c32ce3fe773ad4f2bb35e6f83948766d3058c0dbc016ffed003b891489307f0',
  // No test Kreivo is named yet, so Amoy is still refused until one is. Never live Kreivo: a test deposit must not mint real bUSDC,
  // and `resolveNetwork` refuses a test origin network paired with a production issuing network.
  kreivo: UNSET,
};

/** What a test bed build pins of the contract the bed deployed. */
export interface TestbedPins {
  directedDeposit: Pinned<Address>;
  reserve: Pinned<Address>;
  codeHash: Pinned<Hex>;
}

export const NO_TESTBED_PINS: TestbedPins = { directedDeposit: UNSET, reserve: UNSET, codeHash: UNSET };

/**
 * The local test bed: the anvil fork of Polygon (`e2e/evm`, chain id 137 on 127.0.0.1:8545), under bezent's
 * production id as the harness declares it (`crates/store/src/environment.rs`), and the zombienet Kreivo.
 * The bed deploys DirectedDeposit afresh on each run, so its address, reserve and code hash come from the
 * build's environment (`BRIDGE_TESTBED_DIRECTED_DEPOSIT`, `BRIDGE_TESTBED_RESERVE`, `BRIDGE_TESTBED_CODE_HASH`)
 * and are pinned into that build; left out, the network is refused.
 */
export function testbed(pins: TestbedPins): OriginNetwork {
  return {
    key: 'testbed',
    label: 'Local test bed (anvil fork of Polygon)',
    deployment: 'testbed',
    network: 'polygon',
    chainId: 137,
    chain: {
      name: 'Polygon (local fork)',
      rpcUrls: ['http://127.0.0.1:8545'],
      blockExplorerUrls: [],
      nativeCurrency: { name: 'POL', symbol: 'POL', decimals: 18 },
    },
    token: POLYGON.token,
    directedDeposit: pins.directedDeposit,
    reserve: pins.reserve,
    codeHash: pins.codeHash,
    kreivo: KREIVO_TESTBED,
  };
}

/** The networks a deployment's build carries. */
export function networksFor(deployment: Deployment, testbedPins: TestbedPins = NO_TESTBED_PINS): OriginNetwork[] {
  switch (deployment) {
    case 'production':
      return [POLYGON];
    case 'test':
      return [POLYGON_AMOY];
    case 'testbed':
      return [testbed(testbedPins)];
  }
}

/** A network whose every pinned value is set and well formed. */
export interface ResolvedNetwork extends OriginNetwork {
  token: Address;
  directedDeposit: Address;
  reserve: Address;
  codeHash: Hex;
  kreivo: KreivoNetwork;
}

export class UnsetConfiguration extends Error {}

const LOOPBACK_WS = /^wss?:\/\/(127\.0\.0\.1|localhost|\[::1\])(:\d+)?(\/|$)/;
const HEX32 = /^0x[0-9a-f]{64}$/;

/** Checks `n` before anything is read or signed on it, and refuses it when a pinned value is unset or
 * malformed, when it pairs with an issuing network of another deployment, when a production issuing network
 * names no refused recipient, or when its Kreivo connection is not one the app allows. Never substitutes a
 * value. */
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
  const reserve = pinned('reserve', n.reserve);
  if (n.codeHash === UNSET || n.codeHash === undefined || (n.codeHash as string) === '') {
    throw new UnsetConfiguration(`${n.label}: the DirectedDeposit code hash is not set in this build, so the app refuses this network.`);
  }
  if (!HEX32.test(n.codeHash)) throw new UnsetConfiguration(`${n.label}: the code hash ${n.codeHash} is not 32 bytes of lowercase hex.`);
  const codeHash = n.codeHash;
  if (new Set([token, directedDeposit, reserve]).size !== 3) {
    throw new UnsetConfiguration(`${n.label}: the token, the DirectedDeposit contract and the reserve are not three addresses.`);
  }
  if (!Number.isInteger(n.chainId) || n.chainId <= 0) throw new UnsetConfiguration(`${n.label}: no chain id.`);
  if (n.network === '') throw new UnsetConfiguration(`${n.label}: no bezent network id.`);
  const k = n.kreivo;
  if (k === UNSET || k === undefined) {
    throw new UnsetConfiguration(`${n.label}: the issuing network its deposits mint on is not set in this build, so the app refuses this network.`);
  }
  if (k.deployment !== n.deployment) {
    throw new UnsetConfiguration(`${n.label} (${n.deployment}) cannot mint on ${k.label} (${k.deployment}): a network pairs only with its own deployment.`);
  }
  if (k.deployment === 'production' && k.refusedRecipients.length === 0) {
    throw new UnsetConfiguration(`${k.label}: no refused recipient is pinned in this build (the contract's and the ledger accounts), so the app refuses this network.`);
  }
  const c = k.connection;
  if (c.kind === 'loopback-rpc' && !LOOPBACK_WS.test(c.url)) {
    throw new UnsetConfiguration(`${k.label}: an RPC connection is allowed only to a node on this machine, not ${c.url}.`);
  }
  for (const r of k.refusedRecipients) {
    if (!HEX32.test(r)) throw new UnsetConfiguration(`${k.label}: refused recipient ${r} is not 32 bytes of lowercase hex.`);
  }
  if (!Number.isInteger(k.maxConsumers) || k.maxConsumers <= 0) throw new UnsetConfiguration(`${k.label}: no MaxConsumers.`);
  return { ...n, token, directedDeposit, reserve, codeHash, kreivo: k };
}
