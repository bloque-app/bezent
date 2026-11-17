// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// T-018-15, T-018-17: the bezent contract's messages, answers and events, in SCALE, as the contract's source writes
// them (contracts/bezent) and as the Kreivo endpoint and Swaps' signer encode them. The selectors are checked against
// the ones Swaps' signer pinned from the mainnet redeem of 2026-11-08; the answers against bytes read from the
// contract on Kreivo mainnet on 2026-11-16 (a `ContractsApi.call` dry run at the finalized head).

import { describe, expect, it } from 'vitest';
import {
  CONTRACT_ERRORS,
  REDEEMED_TOPIC,
  backendsData,
  boundsData,
  decodeBackends,
  decodeBool,
  decodeBounds,
  decodeRedeemDryRun,
  decodeRedeemed,
  decodeRedemption,
  getterValue,
  isPullRefusal,
  mintReferenceUsedData,
  pausedData,
  redeemData,
  redemptionData,
  selector,
  signatureTopic,
  type DryRun,
} from '../src/kreivo/bezent';
import { compact, Reader, u128, u64 } from '../src/lib/scale';
import { concat, fromHex, toHex, type Hex } from '../src/lib/bytes';

const ok = (data: Hex, flags = 0): DryRun => ({ result: { success: true, value: { flags, data: fromHex(data) } }, gasRequired: { refTime: 1n, proofSize: 1n }, storageCharge: 0n });
const REF: Hex = `0x${'ab'.repeat(32)}`;
const ALICE: Hex = '0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d';

describe('SCALE', () => {
  it('writes and reads compact integers at every width', () => {
    for (const [v, hex] of [
      [0n, '0x00'],
      [1n, '0x04'],
      [63n, '0xfc'],
      [64n, '0x0101'],
      [16383n, '0xfdff'],
      [16384n, '0x02000100'],
      [1_073_741_823n, '0xfeffffff'],
      [1_073_741_824n, '0x0300000040'],
      [10_000_000_000n, '0x0700e40b5402'],
      [(1n << 128n) - 1n, `0x33${'ff'.repeat(16)}`],
    ] as const) {
      expect(toHex(compact(v))).toBe(hex);
      expect(new Reader(fromHex(hex)).compact()).toBe(v);
    }
  });

  it('refuses short input and trailing bytes', () => {
    expect(() => new Reader(fromHex('0x0102')).u32()).toThrow(/4 bytes wanted/);
    const r = new Reader(fromHex('0x0102'));
    r.u8();
    expect(() => r.end('x')).toThrow(/1 trailing/);
  });
});

describe('the contract’s messages', () => {
  it('have the selectors Swaps’ signer pinned against the mainnet redeem, and the endpoint’s', () => {
    expect(toHex(selector('redeem'))).toBe('0xec3e9290');
    expect(toHex(selector('redemption'))).toBe('0x7f658465');
    expect(toHex(selector('bounds'))).toBe('0x367fb852');
    expect(toHex(selector('mint_reference_used'))).toBe('0xdbb86e51');
  });

  it('redeem(Here(101), amount, Evm{137}, payout, reference) is the 87 bytes Swaps’ signer writes', () => {
    const payout = fromHex(`0x${'11'.repeat(20)}`);
    const data = redeemData(101, 1_000_000n, { kind: 'evm', chainId: 137n }, payout, REF);
    expect(data.length).toBe(87);
    // payment-rails, server/packages/common/busdc/redeem-encoding.ts, `redeemData`: selector, Here(101), u128, Evm
    // variant 0 and the chain id as u64, compact length 20 (0x50), the address, the reference.
    expect(toHex(data)).toBe(toHex(concat([fromHex('0xec3e9290'), fromHex('0x0065000000'), u128(1_000_000n), fromHex('0x00'), u64(137n), fromHex('0x50'), payout, fromHex(REF)])));
  });

  it('encodes the reads', () => {
    expect(toHex(boundsData(101))).toBe('0x367fb8520065000000');
    expect(toHex(backendsData(101))).toBe(`${toHex(selector('backends'))}0065000000`);
    expect(toHex(redemptionData(ALICE, REF))).toBe(`0x7f658465${ALICE.slice(2)}${REF.slice(2)}`);
    expect(toHex(mintReferenceUsedData(REF))).toBe(`0xdbb86e51${REF.slice(2)}`);
    const p = (s: Parameters<typeof pausedData>[0]) => toHex(pausedData(s)).slice(10);
    expect(p({ kind: 'all' })).toBe('00');
    expect(p({ kind: 'direction', redeem: true })).toBe('0101');
    expect(p({ kind: 'asset', asset: 101 })).toBe('020065000000');
    expect(p({ kind: 'asset-direction', asset: 101, redeem: true })).toBe('03006500000001');
    expect(() => redemptionData('0x00', REF)).toThrow(/32 bytes/);
  });
});

describe('the contract’s answers', () => {
  it('decodes bounds(Here(101)) as read from Kreivo mainnet', () => {
    const b = decodeBounds(ok('0x00010010acd153000000000000000000000000b08ef01b00000000000000000000000ba3000000743ba40b000000000000000000000040420f00000000000000000000000000'));
    expect(b).toEqual({
      supplyCeiling: 360_000_000_000n,
      windowAmount: 120_000_000_000n,
      windowBlocks: 41_739,
      perTransferMax: 50_000_000_000n,
      redeemMinimum: 1_000_000n,
    });
    expect(decodeBounds(ok('0x0000'))).toBeUndefined();
  });

  it('decodes backends(Here(101)) and paused(…) as read from Kreivo mainnet', () => {
    expect(decodeBackends(ok('0x0004008900000000000000'))).toEqual([{ kind: 'evm', chainId: 137n }]);
    expect(decodeBool(ok('0x0000'), 'paused')).toBe(false);
    expect(decodeBool(ok('0x0001'), 'paused')).toBe(true);
  });

  it('never reads a revert, a language error or a failed dispatch as an answer', () => {
    expect(() => getterValue(ok('0x0000', 1), 'paused')).toThrow(/reverted/);
    expect(() => getterValue(ok('0x0100'), 'paused')).toThrow(/language error/);
    expect(() => getterValue({ ...ok('0x'), result: { success: false, value: { type: 'Module', value: { type: 'Contracts', value: { type: 'ContractNotFound' } } } } }, 'bounds')).toThrow(
      /Module.Contracts.ContractNotFound/,
    );
    expect(() => decodeBool(ok('0x000100'), 'paused')).toThrow(/trailing/);
  });

  it('decodes a redemption record', () => {
    const payout = `0x${'b5'.repeat(20)}`;
    const data = `0x0001${ALICE.slice(2)}0065000000${toHex(u128(400_000n)).slice(2)}00${toHex(u64(137n)).slice(2)}00${payout.slice(2)}01` as Hex;
    expect(decodeRedemption(ok(data))).toEqual({
      redeemer: ALICE,
      asset: 101,
      amount: 400_000n,
      backend: { kind: 'evm', chainId: 137n },
      payout: { kind: 'address', address: payout },
      refunded: true,
    });
    expect(decodeRedemption(ok('0x0000'))).toBeUndefined();
  });

  it('reads redeem’s dry run: Ok, a named refusal, or only the pull, as mainnet answered for an account with no approval', () => {
    expect(decodeRedeemDryRun(ok('0x0000', 0))).toEqual({ ok: true });
    const below = decodeRedeemDryRun(ok(`0x0001${CONTRACT_ERRORS.indexOf('BelowMinimum').toString(16).padStart(2, '0')}`, 1));
    expect(below).toMatchObject({ ok: false, error: { name: 'BelowMinimum' } });
    expect(!below.ok && below.why).toMatch(/redemption minimum/);
    expect(isPullRefusal(below)).toBe(false);
    // Mainnet, 2026-11-16: flags 1, data 0x00011d0200, "call failed with: Unapproved" (no approval, no balance).
    const pull = decodeRedeemDryRun(ok('0x00011d0200', 1));
    expect(pull).toMatchObject({ ok: false, error: { name: 'Kreivo', detail: 'Assets(CannotDeposit)' } });
    expect(isPullRefusal(pull)).toBe(true);
    const paused = decodeRedeemDryRun(ok('0x000102', 1));
    expect(!paused.ok && paused.why).toBe('redemptions of this asset are paused.');
  });
});

describe('Redeemed', () => {
  const event = (o: { reference?: Hex; redeemer?: Hex; amount?: bigint } = {}) => ({
    topics: [REDEEMED_TOPIC, o.reference ?? REF, o.redeemer ?? ALICE] as Hex[],
    data: concat([u64(7n), fromHex(o.reference ?? REF), fromHex(o.redeemer ?? ALICE), fromHex('0x0065000000'), u128(o.amount ?? 1_000_000n), fromHex('0x00'), u64(137n), fromHex(`0x00${'11'.repeat(20)}`)]),
  });

  it('has ink! 5’s signature topic, the endpoint’s', () => {
    expect(REDEEMED_TOPIC).toBe(signatureTopic('Redeemed(EventSeq,Reference,AccountId,AssetId,Balance,Backend,Payout)'));
    expect(REDEEMED_TOPIC).toMatch(/^0x[0-9a-f]{64}$/);
  });

  it('decodes the event, with its reference and its redeemer as topics', () => {
    const e = event();
    expect(decodeRedeemed(e.topics, e.data)).toEqual({
      seq: 7n,
      reference: REF,
      redeemer: ALICE,
      asset: 101,
      amount: 1_000_000n,
      backend: { kind: 'evm', chainId: 137n },
      payout: { kind: 'address', address: `0x${'11'.repeat(20)}` },
    });
  });

  it('refuses another event, topics that are not its own, and bytes that do not decode exactly', () => {
    const e = event();
    expect(() => decodeRedeemed([`0x${'00'.repeat(32)}`, ...e.topics.slice(1)], e.data)).toThrow(/not a Redeemed/);
    expect(() => decodeRedeemed([e.topics[0]!, `0x${'cd'.repeat(32)}`, e.topics[2]!], e.data)).toThrow(/topics/);
    expect(() => decodeRedeemed(e.topics, concat([e.data, compact(0n)]))).toThrow(/trailing/);
  });
});
