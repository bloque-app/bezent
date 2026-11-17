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
  /** The bezent contract's account (`contracts/bezent`), 32 bytes as hex: what a redemption calls and a deposit's
   * mint reference is checked against (T-018-15, T-018-17). `UNSET` refuses the redeem and the Kreivo side of the
   * status page, never the deposit. */
  bezent: Pinned<string>;
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
  /** The vault's reference emitter (`ReferenceEmitter`, F-008): every payout from the reserve carries its
   * `Reference(bytes32)` log in the same transaction (DEC-10). The status page finds a withdrawal's payout, or a
   * directed deposit's return, by it. `UNSET` leaves the status page without the payout search. */
  emitter: Pinned<Address>;
  /** The first block the status page searches for payouts from: the vault's deployment. */
  payoutsFrom: Pinned<bigint>;
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
  // T-018-18: the accounts the core refuses as a directed deposit's recipient, which the production store declares
  // in `ledger_accounts` (PLAN DEC-15, REQ-IN-5): the bezent contract's own account, and aledgent's settlement
  // contracts on Kreivo mainnet, Bloque's and Bloque Test's (`PROD` and `TEST` of aledgent's
  // `contracts/settlement/scripts/fork/common.mjs`, bloque-app/aledgent). Value minted to any of them credits no one.
  refusedRecipients: [
    // bezent (DoGtfGRPMBd4LHghKAjXAuKB7LY7m8gM1QZsu6mXrwfVdq4).
    '0x361d9535ca78edf68a88c7abd44cdec7086f8e85bcb8aedb724462eb3f2e5821',
    // aledgent's settlement contract, Bloque (Da1uhU5rnKTXtQxZDvkqDsEeS2SUaNBktAxvMa8CTKDjrdk).
    '0x2c00f55589bda36649aa0fd58fcc282de51bffebee9b17e05b9f0094f5c42dc0',
    // aledgent's settlement contract, Bloque Test (GJ4zVXnSXNbFJgo3cUXJXf9JYfVMGscukptNsnnfZLwUBr7).
    '0xa48c1c004f913d893aaa0c8a4911ba9b3e6abd26499b509d56cb3ff71e29ca0a',
  ],
  // The contract on Kreivo mainnet (DoGtfGRPMBd4LHghKAjXAuKB7LY7m8gM1QZsu6mXrwfVdq4; F-015, and
  // contracts/evm/deployments/polygon-amoy.json, which read its bounds).
  bezent: '0x361d9535ca78edf68a88c7abd44cdec7086f8e85bcb8aedb724462eb3f2e5821',
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
  // e2e/kreivo/deploy.mjs deploys the contract afresh on each run: a test bed build pins it
  // (`BRIDGE_TESTBED_BEZENT`, through `testbed`).
  bezent: UNSET,
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
  // T-018-18, deployed at block 97485770 (contracts/evm/deployments/polygon.json).
  directedDeposit: '0x35495A8273F7450fb7D25bCe6bcd5Ad4aa3F824E',
  // The production vault, the Safe the contract forwards to (its immutable `reserve()`).
  reserve: '0x584A9E72ebC1cdBA2F20233764eef08d528792ca',
  codeHash: '0x399c8e357fde99f5f3b8b7db3e77d68dcaad5b71fe705d39f4619674e3feb1ba',
  kreivo: KREIVO,
  // The vault's reference emitter (F-008), and the block of the vault's last deployment transaction: no payout
  // precedes it.
  emitter: '0xd8a9cfC1993e1B0C7D2FA96561688ca9d5cB4EC3',
  payoutsFrom: 96_814_452n,
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
  // T-018-04's vault (contracts/evm/deployments/polygon-amoy.json): its emitter, and the block its deployment began.
  emitter: '0xa1Ebae82c6Ebe16beB0864fc73D5350bE71370B9',
  payoutsFrom: 53_060_034n,
};

/** What a test bed build pins of the contract the bed deployed. */
export interface TestbedPins {
  directedDeposit: Pinned<Address>;
  reserve: Pinned<Address>;
  codeHash: Pinned<Hex>;
  /** The bezent contract the bed's Kreivo deployed (`BRIDGE_TESTBED_BEZENT`). */
  bezent?: Pinned<string>;
  /** The bed's reference emitter (`BRIDGE_TESTBED_EMITTER`) and the block to search payouts from
   * (`BRIDGE_TESTBED_PAYOUTS_FROM`). */
  emitter?: Pinned<Address>;
  payoutsFrom?: Pinned<bigint>;
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
    kreivo: { ...KREIVO_TESTBED, bezent: pins.bezent ?? UNSET },
    emitter: pins.emitter ?? UNSET,
    payoutsFrom: pins.payoutsFrom ?? UNSET,
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
  if (k.bezent !== UNSET && k.bezent !== undefined && (!HEX32.test(k.bezent) || /^0x0{64}$/.test(k.bezent))) {
    throw new UnsetConfiguration(`${k.label}: the bezent contract ${k.bezent} is not a 32-byte account in lowercase hex.`);
  }
  const emitter = n.emitter === UNSET || n.emitter === undefined ? UNSET : pinned('reference emitter', n.emitter);
  if (emitter !== UNSET && [token, directedDeposit, reserve].includes(emitter)) {
    throw new UnsetConfiguration(`${n.label}: the reference emitter is the token, the DirectedDeposit contract or the reserve.`);
  }
  const from = n.payoutsFrom === undefined ? UNSET : n.payoutsFrom;
  if (from !== UNSET && from < 0n) throw new UnsetConfiguration(`${n.label}: a negative block to search payouts from.`);
  return { ...n, token, directedDeposit, reserve, codeHash, kreivo: k, emitter, payoutsFrom: from };
}

/** The bezent contract a redemption calls on `n`'s Kreivo; refuses a build that does not pin it. */
export function bezentContract(n: ResolvedNetwork): Hex {
  const c = n.kreivo.bezent;
  if (c === UNSET || c === undefined) {
    throw new UnsetConfiguration(`${n.kreivo.label}: the bezent contract is not set in this build, so the app refuses to redeem on it.`);
  }
  return c as Hex;
}
