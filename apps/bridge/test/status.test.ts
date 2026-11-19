// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// T-018-17 (REQ-ST-1, REQ-ST-8): the status page's reads. A directed deposit is found by its coordinate (the
// transaction and its Transfer log's index), as the endpoint pairs it, with its block's finality; a payout from the
// reserve is found by the reference its emitter log carries, and checked against the address and amount it owes,
// with no fee taken; the search backs off when a provider refuses the range and says what it did not search. The
// public status route is unset by default; when set, its answer is parsed to what REQ-ST-8 lets it say.
//
// Anyone can emit a Reference log through the emitter (#138's review, M2): every one in the range is collected, and
// only one whose transaction moves exactly one transfer from the reserve is a payout.

import { describe, expect, it, vi } from 'vitest';
import { getAddress, type Hex } from 'viem';
import { connect, deposit, readContract } from '../src/evm/deposit';
import { checkPayout, depositOnPolygon, findPayout } from '../src/status/polygon';
import { depositByCoordinate, parseAnswer, withdrawalByRedemption } from '../src/status/route';
import { BLOCK_HASH, EMITTER, mockWallet, network, OWNER } from './mock-wallet';

const RECIPIENT: Hex = '0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d';
const REF: Hex = `0x${'7d'.repeat(32)}`;
const TO = getAddress('0x5b38da6a701c568545dcfcb03fcb875f56beddc4');
const quiet = () => undefined;

async function session(o: Parameters<typeof mockWallet>[0] = {}) {
  const w = mockWallet(o);
  const s = await connect(w.provider, network, 5);
  return { w, s, c: await readContract(s) };
}

describe('a directed deposit by its coordinate', () => {
  it('finds the Transfer/Directed pair at the Transfer’s log, with its finality', async () => {
    const { s, c } = await session({ finalized: 100 });
    const res = await deposit(s, c, { amount: 2_500_000n, recipient: RECIPIENT }, quiet);
    const d = await depositOnPolygon(s.public, network, res.hash, 41);
    expect(d.found).toBe(true);
    if (!d.found) return;
    expect(d.arrival).toMatchObject({ sender: OWNER.address, amount: 2_500_000n, recipient: RECIPIENT, refund: OWNER.address });
    expect(d.arrival.coordinate).toEqual({ network: 'polygon-amoy', blockNumber: 100n, blockHash: BLOCK_HASH, positionKind: 'log', positionIndex: 41 });
    expect(d.directedLog).toBe(42);
    expect(d.finality).toEqual({ blockNumber: 100n, blockHash: BLOCK_HASH, finalized: 100n, final: true, canonical: true });
  });

  it('says a block above the finalized head is not final yet', async () => {
    const { s, c } = await session({ finalized: 99 });
    const res = await deposit(s, c, { amount: 2_500_000n, recipient: RECIPIENT }, quiet);
    const d = await depositOnPolygon(s.public, network, res.hash, 41);
    expect(d.found && d.finality.final).toBe(false);
  });

  it('refuses another log of the transaction, naming the right one, and a transaction that is no deposit', async () => {
    const { s, c } = await session();
    const res = await deposit(s, c, { amount: 2_500_000n, recipient: RECIPIENT }, quiet);
    const wrong = await depositOnPolygon(s.public, network, res.hash, 42);
    expect(!wrong.found && wrong.reason).toMatch(/Log 42 .* is not a directed arrival's Transfer; this transaction's are at 41/);
    const none = await depositOnPolygon(s.public, network, `0x${'00'.repeat(32)}`, 41);
    expect(none.found).toBe(false);
  });
});

describe('a payout by its reference', () => {
  it('finds the emitter’s Reference log and the reserve’s transfer in the same transaction', async () => {
    const { s } = await session({ payouts: [{ reference: REF, to: TO, amount: 5_000_000n, block: 60 }], finalized: 90 });
    const r = await findPayout(s.public, network, EMITTER, 10n, REF);
    expect(r.complete).toBe(true);
    expect(r.found).toMatchObject({ referenceLog: 8, transfers: [{ to: TO, amount: 5_000_000n, logIndex: 7 }] });
    expect(r.found?.finality).toMatchObject({ blockNumber: 60n, final: true, canonical: true });
    expect(checkPayout(r.found!, TO, 5_000_000n)).toEqual({ ok: true });
    expect(r).toMatchObject({ others: [], unpaid: [], unexamined: 0 });
  });

  it('collects every Reference log carrying the reference, and takes as the payout only one with one transfer from the reserve', async () => {
    const { s } = await session({
      payouts: [{ reference: REF, to: TO, amount: 5_000_000n, block: 60 }],
      // Someone else's, newer and older than the payout: none moves exactly one transfer from the reserve.
      bogusReferences: [{ reference: REF, block: 95 }, { reference: REF, block: 20, reserveTransfers: 2 }],
      finalized: 90,
    });
    const r = await findPayout(s.public, network, EMITTER, 10n, REF);
    expect(r.complete).toBe(true);
    expect(r.found).toMatchObject({ blockNumber: 60n, transfers: [{ to: TO, amount: 5_000_000n }] });
    expect(r.others).toEqual([]);
    expect(r.unpaid.map((u) => [u.blockNumber, u.transfers.length])).toEqual([[95n, 0], [20n, 2]]);
    expect(checkPayout(r.found!, TO, 5_000_000n)).toEqual({ ok: true });
  });

  it('finds no payout when only someone else’s Reference log carries the reference', async () => {
    const { s } = await session({ bogusReferences: [{ reference: REF, block: 60 }] });
    const r = await findPayout(s.public, network, EMITTER, 10n, REF);
    expect(r.found).toBeUndefined();
    expect(r.unpaid).toMatchObject([{ blockNumber: 60n, referenceLog: 9, transfers: [] }]);
  });

  it('names a second payout of the same reference, and stops reading transactions past maxHits', async () => {
    const { s } = await session({ payouts: [{ reference: REF, to: TO, amount: 1n, block: 60 }, { reference: REF, to: TO, amount: 1n, block: 30 }] });
    const r = await findPayout(s.public, network, EMITTER, 10n, REF);
    expect(r.found?.blockNumber).toBe(60n);
    expect(r.others.map((o) => o.blockNumber)).toEqual([30n]);
    const capped = await findPayout(s.public, network, EMITTER, 10n, REF, { maxHits: 1 });
    expect(capped).toMatchObject({ others: [], unexamined: 1 });
  });

  it('reads Reference logs whose transaction transfers from the reserve first, so spam cannot push the payout past the cap', async () => {
    const { s } = await session({
      payouts: [{ reference: REF, to: TO, amount: 5_000_000n, block: 60 }],
      // 120 newer Reference logs anyone could emit, none with a transfer from the reserve.
      bogusReferences: Array.from({ length: 120 }, () => ({ reference: REF, block: 95 })),
      finalized: 90,
    });
    const r = await findPayout(s.public, network, EMITTER, 10n, REF);
    expect(r.found).toMatchObject({ blockNumber: 60n, transfers: [{ to: TO, amount: 5_000_000n }] });
    expect(r.unpaid).toHaveLength(99);
    expect(r.unexamined).toBe(21);
  });

  it('reports what it could not examine when spam past the cap hides every Reference log with a transfer', async () => {
    const { s } = await session({ bogusReferences: Array.from({ length: 120 }, () => ({ reference: REF, block: 95 })) });
    const r = await findPayout(s.public, network, EMITTER, 10n, REF);
    expect(r.found).toBeUndefined();
    expect(r.unexamined).toBe(20);
  });

  it('flags a payout to another address, or of another amount: no fee comes out of a delivery', async () => {
    const { s } = await session({ payouts: [{ reference: REF, to: TO, amount: 4_990_000n, block: 60 }] });
    const r = await findPayout(s.public, network, EMITTER, 10n, REF);
    const c = checkPayout(r.found!, OWNER.address, 5_000_000n);
    expect(!c.ok && c.reason).toBe(`The payout differs: to ${TO}, not ${OWNER.address}; 4990000, not 5000000 (minor units).`);
    expect(checkPayout({ ...r.found!, transfers: [] }, TO, 1n)).toMatchObject({ ok: false, reason: expect.stringMatching(/not a payout/) });
  });

  it('falls back to unranked for a range whose reserve Transfer query fails, and does not throw', async () => {
    const { s } = await session({
      payouts: [{ reference: REF, to: TO, amount: 5_000_000n, block: 60 }],
      bogusReferences: [{ reference: REF, block: 95 }],
      reserveLogsFail: true,
      finalized: 90,
    });
    const r = await findPayout(s.public, network, EMITTER, 10n, REF);
    expect(r.complete).toBe(true);
    expect(r.found).toMatchObject({ blockNumber: 60n, transfers: [{ to: TO, amount: 5_000_000n }] });
    expect(r.unpaid.map((u) => u.blockNumber)).toEqual([95n]);
    // Unranked, the Reference hits are examined newest first, and the cap still leaves the rest counted.
    const capped = await findPayout(s.public, network, EMITTER, 10n, REF, { maxHits: 1 });
    expect(capped).toMatchObject({ found: undefined, unpaid: [{ blockNumber: 95n }], unexamined: 1 });
  });

  it('ranks in three tiers: the reserve’s own first, then a range whose reserve query failed, then the known-not-reserve', async () => {
    const { s } = await session({
      payouts: [{ reference: REF, to: TO, amount: 5_000_000n, block: 60 }],
      // 95 sits in the range whose reserve query fails (unknown); 30 in a range that answered, with no reserve transfer.
      bogusReferences: [{ reference: REF, block: 95 }, { reference: REF, block: 30 }],
      reserveLogsFail: (from) => from > 60,
      finalized: 90,
    });
    const search = (maxHits: number) => findPayout(s.public, network, EMITTER, 10n, REF, { chunk: 40n, maxHits });
    // One slot: the reserve's own, not the newer unknown one.
    expect(await search(1)).toMatchObject({ found: { blockNumber: 60n }, unpaid: [], unexamined: 2 });
    // Two: the unknown one is read before the known-not-reserve, though it is newer and older ones follow.
    const two = await search(2);
    expect(two).toMatchObject({ found: { blockNumber: 60n }, unexamined: 1 });
    expect(two.unpaid.map((u) => u.blockNumber)).toEqual([95n]);
    const three = await search(3);
    expect(three.unpaid.map((u) => u.blockNumber)).toEqual([95n, 30n]);
    expect(three.unexamined).toBe(0);
  });

  it('searches from the newest block back, halving the range when the provider refuses it', async () => {
    const { s, w } = await session({ payouts: [{ reference: REF, to: TO, amount: 1n, block: 12 }], maxLogRange: 2_500, latest: 9_000 });
    const r = await findPayout(s.public, network, EMITTER, 10n, REF, { chunk: 8_000n });
    expect(r.found?.finality.blockNumber).toBe(12n);
    expect(w.logRequests).toEqual([
      { from: 1_001, to: 9_000 }, // refused
      { from: 5_001, to: 9_000 }, // refused
      { from: 7_001, to: 9_000 },
      { from: 5_001, to: 7_000 },
      { from: 3_001, to: 5_000 },
      { from: 1_001, to: 3_000 },
      { from: 10, to: 1_000 },
    ]);
  });

  it('says what it did not search when it runs out of requests, and when there is nothing to find', async () => {
    const { s } = await session();
    const partial = await findPayout(s.public, network, EMITTER, 0n, REF, { chunk: 10n, maxRequests: 3 });
    const nothing = { found: undefined, others: [], unpaid: [], unexamined: 0 };
    expect(partial).toEqual({ ...nothing, searched: { from: 72n, to: 101n }, complete: false });
    const none = await findPayout(s.public, network, EMITTER, 50n, REF);
    expect(none).toEqual({ ...nothing, searched: { from: 50n, to: 101n }, complete: true });
  });
});

describe('the public status route', () => {
  it('is unset by default: nothing is fetched', async () => {
    const fetcher = vi.fn();
    expect(await withdrawalByRedemption(null, 'H…', REF, fetcher)).toEqual({ kind: 'unset' });
    expect(fetcher).not.toHaveBeenCalled();
  });

  it('asks the plan’s paths, without credentials, and keeps only what REQ-ST-8 lets it say', async () => {
    const fetcher = vi.fn(async (url: URL | RequestInfo, init?: RequestInit) => {
      expect(init?.credentials).toBe('omit');
      expect(String(url)).toBe(
        `https://status.example/v0/public/deposits/by-coordinate?network=polygon&block_number=100&block_hash=${BLOCK_HASH}&position_kind=log&position_index=41`,
      );
      return new Response(
        JSON.stringify({
          state: 'Minted',
          conditions: ['CND-DepositWindow', 7],
          coordinates: [{ network: 'kreivo', role: 'mint', block_number: 41, final: true, requester: 'x' }, { nope: 1 }],
          request_reference: 'never shown',
          updated_at: '2026-11-16T00:00:00Z',
        }),
        { status: 200 },
      );
    });
    const r = await depositByCoordinate('https://status.example', { network: 'polygon', blockNumber: 100n, blockHash: BLOCK_HASH, positionKind: 'log', positionIndex: 41 }, fetcher as typeof fetch);
    expect(r).toEqual({
      kind: 'answer',
      answer: { state: 'Minted', conditions: ['CND-DepositWindow'], coordinates: [{ network: 'kreivo', role: 'mint', block_number: 41, final: true }], updated_at: '2026-11-16T00:00:00Z' },
    });
  });

  it('reads the route’s own answer (services/ingress/src/public.rs) and keeps its state, conditions and coordinates', () => {
    // The shape `PublicStatus` serialises to, as the ingress's tests assert it.
    const answer = parseAnswer({
      kind: 'directed_deposit',
      bridge_reference: 'bz-d-071b8b19756a50ad5929639577cdea067cc85a88',
      state: 'Source-Final',
      conditions: ['CND-DepositWindow'],
      coordinates: [
        { network: 'polygon', role: 'arrival', block_number: 100, block_hash: BLOCK_HASH, position_kind: 'log', position_index: 41, final: true },
        { network: 'kreivo', role: 'mint', block_number: 7, block_hash: '0xcc', final: false },
      ],
      transfers: [{ role: 'deposit', bridge_reference: 'bz-d-071b8b19756a50ad5929639577cdea067cc85a88', state: 'Source-Final', condition: 'CND-DepositWindow' }],
      updated_at: '2026-11-17T00:00:00Z',
    });
    expect(answer).toEqual({
      state: 'Source-Final',
      conditions: ['CND-DepositWindow'],
      coordinates: [
        { network: 'polygon', role: 'arrival', block_number: 100, block_hash: BLOCK_HASH, position_kind: 'log', position_index: 41, final: true },
        { network: 'kreivo', role: 'mint', block_number: 7, block_hash: '0xcc', final: false },
      ],
      updated_at: '2026-11-17T00:00:00Z',
    });
  });

  it('says not found, and an error, without throwing', async () => {
    expect(await withdrawalByRedemption('https://s.example', 'H', REF, (async () => new Response('', { status: 404 })) as typeof fetch)).toEqual({ kind: 'not-found' });
    expect(await withdrawalByRedemption('https://s.example', 'H', REF, (async () => new Response('', { status: 503 })) as typeof fetch)).toEqual({
      kind: 'error',
      reason: 'the status route answered 503',
    });
    expect(() => parseAnswer({ conditions: [] })).toThrow(/no state/);
  });
});
