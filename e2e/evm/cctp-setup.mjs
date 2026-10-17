#!/usr/bin/env node
// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// F-013 (T-013-04; plan DEC-19): local CCTP V2 on the Polygon and Base forks. On each fork, as the
// MessageTransmitterV2's attester manager (impersonated), enable the test attester and set the
// signature threshold to 1. relay.mjs then signs and delivers messages with no Circle service.
//
//   node cctp-setup.mjs

import { CCTP, NETWORKS, TEST_ATTESTER, TRANSMITTER_ABI, asImpersonated, config, fork, log } from './lib.mjs';

const cfg = config();
for (const name of ['polygon', 'base']) {
  const f = fork(name, cfg);
  await f.assertAnvil(); // loopback and anvil, before any impersonation or storage write
  const read = (functionName, args = []) =>
    f.pub.readContract({ address: CCTP.messageTransmitter, abi: TRANSMITTER_ABI, functionName, args });
  const domain = await read('localDomain');
  if (domain !== NETWORKS[name].domain) throw new Error(`${f.name}: localDomain ${domain}, want ${NETWORKS[name].domain}`);
  const manager = await read('attesterManager');
  const write = (functionName, args) => asImpersonated(f, manager, { address: CCTP.messageTransmitter, abi: TRANSMITTER_ABI, functionName, args });

  if (!(await read('isEnabledAttester', [TEST_ATTESTER.address]))) await write('enableAttester', [TEST_ATTESTER.address]);
  if ((await read('signatureThreshold')) !== 1n) await write('setSignatureThreshold', [1n]);

  const [enabled, threshold, n] = await Promise.all([
    read('isEnabledAttester', [TEST_ATTESTER.address]),
    read('signatureThreshold'),
    read('getNumEnabledAttesters'),
  ]);
  if (!enabled || threshold !== 1n) throw new Error(`${f.name}: test attester enabled ${enabled}, threshold ${threshold}`);
  log(`${f.name} (domain ${domain}): attester manager ${manager}; test attester ${TEST_ATTESTER.address} enabled; threshold 1 of ${n}`);
}
