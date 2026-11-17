// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// T-018-14: the EIP-2612 permit's typed data. The domains are checked against the DOMAIN_SEPARATOR() each
// token answered on chain (features/F-018-directed-deposits/NOTES.md), so the domain read from the token
// (name, version, chain id, address) is the one the token verifies against.

import { describe, expect, it } from 'vitest';
import {
  encodeAbiParameters,
  domainSeparator,
  hashTypedData,
  keccak256,
  recoverTypedDataAddress,
  toHex,
  type Address,
} from 'viem';
import { privateKeyToAccount } from 'viem/accounts';
import { permitDeadline, permitTypedData, PERMIT_LIFETIME_SECONDS, PERMIT_TYPES } from '../src/evm/permit';

// anvil's first published test key: a test value, never a real account's.
const OWNER = privateKeyToAccount('0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80');
const SPENDER: Address = '0x00000000000000000000000000000000000d1eC7';

const polygon = { tokenName: 'USD Coin', tokenVersion: '2', chainId: 137, token: '0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359' as Address };
const amoy = { tokenName: 'USDC', tokenVersion: '2', chainId: 80002, token: '0x41E94Eb019C0762f9Bfcf9Fb1E58725BfB0e7582' as Address };

const input = (d: typeof polygon) => ({ ...d, owner: OWNER.address, spender: SPENDER, value: 1_234_567n, nonce: 3n, deadline: 1_900_000_000n });

describe('permit typed data', () => {
  it('has the domain native USDC on Polygon verifies against', () => {
    const t = permitTypedData(input(polygon));
    expect(domainSeparator({ domain: t.domain })).toBe('0xcaa2ce1a5703ccbe253a34eb3166df60a705c561b44b192061e28f2a985be2ca');
  });

  it('has the domain Amoy’s test USDC verifies against', () => {
    const t = permitTypedData(input(amoy));
    expect(domainSeparator({ domain: t.domain })).toBe('0x5cddc98319864e897e4469bd1d13c2288c677047b54b8cca12bd342adb6be9eb');
  });

  it('is for exactly the amount, to the contract, as the depositor', () => {
    const t = permitTypedData(input(polygon));
    expect(t.primaryType).toBe('Permit');
    expect(t.types).toBe(PERMIT_TYPES);
    expect(t.message).toEqual({ owner: OWNER.address, spender: SPENDER, value: 1_234_567n, nonce: 3n, deadline: 1_900_000_000n });
  });

  it('hashes as EIP-2612 defines the Permit struct', () => {
    const t = permitTypedData(input(polygon));
    const typehash = keccak256(toHex('Permit(address owner,address spender,uint256 value,uint256 nonce,uint256 deadline)'));
    const struct = keccak256(
      encodeAbiParameters(
        [{ type: 'bytes32' }, { type: 'address' }, { type: 'address' }, { type: 'uint256' }, { type: 'uint256' }, { type: 'uint256' }],
        [typehash, OWNER.address, SPENDER, 1_234_567n, 3n, 1_900_000_000n],
      ),
    );
    const digest = keccak256(`0x1901${'caa2ce1a5703ccbe253a34eb3166df60a705c561b44b192061e28f2a985be2ca'}${struct.slice(2)}`);
    expect(hashTypedData(t)).toBe(digest);
  });

  it('signs to the owner', async () => {
    const t = permitTypedData(input(polygon));
    const signature = await OWNER.signTypedData(t);
    expect(await recoverTypedDataAddress({ ...t, signature })).toBe(OWNER.address);
  });

  it('refuses a zero amount', () => {
    expect(() => permitTypedData({ ...input(polygon), value: 0n })).toThrow();
  });

  it('expires twenty minutes after now', () => {
    expect(permitDeadline(1_000n)).toBe(1_000n + PERMIT_LIFETIME_SECONDS);
    expect(PERMIT_LIFETIME_SECONDS).toBe(1_200n);
  });
});
