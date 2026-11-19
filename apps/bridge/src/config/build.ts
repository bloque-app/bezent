// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// What this build was made for, fixed at build time by vite.config.ts (`BRIDGE_DEPLOYMENT`; for a test bed build
// `BRIDGE_TESTBED_DIRECTED_DEPOSIT`, `BRIDGE_TESTBED_RESERVE`, `BRIDGE_TESTBED_CODE_HASH`, `BRIDGE_TESTBED_BEZENT`,
// `BRIDGE_TESTBED_EMITTER`, `BRIDGE_TESTBED_PAYOUTS_FROM`; and `BRIDGE_STATUS_URL`), so a published bundle's hash
// covers its networks, its addresses and the status route it reads.

import type { Address, Hex } from 'viem';
import { networksFor, UNSET, type Deployment, type OriginNetwork, type Pinned } from './networks';

declare const __BRIDGE_DEPLOYMENT__: Deployment;
declare const __BRIDGE_TESTBED__: { directedDeposit: string; reserve: string; codeHash: string; bezent: string; emitter: string; payoutsFrom: string };
declare const __BRIDGE_STATUS_URL__: string;
declare const __KREIVO_CHAIN_SPEC_SHA256__: string;
declare const __KUSAMA_CHAIN_SPEC_SHA256__: string;
declare const __BRIDGE_REVISION__: string;

export const DEPLOYMENT: Deployment = __BRIDGE_DEPLOYMENT__;
export const REVISION: string = __BRIDGE_REVISION__;
/** The public status route's base URL (T-018-10), or `null`: unset by default, and then the status page reads chain
 * data alone. */
export const STATUS_ROUTE: string | null = __BRIDGE_STATUS_URL__ === '' ? null : __BRIDGE_STATUS_URL__;
/** The SHA-256 of `chain-specs/kreivo.json` and `chain-specs/kusama.json` as built; the app refuses a
 * specification that differs. */
export const CHAIN_SPEC_SHA256 = { kreivo: __KREIVO_CHAIN_SPEC_SHA256__, kusama: __KUSAMA_CHAIN_SPEC_SHA256__ } as const;

const pin = <T extends string>(v: string): Pinned<T> => (v === '' ? UNSET : (v as T));

export const NETWORKS: OriginNetwork[] = networksFor(DEPLOYMENT, {
  directedDeposit: pin<Address>(__BRIDGE_TESTBED__.directedDeposit),
  reserve: pin<Address>(__BRIDGE_TESTBED__.reserve),
  codeHash: pin<Hex>(__BRIDGE_TESTBED__.codeHash),
  bezent: pin<string>(__BRIDGE_TESTBED__.bezent),
  emitter: pin<Address>(__BRIDGE_TESTBED__.emitter),
  payoutsFrom: __BRIDGE_TESTBED__.payoutsFrom === '' ? UNSET : BigInt(__BRIDGE_TESTBED__.payoutsFrom),
});
