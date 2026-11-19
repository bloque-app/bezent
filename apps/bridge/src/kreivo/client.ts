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

export interface Kreivo {
  client: PolkadotClient;
  // The unsafe API is typed by the metadata at run time; kreivo/recipient.ts narrows what it reads.
  api: unknown;
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

export async function connectKreivo(n: KreivoNetwork): Promise<Kreivo> {
  if (n.connection.kind === 'loopback-rpc') {
    const client = createClient(getWsProvider(n.connection.url));
    return { client, api: client.getUnsafeApi(), close: () => client.destroy() };
  }
  const [{ startFromWorker }, { default: SmWorker }, relaySpec, paraSpec] = await Promise.all([
    import('polkadot-api/smoldot/from-worker'),
    import('polkadot-api/smoldot/worker?worker'),
    chainSpec('kusama', 'Kusama'),
    chainSpec('kreivo', n.label),
  ]);
  const smoldot = startFromWorker(new SmWorker(), { forbidTcp: true, forbidNonLocalWs: true });
  const chain = (async () => {
    const relay = await smoldot.addChain({ chainSpec: relaySpec });
    return smoldot.addChain({ chainSpec: paraSpec, potentialRelayChains: [relay] });
  })();
  const client = createClient(getSmProvider(() => chain));
  return {
    client,
    api: client.getUnsafeApi(),
    close: () => {
      client.destroy();
      void smoldot.terminate();
    },
  };
}
