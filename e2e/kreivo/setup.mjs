#!/usr/bin/env node
// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// F-013 (T-013-02): register the dedicated community on the test bed, as aledgent registers its own
// (`contracts/settlement/scripts/drive.mjs`, step `community`), and fund its account, which pays
// for publishing and instantiating through ContractsStore (T-013-05).
//
//   KREIVO_WS=ws://127.0.0.1:9944 node setup.mjs
//
// Genesis already names the community's account as Issuer and Admin of Here(101) to Here(103)
// (e2e/network/src/lib.rs); the account is derived from the id, so it holds those roles before the
// community exists. //Alice is the community's first admin (dev chains only).

import { COMMUNITY_ID, COMMUNITY_NAME, communityAccount, assertTestBed, connect, devAccount, env, log, send } from './lib.mjs';
import { Binary } from 'polkadot-api';

const { client, api, url } = connect();
try {
  await assertTestBed(client);
  const alice = devAccount('//Alice');
  log(`connected to ${url}`);
  if (await api.query.Communities.Info.getValue(COMMUNITY_ID)) {
    log(`community ${COMMUNITY_ID} exists; using it`);
  } else {
    const tx = api.tx.CommunitiesManager.register({
      community_id: COMMUNITY_ID,
      name: Binary.fromText(COMMUNITY_NAME),
      first_admin: { type: 'Id', value: alice.address },
      maybe_decision_method: undefined,
      maybe_track_info: undefined,
    });
    await send(tx, alice, `CommunitiesManager.register(${COMMUNITY_ID}, "${COMMUNITY_NAME}")`);
  }
  const account = communityAccount(COMMUNITY_ID);
  const fund = api.tx.Balances.transfer_keep_alive({
    dest: { type: 'Id', value: account },
    value: BigInt(env('COMMUNITY_FUNDING', '100000000000000')),
  });
  await send(fund, alice, 'fund the community account');
  log(`community ${COMMUNITY_ID}, account ${account}`);
} finally {
  client.destroy();
}
