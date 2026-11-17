// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// T-018-15, T-018-17, against Kreivo mainnet, read-only, and only when asked: `BRIDGE_LIVE_KREIVO_WS=wss://… npm test`.
// Skipped otherwise (CI included), since it needs the network. A public RPC is used here, in a test, and never in the
// bundle, which reads Kreivo through smoldot.
//
// It drives the polkadot-api adapter (kreivo/redeem.ts) against the real runtime and the real bezent contract: the
// runtime's signed extensions are the ones the app registers; the contract's bounds, backends and pauses decode; a
// redemption from an account with nothing to redeem is refused by both dry runs, and nothing is signed; an unused
// mint reference reads as unused.

import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { createClient, type PolkadotClient } from 'polkadot-api';
import { getWsProvider } from 'polkadot-api/ws';
import { KREIVO } from '../src/config/networks';
import { isPullRefusal, decodeRedeemDryRun, redeemData } from '../src/kreivo/bezent';
import { kreivoRuntime, mintReferenceUsed, redeemChain, readRedemption } from '../src/kreivo/redeem';
import type { Kreivo } from '../src/kreivo/client';
import { fromHex, type Hex } from '../src/lib/bytes';
import { limits } from '../src/redeem';

const url = process.env.BRIDGE_LIVE_KREIVO_WS;
const contract = KREIVO.bezent as Hex;
// //Alice's account: on Kreivo mainnet it holds no bUSDC and no KSM.
const ALICE: Hex = '0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d';

describe.skipIf(!url)('Kreivo mainnet, read-only', () => {
  let client: PolkadotClient;
  let k: Kreivo;
  beforeAll(() => {
    client = createClient(getWsProvider(url!));
    k = { client, api: client.getUnsafeApi(), close: () => client.destroy() };
  });
  afterAll(() => client?.destroy());

  it('signs with the extensions the app registers, and gives the wallet their metadata', async () => {
    const { extensions, def } = await kreivoRuntime(k, KREIVO);
    expect(extensions).toEqual([
      'PassAuthenticate',
      'CheckNonZeroSender',
      'CheckSpecVersion',
      'CheckTxVersion',
      'CheckGenesis',
      'CheckMortality',
      'CheckNonce',
      'CheckWeight',
      'ChargeAssetTxPayment',
      'DynamicMaxBlockWeight',
    ]);
    expect(def).toMatchObject({ genesisHash: '0xc710a5f16adc17bcd212cff0aedcbf1c1212a043cdc0fb2dcba861efe5305b01', ss58Format: 2, tokenDecimals: 12, tokenSymbol: 'KSM' });
    expect(def.specVersion).toBeGreaterThanOrEqual(136);
    expect(def.rawMetadata?.slice(0, 12)).toBe('0x6d6574610f'.slice(0, 12)); // "meta", V15
  }, 60_000);

  it('reads the contract’s bounds, backends and pauses for bUSDC', async () => {
    const f = await redeemChain(k, KREIVO, contract).facts(ALICE);
    expect(f.bounds?.redeemMinimum).toBeGreaterThan(0n);
    expect(f.backends).toContainEqual({ kind: 'evm', chainId: 137n });
    expect(f.pauses.map((p) => p.scope)).toEqual(['everything', 'every redemption', 'Here(101)', 'redemptions of Here(101)']);
    expect(f.balance).toBe(0n);
  }, 60_000);

  it('dry-runs a redemption from an account with nothing to redeem: both dry runs refuse it', async () => {
    const chain = redeemChain(k, KREIVO, contract);
    const reference: Hex = `0x${'be'.repeat(32)}`;
    expect(await chain.redemption(ALICE, reference)).toBeUndefined();
    const data = redeemData(101, 1_000_000n, { kind: 'evm', chainId: 137n }, fromHex(`0x${'11'.repeat(20)}`), reference);
    const dry = await chain.dryRunMessage(ALICE, data);
    expect(isPullRefusal(decodeRedeemDryRun(dry))).toBe(true);
    const { gas, storageLimit } = limits(dry);
    const prepared = await chain.prepare({ redeemer: ALICE, asset: 101, amount: 1_000_000n, data }, gas, storageLimit);
    expect(prepared.dryRun.ok).toBe(false);
    expect(prepared.callData.slice(0, 6)).toBe('0x2b02'); // Utility.batch_all
  }, 60_000);

  it('reads an unused mint reference as unused, and no redemption for an unused reference', async () => {
    expect(await mintReferenceUsed(k, KREIVO, contract, `0x${'00'.repeat(31)}01`)).toBe(false);
    expect(await readRedemption(k, KREIVO, contract, ALICE, `0x${'00'.repeat(32)}`)).toBeUndefined();
  }, 60_000);
});
