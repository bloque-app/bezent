// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// T-018-12, T-018-14: the contract's address, its reserve and its code hash are pinned per network, and a network
// with any of them unset is refused, never filled in from elsewhere. A network pairs only with an issuing network
// of its own deployment, and a production issuing network needs its refused recipients pinned.

import { describe, expect, it } from 'vitest';
import {
  KREIVO,
  KREIVO_TESTBED,
  networksFor,
  NO_TESTBED_PINS,
  POLYGON,
  POLYGON_AMOY,
  resolveNetwork,
  testbed,
  UNSET,
  UnsetConfiguration,
  type KreivoNetwork,
  type OriginNetwork,
  type TestbedPins,
} from '../src/config/networks';

const DEPLOYED = '0x5FbDB2315678afecb367f032d93F642f64180aa3';
const RESERVE = '0x000000000000000000000000000000000000Ba5e';
const CODE_HASH = `0x${'c0'.repeat(32)}` as const;
const PINS: TestbedPins = { directedDeposit: DEPLOYED, reserve: RESERVE, codeHash: CODE_HASH };
const REFUSED = `0x${'11'.repeat(32)}`;
const TEST_KREIVO: KreivoNetwork = { ...KREIVO_TESTBED, network: 'kreivo-test', label: 'Kreivo (test)', deployment: 'test' };
/** Amoy with everything pinned and a test Kreivo, as T-018-04 will make it. */
const AMOY: OriginNetwork = { ...POLYGON_AMOY, ...PINS, kreivo: TEST_KREIVO };

describe('resolveNetwork', () => {
  it('refuses Polygon while its DirectedDeposit is not deployed, and Amoy while it names no test Kreivo', () => {
    expect(POLYGON.directedDeposit).toBe(UNSET);
    expect(() => resolveNetwork(POLYGON)).toThrow(UnsetConfiguration);
    expect(() => resolveNetwork({ ...AMOY, directedDeposit: UNSET })).toThrow(/DirectedDeposit address is not set/);
    // T-018-04 pinned Amoy's contract; no test Kreivo is named yet, so Amoy is still refused.
    expect(POLYGON_AMOY.directedDeposit).toBe('0xB8F75BeAE2723637313072fC89C69B7C15EdAec7');
    expect(POLYGON_AMOY.kreivo).toBe(UNSET);
    expect(() => resolveNetwork(POLYGON_AMOY)).toThrow(UnsetConfiguration);
  });

  it('leaves the reserve and the code hash unset for Polygon, pins them for Amoy, and refuses either unset', () => {
    expect(POLYGON.reserve).toBe(UNSET);
    expect(POLYGON.codeHash).toBe(UNSET);
    expect(POLYGON_AMOY.reserve).toBe('0x3530dBB6986Dc584bf3D83a0ed4cbE027238204a');
    expect(POLYGON_AMOY.codeHash).toBe('0x6c32ce3fe773ad4f2bb35e6f83948766d3058c0dbc016ffed003b891489307f0');
    expect(() => resolveNetwork({ ...AMOY, reserve: UNSET })).toThrow(/reserve address is not set/);
    expect(() => resolveNetwork({ ...AMOY, codeHash: UNSET })).toThrow(/code hash is not set/);
    expect(() => resolveNetwork({ ...AMOY, codeHash: '0x1234' })).toThrow(/not 32 bytes/);
    expect(() => resolveNetwork({ ...AMOY, reserve: DEPLOYED })).toThrow(/not three addresses/);
    expect(() => resolveNetwork({ ...AMOY, reserve: POLYGON_AMOY.token! })).toThrow(/not three addresses/);
  });

  it('refuses the test bed without its pins, and resolves it with them', () => {
    expect(() => resolveNetwork(testbed(NO_TESTBED_PINS))).toThrow(UnsetConfiguration);
    expect(() => resolveNetwork(testbed({ ...PINS, reserve: UNSET }))).toThrow(UnsetConfiguration);
    expect(() => resolveNetwork(testbed({ ...PINS, codeHash: UNSET }))).toThrow(UnsetConfiguration);
    const n = resolveNetwork(testbed(PINS));
    expect(n.directedDeposit).toBe(DEPLOYED);
    expect(n.reserve).toBe(RESERVE);
    expect(n.codeHash).toBe(CODE_HASH);
    expect(n.token).toBe('0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359');
  });

  it('refuses an unset token, a malformed or zero address, and one address for both', () => {
    expect(resolveNetwork(AMOY).directedDeposit).toBe(DEPLOYED);
    expect(() => resolveNetwork({ ...AMOY, token: UNSET })).toThrow(/token address is not set/);
    expect(() => resolveNetwork({ ...AMOY, directedDeposit: '0x1234' })).toThrow(/malformed/);
    expect(() => resolveNetwork({ ...AMOY, directedDeposit: `0x${'0'.repeat(40)}` })).toThrow(/zero address/);
    expect(() => resolveNetwork({ ...AMOY, directedDeposit: AMOY.token })).toThrow(/not three addresses/);
    expect(() => resolveNetwork({ ...AMOY, directedDeposit: '' as `0x${string}` })).toThrow(UnsetConfiguration);
  });

  it('allows an RPC connection to Kreivo only on this machine', () => {
    const base: OriginNetwork = testbed(PINS);
    const at = (url: string): OriginNetwork => ({ ...base, kreivo: { ...KREIVO_TESTBED, connection: { kind: 'loopback-rpc', url } } });
    expect(() => resolveNetwork(at('ws://127.0.0.1:9944'))).not.toThrow();
    expect(() => resolveNetwork(at('ws://localhost:9944'))).not.toThrow();
    expect(() => resolveNetwork(at('wss://kreivo.io'))).toThrow(/only to a node on this machine/);
    expect(() => resolveNetwork(at('ws://127.0.0.1.example.com:9944'))).toThrow();
  });

  it('reads live Kreivo through the light client', () => {
    expect(POLYGON.kreivo).toBe(KREIVO);
    expect(KREIVO.connection).toEqual({ kind: 'light-client', relay: 'kusama' });
    expect(KREIVO.ss58Prefix).toBe(2);
    expect(KREIVO.maxConsumers).toBe(16);
  });
});

describe('a production issuing network', () => {
  const polygon: OriginNetwork = { ...POLYGON, ...PINS };

  it('is refused while its refused recipients are not pinned (T-018-18 pins them at the mainnet deploy)', () => {
    expect(KREIVO.refusedRecipients).toEqual([]);
    expect(() => resolveNetwork(polygon)).toThrow(/no refused recipient is pinned/);
  });

  it('resolves once they are', () => {
    const n = resolveNetwork({ ...polygon, kreivo: { ...KREIVO, refusedRecipients: [REFUSED] } });
    expect(n.kreivo.refusedRecipients).toEqual([REFUSED]);
    expect(() => resolveNetwork({ ...polygon, kreivo: { ...KREIVO, refusedRecipients: ['0x11'] } })).toThrow(/not 32 bytes/);
  });
});

describe('pairing an origin network with an issuing network', () => {
  it('names no issuing network for Amoy yet, so Amoy never mints on live Kreivo', () => {
    expect(POLYGON_AMOY.kreivo).toBe(UNSET);
    expect(() => resolveNetwork({ ...POLYGON_AMOY, ...PINS })).toThrow(/issuing network its deposits mint on is not set/);
  });

  it('never pairs a test origin network with a production issuing network', () => {
    const live: KreivoNetwork = { ...KREIVO, refusedRecipients: [REFUSED] };
    expect(() => resolveNetwork({ ...AMOY, kreivo: live })).toThrow(/pairs only with its own deployment/);
    expect(() => resolveNetwork({ ...testbed(PINS), kreivo: live })).toThrow(/pairs only with its own deployment/);
    expect(() => resolveNetwork({ ...POLYGON, ...PINS, kreivo: TEST_KREIVO })).toThrow(/pairs only with its own deployment/);
    expect(resolveNetwork(AMOY).kreivo).toBe(TEST_KREIVO);
  });

  it('holds for every network a test or test bed build carries', () => {
    for (const n of [...networksFor('test'), ...networksFor('testbed', PINS)]) {
      expect(n.deployment).not.toBe('production');
      expect(n.kreivo === UNSET || n.kreivo.deployment === n.deployment).toBe(true);
    }
  });
});

describe('networksFor', () => {
  it('gives each deployment its networks only', () => {
    expect(networksFor('production').map((n) => n.key)).toEqual(['polygon']);
    expect(networksFor('test').map((n) => n.key)).toEqual(['polygon-amoy']);
    expect(networksFor('testbed', PINS).map((n) => [n.key, n.directedDeposit, n.reserve, n.codeHash])).toEqual([
      ['testbed', DEPLOYED, RESERVE, CODE_HASH],
    ]);
  });

  it('derives references under bezent’s network ids, not chain ids', () => {
    expect(POLYGON.network).toBe('polygon');
    expect(POLYGON_AMOY.network).toBe('polygon-amoy');
    expect(testbed(NO_TESTBED_PINS).network).toBe('polygon');
  });
});
