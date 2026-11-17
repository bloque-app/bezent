// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// T-018-14: the locked review. While a deposit is in flight every control that changes what is deposited is
// disabled; the deposit runs on the snapshot taken at the click (the recipient re-check included), never on the
// form; and after the receipt the `Directed` log is compared with the snapshot, any difference failing loudly.

import { readFileSync } from 'node:fs';
import { describe, expect, it, vi } from 'vitest';
import { getAddress, type Hex } from 'viem';
import { connect, deposit, readContract } from '../src/evm/deposit';
import { encodeSs58 } from '../src/lib/ss58';
import { fromHex } from '../src/lib/bytes';
import { compareArrival, LOCKED_CONTAINERS, LOCKED_IDS, lockControls, runDeposit, takeSnapshot, type RunDeps } from '../src/review';
import { REDEEM_LOCKED_CONTAINERS, REDEEM_LOCKED_IDS } from '../src/ui/redeem';
import { CONTRACT, mockWallet, network, OWNER, type MockOptions } from './mock-wallet';

const ALICE: Hex = '0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d';
const BOB: Hex = '0x8eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a48';
const quiet = () => undefined;
const ok = async () => ({ ok: true as const, note: 'fine' });

async function session(o: Parameters<typeof mockWallet>[0] = {}) {
  const w = mockWallet(o);
  const s = await connect(w.provider, network, 5);
  return { w, s, c: await readContract(s) };
}

describe('the lock', () => {
  it('disables and enables again every control it is given', () => {
    const cs = [{ disabled: false }, { disabled: false }, { disabled: true }];
    lockControls(cs, true);
    expect(cs.every((c) => c.disabled)).toBe(true);
    lockControls(cs, false);
    expect(cs.every((c) => !c.disabled)).toBe(true);
  });

  it('covers every input, select and button of the page but Deposit, Redeem and the status page’s read-only lookups, and the wallets’ and accounts’ lists', () => {
    const html = readFileSync(new URL('../index.html', import.meta.url), 'utf8');
    // The status section reads chain data only: it moves nothing, so it is not locked.
    const page = html.slice(0, html.indexOf('<section id="status"'));
    const controls = [...page.matchAll(/<(input|select|button)\b[^>]*\bid="([^"]+)"/g)].map((m) => m[2]);
    expect(controls.length).toBeGreaterThan(8);
    const locked: readonly string[] = [...LOCKED_IDS, ...REDEEM_LOCKED_IDS];
    for (const id of controls) if (id !== 'deposit' && id !== 'redeem') expect(locked).toContain(id);
    for (const id of [...LOCKED_CONTAINERS, ...REDEEM_LOCKED_CONTAINERS]) expect(page).toContain(`id="${id}"`);
  });
});

describe('the snapshot', () => {
  it('is taken at the click, frozen, with the canonical SS58 and the depositor as refund', () => {
    const live = { recipient: { account: ALICE }, amount: 2_500_000n, session: { account: OWNER.address } };
    const snap = takeSnapshot(live, 2)!;
    expect(snap).toEqual({ recipient: { account: ALICE, address: encodeSs58(fromHex(ALICE), 2) }, amount: 2_500_000n, refund: OWNER.address });
    expect(Object.isFrozen(snap) && Object.isFrozen(snap.recipient)).toBe(true);
    live.recipient.account = BOB;
    live.amount = 9n;
    expect(snap.recipient.account).toBe(ALICE);
    expect(snap.amount).toBe(2_500_000n);
    expect(takeSnapshot({ recipient: { account: ALICE }, session: { account: OWNER.address } }, 2)).toBeUndefined();
  });

  it('is what the recipient re-check reads and what the deposit sends, whatever the form says meanwhile', async () => {
    const { w, s, c } = await session();
    const live = { recipient: { account: ALICE }, amount: 2_500_000n, session: s };
    const snap = takeSnapshot(live, 2)!;
    const recheck = vi.fn<RunDeps['recheck']>(async () => {
      // The form changes while Kreivo answers; the deposit must not follow it.
      live.recipient = { account: BOB };
      live.amount = 7_000_000n;
      return ok();
    });
    const out = await runDeposit(snap, s, c, 2, { recheck, deposit }, quiet);
    expect(recheck).toHaveBeenCalledWith(ALICE, 2_500_000n);
    expect(w.sent).toHaveLength(1);
    expect(w.sent[0]!.args.slice(0, 3)).toEqual([2_500_000n, ALICE, OWNER.address]);
    expect(out.comparison.ok).toBe(true);
  });

  it('signs nothing when the re-check on the snapshot refuses the recipient, or cannot read it', async () => {
    for (const answer of [{ ok: false as const, reason: 'Blocked.', advice: '' }, undefined]) {
      const { w, s, c } = await session();
      const snap = takeSnapshot({ recipient: { account: ALICE }, amount: 2_500_000n, session: s }, 2)!;
      await expect(runDeposit(snap, s, c, 2, { recheck: async () => answer, deposit }, quiet)).rejects.toThrow(/nothing was signed/);
      expect(w.requests).not.toContain('eth_signTypedData_v4');
      expect(w.sent).toHaveLength(0);
    }
  });

  it('signs nothing when the wallet’s account is no longer the snapshot’s refund', async () => {
    const { w, s, c } = await session();
    const snap = { recipient: { account: ALICE, address: encodeSs58(fromHex(ALICE), 2) }, amount: 2_500_000n, refund: getAddress(CONTRACT) };
    await expect(runDeposit(snap, s, c, 2, { recheck: ok, deposit }, quiet)).rejects.toThrow(/account changed/);
    expect(w.sent).toHaveLength(0);
  });
});

describe('after the receipt', () => {
  const run = async (record: MockOptions['record']) => {
    const { s, c } = await session({ record });
    const snap = takeSnapshot({ recipient: { account: ALICE }, amount: 2_500_000n, session: s }, 2)!;
    return (await runDeposit(snap, s, c, 2, { recheck: ok, deposit }, quiet)).comparison;
  };

  it('shows the Directed log’s recipient as SS58, its amount and its refund, and accepts them when they match', async () => {
    const cmp = await run(undefined);
    expect(cmp.ok).toBe(true);
    expect(cmp.recorded).toEqual({ recipient: encodeSs58(fromHex(ALICE), 2), recipientAccount: ALICE, amount: 2_500_000n, refund: OWNER.address });
  });

  it('fails loudly when the recorded recipient differs from the snapshot, naming both', async () => {
    const cmp = await run({ recipient: BOB });
    expect(cmp.ok).toBe(false);
    expect(!cmp.ok && cmp.reason).toContain(`recipient ${encodeSs58(fromHex(BOB), 2)}, not ${encodeSs58(fromHex(ALICE), 2)}`);
    expect(cmp.recorded?.recipient).toBe(encodeSs58(fromHex(BOB), 2));
  });

  it('fails loudly when the recorded amount or refund differs', async () => {
    const amount = await run({ amount: 2_000_000n });
    expect(!amount.ok && amount.reason).toMatch(/amount 2000000, not 2500000/);
    const other = getAddress('0x00000000000000000000000000000000000fee00');
    const refund = await run({ refund: other });
    expect(!refund.ok && refund.reason).toContain(`refund ${other}, not ${OWNER.address}`);
  });

  it('fails when the receipt holds no Directed log at all', () => {
    const snap = takeSnapshot({ recipient: { account: ALICE }, amount: 1n, session: { account: OWNER.address } }, 2)!;
    const cmp = compareArrival({ arrival: undefined, arrivals: [], directedLogs: 0 }, snap, 2);
    expect(cmp.ok).toBe(false);
    expect(!cmp.ok && cmp.reason).toMatch(/no Directed log/);
  });

  it('fails loudly when the receipt holds more than one Directed log, paired with a Transfer or not', async () => {
    for (const extraDirected of ['paired', 'unpaired'] as const) {
      const { s, c } = await session({ extraDirected });
      const snap = takeSnapshot({ recipient: { account: ALICE }, amount: 2_500_000n, session: s }, 2)!;
      const { res, comparison: cmp } = await runDeposit(snap, s, c, 2, { recheck: ok, deposit }, quiet);
      expect(res.directedLogs).toBe(2);
      expect(cmp.ok).toBe(false);
      expect(!cmp.ok && cmp.reason).toContain('the receipt holds 2 Directed logs of this contract, not one');
      // What the chain recorded is still shown.
      expect(cmp.recorded?.recipientAccount).toBe(ALICE);
    }
  });
});
