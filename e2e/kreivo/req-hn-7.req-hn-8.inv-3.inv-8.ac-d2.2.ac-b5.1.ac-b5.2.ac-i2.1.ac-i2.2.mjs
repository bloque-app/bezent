#!/usr/bin/env node
// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// F-013 (T-013-05): the bezent contract on the running test bed, deployed by deploy.mjs through
// ContractsStore. Each check names the requirement, invariant or acceptance criterion it proves, at
// the chain layer (SPEC §9), on Kreivo itself rather than the unit tests' mock of it:
//
//   REQ-HN-7  a mint by the delivery role lands at the destination, with its event (reference, seq)
//   INV-3     a mint above the per-transfer maximum, or above the mint window, is refused
//   AC-D2.2   a mint reference used before is refused
//   AC-B5.1   a redemption pulls from the redeemer, destroys only the contract's own holding
//   REQ-HN-8  (never the redeemer's), records (redeemer, reference) and emits `Redeemed`
//   AC-B5.2   the same (redeemer, reference) again is refused
//   F-006     a whole-balance redemption after `approve_transfer` leaves the redeemer at zero: the
//             `Assets::transfer_approved` path through `call_runtime`, which no unit test can dispatch
//   INV-8     one refund mint for a redemption succeeds; a second is refused
//   AC-I2.1   a pause by the pause role stops mints
//   AC-I2.2   an unpause by the pause role is refused (governance only)
//   REQ-HN-7  every event the contract emitted in the run carries the next sequence number
//
// Every refusal is shown twice: the dry-run's exact `Error`, and the submitted extrinsic failing with
// `Contracts.ContractReverted` while balances and supply stay as they were.
//
//   KREIVO_WS=ws://127.0.0.1:9944 CONTRACT_DIR=... CONTRACT_STATE=... node <this file> [--summary out.md]

import { writeFileSync } from 'node:fs';
import { accountHex, assertTestBed, connect, devAccount, json, log, send } from './lib.mjs';
import {
  Contract, TEST_BOUNDS, assetBalance, assetSupply, eventsOf, here_, loadArtefacts, loadState, reference, roleAccounts,
} from './contract.mjs';

const summaryPath = process.argv.includes('--summary') ? process.argv[process.argv.indexOf('--summary') + 1] : null;
const results = [];
const allEvents = [];

class CheckFailed extends Error {}
const expect = (cond, message) => {
  if (!cond) throw new CheckFailed(message);
};
const eq = (label, got, want) => expect(json(got) === json(want), `${label}: got ${json(got)}, want ${json(want)}`);

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
  const artefacts = loadArtefacts();
  const state = loadState();
  const contract = new Contract(api, artefacts, state.address);
  const roles = roleAccounts();
  const eve = devAccount('//Eve');
  const ferdie = devAccount('//Ferdie');
  const me = state.address;
  const B = 101; // bUSDC: mints, redemptions, refund, pause
  const W = 102; // bEURC: the mint window, apart, so the other checks keep their budget
  const asset = here_(B);
  const polygon = { type: 'Evm', value: { chain_id: 137n } };
  const payoutAddress = new Uint8Array(20).fill(0xb5);

  /// Submit a call the contract must accept; return its events.
  const accept = async (who, message, args, label) => {
    const { dry, res, events } = await contract.submit(who, message, args, label);
    expect(dry.ok, `${label}: the dry-run refused it: ${json(dry.error)}`);
    expect(res.ok, `${label}: the extrinsic failed: ${json(res.dispatchError)}`);
    allEvents.push(...events);
    return { res, events };
  };
  /// A call the contract must refuse with `error`: the dry-run's `Error` is `error`, and the
  /// submitted extrinsic reverts with no contract event.
  const refuse = async (who, message, args, error, label) => {
    const { dry, res, events } = await contract.submit(who, message, args, label);
    expect(!dry.ok, `${label}: the dry-run let it through`);
    eq(`${label}: the contract's error`, dry.error?.type, error);
    expect(!res.ok, `${label}: the extrinsic dispatched`);
    eq(`${label}: the dispatch error`, [res.dispatchError.type, res.dispatchError.value?.type], ['Module', 'Contracts']);
    eq(`${label}: the pallet error`, res.dispatchError.value?.value?.type, 'ContractReverted');
    // The chain reports the contract's own `Error` only in a dry-run: a reverted extrinsic keeps no
    // return data. So the same call is dry-run again on the state the submission left, and must give
    // the same `Error`; this also catches a state race between the first dry-run and the submit.
    const again = await contract.dryRun(who.address, message, args);
    expect(!again.ok, `${label}: dry-run after the submission let it through`);
    eq(`${label}: the contract's error after the submission`, again.error?.type, error);
    eq(`${label}: contract events`, events.length, 0);
    return error;
  };
  const snapshot = async (id, who) => ({ balance: await assetBalance(api, id, who), supply: await assetSupply(api, id), counted: await contract.read('supply', { asset: here_(id) }) });

  const m1 = reference('mint-1');
  const minted = 1_000_000n;

  await check('REQ-HN-7', 'a mint by the delivery role lands at the destination, with its event', async () => {
    const seq = await contract.read('event_seq');
    const before = await snapshot(B, eve.address);
    const { res, events } = await accept(roles.delivery, 'mint', { asset, to: eve.address, amount: minted, reference: m1 }, 'mint 1 000 000 bUSDC to //Eve');
    const after = await snapshot(B, eve.address);
    eq('destination balance', after.balance - before.balance, minted);
    eq('asset supply', after.supply - before.supply, minted);
    eq("contract's supply counter", after.counted - before.counted, minted);
    const issued = eventsOf(res, 'Assets', 'Issued');
    eq('Assets.Issued', issued.map((e) => [accountHex(e.owner), e.amount]), [[accountHex(eve.address), minted]]);
    eq('contract events', events.map((e) => e.type), ['Minted']);
    const ev = events[0].value;
    eq('Minted', [ev.seq, ev.reference, ev.asset, accountHex(ev.to), ev.amount], [seq, m1, asset, accountHex(eve.address), minted]);
    eq('event_seq after', await contract.read('event_seq'), seq + 1n);
    expect(await contract.read('mint_reference_used', { reference: m1 }), 'mint reference not recorded');
    return `//Eve +${minted}; Assets.Issued; Minted { seq ${ev.seq}, reference ${m1.slice(0, 10)}… } in block ${res.block.number}`;
  });

  await check('INV-3', 'a mint above the per-transfer maximum is refused', async () => {
    const before = await snapshot(B, eve.address);
    const err = await refuse(roles.delivery, 'mint', { asset, to: eve.address, amount: TEST_BOUNDS.per_transfer_max + 1n, reference: reference('above-max') }, 'AboveTransferMaximum', 'mint per_transfer_max + 1');
    eq('nothing moved', await snapshot(B, eve.address), before);
    return `${TEST_BOUNDS.per_transfer_max + 1n} refused: ${err}, ContractReverted`;
  });

  await check('INV-3', 'a mint above the mint window is refused', async () => {
    const first = TEST_BOUNDS.per_transfer_max; // 2 000 000 of 3 000 000
    await accept(roles.delivery, 'mint', { asset: here_(W), to: eve.address, amount: first, reference: reference('window-1') }, 'mint 2 000 000 bEURC (within the window)');
    eq('minted_in_window', await contract.read('minted_in_window', { asset: here_(W) }), first);
    const over = TEST_BOUNDS.window_amount - first + 1n; // within the per-transfer maximum, over the window
    const before = await snapshot(W, eve.address);
    const err = await refuse(roles.delivery, 'mint', { asset: here_(W), to: eve.address, amount: over, reference: reference('window-2') }, 'AboveWindow', 'mint past the window');
    eq('nothing moved', await snapshot(W, eve.address), before);
    return `${first} then ${over} in ${TEST_BOUNDS.window_blocks} blocks (window ${TEST_BOUNDS.window_amount}): ${err}`;
  });

  await check('AC-D2.2', 'a reused mint reference is refused', async () => {
    const before = await snapshot(B, eve.address);
    const err = await refuse(roles.delivery, 'mint', { asset, to: eve.address, amount: 1n, reference: m1 }, 'ReferenceUsed', 'mint with the first mint reference again');
    eq('nothing moved', await snapshot(B, eve.address), before);
    return `the first mint's reference again: ${err}`;
  });

  const r1 = reference('redeem-1');
  const redeemed = 400_000n;
  await check('AC-B5.1 / REQ-HN-8', 'a redemption pulls, destroys only the contract holding, records (redeemer, reference) and emits', async () => {
    const seq = await contract.read('event_seq');
    const before = await snapshot(B, eve.address);
    const mineBefore = await assetBalance(api, B, me);
    const { res, events } = await accept(eve, 'redeem', { asset, amount: redeemed, backend: polygon, payout: payoutAddress, reference: r1 }, 'redeem 400 000 bUSDC as //Eve');
    const after = await snapshot(B, eve.address);
    eq("redeemer's balance", before.balance - after.balance, redeemed);
    eq('asset supply', before.supply - after.supply, redeemed);
    eq("contract's supply counter", before.counted - after.counted, redeemed);
    eq("contract's own balance, before and after", await assetBalance(api, B, me), mineBefore);
    const pulled = eventsOf(res, 'Assets', 'Transferred');
    eq('Assets.Transferred (the pull)', pulled.map((e) => [accountHex(e.from), accountHex(e.to), e.amount]), [[accountHex(eve.address), accountHex(me), redeemed]]);
    const burnt = eventsOf(res, 'Assets', 'Burned');
    eq('Assets.Burned (only from the contract)', burnt.map((e) => [accountHex(e.owner), e.balance]), [[accountHex(me), redeemed]]);
    const record = await contract.read('redemption', { redeemer: eve.address, reference: r1 });
    eq('redemption record', [accountHex(record?.redeemer), record?.asset, record?.amount, record?.backend, record?.payout, record?.refunded], [accountHex(eve.address), asset, redeemed, polygon, { type: 'Address', value: `0x${'b5'.repeat(20)}` }, false]);
    eq('contract events', events.map((e) => e.type), ['Redeemed']);
    const ev = events[0].value;
    eq('Redeemed', [ev.seq, ev.reference, accountHex(ev.redeemer), ev.asset, ev.amount, ev.backend, ev.payout], [seq, r1, accountHex(eve.address), asset, redeemed, polygon, record.payout]);
    return `//Eve −${redeemed}; Transferred //Eve → contract; Burned from the contract only; supply −${redeemed}; Redeemed { seq ${ev.seq} }`;
  });

  await check('AC-B5.2', 'the same (redeemer, reference) again is refused', async () => {
    const before = await snapshot(B, eve.address);
    const err = await refuse(eve, 'redeem', { asset, amount: redeemed, backend: polygon, payout: payoutAddress, reference: r1 }, 'ReferenceUsed', 'redeem again with (//Eve, the same reference)');
    eq('nothing moved', await snapshot(B, eve.address), before);
    return `(//Eve, ${r1.slice(0, 10)}…) again: ${err}`;
  });

  await check('F-006 transfer_approved', 'a whole-balance redemption after approve_transfer leaves the redeemer at zero', async () => {
    const whole = 500_000n;
    await accept(roles.delivery, 'mint', { asset, to: ferdie.address, amount: whole, reference: reference('mint-ferdie') }, 'mint 500 000 bUSDC to //Ferdie');
    eq("//Ferdie's balance", await assetBalance(api, B, ferdie.address), whole);
    await send(api.tx.Assets.approve_transfer({ id: asset, delegate: { type: 'Id', value: me }, amount: whole }), ferdie, '//Ferdie approves the contract for his whole balance');
    const supplyBefore = await assetSupply(api, B);
    const ref = reference('redeem-whole');
    const { dry, res, events } = await contract.submit(ferdie, 'redeem', { asset, amount: whole, backend: polygon, payout: payoutAddress, reference: ref }, 'redeem //Ferdie\'s whole balance');
    // Report exactly what Kreivo did before judging it.
    const kreivo = (res.events ?? []).filter((e) => ['Assets', 'Contracts', 'System'].includes(e.type)).map((e) => `${e.type}.${e.value.type}`);
    log(`whole-balance redemption: dry-run ${json(dry.ok ? 'ok' : dry.error)}; extrinsic ${res.ok ? 'ok' : json(res.dispatchError)}; events ${kreivo.join(', ')}`);
    if (!dry.ok || !res.ok) {
      const debug = dry.raw?.debug_message?.length ? ` debug: ${Buffer.from(dry.raw.debug_message).toString('utf8').slice(0, 300)}` : '';
      throw new CheckFailed(`Kreivo refused the whole-balance redemption: dry-run ${json(dry.ok ? 'ok' : dry.error)}, extrinsic ${res.ok ? 'ok' : json(res.dispatchError)}; events ${kreivo.join(', ')}.${debug}`);
    }
    allEvents.push(...events);
    eq("//Ferdie's balance", await assetBalance(api, B, ferdie.address), 0n);
    eq("//Ferdie's asset account", await api.query.Assets.Account.getValue(asset, ferdie.address), undefined);
    eq('approval left', await api.query.Assets.Approvals.getValue(asset, ferdie.address, me), undefined);
    // pallet-assets declares `TransferredApproved` but `do_transfer_approved` never deposits it
    // (first run, 2026-10-13): the pull shows as `Transferred`. That it took the approved path, not
    // `assets__deposit`, shows in the approval being used up, and in the whole balance moving at all
    // (`assets__deposit` keeps the caller alive).
    const pulled = eventsOf(res, 'Assets', 'Transferred');
    eq('Assets.Transferred (the approved pull)', pulled.map((e) => [accountHex(e.from), accountHex(e.to), e.amount]), [[accountHex(ferdie.address), accountHex(me), whole]]);
    eq('Assets.Burned', eventsOf(res, 'Assets', 'Burned').map((e) => [accountHex(e.owner), e.balance]), [[accountHex(me), whole]]);
    eq('asset supply', supplyBefore - (await assetSupply(api, B)), whole);
    eq('contract events', events.map((e) => e.type), ['Redeemed']);
    return `approve_transfer, then redeem ${whole}: Transferred //Ferdie → contract out of the approval (used up), Burned from the contract; //Ferdie at 0, asset account reaped`;
  });

  await check('INV-8', 'one refund mint for a redemption succeeds; a second is refused', async () => {
    const before = await snapshot(B, eve.address);
    const { res, events } = await accept(roles.delivery, 'refund_mint', { redeemer: eve.address, redemption_reference: r1 }, "refund //Eve's redemption");
    const after = await snapshot(B, eve.address);
    eq('redeemer refunded', after.balance - before.balance, redeemed);
    eq('asset supply', after.supply - before.supply, redeemed);
    eq("contract's supply counter", after.counted - before.counted, redeemed);
    eq('Assets.Issued', eventsOf(res, 'Assets', 'Issued').map((e) => [accountHex(e.owner), e.amount]), [[accountHex(eve.address), redeemed]]);
    eq('contract events', events.map((e) => e.type), ['RefundMinted']);
    const ev = events[0].value;
    eq('RefundMinted', [ev.reference, accountHex(ev.to), ev.asset, ev.amount], [r1, accountHex(eve.address), asset, redeemed]);
    eq('refunded flag', (await contract.read('redemption', { redeemer: eve.address, reference: r1 }))?.refunded, true);
    const err = await refuse(roles.delivery, 'refund_mint', { redeemer: eve.address, redemption_reference: r1 }, 'AlreadyRefunded', 'refund the same redemption again');
    eq('nothing moved by the second (balance, asset supply, contract counter)', await snapshot(B, eve.address), after);
    return `first: //Eve +${redeemed}, RefundMinted { seq ${ev.seq} }; second: ${err}`;
  });

  await check('AC-I2.1 / AC-I2.2', 'a pause stops mints; an unpause by the pause role is refused', async () => {
    const scope = { type: 'AssetDirection', value: [asset, { type: 'Mint' }] };
    const { events } = await accept(roles.pause, 'pause', { scope }, 'the pause role pauses bUSDC mints');
    eq('contract events', events.map((e) => e.type), ['Paused']);
    eq('paused', await contract.read('paused', { scope }), true);
    const before = await snapshot(B, eve.address);
    const minted_ = await refuse(roles.delivery, 'mint', { asset, to: eve.address, amount: 1n, reference: reference('paused') }, 'Paused', 'mint while paused');
    eq('nothing minted', await snapshot(B, eve.address), before);
    const unpause = await refuse(roles.pause, 'unpause', { scope }, 'Unauthorised', 'the pause role unpauses');
    eq('still paused', await contract.read('paused', { scope }), true);
    const { events: unpaused } = await accept(roles.governance, 'unpause', { scope }, 'governance unpauses');
    eq('contract events', unpaused.map((e) => e.type), ['Unpaused']);
    eq('paused', await contract.read('paused', { scope }), false);
    return `pause by //Charlie; mint: ${minted_}; unpause by //Charlie: ${unpause}; unpause by governance: ok`;
  });

  await check('REQ-HN-7', "the contract's events in the run carry consecutive sequence numbers", async () => {
    const seqs = allEvents.map((e) => BigInt(e.value.seq));
    expect(seqs.length > 0, 'no events');
    seqs.forEach((s, i) => i && expect(s === seqs[i - 1] + 1n, `seq ${seqs[i - 1]} is followed by ${s}`));
    eq('event_seq', await contract.read('event_seq'), seqs.at(-1) + 1n);
    return `${seqs.length} events, seq ${seqs[0]}…${seqs.at(-1)}`;
  });
} catch (e) {
  results.push({ id: 'setup', what: 'connect and load the deployment', pass: false, detail: e.message });
  console.error(e.stack);
} finally {
  client.destroy();
}

const failed = results.filter((r) => !r.pass);
const md = [
  '### bezent contract on Kreivo: end-to-end checks (T-013-05)',
  '',
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
