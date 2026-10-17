#!/usr/bin/env node
// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// F-013 (T-013-05; plan DEC-6, REQ-HN-10): deploy the bezent contract on the test bed as on mainnet,
// through Kreivo's ContractsStore, by the dedicated community (4243), and confirm it holds its mint
// authority. Run after setup.mjs (which registers and funds the community).
//
//   KREIVO_WS=ws://127.0.0.1:9944 CONTRACT_DIR=<dir with bezent_contract.{wasm,json}> node deploy.mjs [--summary out.md]
//
// Steps, as aledgent deploys its settlement contract (`contracts/settlement/scripts/drive.mjs`):
//
//   1. preflight: the community exists and its account is Issuer and Admin of Here(101)–Here(103),
//      as genesis puts it (and the Root referendum on mainnet, DEC-7); the code fits MaxCodeLen.
//   2. `ContractsStore.publish(code)`, `request_license(app)`, `instantiate(app, license, 0, new(...))`,
//      each with the community's origin. On the bed that origin is its first admin, //Alice
//      (`EnsureCommunity` maps the admin's signed origin to the community); on mainnet it is the
//      community's referendum. The community account publishes and owns the instance, so the
//      contract is the community's: Kreivo's assets API lets it mint and burn what the community
//      issues and administers (kreivo#512).
//   3. `new(roles, raise_delay, assets)`: three distinct test role accounts; 101–103 with small test
//      bounds, one backend each, and the supply already issued of each (read from the chain).
//   4. Read the instance back: its community, the constructor's events in order, roles, bounds,
//      backends and supply; and dry-run one mint of each asset by the delivery role, which must be
//      let through (the community's authority reaches the contract).

import { writeFileSync } from 'node:fs';
import { Binary } from 'polkadot-api';
import { COMMUNITY_ID, accountHex, communityAccount, assertTestBed, connect, devAccount, json, log, send } from './lib.mjs';
import {
  Contract, RAISE_DELAY, ROLE_PATHS, TEST_ASSETS, TEST_BOUNDS, assetSupply, eventsOf, here_, loadArtefacts, reference, roleAccounts, saveState,
} from './contract.mjs';

const summaryPath = process.argv.includes('--summary') ? process.argv[process.argv.indexOf('--summary') + 1] : null;
const fail = (m) => {
  throw new Error(m);
};

const { client, api, url } = connect();
try {
  await assertTestBed(client);
  log(`connected to ${url}`);
  const artefacts = loadArtefacts();
  const alice = devAccount('//Alice');
  const roles = roleAccounts();
  const community = communityAccount(COMMUNITY_ID);

  // 1. Preflight.
  if (!(await api.query.Communities.Info.getValue(COMMUNITY_ID))) fail(`community ${COMMUNITY_ID} is not registered: run setup.mjs`);
  for (const { id } of TEST_ASSETS) {
    const asset = await api.query.Assets.Asset.getValue(here_(id));
    if (!asset) fail(`Here(${id}) does not exist`);
    for (const role of ['issuer', 'admin']) {
      if (accountHex(asset[role]) !== accountHex(community)) fail(`Here(${id}) ${role} is ${asset[role]}, not community ${COMMUNITY_ID}'s account`);
    }
  }
  const maxCodeLen = await api.constants.Contracts.MaxCodeLen();
  if (artefacts.wasm.length > maxCodeLen) fail(`code is ${artefacts.wasm.length} bytes, MaxCodeLen is ${maxCodeLen}`);
  log(`preflight ok: community ${COMMUNITY_ID} (${community}) is Issuer and Admin of 101–103; code ${artefacts.wasm.length} of ${maxCodeLen} bytes`);

  // The role accounts pay their calls' fees and storage deposits.
  const need = 10_000_000_000_000n;
  const short = [];
  for (const who of [...Object.values(roles), devAccount('//Eve'), devAccount('//Ferdie')]) {
    const free = (await api.query.System.Account.getValue(who.address)).data.free;
    if (free < need) short.push(api.tx.Balances.transfer_keep_alive({ dest: { type: 'Id', value: who.address }, value: need }).decodedCall);
  }
  if (short.length) await send(api.tx.Utility.batch_all({ calls: short }), alice, `fund ${short.length} test account(s)`);

  // 2. Publish, license, instantiate, as the community.
  const published = await send(
    api.tx.ContractsStore.publish({ code: artefacts.wasm, max_instances: undefined, price: undefined }),
    alice,
    'ContractsStore.publish (community origin)',
  );
  const [app] = eventsOf(published, 'ContractsStore', 'AppPublished');
  if (!app) fail('no AppPublished event');
  if (accountHex(app.publisher) !== accountHex(community)) fail(`publisher is ${app.publisher}, not the community account`);
  const appId = app.id;

  const licensed = await send(api.tx.ContractsStore.request_license({ app_id: appId }), alice, 'ContractsStore.request_license');
  const [license] = eventsOf(licensed, 'ContractsStore', 'AppLicenseEmitted');
  if (!license) fail('no AppLicenseEmitted event');
  const licenseId = license.license_id;

  const assets = [];
  for (const { id, backend } of TEST_ASSETS) {
    // The supply issued before the contract takes the asset over (F-006 NOTES, S4). Zero on the bed.
    assets.push([here_(id), { bounds: TEST_BOUNDS, backends: [backend], initial_supply: await assetSupply(api, id) }]);
  }
  const ctorArgs = {
    roles: { delivery: roles.delivery.address, pause: roles.pause.address, governance: roles.governance.address },
    raise_delay: RAISE_DELAY,
    assets,
  };
  const instantiated = await send(
    api.tx.ContractsStore.instantiate({
      app_id: appId,
      license_id: licenseId,
      value: 0n,
      data: artefacts.ink.constructor('new').encode(ctorArgs),
      salt: Binary.fromText(`bezent-e2e-${Date.now()}`),
    }),
    alice,
    'ContractsStore.instantiate new(roles, raise_delay, assets)',
  );
  if (!eventsOf(instantiated, 'ContractsStore', 'AppInstantiated').length) fail('no AppInstantiated event');
  const address = await api.query.ContractsStore.ContractAccount.getValue([appId, licenseId]);
  if (!address) fail(`no contract account for app ${appId}, license ${licenseId}`);
  log(`contract ${address} (app ${appId}, license ${licenseId})`);

  // 3. Read it back.
  const contract = new Contract(api, artefacts, address);
  const merchant = await api.query.ContractsStore.ContractMerchantId.getValue(address);
  if (merchant !== COMMUNITY_ID) fail(`the contract belongs to community ${merchant}, not ${COMMUNITY_ID}`);

  const ctorEvents = contract.events(instantiated.events);
  const expectedTypes = ['RolesSet', ...TEST_ASSETS.flatMap(() => ['BoundsSet', 'BackendsSet', 'SupplyCounted'])];
  if (json(ctorEvents.map((e) => e.type)) !== json(expectedTypes)) fail(`constructor events ${json(ctorEvents.map((e) => e.type))}, want ${json(expectedTypes)}`);
  ctorEvents.forEach((e, i) => {
    if (BigInt(e.value.seq) !== BigInt(i)) fail(`constructor event ${i} (${e.type}) has seq ${e.value.seq}`);
  });

  const onChainRoles = await contract.read('roles');
  for (const role of Object.keys(roles)) {
    if (accountHex(onChainRoles[role]) !== accountHex(roles[role].address)) fail(`role ${role} is ${onChainRoles[role]}`);
  }
  const rows = [];
  for (const { id, symbol, backend } of TEST_ASSETS) {
    const bounds = await contract.read('bounds', { asset: here_(id) });
    if (json(bounds) !== json(TEST_BOUNDS)) fail(`Here(${id}) bounds ${json(bounds)}`);
    const backends = await contract.read('backends', { asset: here_(id) });
    if (json(backends) !== json([backend])) fail(`Here(${id}) backends ${json(backends)}`);
    const supply = await contract.read('supply', { asset: here_(id) });
    // The community's authority reaches the contract: a mint by the delivery role is let through.
    const dry = await contract.dryRun(roles.delivery.address, 'mint', { asset: here_(id), to: devAccount('//Eve').address, amount: 1n, reference: reference(`deploy-${id}`) });
    if (!dry.ok) fail(`a dry-run mint of Here(${id}) is refused: ${json(dry.error)}`);
    rows.push(`| \`Here(${id})\` | ${symbol} | ${backend.type} ${backend.value.chain_id} | ${supply} | let through |`);
  }

  const state = { address, appId, licenseId, community: { id: COMMUNITY_ID, account: community }, roles: ctorArgs.roles, raiseDelay: RAISE_DELAY, bounds: TEST_BOUNDS };
  saveState(state);

  const md = [
    `### bezent contract on the test bed (T-013-05)`,
    '',
    `Published, licensed and instantiated through ContractsStore by community ${COMMUNITY_ID} (\`${community}\`): app ${appId}, licence ${licenseId}, contract \`${address}\`, code ${artefacts.wasm.length} bytes.`,
    `Roles: delivery ${ROLE_PATHS.delivery}, pause ${ROLE_PATHS.pause}, governance ${ROLE_PATHS.governance} (a test account standing in for the community's). Raise delay ${RAISE_DELAY} blocks.`,
    `Bounds per asset: ceiling ${TEST_BOUNDS.supply_ceiling}, window ${TEST_BOUNDS.window_amount} per ${TEST_BOUNDS.window_blocks} blocks, per transfer ${TEST_BOUNDS.per_transfer_max}, redemption minimum ${TEST_BOUNDS.redeem_minimum}.`,
    '',
    '| Asset | Symbol | Backend | Supply counted | Dry-run mint by delivery |',
    '|---|---|---|---|---|',
    ...rows,
    '',
  ].join('\n');
  console.log(md);
  if (summaryPath) writeFileSync(summaryPath, md);
} finally {
  client.destroy();
}
