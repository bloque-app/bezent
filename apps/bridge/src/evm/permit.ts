// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The EIP-2612 permit the depositor signs (F-018, T-018-14; features/F-018-directed-deposits/NOTES.md): for
// exactly the amount, to the DirectedDeposit contract as spender, with the EIP-712 domain read from the token
// (`name()`, `version()`, the chain id and the token's address), never hard-coded across networks: native
// USDC on Polygon is "USD Coin"/"2", Amoy's test USDC is "USDC"/"2".

import type { Address } from 'viem';

export const PERMIT_TYPES = {
  Permit: [
    { name: 'owner', type: 'address' },
    { name: 'spender', type: 'address' },
    { name: 'value', type: 'uint256' },
    { name: 'nonce', type: 'uint256' },
    { name: 'deadline', type: 'uint256' },
  ],
} as const;

export interface PermitInput {
  /** What the token answers. */
  tokenName: string;
  tokenVersion: string;
  chainId: number;
  token: Address;
  /** The depositor: the permit's owner, the contract's `msg.sender`. */
  owner: Address;
  /** The DirectedDeposit contract. */
  spender: Address;
  /** Exactly the deposit's amount, in minor units. */
  value: bigint;
  /** The token's `nonces(owner)`. */
  nonce: bigint;
  /** Unix seconds. */
  deadline: bigint;
}

/** How long a permit stays valid: long enough to confirm in a wallet, short enough not to linger. */
export const PERMIT_LIFETIME_SECONDS = 20n * 60n;

export function permitDeadline(nowSeconds: bigint): bigint {
  return nowSeconds + PERMIT_LIFETIME_SECONDS;
}

/** The typed data of the permit, as `eth_signTypedData_v4` takes it. */
export function permitTypedData(p: PermitInput) {
  if (p.value <= 0n) throw new Error('a permit is for a positive amount');
  return {
    domain: { name: p.tokenName, version: p.tokenVersion, chainId: p.chainId, verifyingContract: p.token },
    types: PERMIT_TYPES,
    primaryType: 'Permit' as const,
    message: { owner: p.owner, spender: p.spender, value: p.value, nonce: p.nonce, deadline: p.deadline },
  };
}
