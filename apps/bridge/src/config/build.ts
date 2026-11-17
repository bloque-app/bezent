// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// What this build was made for, fixed at build time by vite.config.ts (`BRIDGE_DEPLOYMENT`, and for a test bed
// build `BRIDGE_TESTBED_DIRECTED_DEPOSIT`, `BRIDGE_TESTBED_RESERVE`, `BRIDGE_TESTBED_CODE_HASH`), so a published
// bundle's hash covers its networks and addresses.

import type { Address, Hex } from 'viem';
import { networksFor, UNSET, type Deployment, type OriginNetwork, type Pinned } from './networks';

declare const __BRIDGE_DEPLOYMENT__: Deployment;
declare const __BRIDGE_TESTBED__: { directedDeposit: string; reserve: string; codeHash: string };
declare const __KREIVO_CHAIN_SPEC_SHA256__: string;
declare const __KUSAMA_CHAIN_SPEC_SHA256__: string;
declare const __BRIDGE_REVISION__: string;

export const DEPLOYMENT: Deployment = __BRIDGE_DEPLOYMENT__;
export const REVISION: string = __BRIDGE_REVISION__;
/** The SHA-256 of `chain-specs/kreivo.json` and `chain-specs/kusama.json` as built; the app refuses a
 * specification that differs. */
export const CHAIN_SPEC_SHA256 = { kreivo: __KREIVO_CHAIN_SPEC_SHA256__, kusama: __KUSAMA_CHAIN_SPEC_SHA256__ } as const;

const pin = <T extends string>(v: string): Pinned<T> => (v === '' ? UNSET : (v as T));

export const NETWORKS: OriginNetwork[] = networksFor(DEPLOYMENT, {
  directedDeposit: pin<Address>(__BRIDGE_TESTBED__.directedDeposit),
  reserve: pin<Address>(__BRIDGE_TESTBED__.reserve),
  codeHash: pin<Hex>(__BRIDGE_TESTBED__.codeHash),
});
