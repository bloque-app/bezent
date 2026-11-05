#!/usr/bin/env node
// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// F-013 (issue #89; AC-I2.2, REQ-GV-2, REQ-HN-9, REQ-HN-10): on Kreivo, "more than one approver" is the property
// of the community that holds the contract's governance role, not of the contract. The bed's other checks act as
// governance with one test key (//Dave). This one hands the role to the dedicated community (4243) and shows,
// on the running chain, with nothing but what the runtime does:
//
//   - the community's account holds the governance role, and the key that held it, the pause key and the
//     community's first admin cannot act as it;
//   - one member, alone, cannot unpause: not by calling the contract, not through the community's account,
//     and not with a referendum he alone votes for;
//   - a referendum that reaches the community's support (3 of its 5 members) unpauses, through the runtime's
//     own path (Referenda, Scheduler, `Communities.dispatch_as_account`, Contracts).
//
// The two acts REQ-GV-2 reserves for several approvers, unpause and a raised bound, ride one proposal: a
// referendum does both or neither.
//
// Run after deploy.mjs (which leaves the contract with //Dave as governance and the community's first admin,
// //Alice, as its admin origin):
//
//   KREIVO_WS=ws://127.0.0.1:9944 CONTRACT_DIR=... CONTRACT_STATE=... node ac-i2.2.community-vote.mjs [--summary out.md]
//
// What is set up here, and why it is not sudo: the memberships a community draws its members from are created
// by Root or a Kreivo governance origin only, and the bed has neither, so genesis stocks them
// (e2e/network/src/lib.rs); the community buys what it needs from that stock with its own funds, adds five
// members, and hands its admin origin from its first admin's key to its own origin (`set_admin_origin`), which
// from then on only a referendum of its members reaches. The voting policy is the track the community is
// registered with (setup.mjs, `COMMUNITY_TRACK`).
//
// Mutation knobs, for showing that the check fails when one member is enough (never set in CI; each prints a
// warning): COMMUNITY_MEMBERS (members enrolled, default 5), COMMUNITY_KEEP_ADMIN_KEY=1 (leave the first
// admin's key as the community's admin origin) and, to setup.mjs, COMMUNITY_SUPPORT_PERCENT (default 50).

import { writeFileSync } from 'node:fs';
import { blake2b256 } from '@polkadot-labs/hdkd-helpers';
import { COMMUNITY_ID, COMMUNITY_SUPPORT_PERCENT, accountHex, assertTestBed, communityAccount, connect, devAccount, env, json, log, send, submit } from './lib.mjs';
import { Contract, TEST_ASSETS, TEST_BOUNDS, eventsOf, here_, loadArtefacts, loadState, roleAccounts } from './contract.mjs';

const summaryPath = process.argv.includes('--summary') ? process.argv[process.argv.indexOf('--summary') + 1] : null;
const MEMBERS = Number(env('COMMUNITY_MEMBERS', '5'));
const KEEP_ADMIN_KEY = env('COMMUNITY_KEEP_ADMIN_KEY', '') === '1';
const mutated = MEMBERS !== 5 || KEEP_ADMIN_KEY || COMMUNITY_SUPPORT_PERCENT !== 50;
const results = [];

class CheckFailed extends Error {}
const expect = (cond, message) => {
  if (!cond) throw new CheckFailed(message);
};
const eq = (label, got, want) => expect(json(got) === json(want), `${label}: got ${json(got)}, want ${json(want)}`);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function check(id, what, body) {
  log(`--- ${id}: ${what}`);
  try {
    const detail = await body();
    results.push({ id, what, pass: true, detail: detail ?? '' });
    log(`PASS ${id}: ${detail ?? ''}`);
  } catch (e) {
    results.push({ id, what, pass: false, detail: e.message });
    console.error(`::error::${id} (${what}): ${e.message}`);
    if (!(e instanceof CheckFailed)) console.error(e.stack);
  }
}

const { client, api, url } = connect();
try {
  await assertTestBed(client);
  log(`connected to ${url}`);
  if (mutated) log(`WARNING: mutation run (members ${MEMBERS}, support ${COMMUNITY_SUPPORT_PERCENT}%, keep admin key ${KEEP_ADMIN_KEY}); the check is expected to FAIL`);
  const artefacts = loadArtefacts();
  const state = loadState();
  const contract = new Contract(api, artefacts, state.address);
  const roles = roleAccounts();
  const alice = devAccount('//Alice'); // the community's first admin
  const community = communityAccount(COMMUNITY_ID);
  const voters = Array.from({ length: MEMBERS }, (_, i) => devAccount(`//Voter${i + 1}`));
  const [lone, ...others] = voters;
  const outsider = devAccount('//Ferdie');
  const scope = { type: 'AssetDirection', value: [here_(101), { type: 'Mint' }] };
  const communityOrigin = { type: 'Communities', value: { community_id: COMMUNITY_ID, subset: undefined } };
  const membershipOf = new Map(); // account (hex) -> membership id
  const finalized = async () => (await client.getFinalizedBlock()).number;
  const until = async (label, cond, ms = 600_000) => {
    const end = Date.now() + ms;
    for (;;) {
      const v = await cond();
      if (v) return v;
      if (Date.now() > end) throw new CheckFailed(`timed out waiting for ${label}`);
      await sleep(3_000);
    }
  };

  /// `Contracts.call` of `message` by `from`, sized by a dry-run as `from`.
  const contractsCall = async (from, message, args) => {
    const dry = await contract.dryRun(from, message, args);
    expect(dry.gas, `${message}: no gas estimate`);
    const margin = (w) => (w * 13n) / 10n + 1_000n;
    return api.tx.Contracts.call({
      dest: { type: 'Id', value: state.address },
      value: 0n,
      gas_limit: { ref_time: margin(dry.gas.ref_time), proof_size: margin(dry.gas.proof_size) },
      storage_deposit_limit: undefined,
      data: dry.data,
    });
  };
  /// The call that runs as the community's account: what a passed referendum enacts.
  const asCommunity = (tx) => api.tx.Communities.dispatch_as_account({ call: tx.decodedCall });

  /// The two acts REQ-GV-2 reserves for several approvers: the unpause, and a raised bound (scheduled, applied
  /// after the raise delay). One proposal carries both, so a referendum is shown to do both or neither.
  const asset = here_(101);
  const raised = { ...TEST_BOUNDS, supply_ceiling: TEST_BOUNDS.supply_ceiling * 2n };
  const raiseConfig = { bounds: raised, backends: [TEST_ASSETS[0].backend], initial_supply: 0n };
  const governed = async () =>
    asCommunity(
      api.tx.Utility.batch_all({
        calls: [(await contractsCall(community, 'unpause', { scope })).decodedCall, (await contractsCall(community, 'schedule_raise', { asset, config: raiseConfig })).decodedCall],
      }),
    );

  /// Open a referendum of the community for `call`, put the decision deposit, and return its index.
  const propose = async (proposer, call, label) => {
    const index = await api.query.CommunityReferenda.ReferendumCount.getValue();
    // Up to 128 bytes a proposal rides inline; a longer one is noted as a preimage first.
    const bytes = await call.getEncodedData();
    let proposal = { type: 'Inline', value: bytes };
    if (bytes.length > 128) {
      // The same proposal may have been noted before (a referendum that failed, proposed again).
      const noted = await submit(api.tx.Preimage.note_preimage({ bytes }), proposer, `${label}: note the preimage`);
      if (!noted.ok && json(noted.dispatchError?.value?.value?.type) !== '"AlreadyNoted"') throw new Error(`${label}: note the preimage: ${json(noted.dispatchError)}`);
      proposal = { type: 'Lookup', value: { hash: `0x${Buffer.from(blake2b256(bytes)).toString('hex')}`, len: bytes.length } };
    }
    await send(
      api.tx.CommunityReferenda.submit({ proposal_origin: communityOrigin, proposal, enactment_moment: { type: 'After', value: 1 } }),
      proposer,
      `${label}: referendum ${index} submitted`,
    );
    await send(api.tx.CommunityReferenda.place_decision_deposit({ index }), proposer, `${label}: decision deposit`);
    return index;
  };
  const aye = (who, index, label) =>
    send(
      api.tx.Communities.vote({ membership_id: membershipOf.get(accountHex(who.address)), poll_index: index, vote: { type: 'Standard', value: true } }),
      who,
      `${label}: aye by ${who.address.slice(0, 8)}…`,
    );
  const referendum = async (index) => (await api.query.CommunityReferenda.ReferendumInfoFor.getValue(index))?.type;
  /// Wait for a referendum to leave `Ongoing`; return how it ended.
  const settled = (index) => until(`referendum ${index} to end`, async () => {
    const t = await referendum(index);
    return t && t !== 'Ongoing' ? t : null;
  });

  /// A direct call to the contract that the contract must refuse with `error`.
  const refuseDirect = async (who, message, args, error, label) => {
    const { dry, res, events } = await contract.submit(who, message, args, label);
    expect(!dry.ok, `${label}: the dry-run let it through`);
    eq(`${label}: the contract's error`, dry.error?.type, error);
    expect(!res.ok, `${label}: the extrinsic dispatched`);
    eq(`${label}: the pallet error`, res.dispatchError.value?.value?.type, 'ContractReverted');
    eq(`${label}: contract events`, events.length, 0);
    return error;
  };
  /// A call through the community's account that the runtime must refuse as `BadOrigin`.
  const refuseAsCommunity = async (who, label) => {
    const tx = asCommunity(await contractsCall(community, 'unpause', { scope }));
    const res = await submit(tx, who, label);
    expect(!res.ok, `${label}: the runtime let ${who.address} act as the community`);
    eq(`${label}: the dispatch error`, res.dispatchError.type, 'BadOrigin');
    return 'BadOrigin';
  };

  await check('AC-I2.2 / REQ-HN-10', "the community's account holds the contract's governance role, through a vote of its members", async () => {
    // Members: the community buys memberships from the genesis stock with its own funds, then enrols the voters.
    const need = 10_000_000_000_000n;
    const fund = [...voters, outsider].map((v) => api.tx.Balances.transfer_keep_alive({ dest: { type: 'Id', value: v.address }, value: need }).decodedCall);
    await send(api.tx.Utility.batch_all({ calls: fund }), alice, `fund ${fund.length} accounts`);
    const buys = voters.map((_, i) => asCommunity(api.tx.CommunityMemberships.buy_item({ collection: 0, item: i + 1, bid_price: 0n })).decodedCall);
    await send(api.tx.Utility.batch_all({ calls: buys }), alice, `the community buys ${buys.length} memberships`);
    const enrolled = await send(
      api.tx.Utility.batch_all({ calls: voters.map((v) => api.tx.Communities.add_member({ who: { type: 'Id', value: v.address } }).decodedCall) }),
      alice,
      `enrol ${voters.length} members`,
    );
    eventsOf(enrolled, 'Communities', 'MemberAdded').forEach((e) => membershipOf.set(accountHex(e.who), e.membership_id));
    eq('members enrolled', membershipOf.size, MEMBERS);
    // From here only the community's own origin is its admin: a referendum of its members.
    if (!KEEP_ADMIN_KEY) {
      await send(api.tx.Communities.set_admin_origin({ admin_origin: communityOrigin }), alice, "the community's admin origin is its own");
    }
    // Hand the contract's governance role over: scheduled by the present holder, accepted by the community.
    await send(await contractsCall(roles.governance.address, 'schedule_governance', { governance: community }), roles.governance, 'schedule governance → the community');
    const scheduled = await contract.read('scheduled_governance');
    expect(scheduled && accountHex(scheduled.governance) === accountHex(community), `scheduled governance is ${json(scheduled)}`);
    await until(`block ${scheduled.due} (the raise delay)`, async () => (await finalized()) > Number(scheduled.due) + 1);
    const accept = await contractsCall(community, 'accept_governance', {});
    const index = await propose(lone, asCommunity(accept), 'accept governance');
    await Promise.all(voters.slice(0, Math.ceil((MEMBERS * COMMUNITY_SUPPORT_PERCENT) / 100)).map((v) => aye(v, index, 'accept governance')));
    eq('the referendum', await settled(index), 'Approved');
    const now = await until('the contract to show the new governance', async () => {
      const r = await contract.read('roles');
      return accountHex(r.governance) === accountHex(community) ? r : null;
    });
    expect(accountHex(now.governance) !== accountHex(now.delivery) && accountHex(now.governance) !== accountHex(now.pause), 'governance is delivery or pause');
    return `governance is community ${COMMUNITY_ID}'s account ${community} after referendum ${index}; ${MEMBERS} members, admin origin ${KEEP_ADMIN_KEY ? "left on //Alice's key (mutation)" : "the community's own"}`;
  });

  await check('AC-I2.2', 'one actor alone cannot unpause or raise a bound: not the pause key, not the old governance key, not a member, not the first admin', async () => {
    await contract.submit(roles.pause, 'pause', { scope }, 'the pause role pauses bUSDC mints');
    eq('paused', await contract.read('paused', { scope }), true);
    const seen = [];
    seen.push(`pause key: ${await refuseDirect(roles.pause, 'unpause', { scope }, 'Unauthorised', 'the pause role unpauses')}`);
    seen.push(`old governance key: ${await refuseDirect(roles.governance, 'unpause', { scope }, 'Unauthorised', 'the old governance key unpauses')}`);
    seen.push(`a member: ${await refuseDirect(lone, 'unpause', { scope }, 'Unauthorised', 'one member calls the contract')}`);
    seen.push(`a member raises a bound: ${await refuseDirect(lone, 'schedule_raise', { asset, config: raiseConfig }, 'Unauthorised', 'one member schedules a raised bound')}`);
    seen.push(`the pause key raises a bound: ${await refuseDirect(roles.pause, 'schedule_raise', { asset, config: raiseConfig }, 'Unauthorised', 'the pause role schedules a raised bound')}`);
    seen.push(`a member through the community's account: ${await refuseAsCommunity(lone, 'one member dispatches as the community')}`);
    seen.push(`the first admin through the community's account: ${await refuseAsCommunity(alice, "the community's first admin dispatches as the community")}`);
    eq('still paused', await contract.read('paused', { scope }), true);
    expect(!(await contract.read('scheduled_raise', { asset })), 'a raise is scheduled');
    return seen.join('; ');
  });

  await check('AC-I2.2', 'a referendum with one approver neither unpauses nor raises a bound', async () => {
    const index = await propose(lone, await governed(), 'one approver');
    await aye(lone, index, 'one approver');
    const ended = await settled(index);
    eq('the referendum with one aye ended as', ended, 'Rejected');
    eq('still paused', await contract.read('paused', { scope }), true);
    expect(!(await contract.read('scheduled_raise', { asset })), 'a raise is scheduled');
    return `referendum ${index}: 1 aye of ${MEMBERS} members, ${ended}; the contract stayed paused and no raise was scheduled`;
  });

  await check('AC-I2.2 / REQ-GV-2', "a referendum that reaches the community's support unpauses and schedules a raised bound", async () => {
    const approvers = Math.ceil((MEMBERS * COMMUNITY_SUPPORT_PERCENT) / 100);
    expect(approvers > 1, `the community's policy needs ${approvers} approver(s): one is enough`);
    const seq = await contract.read('event_seq');
    const index = await propose(lone, await governed(), 'quorum');
    await Promise.all(voters.slice(0, approvers).map((v) => aye(v, index, 'quorum')));
    eq('the referendum', await settled(index), 'Approved');
    await until('the contract to be unpaused', async () => !(await contract.read('paused', { scope })));
    eq('paused', await contract.read('paused', { scope }), false);
    const raise = await until('the raise to be scheduled', async () => contract.read('scheduled_raise', { asset }));
    eq('the raised ceiling', raise.config.bounds.supply_ceiling, raised.supply_ceiling);
    eq("bounds in force (a raise waits for its delay)", (await contract.read('bounds', { asset })).supply_ceiling, TEST_BOUNDS.supply_ceiling);
    eq('event_seq (Unpaused, RaiseScheduled)', await contract.read('event_seq'), seq + 2n);
    return `referendum ${index}: ${approvers} ayes of ${MEMBERS} members, Approved; the contract is unpaused (Unpaused, seq ${seq}) and bUSDC's ceiling ${TEST_BOUNDS.supply_ceiling} → ${raised.supply_ceiling} is scheduled (RaiseScheduled, seq ${seq + 1n}, due block ${raise.due})`;
  });

  await check('AC-I2.2', "the policy the chain holds needs more than one approver", async () => {
    const track = await api.query.CommunityTracks.Tracks.getValue(COMMUNITY_ID);
    expect(track, `no track for community ${COMMUNITY_ID}`);
    const floor = Number(track.min_support.value.floor);
    const ceil = Number(track.min_support.value.ceil);
    const needed = Math.ceil((MEMBERS * floor) / 1_000_000_000);
    expect(floor === ceil, 'the support requirement changes over time: a lone vote could pass by waiting');
    expect(needed > 1, `the support requirement (${floor / 10_000_000}% of ${MEMBERS} members) needs ${needed} approver(s)`);
    return `track support ${floor / 10_000_000}% of ${MEMBERS} members → ${needed} approvers`;
  });
} catch (e) {
  results.push({ id: 'setup', what: 'connect and load the deployment', pass: false, detail: e.message });
  console.error(e.stack);
} finally {
  client.destroy();
}

const failed = results.filter((r) => !r.pass);
const md = [
  '### AC-I2.2 on Kreivo: unpause through a community vote (issue #89)',
  '',
  ...(mutated ? ['**Mutation run: this is expected to fail.**', ''] : []),
  '| Id | Check | Result | On the chain |',
  '|---|---|---|---|',
  ...results.map((r) => `| ${r.id} | ${r.what} | ${r.pass ? 'pass' : '**FAIL**'} | ${r.detail.replaceAll('|', '\\|')} |`),
  '',
  failed.length ? `**${failed.length} of ${results.length} failed.**` : `All ${results.length} checks pass.`,
  '',
].join('\n');
console.log(md);
if (summaryPath) writeFileSync(summaryPath, md);
process.exit(failed.length ? 1 : 0);
