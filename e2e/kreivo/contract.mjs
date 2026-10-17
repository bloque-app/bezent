// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// F-013 (T-013-05): what deploy.mjs and the contract's end-to-end check share: the bezent contract's
// artefacts, its test configuration, and calls to it through pallet-contracts with polkadot-api.
//
// Messages are encoded and decoded from the contract's own metadata (`bezent_contract.json`, written
// by `cargo contract build`) with polkadot-api's ink! client; no descriptors are generated. Every
// call is first dry-run through `ContractsApi.call` on the finalized state, which gives the exact
// result (or the contract's `Error`) and the gas it needs; then, if wanted, submitted as
// `Contracts.call` with that gas.

import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { getInkClient } from 'polkadot-api/ink';
import { accountHex, devAccount, env, json, log, submit } from './lib.mjs';

const here = dirname(fileURLToPath(import.meta.url));

/// Where `cargo contract build --release` wrote the contract (the CI job downloads it there).
export const CONTRACT_DIR = resolve(env('CONTRACT_DIR', resolve(here, '../../contracts/bezent/target/ink')));
/// Where deploy.mjs records the deployment for the checks that follow it.
export const STATE_FILE = resolve(env('CONTRACT_STATE', resolve(here, 'contract-state.json')));

/// The three roles, each a distinct test account (`REQ-HN-9`). On mainnet governance is the
/// dedicated community's account (`REQ-HN-10`, F-015); on the bed a dev account stands in for it, so
/// the checks can act as governance without a referendum.
export const ROLE_PATHS = { delivery: '//Bob', pause: '//Charlie', governance: '//Dave' };

/// How many blocks a raise waits. Small, and within the contract's cap of 403 200.
export const RAISE_DELAY = Number(env('RAISE_DELAY', '20'));

/// Small test bounds, the same for the three bridged assets, in each asset's smallest unit. The
/// window is long enough (blocks) that nothing in a run leaves it.
export const TEST_BOUNDS = {
  supply_ceiling: 10_000_000n,
  window_amount: 3_000_000n,
  window_blocks: 1_000,
  per_transfer_max: 2_000_000n,
  redeem_minimum: 1_000n,
};

/// Each bridged asset with the backend its redemptions name (plan "Assets in V0"): bUSDC on Polygon,
/// bEURC on Base, bBTC on BNB Chain, by EVM chain id.
export const TEST_ASSETS = [
  { id: 101, symbol: 'bUSDC', backend: { type: 'Evm', value: { chain_id: 137n } } },
  { id: 102, symbol: 'bEURC', backend: { type: 'Evm', value: { chain_id: 8453n } } },
  { id: 103, symbol: 'bBTC', backend: { type: 'Evm', value: { chain_id: 56n } } },
];

export const here_ = (id) => ({ type: 'Here', value: id });

export function roleAccounts() {
  return Object.fromEntries(Object.entries(ROLE_PATHS).map(([role, path]) => [role, devAccount(path)]));
}

/// A fresh 32-byte on-chain reference, as hex, unique to this run.
const RUN = `${Date.now()}-${process.pid}`;
export const reference = (label) => `0x${createHash('sha256').update(`bezent-e2e/${RUN}/${label}`).digest('hex')}`;

export function loadArtefacts() {
  const wasmPath = resolve(CONTRACT_DIR, 'bezent_contract.wasm');
  const metaPath = resolve(CONTRACT_DIR, 'bezent_contract.json');
  for (const p of [wasmPath, metaPath]) if (!existsSync(p)) throw new Error(`${p} is missing: build the contract (cargo contract build --release)`);
  const metadata = JSON.parse(readFileSync(metaPath, 'utf8'));
  return { wasm: new Uint8Array(readFileSync(wasmPath)), metadata, ink: getInkClient({ metadata }) };
}

export function saveState(state) {
  writeFileSync(STATE_FILE, `${json(state)}\n`);
  log(`deployment recorded in ${STATE_FILE}`);
}

export function loadState() {
  if (!existsSync(STATE_FILE)) throw new Error(`${STATE_FILE} is missing: run deploy.mjs first`);
  return JSON.parse(readFileSync(STATE_FILE, 'utf8'));
}

/// The bezent contract at `address`, called through pallet-contracts.
export class Contract {
  constructor(api, artefacts, address) {
    this.api = api;
    this.ink = artefacts.ink;
    this.address = address;
    this.topics = new Set(artefacts.metadata.spec.events.map((e) => e.signature_topic).filter(Boolean));
  }

  /// Dry-run `message` as `from` on the finalized state. Returns `{ ok, value, error, gas, raw }`:
  /// `ok` with the message's value when it returned `Ok`, `error` with the contract's `Error` (or a
  /// dispatch or language error) when it did not.
  async dryRun(from, message, args = {}) {
    const data = this.ink.message(message).encode(args);
    const raw = await this.api.apis.ContractsApi.call(from, this.address, 0n, undefined, undefined, data);
    if (!raw.result.success) return { ok: false, error: { dispatch: raw.result.value }, gas: raw.gas_required, data, raw };
    const reverted = (raw.result.value.flags & 1) === 1;
    const out = this.ink.message(message).decode({ data: raw.result.value.data });
    if (!out.success) return { ok: false, error: { lang: out.value }, gas: raw.gas_required, data, raw };
    // A message that returns `Result<T, Error>` decodes as `{ success, value }`; a read returns its value.
    const inner = out.value;
    const isResult = inner && typeof inner === 'object' && 'success' in inner && Object.keys(inner).every((k) => k === 'success' || k === 'value');
    if (isResult && !inner.success) return { ok: false, reverted, error: inner.value, gas: raw.gas_required, data, raw };
    return { ok: !reverted, reverted, value: isResult ? inner.value : inner, gas: raw.gas_required, data, raw };
  }

  /// A read (a message that changes nothing), by anyone.
  async read(message, args = {}, from = devAccount('//Alice').address) {
    const r = await this.dryRun(from, message, args);
    if (!r.ok) throw new Error(`${message}: ${json(r.error)}`);
    return r.value;
  }

  /// Submit `message` as `who` with the gas its dry-run needs, whatever the dry-run said, and wait
  /// for finality. Returns `{ dry, res, events }`, `events` being the contract's own, decoded.
  async submit(who, message, args = {}, label = message) {
    const dry = await this.dryRun(who.address, message, args);
    const margin = (w) => (w * 13n) / 10n + 1_000n;
    const tx = this.api.tx.Contracts.call({
      dest: { type: 'Id', value: this.address },
      value: 0n,
      gas_limit: { ref_time: margin(dry.gas.ref_time), proof_size: margin(dry.gas.proof_size) },
      storage_deposit_limit: undefined,
      data: dry.data,
    });
    const res = await submit(tx, who, label);
    return { dry, res, events: this.events(res.events) };
  }

  /// The contract's events among a block's or an extrinsic's events, decoded with its metadata.
  events(events = []) {
    return events
      .filter((e) => e.type === 'Contracts' && e.value.type === 'ContractEmitted' && accountHex(e.value.value.contract) === accountHex(this.address))
      .map((e) => {
        const topic = (e.topics ?? []).find((t) => this.topics.has(t));
        return this.ink.event.decode({ data: e.value.value.data }, topic);
      });
  }
}

/// `res.events` of one pallet and event name.
export const eventsOf = (res, pallet, name) => (res?.events ?? []).filter((e) => e.type === pallet && e.value.type === name).map((e) => e.value.value);

/// The free balance of `who` in asset `Here(id)`; zero when the asset account does not exist.
export async function assetBalance(api, id, who) {
  const account = await api.query.Assets.Account.getValue(here_(id), who);
  return account?.balance ?? 0n;
}

export async function assetSupply(api, id) {
  return (await api.query.Assets.Asset.getValue(here_(id)))?.supply ?? 0n;
}
