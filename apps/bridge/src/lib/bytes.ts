// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Byte helpers: hex both ways and big-endian integers, with no dependency.

export type Hex = `0x${string}`;

const HEX = /^0x([0-9a-fA-F]{2})*$/;

/** Decodes `0x`-prefixed hex with an even number of digits; anything else throws. */
export function fromHex(hex: string): Uint8Array {
  if (!HEX.test(hex)) throw new Error(`not 0x-prefixed hex bytes: ${JSON.stringify(hex)}`);
  const out = new Uint8Array((hex.length - 2) / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(2 + 2 * i, 4 + 2 * i), 16);
  return out;
}

/** Lowercase `0x`-prefixed hex. */
export function toHex(bytes: Uint8Array): Hex {
  let s = '0x';
  for (const b of bytes) s += b.toString(16).padStart(2, '0');
  return s as Hex;
}

/** `value` as `width` big-endian bytes; throws when it does not fit or is negative. */
export function beBytes(value: bigint, width: number): Uint8Array {
  if (value < 0n || value >= 1n << BigInt(8 * width)) {
    throw new Error(`${value} does not fit in ${width} unsigned bytes`);
  }
  const out = new Uint8Array(width);
  let v = value;
  for (let i = width - 1; i >= 0; i--) {
    out[i] = Number(v & 0xffn);
    v >>= 8n;
  }
  return out;
}

export function concat(parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

export function equalBytes(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((x, i) => x === b[i]);
}
