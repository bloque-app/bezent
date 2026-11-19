// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// T-018-13 (REQ-ST-1): the TypeScript derivations match the Rust ones byte for byte. The vectors are
// generated from services/ingress/src/refs.rs and services/core/src/refs.rs by the Rust tests that also check
// them (services/ingress/src/refs_bridge_vectors.rs, services/core/tests/bridge_app_vectors.rs).

import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import {
  coordinateDigest,
  depositReference,
  mintReference,
  unassociatedRefundPayoutReference,
  unassociatedRefundReference,
  payoutReference,
  withdrawalReference,
  type Coordinate,
} from '../src/lib/refs';
import { decodeSs58 } from '../src/lib/ss58';
import type { Hex } from '../src/lib/bytes';

interface RawCoordinate {
  network: string;
  block_number: string;
  block_hash: Hex;
  position_kind: 'log' | 'transaction';
  position_index: number;
}
const load = (f: string) => JSON.parse(readFileSync(new URL(`./vectors/${f}`, import.meta.url), 'utf8'));
const ingress: { vectors: { coordinate: RawCoordinate; deposit: string; unassociated_refund: string }[] } = load('ingress-refs.json');
const core: {
  vectors: { coordinate: RawCoordinate; mint_reference: Hex; unassociated_refund_reference: Hex }[];
  withdrawals: { redeemer: string; reference: Hex; bridge_reference: string; payout_reference: Hex }[];
} = load('core-refs.json');

const coordinate = (c: RawCoordinate): Coordinate => ({
  network: c.network,
  blockNumber: BigInt(c.block_number),
  blockHash: c.block_hash,
  positionKind: c.position_kind,
  positionIndex: c.position_index,
});

describe('REQ-ST-1: references derived as the Rust code derives them', () => {
  it('has vectors, over the same coordinates from both crates', () => {
    expect(ingress.vectors.length).toBeGreaterThanOrEqual(8);
    expect(core.vectors.map((v) => v.coordinate)).toEqual(ingress.vectors.map((v) => v.coordinate));
  });

  it.each(ingress.vectors)('bz-d- and bz-u- at $coordinate.network $coordinate.block_number $coordinate.position_kind $coordinate.position_index', (v) => {
    const c = coordinate(v.coordinate);
    expect(depositReference(c)).toBe(v.deposit);
    expect(unassociatedRefundReference(c)).toBe(v.unassociated_refund);
  });

  it.each(core.vectors)('the mint and refund-payout references at $coordinate.network $coordinate.block_number', (v) => {
    const c = coordinate(v.coordinate);
    expect(mintReference(c)).toBe(v.mint_reference);
    expect(unassociatedRefundPayoutReference(c)).toBe(v.unassociated_refund_reference);
  });

  it.each(core.withdrawals)('T-018-17: bz-w- and the payout reference of ($redeemer, $reference)', (v) => {
    const account = v.redeemer.startsWith('0x') ? (v.redeemer as Hex) : decodeSs58(v.redeemer)!.account;
    expect(withdrawalReference(account, v.reference)).toBe(v.bridge_reference);
    expect(payoutReference(v.bridge_reference)).toBe(v.payout_reference);
  });

  it('has withdrawal vectors, one account written three ways naming one withdrawal', () => {
    expect(core.withdrawals.length).toBeGreaterThanOrEqual(6);
    expect(new Set(core.withdrawals.slice(0, 3).map((w) => w.bridge_reference)).size).toBe(1);
    expect(() => withdrawalReference('0x00', `0x${'00'.repeat(32)}`)).toThrow();
  });

  it('matches the store’s pinned vectors (services/ingress/src/refs.rs, crates/store/tests/directed_deposits.rs)', () => {
    const at = (network: string, block: bigint, hash: Hex, kind: 'log' | 'transaction', index: number) =>
      ({ network, blockNumber: block, blockHash: hash, positionKind: kind, positionIndex: index }) as Coordinate;
    expect(depositReference(at('origin-a', 7n, '0xaa', 'log', 0))).toBe('bz-d-f974333c8ffbef8f6e0e9b3bd970def281dd7767');
    expect(depositReference(at('polygon', 96_983_980n, `0x${'11'.repeat(32)}`, 'log', 0xffffffff))).toBe(
      'bz-d-668330a314e9d999ee65c91f51fb6dedd62becfe',
    );
    expect(depositReference(at('origin-a', 1n, '0xab01', 'transaction', 3))).toBe('bz-d-d1321a13639f570b72f526e3b141f8770dbcd76b');
    expect(unassociatedRefundReference(at('origin-a', 7n, '0xaa', 'log', 1))).toBe('bz-u-38661c655e8905bb6befb155263552f8bd0d1000');
  });

  it('refuses a coordinate the Rust types cannot hold', () => {
    const c: Coordinate = { network: 'polygon', blockNumber: 1n, blockHash: '0x00', positionKind: 'log', positionIndex: 0 };
    expect(() => coordinateDigest({ ...c, positionIndex: 2 ** 32 })).toThrow();
    expect(() => coordinateDigest({ ...c, positionIndex: -1 })).toThrow();
    expect(() => coordinateDigest({ ...c, blockNumber: -1n })).toThrow();
    expect(() => coordinateDigest({ ...c, blockNumber: 2n ** 64n })).toThrow();
    expect(() => coordinateDigest({ ...c, blockHash: '0xabc' as Hex })).toThrow();
  });
});
