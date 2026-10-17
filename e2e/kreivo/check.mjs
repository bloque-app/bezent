#!/usr/bin/env node
// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// F-013 (T-013-02): read each asset's metadata and roles from the running chain, at the finalized
// head, and compare them with the ruled ids, decimals and issuer (plan DEC-7, REQ-HN-10).
//
//   KREIVO_WS=ws://127.0.0.1:9944 node check.mjs [--summary out.md]
//
// Independent of the genesis override on purpose: the table below is the ruling, written again,
// so a mistake in e2e/network/src/lib.rs fails here rather than passing through both.

import { writeFileSync } from 'node:fs';
import { COMMUNITY_ID, accountHex, communityAccount, assertTestBed, connect, devAccount, env, log, text } from './lib.mjs';

const EXPECTED_SPEC = { name: 'kreivo-parachain', version: Number(env('KREIVO_SPEC_VERSION', '134')) };
const COMMUNITY = 'community';
const ALICE = 'alice';
const EXPECTED = [
  { id: 100, name: 'COPM/2', symbol: 'COPM', decimals: 2, sufficient: true, roles: ALICE },
  { id: 101, name: 'Bloque USDC', symbol: 'bUSDC', decimals: 6, sufficient: false, roles: COMMUNITY },
  { id: 102, name: 'Bloque EURC', symbol: 'bEURC', decimals: 6, sufficient: false, roles: COMMUNITY },
  { id: 103, name: 'Bloque BTC', symbol: 'bBTC', decimals: 8, sufficient: false, roles: COMMUNITY },
  { id: 104, name: 'MXNB/2', symbol: 'MXNB', decimals: 2, sufficient: false, roles: ALICE },
  { id: 105, name: 'BRLB/2', symbol: 'BRLB', decimals: 2, sufficient: false, roles: ALICE },
];

const summaryPath = process.argv.includes('--summary') ? process.argv[process.argv.indexOf('--summary') + 1] : null;
const failures = [];
const rows = [];
const expect = (label, got, want) => {
  if (got !== want) failures.push(`${label}: got ${JSON.stringify(got)}, want ${JSON.stringify(want)}`);
};

const { client, api, url } = connect();
try {
  await assertTestBed(client);
  const head = await client.getFinalizedBlock();
  const at = { at: head.hash };
  log(`connected to ${url}; finalized block ${head.number}`);

  const version = await api.constants.System.Version();
  expect('runtime spec_name', version.spec_name, EXPECTED_SPEC.name);
  expect('runtime spec_version', version.spec_version, EXPECTED_SPEC.version);

  const community = communityAccount(COMMUNITY_ID);
  const alice = devAccount('//Alice').address;
  const who = (a) => (accountHex(a) === accountHex(community) ? `community ${COMMUNITY_ID}` : accountHex(a) === accountHex(alice) ? '//Alice' : a);
  const info = await api.query.Communities.Info.getValue(COMMUNITY_ID, at);
  if (!info) failures.push(`community ${COMMUNITY_ID} is not registered`);

  for (const want of EXPECTED) {
    const key = { type: 'Here', value: want.id };
    const [asset, meta] = await Promise.all([
      api.query.Assets.Asset.getValue(key, at),
      api.query.Assets.Metadata.getValue(key, at),
    ]);
    const label = `Here(${want.id})`;
    if (!asset) {
      failures.push(`${label} does not exist`);
      continue;
    }
    const holder = want.roles === COMMUNITY ? community : alice;
    expect(`${label} name`, text(meta?.name), want.name);
    expect(`${label} symbol`, text(meta?.symbol), want.symbol);
    expect(`${label} decimals`, meta?.decimals, want.decimals);
    expect(`${label} is_sufficient`, asset.is_sufficient, want.sufficient);
    expect(`${label} status`, asset.status?.type, 'Live');
    for (const role of ['issuer', 'admin', 'owner']) expect(`${label} ${role}`, accountHex(asset[role]), accountHex(holder));
    rows.push(
      `| \`Here(${want.id})\` | ${text(meta?.symbol)} | ${text(meta?.name)} | ${meta?.decimals} | ${asset.is_sufficient} | ${asset.min_balance} | ${asset.supply} | ${who(asset.issuer)} |`,
    );
  }

  const md = [
    `### Kreivo assets (finalized block ${head.number}, ${version.spec_name} ${version.spec_version})`,
    '',
    `Community ${COMMUNITY_ID}: ${info ? 'registered' : '**missing**'}, account \`${community}\`.`,
    '',
    '| Id | Symbol | Name | Decimals | Sufficient | Min | Supply | Issuer and Admin |',
    '|---|---|---|---|---|---|---|---|',
    ...rows,
    '',
    failures.length ? `**${failures.length} mismatch(es):**\n\n${failures.map((f) => `- ${f}`).join('\n')}` : 'All six assets match the ruling.',
    '',
  ].join('\n');
  console.log(md);
  if (summaryPath) writeFileSync(summaryPath, md);
} finally {
  client.destroy();
}
if (failures.length) {
  for (const f of failures) console.error(`::error::${f}`);
  process.exit(1);
}
