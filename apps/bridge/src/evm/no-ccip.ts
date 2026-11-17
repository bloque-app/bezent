// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Stands in for viem's `utils/ccip.js` in the build (vite.config.ts, `noCcip`): the app never follows a
// contract's CCIP-Read (EIP-3668) redirect to an off-chain URL, so the code that would is left out. The clients
// also set `ccipRead: false`, so nothing reaches these functions; if anything did, it fails rather than fetches.

export const offchainLookupSignature = '0x556f1830';

export const offchainLookupAbiItem = {
  name: 'OffchainLookup',
  type: 'error',
  inputs: [
    { name: 'sender', type: 'address' },
    { name: 'urls', type: 'string[]' },
    { name: 'callData', type: 'bytes' },
    { name: 'callbackFunction', type: 'bytes4' },
    { name: 'extraData', type: 'bytes' },
  ],
} as const;

const disabled = (): never => {
  throw new Error('CCIP-Read is disabled in the bezent bridge app.');
};

export async function offchainLookup(): Promise<never> {
  return disabled();
}

export async function ccipRequest(): Promise<never> {
  return disabled();
}
