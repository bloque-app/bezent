// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Kreivo through polkadot-api (F-018 plan part 4): on a live network, smoldot in a worker, started with the
// relay chain's specification (Kusama's, as polkadot-api's known chains carry it) and Kreivo's, both shipped
// beside the page as their own files and checked against the build's hashes, so the app trusts no RPC and runs
// none. The local test bed's chain specification changes with every spawn, so there the app
// reads its collator over a loopback WebSocket instead (config/networks.ts refuses any other URL).
//
// polkadot-api's unsafe API is used, as in e2e/kreivo: there are no descriptors to generate or commit, and
// storage is named as in the metadata (`System.Account`, `Assets.Asset`).

import { createClient, type PolkadotClient } from 'polkadot-api';
import { getSmProvider } from 'polkadot-api/sm-provider';
import { getWsProvider } from 'polkadot-api/ws';
import type { KreivoNetwork } from '../config/networks';
import { CHAIN_SPEC_SHA256 } from '../config/build';
import { toHex } from '../lib/bytes';
import { sha256 } from '@noble/hashes/sha2';
import { DB_MAX_BYTES, loadDatabase, persistDatabases } from './db-cache';
import { withTimeout } from '../lib/timeout';

/** How long the light client may take to reach its first finalized block before the app gives up and offers a
 * retry: a first visit warp-syncs Kusama (about a minute), a later one starts from the saved database. */
export const SYNC_TIMEOUT_MS = 180_000;

export interface Kreivo {
  client: PolkadotClient;
  // The unsafe API is typed by the metadata at run time; kreivo/recipient.ts narrows what it reads.
  api: unknown;
  /** Settles when the chain has its first finalized block: until then a read would only wait for it. Rejects
   * after `SYNC_TIMEOUT_MS`. Reads start their own deadlines after it, never before. */
  synced: Promise<void>;
  close(): void;
}

/** Fetches a chain specification shipped beside the app, and refuses one whose hash differs from the
 * build's. */
async function chainSpec(name: keyof typeof CHAIN_SPEC_SHA256, label: string): Promise<string> {
  const res = await fetch(new URL(`chain-specs/${name}.json`, document.baseURI), { cache: 'no-cache' });
  if (!res.ok) throw new Error(`${label}'s chain specification could not be loaded (${res.status}).`);
  const text = await res.text();
  const have = toHex(sha256(new TextEncoder().encode(text))).slice(2);
  if (have !== CHAIN_SPEC_SHA256[name]) throw new Error(`${label}’s chain specification is not the one this build ships; refusing it.`);
  return text;
}

const SYNC_LATE = (label: string) =>
  `${label} did not sync within ${Math.round(SYNC_TIMEOUT_MS / 60_000)} minutes: the light client found no peers, or the network is slow. Nothing was signed.`;

/** The first finalized block, within `SYNC_TIMEOUT_MS`. The rejection is also handled here, so a caller that
 * closes the client before awaiting it leaves no unhandled rejection. */
function firstFinalized(client: PolkadotClient, label: string): Promise<void> {
  const p = withTimeout(SYNC_TIMEOUT_MS, SYNC_LATE(label), () => client.getFinalizedBlock()).then(() => undefined);
  p.catch(() => undefined);
  return p;
}

export async function connectKreivo(n: KreivoNetwork): Promise<Kreivo> {
  if (n.connection.kind === 'loopback-rpc') {
    const client = createClient(getWsProvider(n.connection.url));
    return { client, api: client.getUnsafeApi(), synced: firstFinalized(client, n.label), close: () => client.destroy() };
  }
  const [{ startFromWorker }, { default: SmWorker }, relaySpec, paraSpec, relayDb, paraDb] = await Promise.all([
    import('polkadot-api/smoldot/from-worker'),
    import('polkadot-api/smoldot/worker?worker'),
    chainSpec('kusama', 'Kusama'),
    chainSpec('kreivo', n.label),
    loadDatabase(CHAIN_SPEC_SHA256.kusama),
    loadDatabase(CHAIN_SPEC_SHA256.kreivo),
  ]);
  const smoldot = startFromWorker(new SmWorker(), { forbidTcp: true, forbidNonLocalWs: true });
  /** A saved database is trusted input to smoldot, and it is ours, keyed by the specification's hash; one that
   * smoldot refuses means a cold start, not a failure. */
  const add = async (opts: Parameters<typeof smoldot.addChain>[0], databaseContent: string | undefined) => {
    if (databaseContent === undefined) return smoldot.addChain(opts);
    try {
      return await smoldot.addChain({ ...opts, databaseContent });
    } catch {
      return smoldot.addChain(opts);
    }
  };
  const chains = (async () => {
    const relay = await add({ chainSpec: relaySpec }, relayDb);
    const para = await add({ chainSpec: paraSpec, potentialRelayChains: [relay] }, paraDb);
    return { relay, para };
  })();
  const client = createClient(getSmProvider(async () => (await chains).para));
  // The relay chain's database is asked for through a client of its own on the relay chain, which only makes
  // that one request.
  const relayClient = createClient(getSmProvider(async () => (await chains).relay));
  const synced = firstFinalized(client, n.label);
  let stop: (() => void) | undefined;
  let closed = false;
  void synced.then(() => {
    if (closed) return;
    const dump = (c: PolkadotClient) => () => c._request('chainHead_unstable_finalizedDatabase', [DB_MAX_BYTES]);
    stop = persistDatabases([
      { spec: CHAIN_SPEC_SHA256.kusama, dump: dump(relayClient) },
      { spec: CHAIN_SPEC_SHA256.kreivo, dump: dump(client) },
    ]);
  });
  return {
    client,
    api: client.getUnsafeApi(),
    synced,
    close: () => {
      closed = true;
      stop?.();
      relayClient.destroy();
      client.destroy();
      void smoldot.terminate();
    },
  };
}
