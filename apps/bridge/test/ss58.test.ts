// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// T-018-14: the recipient is an SS58 address with its checksum and Kreivo's prefix checked; never raw hex.
// The decoding vectors come from the core's `refs::account32` (test/vectors/core-refs.json).

import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { decodeSs58, encodeSs58, KREIVO_SS58_PREFIX, parseRecipient } from '../src/lib/ss58';
import { fromHex } from '../src/lib/bytes';

const core: { accounts: { address: string; account: string | null }[] } = JSON.parse(
  readFileSync(new URL('./vectors/core-refs.json', import.meta.url), 'utf8'),
);
const ALICE = '0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d';
const ALICE_KREIVO = 'HNZata7iMYWmk5RvZRTiAsSDhV8366zq2YGb3tLH5Upf74F';
const ALICE_GENERIC = '5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY';

describe('SS58', () => {
  it.each(core.accounts.filter((a) => !a.address.startsWith('0x')))('decodes $address as the core does', (v) => {
    expect(decodeSs58(v.address)?.account ?? null).toBe(v.account);
  });

  it('reads the prefix', () => {
    expect(decodeSs58(ALICE_KREIVO)).toEqual({ prefix: 2, account: ALICE });
    expect(decodeSs58(ALICE_GENERIC)).toEqual({ prefix: 42, account: ALICE });
  });

  it('encodes what it decodes', () => {
    expect(encodeSs58(fromHex(ALICE), 2)).toBe(ALICE_KREIVO);
    expect(encodeSs58(fromHex(ALICE), 42)).toBe(ALICE_GENERIC);
    expect(() => encodeSs58(fromHex(ALICE), 64)).toThrow();
    expect(() => encodeSs58(new Uint8Array(31), 2)).toThrow();
  });
});

describe('parseRecipient', () => {
  it('takes a Kreivo address, trimmed', () => {
    expect(parseRecipient(`  ${ALICE_KREIVO}\n`)).toEqual({ ok: true, account: ALICE, address: ALICE_KREIVO });
    expect(KREIVO_SS58_PREFIX).toBe(2);
  });

  it('gives the canonical address, encoded again from the account, not the text typed', () => {
    const r = parseRecipient(`\t${ALICE_KREIVO} `);
    expect(r.ok && r.address).toBe(encodeSs58(fromHex(ALICE), 2));
  });

  it('refuses another network’s prefix, naming it', () => {
    const r = parseRecipient(ALICE_GENERIC);
    expect(r.ok).toBe(false);
    expect(!r.ok && r.reason).toMatch(/prefix 42/);
  });

  it('refuses raw hex, even a valid 32-byte account', () => {
    const r = parseRecipient(ALICE);
    expect(r.ok).toBe(false);
    expect(!r.ok && r.reason).toMatch(/not as hex/);
  });

  it('refuses a broken checksum, an empty input and garbage', () => {
    for (const s of ['HNZata7iMYWmk5RvZRTiAsSDhV8366zq2YGb3tLH5Upf74G', '', '   ', 'not an address', '0OIl']) {
      expect(parseRecipient(s).ok).toBe(false);
    }
  });

  it('refuses the zero account', () => {
    expect(parseRecipient(encodeSs58(new Uint8Array(32), 2)).ok).toBe(false);
  });
});
