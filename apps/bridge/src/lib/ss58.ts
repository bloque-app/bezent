// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// SS58 addresses (F-018, T-018-14): the recipient is a Kreivo account, pasted as SS58 with its checksum and
// its network prefix checked (plan part 4: "Never take raw hex"). Decoding mirrors the core's
// `refs::account32` (`services/core/src/refs.rs`), which the vectors check.

import { base58 } from '@scure/base';
import { blake2b } from '@noble/hashes/blake2';
import { concat, equalBytes, fromHex, type Hex, toHex } from './bytes';

/** Kreivo's SS58 prefix (its chain specification's `ss58Format`). */
export const KREIVO_SS58_PREFIX = 2;

const SS58PRE = new TextEncoder().encode('SS58PRE');

function checksum(body: Uint8Array): Uint8Array {
  return blake2b(concat([SS58PRE, body]), { dkLen: 64 }).slice(0, 2);
}

export interface Decoded {
  prefix: number;
  account: Hex;
}

/** Decodes an SS58 address of a 32-byte account: its prefix and account, or `null` when it is not one
 * (bad base58, a length other than a 32-byte account's, or a wrong checksum). */
export function decodeSs58(address: string): Decoded | null {
  let bytes: Uint8Array;
  try {
    bytes = base58.decode(address);
  } catch {
    return null;
  }
  let prefix: number;
  let prefixLen: number;
  if (bytes.length === 35 && bytes[0]! < 64) {
    prefix = bytes[0]!;
    prefixLen = 1;
  } else if (bytes.length === 36 && bytes[0]! >= 64 && bytes[0]! < 128) {
    // The two-byte form (SS58 spec): six low bits of the first byte, then the second byte.
    const [a, b] = [bytes[0]!, bytes[1]!];
    prefix = ((a & 0x3f) << 2) | (b >> 6) | ((b & 0x3f) << 8);
    prefixLen = 2;
  } else {
    return null;
  }
  const body = bytes.slice(0, bytes.length - 2);
  if (!equalBytes(checksum(body), bytes.slice(bytes.length - 2))) return null;
  return { prefix, account: toHex(body.slice(prefixLen)) };
}

/** Encodes a 32-byte account with a one-byte prefix (0 to 63). */
export function encodeSs58(account: Uint8Array, prefix: number): string {
  if (account.length !== 32) throw new Error('an account is 32 bytes');
  if (!Number.isInteger(prefix) || prefix < 0 || prefix > 63) throw new Error(`prefix ${prefix} is not a one-byte SS58 prefix`);
  const body = concat([Uint8Array.of(prefix), account]);
  return base58.encode(concat([body, checksum(body)]));
}

export type RecipientInput =
  | { ok: true; account: Hex; address: string }
  | { ok: false; reason: string };

/** Validates a pasted recipient for a network whose SS58 prefix is `prefix`. Takes SS58 only: raw hex is
 * refused, because a 32-byte string carries no checksum and no network. Returns the account and its
 * canonical address, encoded again from the account rather than echoed from the input, which is what the page
 * shows. */
export function parseRecipient(input: string, prefix: number = KREIVO_SS58_PREFIX): RecipientInput {
  const text = input.trim();
  if (text === '') return { ok: false, reason: 'Enter the Kreivo account that receives the bUSDC.' };
  if (/^0x/i.test(text)) {
    return { ok: false, reason: 'Paste the account as an SS58 address, not as hex: hex carries no checksum.' };
  }
  const decoded = decodeSs58(text);
  if (!decoded) return { ok: false, reason: 'This is not a valid SS58 address (its checksum or length is wrong).' };
  if (decoded.prefix !== prefix) {
    return {
      ok: false,
      reason: `This address is for another network (SS58 prefix ${decoded.prefix}); a Kreivo address has prefix ${prefix}.`,
    };
  }
  if (/^0x0{64}$/.test(decoded.account)) return { ok: false, reason: 'The zero account cannot receive anything.' };
  return { ok: true, account: decoded.account, address: encodeSs58(fromHex(decoded.account), prefix) };
}
