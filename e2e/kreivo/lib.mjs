// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Shared by setup.mjs and check.mjs: the client, dev signers, and Kreivo's community account.
//
// polkadot-api's unsafe API: no descriptors to generate or commit, since the bed's runtime is a
// release artifact that changes with KREIVO_RELEASE. Calls and storage are named as in the
// metadata (`CommunitiesManager.register`, `Assets.Asset`).

import { AccountId, Binary, createClient } from 'polkadot-api';
import { getPolkadotSigner } from 'polkadot-api/signer';
import { getWsProvider } from 'polkadot-api/ws';
import { sr25519CreateDerive } from '@polkadot-labs/hdkd';
import { DEV_PHRASE, entropyToMiniSecret, mnemonicToEntropy } from '@polkadot-labs/hdkd-helpers';

export const env = (name, fallback) => process.env[name] ?? fallback;
export const log = (...args) => console.log('[kreivo]', ...args);
export const text = (b) => (b ? Binary.toText(b) : '');

/// The test bed's dedicated community (e2e/network/src/lib.rs, `COMMUNITY_ID`).
export const COMMUNITY_ID = Number(env('COMMUNITY_ID', '4243'));
export const COMMUNITY_NAME = env('COMMUNITY_NAME', 'bezent test');

const LOOPBACK = /^wss?:\/\/(127\.0\.0\.1|localhost|\[::1\])(:\d+)?(\/|$)/;

/// Dev keys (`//Alice`, `//Bob`, ...) are publicly known, so these scripts act only on the bed they
/// were spawned for: the endpoint must be loopback, and (see `assertTestBed`) the chain's genesis
/// hash must be the one the harness recorded. Refuses loudly, before anything is signed.
export function connect() {
  const url = env('KREIVO_WS', 'ws://127.0.0.1:9944');
  if (!LOOPBACK.test(url)) {
    throw new Error(`refusing KREIVO_WS=${url}: dev keys are used only on a loopback endpoint (ws://127.0.0.1, ws://localhost, ws://[::1])`);
  }
  const client = createClient(getWsProvider(url));
  return { client, api: client.getUnsafeApi(), url };
}

/// Call first, before any dev-key deploy, call or registration: the chain's genesis hash must equal
/// `E2E_GENESIS_HASH`, which the workflow records from the zombienet it spawned. Unset or different:
/// throw.
export async function assertTestBed(client) {
  const want = env('E2E_GENESIS_HASH', '').toLowerCase();
  if (!/^0x[0-9a-f]{64}$/.test(want)) {
    throw new Error("refusing to use dev keys: E2E_GENESIS_HASH (the spawned zombienet's genesis hash) is not set");
  }
  const got = String(await client._request('chain_getBlockHash', [0])).toLowerCase();
  if (got !== want) throw new Error(`refusing to use dev keys: the chain's genesis hash is ${got}, not the test bed's ${want}`);
  log(`genesis hash ${got} is the test bed's`);
}

/// A dev account (`//Alice`, ...), dev chains only. Returns its SS58 address and a signer.
export function devAccount(path) {
  const derive = sr25519CreateDerive(entropyToMiniSecret(mnemonicToEntropy(DEV_PHRASE)));
  const pair = derive(path);
  return {
    address: AccountId().dec(pair.publicKey),
    signer: getPolkadotSigner(pair.publicKey, 'Sr25519', pair.sign),
  };
}

/// Kreivo's `Communities::community_account(id)`:
/// `PalletId(*b"kv/cmtys").into_sub_account_truncating(id)`, zero-padded to 32 bytes.
export function communityAccount(id) {
  const raw = new Uint8Array(32);
  raw.set(new TextEncoder().encode('modlkv/cmtys'));
  raw[12] = id & 0xff;
  raw[13] = (id >> 8) & 0xff;
  return AccountId().dec(raw);
}

/// An account as hex, whatever its SS58 prefix: polkadot-api decodes storage with the chain's own
/// prefix, while the helpers above use the generic one.
export function accountHex(address) {
  return address ? `0x${Buffer.from(AccountId().enc(address)).toString('hex')}` : null;
}

/// JSON with bigints and byte arrays readable.
export const json = (v) =>
  JSON.stringify(v, (_, x) => (typeof x === 'bigint' ? x.toString() : x instanceof Uint8Array ? Binary.toHex(x) : x));

/// Submit and wait for finality; return the result whether or not it dispatched.
export async function submit(tx, who, label) {
  const res = await tx.signAndSubmit(who.signer);
  log(`${label}: ${res.ok ? 'ok' : `failed (${json(res.dispatchError)})`}, finalized in block ${res.block.number}`);
  return res;
}

/// Submit and wait for finality; throw on a dispatch error.
export async function send(tx, who, label) {
  const res = await submit(tx, who, label);
  if (!res.ok) throw new Error(`${label}: ${json(res.dispatchError)}`);
  return res;
}
