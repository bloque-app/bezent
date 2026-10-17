// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Shared by the EVM scripts: the forks, the tokens, and the CCTP V2 contracts.
//
// Addresses checked on 2026-10-13 against Circle's published CCTP V2 deployment
// (developers.circle.com/cctp/references/contract-addresses) and against the bytecode at the pinned
// blocks (code present; `localDomain()` 7 on Polygon and 6 on Base; `version()` 1; signature
// threshold 2 with 2 enabled attesters before the harness touches them).

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createPublicClient, createWalletClient, defineChain, http, keccak256, parseAbi, toBytes } from 'viem';
import { mnemonicToAccount, privateKeyToAccount } from 'viem/accounts';

const HERE = dirname(fileURLToPath(import.meta.url));

export const log = (...args) => console.log('[evm]', ...args);

/// forks.env and the run directory's endpoints.env, as one map.
export function config() {
  const out = process.env.EVM_OUT ?? join(HERE, '.run');
  const parse = (file) =>
    Object.fromEntries(
      readFileSync(file, 'utf8')
        .split('\n')
        .map((l) => l.trim())
        .filter((l) => l && !l.startsWith('#') && l.includes('='))
        .map((l) => [l.slice(0, l.indexOf('=')), l.slice(l.indexOf('=') + 1)]),
    );
  return { ...parse(join(HERE, 'forks.env')), ...parse(join(out, 'endpoints.env')), out };
}

/// The networks the forks stand for. `domain` is the CCTP domain, where there is one.
export const NETWORKS = {
  polygon: { key: 'POLYGON', name: 'Polygon', domain: 7 },
  base: { key: 'BASE', name: 'Base', domain: 6 },
  bsc: { key: 'BSC', name: 'BNB Chain' },
};

/// The origin tokens of plan DEC-7 on each fork (symbol and decimals are checked on the fork).
export const TOKENS = {
  polygon: [{ symbol: 'USDC', address: '0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359', decimals: 6 }],
  base: [
    { symbol: 'USDC', address: '0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913', decimals: 6 },
    { symbol: 'EURC', address: '0x60a3E35Cc302bFA44Cb288Bc5a4F316Fdb1adb42', decimals: 6 },
  ],
  bsc: [{ symbol: 'BTCB', address: '0x7130d2A12B9BCbFAe4f2634d864A1Ee1Ce3Ead9c', decimals: 18 }],
};

/// CCTP V2: the same addresses on every EVM domain.
export const CCTP = {
  tokenMessenger: '0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d',
  messageTransmitter: '0x81D40F21F12A8F0E3252Bccb954D722d4c464B64',
  tokenMinter: '0xfd78EE919681417d192449715b2594ab58f5D002',
};

/// `FINALITY_THRESHOLD_FINALIZED` in Circle's FinalityThresholds.sol: a standard transfer.
export const FINALIZED = 2000;

export const ERC20_ABI = parseAbi([
  'function symbol() view returns (string)',
  'function decimals() view returns (uint8)',
  'function totalSupply() view returns (uint256)',
  'function balanceOf(address) view returns (uint256)',
  'function approve(address,uint256) returns (bool)',
]);

export const TRANSMITTER_ABI = parseAbi([
  'function localDomain() view returns (uint32)',
  'function version() view returns (uint32)',
  'function attesterManager() view returns (address)',
  'function signatureThreshold() view returns (uint256)',
  'function getNumEnabledAttesters() view returns (uint256)',
  'function isEnabledAttester(address) view returns (bool)',
  'function enableAttester(address)',
  'function setSignatureThreshold(uint256)',
  'function usedNonces(bytes32) view returns (uint256)',
  'function receiveMessage(bytes message, bytes attestation) returns (bool)',
  'event MessageSent(bytes message)',
  'event MessageReceived(address indexed caller, uint32 sourceDomain, bytes32 indexed nonce, bytes32 sender, uint32 indexed finalityThresholdExecuted, bytes messageBody)',
]);

export const MESSENGER_ABI = parseAbi([
  'function depositForBurn(uint256 amount, uint32 destinationDomain, bytes32 mintRecipient, address burnToken, bytes32 destinationCaller, uint256 maxFee, uint32 minFinalityThreshold)',
  'function getMinFeeAmount(uint256 amount) view returns (uint256)',
]);

/// The test attester. A test value, public on purpose: it signs only for the forks, and only after
/// cctp-setup.mjs has enabled it there. Never a key of any real network.
export const TEST_ATTESTER_KEY = keccak256(toBytes('bezent e2e: local CCTP test attester'));
export const TEST_ATTESTER = privateKeyToAccount(TEST_ATTESTER_KEY);

/// anvil's default dev accounts (the well-known "test test ... junk" mnemonic), dev forks only.
export const devAccount = (index) =>
  mnemonicToAccount('test test test test test test test test test test test junk', { addressIndex: index });

/// Clients for one fork.
export function fork(name, cfg = config()) {
  const n = NETWORKS[name];
  const url = cfg[`${n.key}_RPC`];
  if (!url) throw new Error(`no ${n.key}_RPC in ${cfg.out}/endpoints.env: run e2e/evm/up.sh`);
  const chain = defineChain({
    id: Number(cfg[`${n.key}_CHAIN_ID`]),
    name: n.name,
    nativeCurrency: { name: 'native', symbol: 'ETH', decimals: 18 },
    rpcUrls: { default: { http: [url] } },
  });
  const transport = http(url, { timeout: 120_000 });
  const pub = createPublicClient({ chain, transport, pollingInterval: 500 });
  const wallet = (account) => createWalletClient({ chain, transport, account });
  /// Refuse unless the endpoint is a local anvil node. Checked once, and before the first anvil_*
  /// call: those calls rewrite state, and on a real RPC they would be an attack, not a test.
  let checked;
  const assertAnvil = () => (checked ??= assertLocalAnvil(url, n.name));
  /// anvil's own methods (impersonation, balances, storage).
  const anvil = async (method, params) => {
    await assertAnvil();
    return pub.request({ method, params });
  };
  return { name, ...n, chain, url, pub, wallet, anvil, assertAnvil, block: BigInt(cfg[`${n.key}_BLOCK`]) };
}

const LOOPBACK = new Set(['127.0.0.1', '::1', '[::1]', 'localhost']);

/// Throw unless `url` is loopback (127.0.0.1, ::1, localhost) and its `web3_clientVersion` says anvil.
export async function assertLocalAnvil(url, label = url) {
  let host;
  try {
    host = new URL(url).hostname.toLowerCase();
  } catch {
    throw new Error(`${label}: refusing to run: "${url}" is not a URL`);
  }
  if (!LOOPBACK.has(host)) {
    throw new Error(`${label}: refusing to run anvil_* methods against non-loopback host "${host}"`);
  }
  let version;
  try {
    const res = await fetch(url, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'web3_clientVersion', params: [] }),
      signal: AbortSignal.timeout(15_000),
    });
    version = (await res.json()).result;
  } catch (e) {
    throw new Error(`${label}: refusing to run: web3_clientVersion failed (${e.message})`);
  }
  if (typeof version !== 'string' || !/^anvil/i.test(version)) {
    throw new Error(`${label}: refusing to run: web3_clientVersion is "${version}", not anvil`);
  }
}

/// Wait for a receipt by asking for it. viem's `waitForTransactionReceipt` waits on new block
/// numbers, and an anvil fork mines only when a transaction arrives, so a missed block can leave it
/// waiting for one that never comes.
export async function receipt(f, hash, timeoutMs = 300_000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    try {
      return await f.pub.getTransactionReceipt({ hash });
    } catch (e) {
      if (e.name !== 'TransactionReceiptNotFoundError' || Date.now() > deadline) throw e;
    }
    await new Promise((r) => setTimeout(r, 500));
  }
}

/// Send as an impersonated account (anvil_impersonateAccount), with gas money set first.
export async function asImpersonated(f, address, write) {
  await f.assertAnvil();
  await f.anvil('anvil_setBalance', [address, '0x56BC75E2D63100000']);
  await f.anvil('anvil_impersonateAccount', [address]);
  try {
    const hash = await f.wallet(address).writeContract({ ...write, account: address });
    const rcpt = await receipt(f, hash);
    if (rcpt.status !== 'success') throw new Error(`${write.functionName} as ${address} reverted`);
    return rcpt;
  } finally {
    await f.anvil('anvil_stopImpersonatingAccount', [address]);
  }
}

export const toBytes32 = (address) => `0x${address.slice(2).toLowerCase().padStart(64, '0')}`;
