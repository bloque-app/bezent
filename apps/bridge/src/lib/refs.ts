// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The references bezent derives from an arrival's coordinate (F-018, T-018-13; REQ-ST-1, REQ-ID-3), in
// TypeScript, byte for byte as the Rust code derives them:
//
// - `services/ingress/src/refs.rs`: the bridge reference of the deposit that binds the arrival (`bz-d-…`,
//   `refs::deposit`) and of the refund of an unassociated arrival (`bz-u-…`, `refs::unassociated_refund`);
// - `services/core/src/refs.rs`: the 32-byte on-chain reference the mint carries (`refs::mint_reference`)
//   and the one the refund's payout carries (`refs::unassociated_refund_reference`).
//
// Every derivation is SHA-256 over a domain tag and length-prefixed fields (an 8-byte big-endian length,
// then the bytes). `test/vectors/*.json` are generated from the Rust code, and the tests here check every
// vector, so the two never drift.

import { sha256 } from '@noble/hashes/sha2';
import { beBytes, concat, fromHex, toHex, type Hex } from './bytes';

/** A coordinate as bezent's store holds it (`REQ-IN-3`): network id, block, block hash and position. */
export interface Coordinate {
  /** bezent's id of the network (`polygon`, `polygon-amoy`), not a chain id. */
  network: string;
  blockNumber: bigint;
  /** The block hash's bytes, as `0x` hex. */
  blockHash: Hex;
  positionKind: 'log' | 'transaction';
  /** The log's index within its block (`logIndex`), or the transaction's. */
  positionIndex: number;
}

const utf8 = (s: string) => new TextEncoder().encode(s);

/** SHA-256 over `tag` and each field, each prefixed by its length as 8 big-endian bytes. */
export function tagged(tag: string, fields: Uint8Array[]): Uint8Array {
  const parts: Uint8Array[] = [];
  const push = (b: Uint8Array) => parts.push(beBytes(BigInt(b.length), 8), b);
  push(utf8(tag));
  fields.forEach(push);
  return sha256(concat(parts));
}

/** The coordinate digest (`bezent/coordinate/v0`) every coordinate-derived reference starts from. */
export function coordinateDigest(c: Coordinate): Uint8Array {
  if (!Number.isInteger(c.positionIndex) || c.positionIndex < 0 || c.positionIndex > 0xffff_ffff) {
    throw new Error(`position index ${c.positionIndex} is not a u32`);
  }
  if (c.blockNumber < 0n) throw new Error('negative block number');
  return tagged('bezent/coordinate/v0', [
    utf8(c.network),
    beBytes(c.blockNumber, 8),
    fromHex(c.blockHash),
    utf8(c.positionKind),
    beBytes(BigInt(c.positionIndex), 4),
  ]);
}

/** `prefix` and the first 20 bytes of `digest` in lowercase hex. */
function short(prefix: string, digest: Uint8Array): string {
  return prefix + toHex(digest.slice(0, 20)).slice(2);
}

/** The bridge reference of the deposit that binds the arrival at `c` (`bz-d-…`; ingress `refs::deposit`). */
export function depositReference(c: Coordinate): string {
  return short('bz-d-', tagged('bezent/deposit/v0', [coordinateDigest(c)]));
}

/** The bridge reference of the refund of the unassociated arrival at `c` (`bz-u-…`; ingress
 * `refs::unassociated_refund`): the return of a directed arrival that can never be minted. */
export function unassociatedRefundReference(c: Coordinate): string {
  return short('bz-u-', tagged('bezent/refund/v0', [coordinateDigest(c)]));
}

/** The 32-byte on-chain reference the mint of the deposit at `c` carries on Kreivo (core
 * `refs::mint_reference`), which the contract's `mint_reference_used` answers for. */
export function mintReference(c: Coordinate): Hex {
  return toHex(tagged('bezent/mint-reference/v0', [coordinateDigest(c)]));
}

/** The 32-byte reference the payout of the refund of the unassociated arrival at `c` carries on the origin
 * network (core `refs::unassociated_refund_reference`). */
export function unassociatedRefundPayoutReference(c: Coordinate): Hex {
  return toHex(tagged('bezent/unassociated-refund-reference/v0', [coordinateDigest(c)]));
}
