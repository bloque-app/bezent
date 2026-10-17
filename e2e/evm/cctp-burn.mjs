#!/usr/bin/env node
// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// F-013 (T-013-04): a USDC burn through CCTP V2 on the Polygon fork, minted on the Base fork by the
// local relay. relay.mjs must be running, and cctp-setup.mjs must have run.
//
//   node cctp-burn.mjs [--summary out.md]
//
// A standard transfer (minFinalityThreshold 2000) of 10 USDC from anvil dev account 1 on Polygon to
// dev account 2 on Base. The relay executes no fee, so exactly the burned amount is minted.

import { writeFileSync } from 'node:fs';
import { formatUnits, parseUnits, zeroHash } from 'viem';
import { CCTP, ERC20_ABI, FINALIZED, MESSENGER_ABI, TOKENS, config, devAccount, fork, log, receipt, toBytes32 } from './lib.mjs';

const summaryPath = process.argv.includes('--summary') ? process.argv[process.argv.indexOf('--summary') + 1] : null;
const TIMEOUT_MS = Number(process.env.CCTP_TIMEOUT_MS ?? '180000');
const cfg = config();
const polygon = fork('polygon', cfg);
const base = fork('base', cfg);
const usdc = { polygon: TOKENS.polygon[0].address, base: TOKENS.base[0].address };
const sender = devAccount(1);
const recipient = devAccount(2).address;
const amount = parseUnits('10', 6);

const balance = (f, token, who) => f.pub.readContract({ address: token, abi: ERC20_ABI, functionName: 'balanceOf', args: [who] });
const send = async (f, write) => {
  const hash = await f.wallet(sender).writeContract(write);
  const rcpt = await receipt(f, hash);
  if (rcpt.status !== 'success') throw new Error(`${write.functionName} on ${f.name} reverted (${hash})`);
  return rcpt;
};

await polygon.anvil('anvil_setBalance', [sender.address, '0x56BC75E2D63100000']);
await polygon.anvil('anvil_dealERC20', [sender.address, usdc.polygon, `0x${(amount * 10n).toString(16)}`]);
const before = { polygon: await balance(polygon, usdc.polygon, sender.address), base: await balance(base, usdc.base, recipient) };

// The TokenMessengerV2 deployed at the pinned block predates Circle's `minFee` (the call reverts):
// no minimum, so a standard transfer carries a max fee of 0. A later implementation answers.
const maxFee = await polygon.pub
  .readContract({ address: CCTP.tokenMessenger, abi: MESSENGER_ABI, functionName: 'getMinFeeAmount', args: [amount] })
  .catch(() => 0n);
await send(polygon, { address: usdc.polygon, abi: ERC20_ABI, functionName: 'approve', args: [CCTP.tokenMessenger, amount] });
const burn = await send(polygon, {
  address: CCTP.tokenMessenger,
  abi: MESSENGER_ABI,
  functionName: 'depositForBurn',
  args: [amount, base.domain, toBytes32(recipient), usdc.polygon, zeroHash, maxFee, FINALIZED],
});
const burned = before.polygon - (await balance(polygon, usdc.polygon, sender.address));
log(`burned ${formatUnits(burned, 6)} USDC on Polygon in ${burn.transactionHash} (maxFee ${maxFee}); waiting for the mint on Base`);

const deadline = Date.now() + TIMEOUT_MS;
let minted = 0n;
while (Date.now() < deadline) {
  minted = (await balance(base, usdc.base, recipient)) - before.base;
  if (minted >= amount) break;
  await new Promise((r) => setTimeout(r, 1000));
}

const ok = burned === amount && minted === amount;
const md = [
  '### Local CCTP: Polygon fork to Base fork',
  '',
  `Burned **${formatUnits(burned, 6)} USDC** on Polygon (\`${burn.transactionHash}\`); minted **${formatUnits(minted, 6)} USDC** to \`${recipient}\` on Base, delivered by the local relay with the test attester at threshold 1.`,
  '',
  ok ? 'The burn was minted in full.' : `**Mismatch: burned ${burned}, minted ${minted}, want ${amount} each.**`,
  '',
].join('\n');
console.log(md);
if (summaryPath) writeFileSync(summaryPath, md);
if (!ok) {
  console.error(`::error::CCTP burn on Polygon: burned ${burned}, minted on Base ${minted}, want ${amount}`);
  process.exit(1);
}
