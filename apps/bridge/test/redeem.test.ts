// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// T-018-15 (US-B5, REQ-RQ-9, REQ-OT-2): the locked redeem, against a model of Kreivo and the bezent contract. The
// snapshot is frozen at the click and is what is checked, dry-run, signed and compared; the contract is read again
// on it (bounds, backends, pauses, the balance, the reference) and refuses before signing; `redeem` is dry-run
// through the contract and the whole batch through Kreivo, and a refusal of either is shown and nothing is signed;
// after finality the `Redeemed` event is compared with the snapshot, failing loudly on any difference.
//
// From the security review of #138: the payout address is never one of bezent's own contracts; the account's free
// KSM covers the fee, the storage deposit limit and a new approval's deposit before anything is signed; a BadProof
// from Kreivo is a refused signature with the metadata hint, never "signed, but following it failed"; and an
// unclear outcome keeps its reference until the contract records it or its mortality runs out.

import { describe, expect, it, vi } from 'vitest';
import type { PolkadotSigner } from 'polkadot-api/signer';
import { InvalidTxError } from 'polkadot-api';
import {
  compareRedeemed,
  freshReference,
  ksmNeeded,
  limits,
  ownContracts,
  parsePayout,
  redeemRows,
  refusal,
  runRedeem,
  decodePending,
  pendingProblem,
  encodePending,
  pendingKey,
  settle,
  takeRedeemSnapshot,
  type Pending,
  type RedeemSnapshot,
  type SettleRead,
} from '../src/redeem';
import { CONTRACT_ERRORS, REDEEMED_TOPIC, type DryRun, type Redemption } from '../src/kreivo/bezent';
import type { Finalized, Plan, Prepared, RedeemChain, RedeemFacts } from '../src/kreivo/redeem';
import { MORTALITY, REDEEM_GAS_FLOOR, REDEEM_STORAGE_FLOOR, redeemChain, submitError, UnclearOutcome } from '../src/kreivo/redeem';
import { WalletSignatureMismatch } from '../src/kreivo/extensions';
import { KREIVO_TESTBED, POLYGON_AMOY } from '../src/config/networks';
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
  approval: undefined,
  approvalDeposit: 6_700_000_000n,
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
    finalized: async () => 41_026_407,
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

  it('refuses a payout to one of bezent’s own contracts, in any case, and leaves an unset one out', () => {
    const own = ownContracts(POLYGON_AMOY);
    expect(own.map((o) => o.address)).toEqual([POLYGON_AMOY.token, POLYGON_AMOY.reserve, POLYGON_AMOY.directedDeposit, POLYGON_AMOY.emitter]);
    for (const o of own) {
      for (const form of [o.address, o.address.toLowerCase(), `0x${o.address.slice(2).toUpperCase()}`]) {
        const p = parsePayout(form, own);
        expect(p.ok).toBe(false);
        expect(!p.ok && p.reason).toContain(`${o.address} is ${o.what}, not yours`);
      }
    }
    expect(parsePayout(PAYOUT, own)).toEqual({ ok: true, address: PAYOUT });
    // Polygon pins no DirectedDeposit, reserve or emitter yet: only its token is refused.
    expect(ownContracts({ token: POLYGON_AMOY.token, reserve: null, directedDeposit: null, emitter: null })).toEqual([{ what: 'the USDC token contract', address: POLYGON_AMOY.token }]);
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

  it('signs nothing when the account’s free KSM is short of the fee, the storage deposit limit and a new approval’s deposit', async () => {
    // The model's dry run charges 0.006695 KSM of storage: the limit is twice that.
    const storageLimit = 13_390_000_000n;
    const need = ksmNeeded(facts(), { fee: 1_000_000_000n, storageLimit });
    expect(need.total).toBe(1_000_000_000n + storageLimit + 6_700_000_000n);
    const short = model({ facts: facts({ ksm: need.total - 1n }) });
    await expect(runRedeem(snapshot(), short.chain, signer(), quiet)).rejects.toThrow(
      'This account holds 0.021089999999 KSM free; the redemption may need up to 0.02109 KSM: the fee (about 0.001 KSM), the storage deposit limit (0.01339 KSM), the approval’s deposit (0.0067 KSM, returned when the redemption uses the approval up). Add KSM to it, then try again. Nothing was signed.',
    );
    expect(short.submitted).toHaveLength(0);
    expect((await runRedeem(snapshot(), model({ facts: facts({ ksm: need.total }) }).chain, signer(), quiet)).comparison.ok).toBe(true);
    // An existing approval already holds its deposit: the batch adds to it, so no new deposit is needed.
    const approved = facts({ ksm: need.total - 6_700_000_000n, approval: { amount: 2_000_000n, deposit: 6_700_000_000n } });
    expect(ksmNeeded(approved, { fee: 1_000_000_000n, storageLimit }).approval).toBe(0n);
    expect((await runRedeem(snapshot(), model({ facts: approved }).chain, signer(), quiet)).comparison.ok).toBe(true);
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

describe('following the submission', () => {
  const TX: Hex = `0x${'77'.repeat(32)}`;

  /** A Kreivo whose batch signs, then answers each of polkadot-api's watch events, then fails with `fail`. */
  function kreivo(events: { type: string; txHash?: string }[], fail: unknown) {
    const tx = { decodedCall: {}, getEncodedData: async () => '0x2b02', getEstimatedFees: async () => 1n };
    const api = {
      apis: {
        DryRunApi: { dry_run_call: async () => ({ success: true, value: { execution_result: { success: true, value: {} }, emitted_events: [] } }) },
      },
      tx: {
        Assets: { approve_transfer: () => tx },
        Contracts: { call: () => tx },
        Utility: {
          batch_all: () => ({
            ...tx,
            signSubmitAndWatch: (s: PolkadotSigner) => ({
              subscribe(o: { next(e: unknown): void; error(e: unknown): void }) {
                void (async () => {
                  // polkadot-api signs first, with the block the transaction is built at.
                  await s.signTx(new Uint8Array(0), {}, new Uint8Array(0), 41_026_400);
                  for (const e of events) o.next({ txHash: TX, ...e });
                  o.error(fail);
                })();
                return { unsubscribe: () => undefined };
              },
            }),
          }),
        },
      },
    };
    const n = { ...KREIVO_TESTBED, bezent: CONTRACT };
    return redeemChain({ client: {}, api, close: () => undefined } as never, n, CONTRACT, 1_000);
  }
  const plan = { redeemer: ALICE, asset: 101, amount: 1n, data: new Uint8Array(0) };
  const sign: PolkadotSigner = { publicKey: fromHex(ALICE), signTx: async () => new Uint8Array(0), signBytes: vi.fn() };
  const badProof = new InvalidTxError({ type: 'Invalid', value: { type: 'BadProof' } });

  it('reports a BadProof after `signed` as a refused signature, with the metadata hint, never as an unclear outcome', async () => {
    const p = await kreivo([{ type: 'signed' }], badProof).prepare(plan, REDEEM_GAS_FLOOR, REDEEM_STORAGE_FLOOR);
    const steps: string[] = [];
    const e = await p.submit(sign, (t) => steps.push(t)).catch((x: unknown) => x);
    expect(e).toBeInstanceOf(WalletSignatureMismatch);
    expect((e as Error).message).toMatch(/^Kreivo rejected the signature \(.*BadProof.*\): the wallet signed another payload[^]*Offer the wallet Kreivo's metadata again[^]*Nothing was executed\.$/);
    expect((e as Error).message).not.toMatch(/following it failed|Look it up/);
    expect(steps.join(' ')).toContain(`Signed: ${TX}`);
  });

  it('says a failure before the broadcast sent nothing', async () => {
    const p = await kreivo([{ type: 'signed' }], new Error('the connection dropped')).prepare(plan, REDEEM_GAS_FLOOR, REDEEM_STORAGE_FLOOR);
    const e = await p.submit(sign, quiet).catch((x: unknown) => x);
    expect(e).not.toBeInstanceOf(UnclearOutcome);
    expect((e as Error).message).toBe('the connection dropped. Nothing was sent.');
  });

  it('says a failure after the broadcast leaves the outcome unclear, with the last block that may include it', async () => {
    const p = await kreivo([{ type: 'signed' }, { type: 'broadcasted' }], new Error('the connection dropped')).prepare(plan, REDEEM_GAS_FLOOR, REDEEM_STORAGE_FLOOR);
    const e = await p.submit(sign, quiet).catch((x: unknown) => x);
    expect(e).toBeInstanceOf(UnclearOutcome);
    expect(e).toMatchObject({ txHash: TX, validUntil: 41_026_400 + MORTALITY });
    expect((e as Error).message).toMatch(/was broadcast, but following it failed: the connection dropped\. It may still be included until block 41026464; its reference stays locked/);
    // An InvalidTxError after the broadcast is Kreivo refusing it at the finalized block: nothing is on chain.
    expect(submitError(badProof, { hash: TX, broadcast: true, birth: 1 })).toBeInstanceOf(WalletSignatureMismatch);
    expect(submitError(new InvalidTxError({ type: 'Invalid', value: { type: 'Stale' } }), { hash: TX, broadcast: true, birth: 1 })).not.toBeInstanceOf(UnclearOutcome);
  });
});

describe('an unclear outcome', () => {
  const u = { redeemer: ALICE, reference: REF, txHash: `0x${'77'.repeat(32)}` as Hex, validUntil: 1_064 };
  const record: Redemption = { redeemer: ALICE, asset: 101, amount: 5_000_000n, backend: evm137, payout: { kind: 'address', address: PAYOUT.toLowerCase() as Hex }, refunded: false };
  /** A chain whose finalized head and record follow `script`, one entry per read; a `throw` entry fails that read. */
  function chain(script: ({ head: number; recorded?: boolean } | 'throw')[]) {
    let i = 0;
    let turn: (typeof script)[number] = script[0]!;
    const c = {
      finalized: async () => {
        turn = script[Math.min(i++, script.length - 1)]!;
        if (turn === 'throw') throw new Error('the light client is syncing');
        return turn.head;
      },
      redemption: async () => (turn !== 'throw' && turn.recorded ? record : undefined),
    } as unknown as RedeemChain;
    return { c, reads: () => i };
  }
  const sleeps: number[] = [];
  const sleep = async (ms: number) => void sleeps.push(ms);

  it('keeps waiting while the transaction may still be included, and settles when the contract records it', async () => {
    const { c, reads } = chain([{ head: 1_000 }, 'throw', { head: 1_064 }, { head: 1_065, recorded: true }]);
    expect(await settle(u, () => c, { sleep })).toEqual({ kind: 'recorded', redemption: record, at: 1_065 });
    expect(reads()).toBe(4);
  });

  it('frees the reference only once the finalized head is past the mortality and nothing is recorded', async () => {
    const { c } = chain([{ head: 1_063 }, { head: 1_064 }, { head: 1_065 }]);
    expect(await settle(u, () => c, { sleep })).toEqual({ kind: 'expired', at: 1_065 });
    // Without a known mortality, only the record settles it.
    const never = chain([{ head: 5_000 }, { head: 5_001 }, { head: 5_002, recorded: true }]);
    expect(await settle({ ...u, validUntil: undefined }, () => never.c, { sleep })).toMatchObject({ kind: 'recorded', at: 5_002 });
  });

  it('reports each read to onRead, and a failed one by its message, without changing what it returns', async () => {
    const { c } = chain([{ head: 1_000 }, 'throw', { head: 1_065, recorded: true }]);
    const heard: SettleRead[] = [];
    expect(await settle(u, () => c, { sleep, onRead: (r) => heard.push(r) })).toEqual({ kind: 'recorded', redemption: record, at: 1_065 });
    expect(heard).toEqual([{ ok: true, head: 1_000 }, { ok: false, error: 'the light client is syncing' }, { ok: true, head: 1_065 }]);
    // A display that throws does not stop the wait.
    const again = chain([{ head: 1_000 }, { head: 1_065, recorded: true }]);
    expect(await settle(u, () => again.c, { sleep, onRead: () => { throw new Error('display'); } })).toMatchObject({ kind: 'recorded', at: 1_065 });
  });

  it('skips a turn while the page is on another network, and stops when asked', async () => {
    const { c } = chain([{ head: 1_000, recorded: true }]);
    let turns = 0;
    expect(await settle(u, () => (turns++ < 2 ? undefined : c), { sleep })).toMatchObject({ kind: 'recorded' });
    expect(turns).toBe(3);
    expect(await settle(u, () => c, { sleep, stopped: () => true })).toBeUndefined();
  });
});

describe('the stored pending record', () => {
  const pending: Pending = {
    redeemer: ALICE,
    reference: REF,
    txHash: `0x${'77'.repeat(32)}`,
    validUntil: 1_064,
    network: 'polygon',
    networkLabel: 'Polygon',
    kreivo: 'kreivo',
    snapshot: snapshot({ whole: true }),
  };

  it('is one versioned key per Kreivo network and redeemer', () => {
    expect(pendingKey('kreivo', ALICE)).toBe(`bezent.bridge.pending/v1/kreivo/${ALICE}`);
  });

  it('round-trips, bigints and an unknown mortality included', () => {
    expect(decodePending(encodePending(pending))).toEqual(pending);
    const open = { ...pending, validUntil: undefined };
    expect(decodePending(encodePending(open))).toEqual(open);
    expect(decodePending(encodePending(pending))?.snapshot.amount).toBe(5_000_000n);
  });

  it('refuses a record that is not whole and well formed, or of another version', () => {
    const j = JSON.parse(encodePending(pending));
    for (const bad of [
      'not json',
      '{}',
      JSON.stringify({ ...j, v: 2 }),
      JSON.stringify({ ...j, txHash: '0x77' }),
      JSON.stringify({ ...j, snapshot: { ...j.snapshot, amount: '-1' } }),
      JSON.stringify({ ...j, snapshot: { ...j.snapshot, payout: '0x12' } }),
      JSON.stringify({ ...j, snapshot: { ...j.snapshot, reference: `0x${'cd'.repeat(32)}` } }),
    ]) {
      expect(decodePending(bad)).toBeUndefined();
    }
  });
});

type Json = Record<string, any>; // eslint-disable-line @typescript-eslint/no-explicit-any

describe('a stored record checked against reality', () => {
  const net = { key: 'polygon', label: 'Polygon', chainId: 137, kreivo: { network: 'kreivo', ss58Prefix: 2, asset: { id: 101, symbol: 'bUSDC', decimals: 6 } } };
  const good = (): Pending => {
    const s = snapshot({ whole: true });
    return {
      redeemer: s.redeemer.account,
      reference: s.reference,
      txHash: `0x${'77'.repeat(32)}`,
      validUntil: 1_064,
      network: 'polygon',
      networkLabel: 'Polygon',
      kreivo: 'kreivo',
      snapshot: Object.freeze({ ...s, backend: { kind: 'evm' as const, chainId: 137n } }),
    };
  };
  const body = (edit: (j: Json) => void) => {
    const j = JSON.parse(encodePending(good()));
    edit(j);
    return JSON.stringify(j);
  };

  it('accepts a record this build wrote', () => {
    expect(pendingProblem(good(), [net])).toBeUndefined();
  });

  it('reads a validUntil of 0 or none as no expiry, and refuses a negative, fractional, text or over-u32 one', () => {
    expect(decodePending(body((j) => (j.validUntil = 0)))?.validUntil).toBeUndefined();
    expect(decodePending(body((j) => (j.validUntil = null)))?.validUntil).toBeUndefined();
    expect(decodePending(body((j) => (j.validUntil = 1)))?.validUntil).toBe(1);
    for (const v of [-1, 1.5, '1064', 2 ** 32, Number.MAX_SAFE_INTEGER, true]) expect(decodePending(body((j) => (j.validUntil = v)))).toBeUndefined();
  });

  it('refuses decimals outside 0..36 and an asset id that is not a u32', () => {
    for (const d of [-1, 37, 1.5, '6']) expect(decodePending(body((j) => (j.snapshot.asset.decimals = d)))).toBeUndefined();
    for (const id of [-1, 2 ** 32, 1.5, '101']) expect(decodePending(body((j) => (j.snapshot.asset.id = id)))).toBeUndefined();
    expect(decodePending(body((j) => (j.snapshot.asset.decimals = 0)))).toBeDefined();
    expect(decodePending(body((j) => (j.snapshot.asset.decimals = 36)))).toBeDefined();
  });

  it('finds a record the network’s configuration contradicts', () => {
    const p = good();
    const s = p.snapshot;
    const withSnap = (o: object): Pending => ({ ...p, snapshot: Object.freeze({ ...s, ...o }) });
    expect(pendingProblem({ ...p, network: 'base' }, [net])).toMatch(/not one this build carries/);
    expect(pendingProblem(p, [])).toMatch(/not one this build carries/);
    expect(pendingProblem({ ...p, networkLabel: 'Other' }, [net])).toMatch(/label/);
    expect(pendingProblem({ ...p, kreivo: 'kreivo-test' }, [net])).toMatch(/kreivo-test, not kreivo/);
    expect(pendingProblem(withSnap({ redeemer: { account: s.redeemer.account, address: 'HNZata7iMYWmk5RvZRTiAsSDhV8366zq2YGb3tLH5Upf74G' } }), [net])).toMatch(/redeemer address/);
    expect(pendingProblem(withSnap({ asset: { ...s.asset, id: 7 } }), [net])).toMatch(/asset/);
    expect(pendingProblem(withSnap({ asset: { ...s.asset, decimals: 18 } }), [net])).toMatch(/asset/);
    expect(pendingProblem(withSnap({ backend: { kind: 'evm', chainId: 1n } }), [net])).toMatch(/backend/);
    expect(pendingProblem(withSnap({ backend: { kind: 'attested', id: 2 } }), [net])).toMatch(/backend/);
  });
});

