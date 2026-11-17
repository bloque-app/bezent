// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// What this build was made for, fixed at build time by vite.config.ts (`BRIDGE_DEPLOYMENT`,
// `BRIDGE_TESTBED_DIRECTED_DEPOSIT`), so a published bundle's hash covers its networks and addresses.

import type { Address } from 'viem';
import { networksFor, UNSET, type Deployment, type OriginNetwork } from './networks';

declare const __BRIDGE_DEPLOYMENT__: Deployment;
declare const __BRIDGE_TESTBED_DIRECTED_DEPOSIT__: string;
declare const __KREIVO_CHAIN_SPEC_SHA256__: string;
declare const __BRIDGE_REVISION__: string;

export const DEPLOYMENT: Deployment = __BRIDGE_DEPLOYMENT__;
export const REVISION: string = __BRIDGE_REVISION__;
/** The SHA-256 of `chain-specs/kreivo.json` as built; the app refuses a specification that differs. */
export const KREIVO_CHAIN_SPEC_SHA256: string = __KREIVO_CHAIN_SPEC_SHA256__;

export const NETWORKS: OriginNetwork[] = networksFor(
  DEPLOYMENT,
  __BRIDGE_TESTBED_DIRECTED_DEPOSIT__ === '' ? UNSET : (__BRIDGE_TESTBED_DIRECTED_DEPOSIT__ as Address),
);
