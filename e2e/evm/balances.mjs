#!/usr/bin/env node
// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// F-013 (T-013-03): the three forks are up at their pinned blocks, and USDC, EURC and BTCB are the
// real contracts there, with balances readable. Each token is checked by symbol and decimals; the
// test account is given a known balance (anvil_dealERC20) and that balance is read back.
//
//   node balances.mjs [--summary out.md]

import { writeFileSync } from 'node:fs';
import { formatUnits, parseUnits } from 'viem';
import { ERC20_ABI, TOKENS, config, devAccount, fork, log } from './lib.mjs';

const summaryPath = process.argv.includes('--summary') ? process.argv[process.argv.indexOf('--summary') + 1] : null;
const cfg = config();
const holder = devAccount(0).address;
const failures = [];
const rows = [];

for (const name of Object.keys(TOKENS)) {
  const f = fork(name, cfg);
  const info = await f.anvil('anvil_nodeInfo', []);
  const forkBlock = BigInt(info.forkConfig?.forkBlockNumber ?? -1);
  if (forkBlock !== f.block) failures.push(`${f.name}: forked at block ${forkBlock}, want ${f.block}`);
  const chainId = await f.pub.getChainId();
  if (chainId !== f.chain.id) failures.push(`${f.name}: chain id ${chainId}, want ${f.chain.id}`);

  for (const t of TOKENS[name]) {
    const read = (functionName, args = []) => f.pub.readContract({ address: t.address, abi: ERC20_ABI, functionName, args });
    const [symbol, decimals, supply] = await Promise.all([read('symbol'), read('decimals'), read('totalSupply')]);
    if (symbol !== t.symbol) failures.push(`${f.name} ${t.address}: symbol ${symbol}, want ${t.symbol}`);
    if (decimals !== t.decimals) failures.push(`${f.name} ${t.symbol}: decimals ${decimals}, want ${t.decimals}`);
    const amount = parseUnits('1000', t.decimals);
    await f.anvil('anvil_dealERC20', [holder, t.address, `0x${amount.toString(16)}`]);
    const balance = await read('balanceOf', [holder]);
    if (balance !== amount) failures.push(`${f.name} ${t.symbol}: balance ${balance} after dealing ${amount}`);
    log(`${f.name} @${forkBlock}: ${symbol}/${decimals}, supply ${formatUnits(supply, decimals)}, test account ${formatUnits(balance, decimals)}`);
    rows.push(`| ${f.name} | ${forkBlock} | ${symbol} | \`${t.address}\` | ${decimals} | ${formatUnits(supply, decimals)} | ${formatUnits(balance, decimals)} |`);
  }
}

const md = [
  '### Origin-network forks',
  '',
  '| Network | Fork block | Token | Address | Decimals | Total supply | Test account |',
  '|---|---|---|---|---|---|---|',
  ...rows,
  '',
  failures.length ? failures.map((f) => `- **${f}**`).join('\n') : 'All three forks at their pinned blocks; every token readable.',
  '',
].join('\n');
console.log(md);
if (summaryPath) writeFileSync(summaryPath, md);
if (failures.length) {
  for (const f of failures) console.error(`::error::${f}`);
  process.exit(1);
}
