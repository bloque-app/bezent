// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The little SCALE the app writes and reads by hand: the bezent contract's messages, their answers and its events
// (kreivo/bezent.ts), as the Kreivo endpoint does in Rust (`services/kreivo-endpoint/src/kreivo.rs`). Integers are
// little-endian; a compact integer is SCALE's variable-length form. Every reader throws on short input, and the
// callers refuse trailing bytes, so nothing is guessed at.

import { concat } from './bytes';

/** `value` as `width` little-endian bytes; throws when it does not fit or is negative. */
export function le(value: bigint, width: number): Uint8Array {
  if (value < 0n || value >= 1n << BigInt(8 * width)) throw new Error(`${value} does not fit in ${width} unsigned bytes`);
  const out = new Uint8Array(width);
  let v = value;
  for (let i = 0; i < width; i++) {
    out[i] = Number(v & 0xffn);
    v >>= 8n;
  }
  return out;
}

export const u8 = (v: number) => le(BigInt(v), 1);
export const u32 = (v: number) => le(BigInt(v), 4);
export const u64 = (v: bigint) => le(v, 8);
export const u128 = (v: bigint) => le(v, 16);

/** SCALE's compact encoding of an unsigned integer. */
export function compact(value: bigint): Uint8Array {
  if (value < 0n) throw new Error('a compact integer is unsigned');
  if (value < 1n << 6n) return le(value << 2n, 1);
  if (value < 1n << 14n) return le((value << 2n) | 1n, 2);
  if (value < 1n << 30n) return le((value << 2n) | 2n, 4);
  let n = 0;
  for (let v = value; v > 0n; v >>= 8n) n++;
  if (n > 67) throw new Error('too large for a compact integer');
  return concat([Uint8Array.of(((n - 4) << 2) | 3), le(value, n)]);
}

/** A `Vec<u8>`: its compact length, then the bytes. */
export const vecU8 = (b: Uint8Array) => concat([compact(BigInt(b.length)), b]);

/** Reads SCALE from a byte string, front to back. */
export class Reader {
  private at = 0;
  constructor(private readonly bytes: Uint8Array) {}

  get remaining(): number {
    return this.bytes.length - this.at;
  }

  take(n: number): Uint8Array {
    if (n < 0 || this.at + n > this.bytes.length) throw new Error(`SCALE: ${n} bytes wanted, ${this.remaining} left`);
    const out = this.bytes.slice(this.at, this.at + n);
    this.at += n;
    return out;
  }

  uint(width: number): bigint {
    const b = this.take(width);
    let v = 0n;
    for (let i = width - 1; i >= 0; i--) v = (v << 8n) | BigInt(b[i]!);
    return v;
  }

  u8(): number {
    return Number(this.uint(1));
  }
  u32(): number {
    return Number(this.uint(4));
  }
  u64(): bigint {
    return this.uint(8);
  }
  u128(): bigint {
    return this.uint(16);
  }

  bool(): boolean {
    const b = this.u8();
    if (b > 1) throw new Error(`SCALE: ${b} is not a bool`);
    return b === 1;
  }

  compact(): bigint {
    const first = this.bytes[this.at];
    if (first === undefined) throw new Error('SCALE: a compact integer wanted, nothing left');
    switch (first & 3) {
      case 0:
        return this.uint(1) >> 2n;
      case 1:
        return this.uint(2) >> 2n;
      case 2:
        return this.uint(4) >> 2n;
      default: {
        this.take(1);
        return this.uint((first >> 2) + 4);
      }
    }
  }

  vecU8(): Uint8Array {
    return this.take(Number(this.compact()));
  }

  /** Throws unless every byte was read. */
  end(what: string): void {
    if (this.remaining !== 0) throw new Error(`${what}: ${this.remaining} trailing bytes`);
  }
}
