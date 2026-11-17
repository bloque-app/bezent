// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// T-018-15 (US-B5, REQ-RQ-9, REQ-OT-2): the locked redeem, against a model of Kreivo and the bezent contract. The
// snapshot is frozen at the click and is what is checked, dry-run, signed and compared; the contract is read again
// on it (bounds, backends, pauses, the balance, the reference) and refuses before signing; `redeem` is dry-run
// through the contract and the whole batch through Kreivo, and a refusal of either is shown and nothing is signed;
// after finality the `Redeemed` event is compared with the snapshot, failing loudly on any difference.

import { describe, expect, it, vi } from 'vitest';
import type { PolkadotSigner } from 'polkadot-api/signer';
import { compareRedeemed, freshReference, limits, parsePayout, redeemRows, refusal, runRedeem, takeRedeemSnapshot, type RedeemSnapshot } from '../src/redeem';
import { CONTRACT_ERRORS, REDEEMED_TOPIC, type DryRun } from '../src/kreivo/bezent';
import type { Finalized, Plan, Prepared, RedeemChain, RedeemFacts } from '../src/kreivo/redeem';
import { REDEEM_GAS_FLOOR, REDEEM_STORAGE_FLOOR } from '../src/kreivo/redeem';
import { Reader, u128, u64 } from '../src/lib/scale';
import { concat, fromHex, toHex, type Hex } from '../src/lib/bytes';
import { encodeSs58 } from '../src/lib/ss58';
import { withdrawalReference } from '../src/lib/refs';

const ALICE: Hex = '0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d';
const BOB: Hex = '0x8eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a48';
const CONTRACT: Hex = `0x${'c0'.repeat(32)}`;
const PAYOUT = '0x5B38Da6a701c568545dCfcB03FcB875f56beddC4' as const;
const REF: Hex = `0x${'ab'.repeat(32)}`;
const asset = { id: 101, symbol: 'bUSDC', decimals: 6 };
const evm137 = { kind: 'evm' as const, chainId: 137n };

const signer = (account: Hex = ALICE): PolkadotSigner => ({ publicKey: fromHex(account), signTx: vi.fn(), signBytes: vi.fn() });

const snapshot = (o: Partial<Parameters<typeof takeRedeemSnapshot>[0]> = {}) =>
  takeRedeemSnapshot({ redeemer: { account: ALICE }, asset, amount: 5_000_000n, whole: false, backend: evm137, payout: PAYOUT, reference: REF, ...o }, 2)!;

const facts = (o: Partial<RedeemFacts> = {}): RedeemFacts => ({
  bounds: { supplyCeiling: 1n << 60n, windowAmount: 1n << 50n, windowBlocks: 41_739, perTransferMax: 50_000_000_000n, redeemMinimum: 1_000_000n },
  backends: [evm137],
  pauses: [
    { scope: 'everything', paused: false },
    { scope: 'every redemption', paused: false },
    { scope: 'Here(101)', paused: false },
    { scope: 'redemptions of Here(101)', paused: false },
  ],
  balance: 10_000_000n,
  assetStatus: 'Liquid',
  ksm: 1_000_000_000_000n,
  at: { number: 41_026_407, hash: `0x${'5b'.repeat(32)}` },
  ...o,
});

/** The `Redeemed` event the contract emits for a `redeem` input, from the redeemer. */
function redeemedFor(redeemer: Hex, data: Uint8Array, change: { amount?: bigint; payout?: Hex; reference?: Hex } = {}) {
  const r = new Reader(data);
  r.take(4);
  const assetBytes = r.take(5);
  const amount = r.u128();
  const backend = r.take(9);
  r.compact();
  const payout = r.take(20);
  const reference = change.reference ?? toHex(r.take(32));
  return {
    topics: [REDEEMED_TOPIC, reference, redeemer] as Hex[],
    data: concat([u64(3n), fromHex(reference), fromHex(redeemer), assetBytes, u128(change.amount ?? amount), backend, Uint8Array.of(0), change.payout ? fromHex(change.payout) : payout]),
  };
}

const ok = (hex: Hex, flags = 0): DryRun => ({ result: { success: true, value: { flags, data: fromHex(hex) } }, gasRequired: { refTime: 2_000_000_000n, proofSize: 140_000n }, storageCharge: 6_695_000_000n });

interface ModelOptions {
  facts?: RedeemFacts;
  used?: boolean;
  /** The contract's answer to `redeem` alone. */
  message?: DryRun;
  /** The batch's dry run. */
  batch?: { ok: false; error: string } | { change: Parameters<typeof redeemedFor>[2] };
  /** What the finalized transaction emitted, changed. */
  final?: { change?: Parameters<typeof redeemedFor>[2]; twice?: boolean; failed?: string; none?: boolean };
}

function model(o: ModelOptions = {}) {
  const plans: Plan[] = [];
  const submitted: Plan[] = [];
  const chain: RedeemChain = {
    contract: CONTRACT,
    facts: async () => o.facts ?? facts(),
    redemption: async () => (o.used ? { redeemer: ALICE, asset: 101, amount: 1n, backend: evm137, payout: { kind: 'address', address: PAYOUT.toLowerCase() as Hex }, refunded: false } : undefined),
    dryRunMessage: async () => o.message ?? ok('0x0000'),
    prepare: async (plan, gas, storageLimit): Promise<Prepared> => {
      plans.push(plan);
      const b = o.batch;
      const emitted = b && 'change' in b ? redeemedFor(plan.redeemer, plan.data, b.change) : redeemedFor(plan.redeemer, plan.data);
      return {
        gas,
        storageLimit,
        fee: 1_000_000_000n,
        dryRun: b && 'error' in b ? b : { ok: true, emitted: [{ contract: CONTRACT, data: emitted.data }] },
        callData: '0x2b02',
        submit: async (_signer, progress): Promise<Finalized> => {
          submitted.push(plan);
          progress('Signed.');
          const f = o.final ?? {};
          const e = { ...redeemedFor(plan.redeemer, plan.data, f.change), contract: CONTRACT, index: 4 };
          return {
            txHash: `0x${'77'.repeat(32)}`,
            block: { hash: `0x${'66'.repeat(32)}`, number: 41_026_420, index: 2 },
            ok: !f.failed,
            ...(f.failed ? { dispatchError: f.failed } : {}),
            events: f.none ? [] : f.twice ? [e, { ...e, index: 5 }] : [e],
          };
        },
      };
    },
  };
  return { chain, plans, submitted };
}

const quiet = () => undefined;

describe('the redeem form', () => {
  it('picks a fresh random 32-byte reference each time, never zeros', () => {
    const a = freshReference();
    expect(a).toMatch(/^0x[0-9a-f]{64}$/);
    expect(freshReference()).not.toBe(a);
    expect(() => freshReference((b) => b)).toThrow(/zeros/);
  });

  it('takes a payout address in any case, checksums it, and refuses the zero address and a bad checksum', () => {
    expect(parsePayout(PAYOUT.toLowerCase())).toEqual({ ok: true, address: PAYOUT });
    expect(parsePayout(` ${PAYOUT} `)).toEqual({ ok: true, address: PAYOUT });
    expect(parsePayout('0x0000000000000000000000000000000000000000').ok).toBe(false);
    expect(parsePayout('0x5b38Da6a701c568545dCfcB03FcB875f56beddC4').ok).toBe(false);
    expect(parsePayout('0x1234').ok).toBe(false);
    expect(parsePayout('').ok).toBe(false);
  });

  it('freezes the snapshot, with the canonical SS58 and the payout checksummed', () => {
    const live = { redeemer: { account: ALICE }, asset, amount: 5_000_000n, backend: evm137, payout: PAYOUT.toLowerCase() as `0x${string}`, reference: REF };
    const s = takeRedeemSnapshot(live, 2)!;
    expect(s).toMatchObject({ redeemer: { account: ALICE, address: encodeSs58(fromHex(ALICE), 2) }, amount: 5_000_000n, payout: PAYOUT, reference: REF, whole: false });
    expect(Object.isFrozen(s) && Object.isFrozen(s.redeemer) && Object.isFrozen(s.backend)).toBe(true);
    live.amount = 1n;
    live.redeemer.account = BOB;
    expect(s.amount).toBe(5_000_000n);
    expect(s.redeemer.account).toBe(ALICE);
    expect(takeRedeemSnapshot({ ...live, payout: undefined }, 2)).toBeUndefined();
    expect(redeemRows(s)).toContainEqual(['Bridge reference', withdrawalReference(ALICE, REF)]);
  });

  it('refuses before signing, from the contract’s facts: unknown asset, a pause, an undeclared backend, the minimum, the balance', () => {
    const a = { amount: 5_000_000n, backend: evm137, whole: false, decimals: 6, symbol: 'bUSDC' };
    expect(refusal(facts(), a)).toBeUndefined();
    expect(refusal(facts({ bounds: undefined }), a)).toMatch(/does not know this asset/);
    const paused = facts();
    paused.pauses[3]!.paused = true;
    expect(refusal(paused, a)).toBe('Redemptions are paused (redemptions of Here(101)).');
    expect(refusal(facts({ backends: [{ kind: 'evm', chainId: 8453n }] }), a)).toMatch(/does not declare the backend Evm \{ chain_id: 137 \}/);
    expect(refusal(facts(), { ...a, amount: 999_999n })).toBe('The route’s redemption minimum is 1 bUSDC.');
    expect(refusal(facts(), { ...a, amount: 10_000_001n })).toBe('This account holds 10 bUSDC.');
    expect(refusal(facts({ assetStatus: 'Frozen' }), a)).toMatch(/frozen/);
    expect(refusal(facts(), { ...a, amount: 9_000_000n, whole: true })).toMatch(/balance moved since the review/);
    expect(refusal(facts(), { ...a, amount: 10_000_000n, whole: true })).toBeUndefined();
  });

  it('sets the gas and the storage limit at twice the dry run, never under the proven floors', () => {
    expect(limits({ gasRequired: { refTime: 1n, proofSize: 1n }, storageCharge: 1n })).toEqual({ gas: REDEEM_GAS_FLOOR, storageLimit: REDEEM_STORAGE_FLOOR });
    expect(limits({ gasRequired: { refTime: 6_000_000_000n, proofSize: 300_000n }, storageCharge: 6_000_000_000n })).toEqual({
      gas: { refTime: 12_000_000_000n, proofSize: 600_000n },
      storageLimit: 12_000_000_000n,
    });
  });
});

describe('the locked redeem', () => {
  it('reads again, dry-runs both, signs the snapshot and accepts a Redeemed event that matches it', async () => {
    const m = model();
    const steps: string[] = [];
    const out = await runRedeem(snapshot(), m.chain, signer(), (t) => steps.push(t));
    expect(out.comparison.ok).toBe(true);
    expect(m.submitted).toHaveLength(1);
    expect(m.plans[0]).toMatchObject({ redeemer: ALICE, asset: 101, amount: 5_000_000n });
    expect(toHex(m.plans[0]!.data).slice(0, 10)).toBe('0xec3e9290');
    expect(steps.join('\n')).toMatch(/Dry-running redeem through the contract[\s\S]*whole batch[\s\S]*Sign the redemption/);
  });

  it('is the snapshot, whatever the form says once it was taken', async () => {
    const live = { redeemer: { account: ALICE }, asset, amount: 5_000_000n, backend: evm137, payout: PAYOUT, reference: REF };
    const s = takeRedeemSnapshot(live, 2)!;
    const m = model();
    const chain = { ...m.chain, facts: async () => ((live.amount = 9n), (live.reference = `0x${'cd'.repeat(32)}`), facts()) };
    await runRedeem(s, chain, signer(), quiet);
    expect(m.plans[0]!.amount).toBe(5_000_000n);
    expect(toHex(m.plans[0]!.data)).toContain(REF.slice(2));
  });

  it('signs nothing when the wallet’s account is not the snapshot’s redeemer', async () => {
    const m = model();
    await expect(runRedeem(snapshot(), m.chain, signer(BOB), quiet)).rejects.toThrow(/account changed/);
    expect(m.plans).toHaveLength(0);
  });

  it('signs nothing when the contract’s facts refuse it, or the redeemer used the reference before', async () => {
    const paused = facts();
    paused.pauses[0]!.paused = true;
    for (const o of [{ facts: paused }, { facts: facts({ balance: 1_000_000n }) }, { used: true }] as ModelOptions[]) {
      const m = model(o);
      await expect(runRedeem(snapshot(), m.chain, signer(), quiet)).rejects.toThrow(/Nothing was signed/);
      expect(m.plans).toHaveLength(0);
    }
  });

  it('shows the contract’s refusal from the dry run, and signs nothing', async () => {
    const used = `0x0001${CONTRACT_ERRORS.indexOf('ReferenceUsed').toString(16).padStart(2, '0')}` as Hex;
    const m = model({ message: ok(used, 1) });
    await expect(runRedeem(snapshot(), m.chain, signer(), quiet)).rejects.toThrow(
      'The contract refuses this redemption: you used this reference for a redemption before. Pick a new one. Nothing was signed.',
    );
    expect(m.plans).toHaveLength(0);
  });

  it('goes on when the dry run of redeem alone fails only at its pull: the batch’s approval decides it', async () => {
    const m = model({ message: ok('0x00011d0200', 1), facts: facts({ balance: 5_000_000n }) });
    const out = await runRedeem(snapshot({ whole: true }), m.chain, signer(), quiet);
    expect(out.comparison.ok).toBe(true);
  });

  it('signs nothing when the batch’s dry run fails, and says why', async () => {
    const m = model({ batch: { ok: false, error: 'Module.Balances.InsufficientBalance' } });
    await expect(runRedeem(snapshot(), m.chain, signer(), quiet)).rejects.toThrow(/Balances.InsufficientBalance\). The account needs a little KSM/);
    expect(m.submitted).toHaveLength(0);
  });

  it('signs nothing when the batch’s dry run would record something else than the snapshot', async () => {
    const m = model({ batch: { change: { payout: `0x${'99'.repeat(20)}` } } });
    await expect(runRedeem(snapshot(), m.chain, signer(), quiet)).rejects.toThrow(/would not record what you reviewed: The Redeemed event differs[^]*payout 0x9999/);
    expect(m.submitted).toHaveLength(0);
  });

  it('fails loudly when the final Redeemed event differs from the snapshot, naming the difference', async () => {
    const amount = await runRedeem(snapshot(), model({ final: { change: { amount: 4_000_000n } } }).chain, signer(), quiet);
    expect(amount.comparison.ok).toBe(false);
    expect(!amount.comparison.ok && amount.comparison.reason).toContain('amount 4000000, not 5000000');
    expect(amount.comparison.redeemed?.amount).toBe(4_000_000n);
    const twice = await runRedeem(snapshot(), model({ final: { twice: true } }).chain, signer(), quiet);
    expect(!twice.comparison.ok && twice.comparison.reason).toBe('The transaction emitted 2 Redeemed events, not one.');
    const none = await runRedeem(snapshot(), model({ final: { none: true } }).chain, signer(), quiet);
    expect(!none.comparison.ok && none.comparison.reason).toMatch(/no Redeemed event/);
  });

  it('says a final transaction that failed redeemed nothing, naming it', async () => {
    await expect(runRedeem(snapshot(), model({ final: { failed: 'Module.Contracts.ContractReverted' } }).chain, signer(), quiet)).rejects.toThrow(
      /0x7777[0-9a-f]+ is final in block 41026420 but failed \(Module.Contracts.ContractReverted\); nothing was redeemed/,
    );
  });
});

describe('compareRedeemed', () => {
  const s: RedeemSnapshot = snapshot();
  const ev = (change: Parameters<typeof redeemedFor>[2] = {}) => {
    const data = concat([fromHex('0xec3e9290'), fromHex('0x0065000000'), u128(s.amount), fromHex('0x00'), u64(137n), fromHex('0x50'), fromHex(PAYOUT.toLowerCase() as Hex), fromHex(REF)]);
    return redeemedFor(ALICE, data, change);
  };

  it('compares the payout case-insensitively, as bytes', () => {
    expect(compareRedeemed([ev()], s).ok).toBe(true);
  });

  it('names a different reference', () => {
    const c = compareRedeemed([ev({ reference: `0x${'cd'.repeat(32)}` })], s);
    expect(!c.ok && c.reason).toContain(`reference 0x${'cd'.repeat(32)}, not ${REF}`);
  });
});
