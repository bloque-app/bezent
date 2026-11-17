// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The public status route (F-018, T-018-10; REQ-ST-8, amendment 0029 Open 8): bezent's own answer for a directed
// deposit, by its arrival's coordinate, and for a withdrawal, by its redemption's on-chain reference. T-018-10 is
// not built yet, so its URL is a build setting (`BRIDGE_STATUS_URL`), unset by default, and the status page reads
// chain data alone without it. When it is set, its answer is shown beside the chain's, as what bezent reports:
// state, named conditions and coordinates. It is never taken as an outcome (guardrail 3, REQ-RQ-7); a coordinate it
// names on the origin network is only a pointer the page reads and checks itself.
//
// The paths are the plan's (`GET /v0/public/deposits/by-coordinate`, `GET /v0/public/withdrawals/by-redemption`).
// The query parameters and the answer's fields below are this app's expectation of T-018-10, to be aligned when it
// lands; a field the answer does not carry is shown as absent, and anything else in it is ignored.

import type { Hex } from '../lib/bytes';
import { withTimeout } from '../lib/timeout';

export interface RouteCoordinate {
  network: string;
  role?: string;
  tx_hash?: string;
  block_number?: string | number;
  block_hash?: string;
  position_kind?: string;
  position_index?: number;
  final?: boolean;
}

export interface RouteAnswer {
  state: string;
  conditions: string[];
  coordinates: RouteCoordinate[];
  /** The time of the last change, as the route writes it. */
  updated_at?: string;
}

export type RouteResult = { kind: 'unset' } | { kind: 'not-found' } | { kind: 'answer'; answer: RouteAnswer } | { kind: 'error'; reason: string };

/** Reads only what REQ-ST-8 lets the route say, and drops everything else. */
export function parseAnswer(json: unknown): RouteAnswer {
  if (!json || typeof json !== 'object') throw new Error('not an object');
  const o = json as Record<string, unknown>;
  if (typeof o.state !== 'string' || o.state === '') throw new Error('no state');
  const conditions = Array.isArray(o.conditions) ? o.conditions.filter((c): c is string => typeof c === 'string') : [];
  const coordinates = Array.isArray(o.coordinates)
    ? o.coordinates
        .filter((c): c is Record<string, unknown> => !!c && typeof c === 'object' && typeof (c as { network?: unknown }).network === 'string')
        .map((c) => {
          const out: RouteCoordinate = { network: c.network as string };
          for (const k of ['role', 'tx_hash', 'block_hash', 'position_kind'] as const) if (typeof c[k] === 'string') out[k] = c[k] as string;
          if (typeof c.block_number === 'string' || typeof c.block_number === 'number') out.block_number = c.block_number;
          if (typeof c.position_index === 'number') out.position_index = c.position_index;
          if (typeof c.final === 'boolean') out.final = c.final;
          return out;
        })
    : [];
  return { state: o.state, conditions, coordinates, ...(typeof o.updated_at === 'string' ? { updated_at: o.updated_at } : {}) };
}

export const ROUTE_TIMEOUT_MS = 15_000;

async function ask(base: string | null, path: string, query: Record<string, string>, fetcher: typeof fetch): Promise<RouteResult> {
  if (!base) return { kind: 'unset' };
  const url = new URL(`${base}${path}`);
  for (const [k, v] of Object.entries(query)) url.searchParams.set(k, v);
  try {
    const res = await withTimeout(ROUTE_TIMEOUT_MS, `the status route did not answer within ${ROUTE_TIMEOUT_MS / 1000} s`, (signal) =>
      fetcher(url, { signal, credentials: 'omit', referrerPolicy: 'no-referrer', cache: 'no-store', headers: { accept: 'application/json' } }),
    );
    if (res.status === 404) return { kind: 'not-found' };
    if (!res.ok) return { kind: 'error', reason: `the status route answered ${res.status}` };
    return { kind: 'answer', answer: parseAnswer(await res.json()) };
  } catch (e) {
    return { kind: 'error', reason: (e as Error).message };
  }
}

/** bezent's answer for the directed deposit at the coordinate. */
export function depositByCoordinate(
  base: string | null,
  c: { network: string; blockHash: Hex; positionKind: string; positionIndex: number },
  fetcher: typeof fetch = fetch,
) {
  return ask(base, '/v0/public/deposits/by-coordinate', {
    network: c.network,
    block_hash: c.blockHash,
    position_kind: c.positionKind,
    position_index: String(c.positionIndex),
  }, fetcher);
}

/** bezent's answer for the withdrawal of the redemption (`redeemer`, `reference`). */
export function withdrawalByRedemption(base: string | null, redeemer: string, reference: Hex, fetcher: typeof fetch = fetch) {
  return ask(base, '/v0/public/withdrawals/by-redemption', { redeemer, reference }, fetcher);
}
