#!/usr/bin/env node
// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// F-013 (T-013-04; plan DEC-19): the local CCTP relay between the Polygon and Base forks. It stands
// in for Circle's attestation service and for whoever calls `receiveMessage`:
//
//   1. watch `MessageSent(bytes)` on each fork's MessageTransmitterV2;
//   2. fill in what the attestation service fills in (MessageV2.sol's `_formatMessageForRelay`
//      leaves them empty): a nonce, unique per source log, and `finalityThresholdExecuted` (2000,
//      finalized). The burn body's `feeExecuted` and `expirationBlock` stay 0;
//   3. sign keccak256(message) with the test attester (Attestable.sol recovers from the raw digest);
//   4. call `receiveMessage(message, signature)` on the destination fork.
//
//   node relay.mjs            run until SIGINT or SIGTERM
//
// cctp-setup.mjs must have run on both forks. The relay sends from anvil dev account 9.

import { concat, decodeEventLog, encodePacked, keccak256, pad, slice, toHex } from 'viem';
import { sign } from 'viem/accounts';
import { CCTP, FINALIZED, TEST_ATTESTER, TEST_ATTESTER_KEY, TRANSMITTER_ABI, config, devAccount, fork, log, receipt } from './lib.mjs';

const cfg = config();
const forks = ['polygon', 'base'].map((name) => fork(name, cfg));
const byDomain = new Map(forks.map((f) => [f.domain, f]));
const relayer = devAccount(9);
const POLL_MS = Number(process.env.RELAY_POLL_MS ?? '1000');
const MESSAGE_SENT = TRANSMITTER_ABI.find((x) => x.type === 'event' && x.name === 'MessageSent');

/// Fill in the nonce and the finality, as the attestation service does. Offsets from MessageV2.sol:
/// nonce at 12 (32 bytes), finalityThresholdExecuted at 144 (4 bytes).
function attestable(message, nonce) {
  return concat([
    slice(message, 0, 12),
    nonce,
    slice(message, 44, 144),
    pad(toHex(FINALIZED), { size: 4 }),
    slice(message, 148),
  ]);
}

async function deliver(source, entry) {
  const { args } = decodeEventLog({ abi: TRANSMITTER_ABI, data: entry.data, topics: entry.topics });
  const raw = args.message;
  const destDomain = Number(BigInt(slice(raw, 8, 12)));
  const dest = byDomain.get(destDomain);
  if (!dest) {
    log(`skip: a message from ${source.name} to domain ${destDomain}, which is not a fork here`);
    return;
  }
  const nonce = keccak256(
    encodePacked(['uint32', 'bytes32', 'uint32'], [source.domain, entry.transactionHash, entry.logIndex]),
  );
  const message = attestable(raw, nonce);
  const attestation = await sign({ hash: keccak256(message), privateKey: TEST_ATTESTER_KEY, to: 'hex' });
  await dest.anvil('anvil_setBalance', [relayer.address, '0x56BC75E2D63100000']);
  const hash = await dest.wallet(relayer).writeContract({
    address: CCTP.messageTransmitter,
    abi: TRANSMITTER_ABI,
    functionName: 'receiveMessage',
    args: [message, attestation],
  });
  const rcpt = await receipt(dest, hash);
  log(`${source.name} ${entry.transactionHash} -> ${dest.name} receiveMessage ${hash}: ${rcpt.status}`);
}

let running = true;
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => (running = false));

const cursors = new Map();
for (const f of forks) cursors.set(f, (await f.pub.getBlockNumber()) + 1n);
log(`relaying Polygon <-> Base; attester ${TEST_ATTESTER.address}`);

while (running) {
  for (const f of forks) {
    try {
      const head = await f.pub.getBlockNumber();
      if (head < cursors.get(f)) continue;
      const logs = await f.pub.getLogs({ address: CCTP.messageTransmitter, event: MESSAGE_SENT, fromBlock: cursors.get(f), toBlock: head });
      for (const entry of logs) await deliver(f, entry);
      cursors.set(f, head + 1n);
    } catch (e) {
      log(`error on ${f.name}: ${e.shortMessage ?? e.message}`);
    }
  }
  await new Promise((r) => setTimeout(r, POLL_MS));
}
log('stopped');
