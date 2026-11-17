// @vitest-environment happy-dom
// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// T-018-17 (REQ-ST-1, REQ-ST-8): the status page, in a DOM, over the fake wallet's Polygon and a model of the bezent
// contract's reads on Kreivo. Each state of a directed deposit (on Polygon and not final, final and not minted,
// minted, returned) and of a withdrawal (no such redemption, redeemed and not paid, paid out, refunded) is shown with
// its coordinates, and a payout that differs from what the redemption owes is flagged.
//
// From #138's review: a Reference log anyone emitted (no transfer from the reserve in its transaction) is listed
// apart and never changes the headline; a payout or a return is "paid" or "returned" only once final and canonical;
// minted and returned, like refunded and paid, is flagged as an incident.

import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { getAddress } from 'viem';
import { connect, deposit, readContract, type Session } from '../src/evm/deposit';
import { depositReference, mintReference, payoutReference, unassociatedRefundPayoutReference, withdrawalReference } from '../src/lib/refs';
import type { Hex } from '../src/lib/bytes';
import type { Redemption } from '../src/kreivo/bezent';
import { initStatus, searchRows, UNPAID_LABEL } from '../src/ui/status';
import type { Payout, PayoutSearch } from '../src/status/polygon';
import { mockWallet, network, OWNER } from './mock-wallet';

const kreivo = vi.hoisted(() => ({ minted: false, redemption: undefined as unknown }));
vi.mock('../src/kreivo/redeem', async (original) => ({
  ...(await original<typeof import('../src/kreivo/redeem')>()),
  mintReferenceUsed: async () => kreivo.minted,
  readRedemption: async () => kreivo.redemption,
}));

const ALICE: Hex = '0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d';
const ALICE_SS58 = 'HNZata7iMYWmk5RvZRTiAsSDhV8366zq2YGb3tLH5Upf74F';
const REF: Hex = `0x${'ab'.repeat(32)}`;
const TO = getAddress('0x5b38da6a701c568545dcfcb03fcb875f56beddc4');
const html = readFileSync(resolve(process.cwd(), 'index.html'), 'utf8');
const quiet = () => undefined;

/** The status rows as label → value. */
const rows = () => {
  const dl = document.getElementById('status-result')!;
  const out: Record<string, string> = {};
  const dts = [...dl.querySelectorAll('dt')];
  dts.forEach((dt) => (out[dt.textContent!] = (out[dt.textContent!] ? `${out[dt.textContent!]} | ` : '') + dt.nextElementSibling!.textContent!));
  return out;
};

async function page(o: Parameters<typeof mockWallet>[0] = {}) {
  document.body.innerHTML = html.slice(html.indexOf('<body>') + 6, html.indexOf('<script'));
  const w = mockWallet(o);
  const s = await connect(w.provider, network, 5);
  const status = initStatus({ network: () => network, kreivo: () => ({ client: {}, api: {}, close: () => undefined }) as never, session: () => s, route: null });
  return { w, s, status };
}

async function deposited(s: Session) {
  return deposit(s, await readContract(s), { amount: 2_500_000n, recipient: ALICE }, quiet);
}

beforeEach(() => {
  kreivo.minted = false;
  kreivo.redemption = undefined;
});

describe('a deposit’s status', () => {
  it('on Polygon, not final yet: the pair at its coordinate, and the references derived from it', async () => {
    const { s, status } = await page({ finalized: 99 });
    const res = await deposited(s);
    await status.deposit(res.hash, '41');
    const r = rows();
    const c = res.arrival!.coordinate;
    expect(r['State (from chain data)']).toBe('on Polygon, not final yet');
    expect(r['Arrival (coordinate)']).toBe(`polygon-amoy · block 100 (${c.blockHash}) · log 41`);
    expect(r['Polygon']).toBe('included, not final yet (block 100; finalized head 99)');
    expect(r['Transfer → Directed']).toBe(`${res.hash}: Transfer log 41, Directed log 42`);
    expect(r['Recipient (Kreivo)']).toBe(ALICE_SS58);
    expect(r['Refund to']).toBe(OWNER.address);
    expect(r['Bridge reference']).toBe(depositReference(c));
    expect(r['Mint reference']).toBe(mintReference(c));
    expect(r['Kreivo']).toMatch(/^not minted/);
    expect(r['Return']).toMatch(/^none found in blocks 100 to 101$/);
    expect(r['bezent’s status route']).toMatch(/not set in this build/);
  });

  it('final and not minted, then minted', async () => {
    const { s, status } = await page({ finalized: 100 });
    const res = await deposited(s);
    await status.deposit(res.hash, '41');
    expect(rows()['State (from chain data)']).toBe('final on Polygon, not minted yet');
    kreivo.minted = true;
    await status.deposit(res.hash, '41');
    expect(rows()['State (from chain data)']).toBe('minted on Kreivo');
    expect(rows()['Kreivo']).toMatch(/^minted: the contract recorded this mint reference/);
  });

  it('returned to its refund address, by the return’s payout reference', async () => {
    // The fake wallet's first deposit is at block 100, log 41: its coordinate is known before it is made.
    const c = { network: 'polygon-amoy', blockNumber: 100n, blockHash: `0x${'ab'.repeat(32)}` as Hex, positionKind: 'log' as const, positionIndex: 41 };
    const { s, status } = await page({ finalized: 101, payouts: [{ reference: unassociatedRefundPayoutReference(c), to: OWNER.address, amount: 2_500_000n, block: 101 }] });
    const res = await deposited(s);
    await status.deposit(res.hash, '41');
    expect(rows()['State (from chain data)']).toBe('returned to the refund address');
    expect(rows()['Return']).toMatch(/Reference log 8; final \(block 101 ≤ finalized head 101\)$/);
    expect(rows()['Paid']).toBe(`2500000 (minor units) to ${OWNER.address}, Transfer log 7`);
  });

  // The fake wallet's first deposit is at block 100, log 41.
  const first = { network: 'polygon-amoy', blockNumber: 100n, blockHash: `0x${'ab'.repeat(32)}` as Hex, positionKind: 'log' as const, positionIndex: 41 };

  it('a return included but not final yet is not "returned"', async () => {
    const { s, status } = await page({ finalized: 100, latest: 102, payouts: [{ reference: unassociatedRefundPayoutReference(first), to: OWNER.address, amount: 2_500_000n, block: 101 }] });
    const res = await deposited(s);
    await status.deposit(res.hash, '41');
    expect(rows()['State (from chain data)']).toBe('a return to the refund address is included on Polygon, not final');
    expect(rows()['Transfer (included, not final)']).toBe(`2500000 (minor units) to ${OWNER.address}, Transfer log 7`);
    expect(rows()['Paid']).toBeUndefined();
  });

  it('minted and returned is an incident, never a state', async () => {
    kreivo.minted = true;
    const { s, status } = await page({ finalized: 101, payouts: [{ reference: unassociatedRefundPayoutReference(first), to: OWNER.address, amount: 2_500_000n, block: 101 }] });
    const res = await deposited(s);
    await status.deposit(res.hash, '41');
    expect(rows()['State (from chain data)']).toBe('minted on Kreivo, and a return carries its reference: both should never happen; report it');
  });

  it('a Reference log someone else emitted with the return’s reference does not replace “minted”', async () => {
    kreivo.minted = true;
    const { s, status } = await page({ finalized: 101, bogusReferences: [{ reference: unassociatedRefundPayoutReference(first), block: 101 }] });
    const res = await deposited(s);
    await status.deposit(res.hash, '41');
    const r = rows();
    expect(r['State (from chain data)']).toBe('minted on Kreivo');
    expect(r['Return']).toMatch(/^none found/);
    expect(r[UNPAID_LABEL]).toMatch(/^0x[0-9a-f]{64}, Reference log 9, block 101: its transaction moves nothing from the reserve$/);
    expect(r['Check']).toBeUndefined();
  });

  it('a transaction or a log that is not a directed deposit', async () => {
    const { s, status } = await page();
    const res = await deposited(s);
    await status.deposit(res.hash, '42');
    expect(rows()['State']).toBe('not a directed deposit');
    expect(rows()['Why']).toMatch(/this transaction's are at 41/);
    await status.deposit('0x12', '41');
    expect(document.getElementById('status-note')!.textContent).toMatch(/64 hex digits/);
  });
});

describe('a withdrawal’s status', () => {
  const record = (o: Partial<Redemption> = {}): Redemption => ({
    redeemer: ALICE,
    asset: 101,
    amount: 5_000_000n,
    backend: { kind: 'evm', chainId: 80002n },
    payout: { kind: 'address', address: TO.toLowerCase() as Hex },
    refunded: false,
    ...o,
  });
  const ref = payoutReference(withdrawalReference(ALICE, REF));

  it('no such redemption: its references are still shown', async () => {
    const { status } = await page();
    await status.withdrawal(ALICE_SS58, REF);
    expect(rows()['State (from chain data)']).toBe('no such redemption');
    expect(rows()['On-chain reference']).toBe(`${ALICE_SS58} + ${REF}`);
    expect(rows()['Bridge reference']).toBe(withdrawalReference(ALICE, REF));
    expect(rows()['Payout reference']).toBe(ref);
  });

  it('redeemed, not paid yet; then paid out, with the payout’s coordinates', async () => {
    kreivo.redemption = record();
    const unpaid = await page();
    await unpaid.status.withdrawal(ALICE_SS58, REF);
    expect(rows()['State (from chain data)']).toBe('redeemed on Kreivo; payout not found yet');
    expect(rows()['Paid to']).toBe(TO);
    const paid = await page({ payouts: [{ reference: ref, to: TO, amount: 5_000_000n, block: 80 }] });
    await paid.status.withdrawal(ALICE_SS58, REF);
    expect(rows()['State (from chain data)']).toBe('paid out on Polygon');
    expect(rows()['Payout']).toMatch(/^0x[0-9a-f]{64}, Reference log 8; final \(block 80 ≤ finalized head 90\)$/);
  });

  it('flags a payout that differs from what the redemption owes', async () => {
    kreivo.redemption = record();
    const { status } = await page({ payouts: [{ reference: ref, to: TO, amount: 4_000_000n, block: 80 }] });
    await status.withdrawal(ALICE_SS58, REF);
    expect(rows()['State (from chain data)']).toMatch(/differs/);
    expect(rows()['Check']).toBe('The payout differs: 4000000, not 5000000 (minor units).');
  });

  it('a Reference log someone else emitted beside the real payout is listed apart, and the payout decides', async () => {
    kreivo.redemption = record();
    const { status } = await page({ payouts: [{ reference: ref, to: TO, amount: 5_000_000n, block: 80 }], bogusReferences: [{ reference: ref, block: 85 }] });
    await status.withdrawal(ALICE_SS58, REF);
    const r = rows();
    expect(r['State (from chain data)']).toBe('paid out on Polygon');
    expect(r['Payout']).toMatch(/Reference log 8; final/);
    expect(r[UNPAID_LABEL]).toMatch(/block 85: its transaction moves nothing from the reserve$/);
    expect(r['Check']).toBeUndefined();
  });

  it('a Reference log someone else emitted alone is no payout, and does not say “differs”', async () => {
    kreivo.redemption = record();
    const { status } = await page({ bogusReferences: [{ reference: ref, block: 85 }] });
    await status.withdrawal(ALICE_SS58, REF);
    const r = rows();
    expect(r['State (from chain data)']).toBe('redeemed on Kreivo; payout not found yet');
    expect(r['State (from chain data)']).not.toMatch(/differs/);
    expect(r['Payout']).toMatch(/^none found/);
    expect(r[UNPAID_LABEL]).toMatch(/block 85/);
  });

  it('more Reference logs than the page reads, and none a payout, is “could not be determined”, not “not paid”', async () => {
    kreivo.redemption = record();
    const { status } = await page({ bogusReferences: Array.from({ length: 120 }, () => ({ reference: ref, block: 85 })) });
    await status.withdrawal(ALICE_SS58, REF);
    const r = rows();
    expect(r['State (from chain data)']).toMatch(/could not be determined: too many Reference logs/);
    expect(r['State (from chain data)']).not.toMatch(/payout not found yet/);
    expect(r['Undetermined']).toMatch(/^20 more Reference logs/);
    // Said once: the count is not repeated among the logs without a payout.
    expect(r[UNPAID_LABEL]).not.toMatch(/more not examined/);
  });

  it('a real payout is found even behind more than 100 Reference logs anyone emitted', async () => {
    kreivo.redemption = record();
    const { status } = await page({
      payouts: [{ reference: ref, to: TO, amount: 5_000_000n, block: 80 }],
      bogusReferences: Array.from({ length: 120 }, () => ({ reference: ref, block: 95 })),
    });
    await status.withdrawal(ALICE_SS58, REF);
    expect(rows()['State (from chain data)']).toBe('paid out on Polygon');
  });

  it('a payout included but not final yet is not “paid”', async () => {
    kreivo.redemption = record();
    const { status } = await page({ finalized: 79, payouts: [{ reference: ref, to: TO, amount: 5_000_000n, block: 80 }] });
    await status.withdrawal(ALICE_SS58, REF);
    expect(rows()['State (from chain data)']).toBe('a payout is included on Polygon, not final');
    expect(rows()['Transfer (included, not final)']).toBe(`5000000 (minor units) to ${TO}, Transfer log 7`);
    expect(rows()['Paid']).toBeUndefined();
  });

  it('refunded and paid is an incident', async () => {
    kreivo.redemption = record({ refunded: true });
    const { status } = await page({ payouts: [{ reference: ref, to: TO, amount: 5_000_000n, block: 80 }] });
    await status.withdrawal(ALICE_SS58, REF);
    expect(rows()['State (from chain data)']).toBe('refunded on Kreivo, and a payout carries its reference: both should never happen; report it');
  });

  it('refunded on Kreivo', async () => {
    kreivo.redemption = record({ refunded: true });
    const { status } = await page();
    await status.withdrawal(ALICE_SS58, REF);
    expect(rows()['State (from chain data)']).toBe('refunded on Kreivo');
    expect(rows()['Refunded']).toMatch(/^yes/);
  });

  it('refuses a redeemer that is not a Kreivo address', async () => {
    const { status } = await page();
    await status.withdrawal('5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY', REF);
    expect(document.getElementById('status-note')!.textContent).toMatch(/prefix 2/);
  });
});

describe('a second payout that is included but not final', () => {
  const payout = (block: bigint, final: boolean, canonical = true): Payout => ({
    txHash: `0x${block.toString(16).padStart(64, '0')}`,
    referenceLog: 3,
    blockNumber: block,
    blockHash: `0x${'aa'.repeat(32)}`,
    transfers: [{ to: TO, amount: 5_000_000n, logIndex: 2 }],
    finality: { blockNumber: block, blockHash: `0x${'aa'.repeat(32)}`, finalized: final ? block : block - 1n, final, canonical },
  });
  const search = (found: Payout, others: Payout[]): PayoutSearch => ({ found, others, unpaid: [], unexamined: 0, searched: { from: 1n, to: 100n }, complete: true });
  const check = { to: TO, amount: 5_000_000n };

  it('is a pending possible double payout, not plain “paid”', () => {
    const r = searchRows('Payout', search(payout(60n, true), [payout(50n, false)]), check);
    expect(r.paid).toBe('maybe-twice');
    expect(r.rows.map(([k]) => k)).toContain('Possible double payout (pending)');
    expect(searchRows('Payout', search(payout(60n, true), []), check).paid).toBe('yes');
  });

  it('stays a double payout when the second is final, and ignores one that was reorganised away', () => {
    expect(searchRows('Payout', search(payout(60n, true), [payout(50n, true)]), check).paid).toBe('twice');
    expect(searchRows('Payout', search(payout(60n, true), [payout(50n, false, false)]), check).paid).toBe('yes');
  });
});
