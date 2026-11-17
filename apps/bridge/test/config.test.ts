// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// T-018-12, T-018-14: the contract's address is pinned per network, and a network whose address is unset is
// refused, never filled in from elsewhere.

import { describe, expect, it } from 'vitest';
import {
  networksFor,
  POLYGON,
  POLYGON_AMOY,
  resolveNetwork,
  testbed,
  UNSET,
  UnsetConfiguration,
  type OriginNetwork,
} from '../src/config/networks';

const DEPLOYED = '0x5FbDB2315678afecb367f032d93F642f64180aa3';

describe('resolveNetwork', () => {
  it('refuses Amoy and Polygon while their DirectedDeposit is not deployed', () => {
    expect(POLYGON_AMOY.directedDeposit).toBe(UNSET);
    expect(POLYGON.directedDeposit).toBe(UNSET);
    expect(() => resolveNetwork(POLYGON_AMOY)).toThrow(UnsetConfiguration);
    expect(() => resolveNetwork(POLYGON_AMOY)).toThrow(/DirectedDeposit address is not set/);
    expect(() => resolveNetwork(POLYGON)).toThrow(UnsetConfiguration);
  });

  it('refuses the test bed without an address, and resolves it with one', () => {
    expect(() => resolveNetwork(testbed(UNSET))).toThrow(UnsetConfiguration);
    const n = resolveNetwork(testbed(DEPLOYED));
    expect(n.directedDeposit).toBe(DEPLOYED);
    expect(n.token).toBe('0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359');
  });

  it('refuses an unset token, a malformed or zero address, and one address for both', () => {
    const base: OriginNetwork = { ...POLYGON_AMOY, directedDeposit: DEPLOYED };
    expect(resolveNetwork(base).directedDeposit).toBe(DEPLOYED);
    expect(() => resolveNetwork({ ...base, token: UNSET })).toThrow(/token address is not set/);
    expect(() => resolveNetwork({ ...base, directedDeposit: '0x1234' })).toThrow(/malformed/);
    expect(() => resolveNetwork({ ...base, directedDeposit: `0x${'0'.repeat(40)}` })).toThrow(/zero address/);
    expect(() => resolveNetwork({ ...base, directedDeposit: base.token })).toThrow(/one address/);
    expect(() => resolveNetwork({ ...base, directedDeposit: '' as `0x${string}` })).toThrow(UnsetConfiguration);
  });

  it('allows an RPC connection to Kreivo only on this machine', () => {
    const base: OriginNetwork = testbed(DEPLOYED);
    const at = (url: string) => ({ ...base, kreivo: { ...base.kreivo, connection: { kind: 'loopback-rpc' as const, url } } });
    expect(() => resolveNetwork(at('ws://127.0.0.1:9944'))).not.toThrow();
    expect(() => resolveNetwork(at('ws://localhost:9944'))).not.toThrow();
    expect(() => resolveNetwork(at('wss://kreivo.io'))).toThrow(/only to a node on this machine/);
    expect(() => resolveNetwork(at('ws://127.0.0.1.example.com:9944'))).toThrow();
  });

  it('reads live Kreivo through the light client', () => {
    expect(POLYGON.kreivo.connection).toEqual({ kind: 'light-client', relay: 'kusama' });
    expect(POLYGON_AMOY.kreivo.connection.kind).toBe('light-client');
    expect(POLYGON.kreivo.ss58Prefix).toBe(2);
  });
});

describe('networksFor', () => {
  it('gives each deployment its networks only', () => {
    expect(networksFor('production').map((n) => n.key)).toEqual(['polygon']);
    expect(networksFor('test').map((n) => n.key)).toEqual(['polygon-amoy']);
    expect(networksFor('testbed', DEPLOYED).map((n) => [n.key, n.directedDeposit])).toEqual([['testbed', DEPLOYED]]);
  });

  it('derives references under bezent’s network ids, not chain ids', () => {
    expect(POLYGON.network).toBe('polygon');
    expect(POLYGON_AMOY.network).toBe('polygon-amoy');
    expect(testbed(UNSET).network).toBe('polygon');
  });
});
