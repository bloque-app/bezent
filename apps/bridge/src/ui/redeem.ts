// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The redeem section of the page (F-018, T-018-15; US-B5): connect a Kreivo extension wallet with Kreivo's custom
// signed extensions registered, show the redeemer's bUSDC and the contract's minimum and pauses, take the amount (or
// the whole balance) and the payout address, pick a fresh random reference, and run the locked redeem (src/redeem.ts).
// The result names the redemption's on-chain reference (the redeemer and the reference) and links to its status.
//
// The payout address is refused when it is one of bezent's own contracts, and flagged (not refused) when Polygon
// holds code at it. A redemption whose outcome is unclear (broadcast, but following it failed) keeps its reference
// locked until the contract records it or its mortality runs out. A signature the wallet made over another payload
// (refused in the page, or BadProof from Kreivo) forgets that the wallet took Kreivo's metadata, and the page offers
// it again.

import { getAddress, type Address, type Hex as EvmHex } from 'viem';
import type { ResolvedNetwork } from '../config/networks';
import { bezentContract } from '../config/networks';
import type { Kreivo } from '../kreivo/client';
import { backendText, type Backend } from '../kreivo/bezent';
import { KREIVO_USER_EXTENSIONS, WalletSignatureMismatch, type MetadataDef } from '../kreivo/extensions';
import { kreivoRuntime, redeemChain, UnclearOutcome, type RedeemChain, type RedeemFacts } from '../kreivo/redeem';
import { enableKreivoWallet, kreivoWalletNames, type KreivoAccount, type KreivoWallet } from '../kreivo/wallets';
import { formatAmount, parseAmount } from '../lib/amount';
import { encodeSs58 } from '../lib/ss58';
import { fromHex, type Hex } from '../lib/bytes';
import { payoutReference, withdrawalReference } from '../lib/refs';
import {
  decodePending,
  encodePending,
  freshReference,
  ownContracts,
  parsePayout,
  PENDING_PREFIX,
  pendingProblem,
  pendingKey,
  redeemRows,
  refusal,
  runRedeem,
  sameRedemption,
  settle,
  takeRedeemSnapshot,
  type Pending,
  type PendingConfig,
  type RedeemSnapshot,
  type SettleRead,
} from '../redeem';
import { $, errorText, facts, note, progressList, say } from './dom';

/** The redeem's controls the lock disables, by id, and the containers whose buttons it disables. */
export const REDEEM_LOCKED_IDS = ['redeem-connect', 'redeem-offer-metadata', 'redeem-amount', 'redeem-whole', 'redeem-payout', 'redeem-new-reference'] as const;
export const REDEEM_LOCKED_CONTAINERS = ['redeem-accounts'] as const;

/** What the redeem needs from the rest of the page. */
export interface RedeemHost {
  network(): ResolvedNetwork | undefined;
  /** The networks this build carries, to check a stored record against; defaults to the selected one. */
  networks?(): readonly PendingConfig[];
  kreivo(): Kreivo | undefined;
  /** The connected EVM account, the payout address's default. */
  evmAccount(): Address | undefined;
  busy(): boolean;
  /** Locks or unlocks the whole page (one value-moving action at a time) and draws it again. */
  setBusy(busy: boolean): void;
  /** Where a redemption's status is shown. */
  statusLink(redeemer: string, reference: Hex): string;
  /** The code at an address on the network's EVM chain, through the connected wallet; `undefined` when it cannot
   * be read (no wallet, or one on another chain). Only warns: it never refuses a payout. */
  payoutCode?(address: Address): Promise<EvmHex | undefined>;
  /** Waits between reads of an unclear redemption; overridable for tests. */
  sleep?: (ms: number) => Promise<void>;
  /** Overridable for tests. */
  chain?: (k: Kreivo, n: ResolvedNetwork, contract: Hex) => RedeemChain;
  enable?: typeof enableKreivoWallet;
  walletNames?: () => string[];
  runtime?: typeof kreivoRuntime;
}

/** Remembers, per wallet and runtime, that this page gave the wallet Kreivo's metadata with its extensions
 * registered, so it asks once. A per-browser convenience: forgetting it only asks again. */
const remembered = (wallet: string, def: MetadataDef) => `bezent.bridge.metadata/${wallet}/${def.genesisHash}/${def.specVersion}/${Object.keys(KREIVO_USER_EXTENSIONS).join(',')}`;
function recall(key: string): boolean {
  try {
    return localStorage.getItem(key) === '1';
  } catch {
    return false;
  }
}
function remember(key: string) {
  try {
    localStorage.setItem(key, '1');
  } catch {
    // Storage blocked: the wallet is asked again next time.
  }
}
function forget(key: string) {
  try {
    localStorage.removeItem(key);
  } catch {
    // Storage blocked: nothing was remembered.
  }
}

/** What a stored value is: a record this build wrote and can check, or why it is not one. */
function classify(key: string, raw: string | null, nets: readonly PendingConfig[]): { ok: true; p: Pending } | { ok: false; why: string } {
  const p = decodePending(raw ?? '');
  if (!p) return { ok: false, why: 'unknown version, malformed or incomplete' };
  if (pendingKey(p.kreivo, p.redeemer) !== key) return { ok: false, why: 'its key does not match its body' };
  const problem = pendingProblem(p, nets);
  return problem ? { ok: false, why: problem } : { ok: true, p };
}

/** Keeps the unclear redemption in this browser, so a reload does not erase its lock. It never overwrites another
 * record: another tab's lock for the same redeemer (`other`) is the lock instead, and a record it cannot read stays
 * as it is (`failed`, as when storage refused). */
function keep(p: Pending, nets: readonly PendingConfig[]): { kind: 'kept' } | { kind: 'other'; existing: Pending } | { kind: 'failed' } {
  try {
    const key = pendingKey(p.kreivo, p.redeemer);
    const have = localStorage.getItem(key);
    if (have !== null) {
      const c = classify(key, have, nets);
      if (c.ok && c.p.txHash === p.txHash) return { kind: 'kept' };
      return c.ok ? { kind: 'other', existing: c.p } : { kind: 'failed' };
    }
    localStorage.setItem(key, encodePending(p));
    return { kind: 'kept' };
  } catch {
    return { kind: 'failed' };
  }
}
/** Removes the record only if it is still the settled one (compare-and-delete): another tab's newer lock for the
 * same redeemer is not this tab's to remove. */
function drop(p: Pending) {
  try {
    const key = pendingKey(p.kreivo, p.redeemer);
    const have = localStorage.getItem(key);
    if (have === null) return;
    let stored: unknown;
    try {
      stored = JSON.parse(have)?.txHash;
    } catch {
      return; // Not readable: not this record, and not ours to remove.
    }
    if (stored === p.txHash) localStorage.removeItem(key);
  } catch {
    // Storage blocked: the record stays, and is read (and settled) again on the next load.
  }
}
interface Unreadable {
  key: string;
  raw: string;
  why: string;
}
/** The kept records, read from storage: the ones this build wrote and can check, the ones it cannot (each locks), and
 * whether storage could be read at all. */
function kept(nets: readonly PendingConfig[]): { found: Pending[]; unreadable: Unreadable[]; down: boolean } {
  const found: Pending[] = [];
  const unreadable: Unreadable[] = [];
  let down = false;
  try {
    for (let i = 0; i < localStorage.length; i++) {
      const k = localStorage.key(i);
      if (!k || !k.startsWith(PENDING_PREFIX)) continue;
      const raw = localStorage.getItem(k) ?? '';
      const c = classify(k, raw, nets);
      if (c.ok) found.push(c.p);
      else unreadable.push({ key: k, raw, why: c.why });
    }
  } catch {
    down = true;
  }
  return { found, unreadable, down };
}
/** The network and redeemer a key names, for the user to recognise a record by. */
function keyNames(key: string, nets: readonly PendingConfig[]): { kreivo: string; redeemer: string } {
  const rest = key.slice(PENDING_PREFIX.length);
  const cut = rest.lastIndexOf('/');
  const kreivo = cut < 0 ? rest : rest.slice(0, cut);
  const hex = cut < 0 ? '' : rest.slice(cut + 1);
  const prefix = nets.find((n) => n.kreivo?.network === kreivo)?.kreivo?.ss58Prefix;
  let redeemer = hex || '(none)';
  if (prefix !== undefined && /^0x[0-9a-f]{64}$/.test(hex)) {
    try {
      redeemer = `${encodeSs58(fromHex(hex as Hex), prefix)} (${hex})`;
    } catch {
      // Shown as stored.
    }
  }
  return { kreivo, redeemer };
}

const clock = () => `${new Date().toISOString().slice(11, 19)} UTC`;

export function initRedeem(host: RedeemHost) {
  const chainOf = host.chain ?? ((k, n, c) => redeemChain(k, n.kreivo, c));
  const enable = host.enable ?? enableKreivoWallet;
  const names = host.walletNames ?? kreivoWalletNames;
  const runtimeOf = host.runtime ?? kreivoRuntime;
  const sleep = host.sleep ?? ((ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)));

  const r: {
    account?: KreivoAccount;
    facts?: RedeemFacts;
    reading: boolean;
    gen: number;
    amount?: bigint;
    payout?: Address;
    payoutEdited: boolean;
    /** The default payout address last shown (the connected EVM account), to notice when it changes. */
    payoutDefault?: Address;
    /** Drops a code read for a payout address the form moved on from. */
    codeGen: number;
    reference: Hex;
    locked?: RedeemSnapshot;
    /** The wallets connected, and the metadata they were offered. */
    wallets: KreivoWallet[];
    def?: MetadataDef;
    /** A redemption whose outcome is unclear: its reference stays locked until it settles. */
    pending?: Pending;
    /** The pending record could not be stored: a reload would lose the lock. */
    unkept?: boolean;
    /** The pending record was read back from storage after a reload. */
    restored?: boolean;
    /** What the watcher last read, for the lock row. */
    watch?: string;
    /** Stored records this build cannot read or check: each locks Redeem until the user clears it. */
    unreadable: Unreadable[];
    /** The keys last announced as unreadable, so the notice is not printed again on every connect. */
    announced: string;
    /** Storage could not be read: a lock from an earlier visit cannot be checked. */
    storageDown: boolean;
  } = { unreadable: [], announced: '', storageDown: false, reading: false, gen: 0, payoutEdited: false, codeGen: 0, reference: freshReference(), wallets: [] };

  const amountInput = $<HTMLInputElement>('redeem-amount');
  const wholeInput = $<HTMLInputElement>('redeem-whole');
  const payoutInput = $<HTMLInputElement>('redeem-payout');
  const button = $<HTMLButtonElement>('redeem');

  /** The networks a stored record is checked against. */
  const nets = (): readonly PendingConfig[] => host.networks?.() ?? (host.network() ? [host.network()!] : []);
  /** Transactions this page saw settle: a record of one that could not be removed is never locked again. */
  const settled = new Set<string>();
  // Where the unreadable records are listed, each with its own button to clear it.
  const clearBox = document.createElement('div');
  clearBox.id = 'redeem-unreadable';
  $('redeem-review').after(clearBox);

  const backend = (n: ResolvedNetwork): Backend => ({ kind: 'evm', chainId: BigInt(n.chainId) });

  function contract(): { n: ResolvedNetwork; k: Kreivo; c: Hex } | string {
    const n = host.network();
    if (!n) return 'Pick a network the app does not refuse first.';
    let c: Hex;
    try {
      c = bezentContract(n);
    } catch (e) {
      return errorText(e);
    }
    const k = host.kreivo();
    if (!k) return `${n.kreivo.label} is not connected yet.`;
    return { n, k, c };
  }

  // The wallets.
  $('redeem-connect').addEventListener('click', () => void connectWallets());
  async function connectWallets() {
    if (host.busy()) return;
    const status = $('redeem-wallet-status');
    const box = $('redeem-accounts');
    const t = contract();
    if (typeof t === 'string') return say(status, t, 'error');
    const found = names();
    if (found.length === 0) {
      box.replaceChildren(note('No Kreivo wallet (Talisman, SubWallet, Polkadot.js) is installed in this browser.'));
      return;
    }
    say(status, `Reading ${t.n.kreivo.label}’s runtime and its signed extensions…`);
    let rt: Awaited<ReturnType<typeof kreivoRuntime>>;
    try {
      rt = await runtimeOf(t.k, t.n.kreivo);
    } catch (e) {
      return say(status, errorText(e), 'error');
    }
    const custom = Object.keys(KREIVO_USER_EXTENSIONS).join(' and ');
    const notes: string[] = [`Kreivo signs ${rt.extensions.join(', ')}; ${custom} are registered with each wallet before it signs.`];
    const buttons: HTMLElement[] = [];
    r.wallets = [];
    r.def = rt.def;
    for (const name of found) {
      let w: KreivoWallet;
      try {
        w = await enable(name, 'bezent bridge', rt.def.genesisHash as Hex);
      } catch {
        continue; // The user declined this extension; the others may still answer.
      }
      r.wallets.push(w);
      const key = remembered(name, rt.def);
      if (!w.takesMetadata) {
        notes.push(`${name} takes no metadata from pages: it signs only if it knows Kreivo’s ${custom} itself.`);
      } else if (!recall(key)) {
        const res = await w.provideMetadata(rt.def).catch(() => 'declined' as const);
        if (res === 'provided') remember(key);
        notes.push(res === 'provided' ? `${name} took Kreivo’s metadata.` : `${name} did not take Kreivo’s metadata; its signatures may be refused.`);
      }
      for (const a of w.accounts) {
        const address = encodeSs58(fromHex(a.account), t.n.kreivo.ss58Prefix);
        const b = document.createElement('button');
        b.type = 'button';
        b.textContent = `${a.name ?? name}: ${address.slice(0, 8)}…${address.slice(-6)}`;
        b.disabled = host.busy();
        b.addEventListener('click', () => void useAccount(a));
        buttons.push(b);
      }
    }
    say(status, notes.join(' '));
    box.replaceChildren(...(buttons.length ? buttons : [note('No Kreivo account was shared.')]));
  }

  // Offering the metadata again: after a signature over another payload, or whenever the user asks.
  $('redeem-offer-metadata').addEventListener('click', () => void offerMetadata());
  async function offerMetadata() {
    if (host.busy()) return;
    const status = $('redeem-wallet-status');
    const def = r.def;
    if (!def || r.wallets.length === 0) return say(status, 'Connect a Kreivo wallet first.', 'error');
    const notes: string[] = [];
    for (const w of r.wallets) {
      if (!w.takesMetadata) {
        notes.push(`${w.name} takes no metadata from pages.`);
        continue;
      }
      const res = await w.provideMetadata(def).catch(() => 'declined' as const);
      if (res === 'provided') remember(remembered(w.name, def));
      notes.push(res === 'provided' ? `${w.name} took Kreivo’s metadata.` : `${w.name} did not take Kreivo’s metadata; its signatures may be refused.`);
    }
    say(status, notes.join(' '));
  }

  async function useAccount(a: KreivoAccount) {
    if (host.busy()) return;
    r.account = a;
    r.facts = undefined;
    // Before Redeem can be enabled for this account: a lock kept by an earlier page comes back first.
    restore();
    await readFacts();
  }

  /** Reads the redeemer's facts; an answer for an account or network the page moved on from is dropped. */
  async function readFacts() {
    const a = r.account;
    const t = contract();
    const out = $('redeem-facts');
    if (!a) return render();
    if (typeof t === 'string') {
      facts(out, [['Redeemer', a.address], ['Contract', t]]);
      return render();
    }
    const gen = ++r.gen;
    r.reading = true;
    render();
    facts(out, [['Redeemer', encodeSs58(fromHex(a.account), t.n.kreivo.ss58Prefix)], ['Reading', `${t.n.kreivo.label}…`]]);
    try {
      const f = await chainOf(t.k, t.n, t.c).facts(a.account);
      if (gen !== r.gen || r.account !== a) return;
      r.facts = f;
      const { symbol, decimals } = t.n.kreivo.asset;
      const fmt = (u: bigint) => `${formatAmount(u, decimals)} ${symbol}`;
      const paused = f.pauses.filter((p) => p.paused).map((p) => p.scope);
      facts(out, [
        ['Redeemer', encodeSs58(fromHex(a.account), t.n.kreivo.ss58Prefix)],
        ['Contract', encodeSs58(fromHex(t.c), t.n.kreivo.ss58Prefix)],
        ['Balance', `${fmt(f.balance)} (Here(${t.n.kreivo.asset.id}))${f.assetStatus && f.assetStatus !== 'Liquid' ? `, ${f.assetStatus}` : ''}`],
        ['KSM', `${formatAmount(f.ksm, 12)} KSM, for the fee, the storage deposit and the approval’s deposit`],
        [
          'Existing approval',
          f.approval
            ? `${fmt(f.approval.amount)} already approved to the contract (its deposit, ${formatAmount(f.approval.deposit, 12)} KSM, is held). The batch’s approve_transfer adds the amount to it, and the redemption pulls only the amount, so ${fmt(f.approval.amount)} stays approved after it.`
            : 'none: the batch approves the amount, and the redemption uses it up',
        ],
        ['Redemption minimum', f.bounds ? `${fmt(f.bounds.redeemMinimum)}, from the contract` : 'the contract does not know this asset'],
        ['Paused', paused.length ? paused.join(', ') : 'no'],
        ['Backend', `${backendText(backend(t.n))}${f.backends.length ? `; the contract declares ${f.backends.map(backendText).join(', ')}` : ''}`],
        ['Read at', `block ${f.at.number} (${f.at.hash}), finalized`],
      ]);
    } catch (e) {
      if (gen !== r.gen) return;
      facts(out, [['Redeemer', a.address], ['Contract', `could not be read: ${errorText(e)}`]]);
    } finally {
      if (gen === r.gen) {
        r.reading = false;
        onAmount();
      }
    }
  }

  // The amount.
  amountInput.addEventListener('input', onAmount);
  wholeInput.addEventListener('change', onAmount);
  function onAmount() {
    if (host.busy()) return;
    const n = host.network();
    const out = $('redeem-amount-status');
    r.amount = undefined;
    const f = r.facts;
    if (!n || !f) {
      say(out, r.account ? '' : 'Connect a Kreivo wallet and pick the account to redeem from.');
      return render();
    }
    const { symbol, decimals } = n.kreivo.asset;
    if (wholeInput.checked) amountInput.value = formatAmount(f.balance, decimals);
    amountInput.readOnly = wholeInput.checked;
    if (!amountInput.value.trim()) {
      say(out, f.bounds ? `At least ${formatAmount(f.bounds.redeemMinimum, decimals)} ${symbol}.` : '');
      return render();
    }
    const p = parseAmount(amountInput.value, decimals);
    if (!p.ok) {
      say(out, p.reason, 'error');
      return render();
    }
    const why = refusal(f, { amount: p.units, backend: backend(n), whole: false, decimals, symbol });
    if (why) say(out, why, 'error');
    else {
      r.amount = p.units;
      say(out, `${formatAmount(p.units, decimals)} ${symbol} is destroyed on Kreivo, and ${formatAmount(p.units, decimals)} USDC is paid out on ${n.label}.`, 'ok');
    }
    render();
  }

  // The payout address: the connected EVM account by default, editable.
  payoutInput.addEventListener('input', () => {
    if (host.busy()) return;
    r.payoutEdited = payoutInput.value.trim() !== '';
    onPayout();
  });
  function onPayout() {
    const out = $('redeem-payout-status');
    const warning = $('redeem-payout-warning');
    let changed: { from: Address; to: Address } | undefined;
    if (!r.payoutEdited) {
      const evm = host.evmAccount();
      if (r.payoutDefault && evm && evm !== r.payoutDefault) changed = { from: r.payoutDefault, to: evm };
      r.payoutDefault = evm;
      payoutInput.value = evm ?? '';
    }
    r.payout = undefined;
    const gen = ++r.codeGen;
    say(warning, '', 'warn');
    if (!payoutInput.value.trim()) {
      say(out, 'The connected EVM account is used when there is one.');
      return render();
    }
    const n = host.network();
    const p = parsePayout(payoutInput.value, n ? ownContracts(n) : []);
    if (p.ok) {
      r.payout = p.address;
      if (changed) {
        say(out, `The payout address changed to ${p.address}: it follows the connected EVM account, which changed (it was ${changed.from}). Check it before you redeem.`, 'warn');
      } else {
        say(out, `Paid to ${p.address}${host.evmAccount() === p.address ? ' (the connected EVM account)' : ''}.`, 'ok');
      }
      void checkCode(p.address, gen);
    } else say(out, p.reason, 'error');
    render();
  }

  /** Warns, without refusing, when Polygon holds code at the payout address: a contract that cannot move USDC on
   * would keep it. */
  async function checkCode(address: Address, gen: number) {
    if (!host.payoutCode) return;
    let code: EvmHex | undefined;
    try {
      code = await host.payoutCode(address);
    } catch {
      return; // Unreadable: nothing to warn about, and nothing is refused.
    }
    if (gen !== r.codeGen || r.payout !== address || !code || code === '0x') return;
    // An EIP-7702 delegation (0xef0100 and an address) is an account its key controls, not a contract's own.
    if (/^0xef0100[0-9a-f]{40}$/i.test(code)) return;
    const n = host.network();
    say(
      $('redeem-payout-warning'),
      `${address} is a contract on ${n?.label ?? 'this network'}, not a plain account. The USDC is paid to it as to any address; if that contract cannot move USDC on, it stays there. Make sure it is a wallet you control there (a smart account), not an exchange’s or a token’s contract.`,
      'warn',
    );
  }

  // The reference.
  $('redeem-new-reference').addEventListener('click', () => {
    if (host.busy() || r.pending) return;
    r.reference = freshReference();
    render();
  });

  function render() {
    const n = host.network();
    $('redeem-reference').textContent = r.reference;
    const rows: [string, string][] = [];
    if (r.locked) rows.push(...redeemRows(r.locked), ['Review', 'Locked until this redemption ends.']);
    else {
      const snap = live();
      if (snap) rows.push(...redeemRows(snap));
    }
    const p = r.pending;
    if (p) {
      rows.push(
        ['Reference', `locked: the redemption ${p.txHash} (${p.snapshot.redeemer.address}, ${p.networkLabel}) may still be included, so this reference is kept, and Redeem is off, until Kreivo records it or it can no longer be included`],
        ['Do not reload or close this page', r.unkept ? 'until it settles. This browser could not store the lock, so a reload or closing the page would lose it, and a second redemption could then be signed.' : 'until it settles. The lock is kept in this browser, but leaving early still leaves the redemption unwatched.'],
      );
      if (r.restored) rows.push(['Restored', 'read back from this browser after a reload']);
      if (n?.key !== p.network) rows.push(['Network', `this redemption belongs to ${p.networkLabel}, which is not selected: select it again to settle it. Nothing is read meanwhile.`]);
      rows.push(['Watcher', r.watch ?? 'has not read Kreivo yet']);
    }
    for (const u of r.unreadable) {
      const { kreivo, redeemer } = keyNames(u.key, nets());
      rows.push(['Stored record', `a stored redemption record can’t be read (${u.why}; ${kreivo}, redeemer ${redeemer}); Redeem is off until you check it`]);
    }
    if (r.storageDown) rows.push(['Earlier lock', 'storage unavailable: a lock from an earlier visit can’t be checked. Redeem stays usable, but a redemption an earlier visit left unclear would not be known here.']);
    facts($('redeem-review'), rows);
    clearBox.replaceChildren(
      ...r.unreadable.map((u) => {
        const b = document.createElement('button');
        b.type = 'button';
        b.textContent = `Clear this record (${keyNames(u.key, nets()).kreivo})`;
        b.disabled = host.busy();
        b.addEventListener('click', () => clearRecord(u));
        return b;
      }),
    );
    button.disabled = host.busy() || r.reading || !!r.pending || r.unreadable.length > 0 || !n || !r.facts || !live();
    // After the page's lock (main.ts draws it first): an unclear redemption keeps its reference.
    $<HTMLButtonElement>('redeem-new-reference').disabled = host.busy() || !!r.pending;
  }

  function live(): RedeemSnapshot | undefined {
    const n = host.network();
    if (!n || !r.account || !r.facts) return undefined;
    return takeRedeemSnapshot(
      { redeemer: r.account, asset: n.kreivo.asset, amount: r.amount, whole: wholeInput.checked, backend: backend(n), payout: r.payout, reference: r.reference },
      n.kreivo.ss58Prefix,
    );
  }

  button.addEventListener('click', () => void onRedeem());
  async function onRedeem() {
    if (host.busy() || r.reading) return;
    restore(); // Another tab may have left a lock since this page read the storage.
    render();
    if (r.unreadable.length > 0) {
      progressList($('redeem-progress'))('A stored redemption record can’t be read; Redeem is off until you check it. Nothing was signed.', 'error');
      return;
    }
    if (r.pending) {
      // L6: a second redemption could pay out twice while the first may still be included.
      progressList($('redeem-progress'))(`The redemption ${r.pending.txHash} may still be included, so no other is sent until Kreivo records it or it can no longer be included. Nothing was signed.`, 'error');
      return;
    }
    const t = contract();
    const a = r.account;
    // The snapshot: from here on the redemption uses only this, never the form.
    const snap = live();
    if (typeof t === 'string' || !a || !snap) return;
    const step = progressList($('redeem-progress'));
    const again = parsePayout(snap.payout, ownContracts(t.n));
    if (!again.ok) return step(`${again.reason} Nothing was signed.`, 'error');
    $('redeem-result').replaceChildren();
    $('redeem-link').replaceChildren();
    r.locked = snap;
    r.gen++; // Any read in flight is for the form, not for what is redeemed.
    host.setBusy(true);
    try {
      const { symbol, decimals } = snap.asset;
      const out = await runRedeem(snap, chainOf(t.k, t.n, t.c), a.signer, step, (p) =>
        step(
          `Dry runs pass. The batch takes at most ${p.gas.refTime} ref-time and ${p.gas.proofSize} proof size of gas and ${formatAmount(p.storageLimit, 12)} KSM of storage deposit${p.fee !== undefined ? `; the fee is about ${formatAmount(p.fee, 12)} KSM` : ''}.`,
        ),
      );
      const { finalized: f, comparison: cmp } = out;
      step(`Final in block ${f.block.number}: ${f.txHash}.`, cmp.ok ? 'ok' : 'muted');
      const bridge = withdrawalReference(snap.redeemer.account, snap.reference);
      const rows: [string, string][] = [
        ['Transaction', f.txHash],
        ['Kreivo block', `${f.block.number} (${f.block.hash}), extrinsic ${f.block.index}`],
      ];
      if (cmp.redeemed) {
        const e = cmp.redeemed;
        rows.push(
          ['Recorded redeemer', encodeSs58(fromHex(e.redeemer), t.n.kreivo.ss58Prefix)],
          ['Recorded amount', `${formatAmount(e.amount, decimals)} ${symbol}`],
          ['Recorded backend', backendText(e.backend)],
          ['Recorded payout', e.payout.kind === 'address' ? getAddress(e.payout.address) : e.payout.commitment],
          ['Event sequence', String(e.seq)],
        );
      }
      rows.push(
        ['On-chain reference', `${snap.redeemer.address} + ${snap.reference}`],
        ['Bridge reference', bridge],
        ['Payout reference (Polygon)', payoutReference(bridge)],
      );
      facts($('redeem-result'), rows);
      if (cmp.ok) {
        step('The Redeemed event matches what you reviewed. bezent pays it out on Polygon once Kreivo’s block is final, as it now is.', 'ok');
      } else {
        step(`${cmp.reason} bezent acts on what Kreivo recorded, shown above, not on what you reviewed. Keep the transaction hash.`, 'error');
      }
      const link = document.createElement('a');
      link.href = host.statusLink(snap.redeemer.address, snap.reference);
      link.textContent = 'Follow this redemption’s status';
      $('redeem-link').replaceChildren(link);
      r.reference = freshReference();
    } catch (e) {
      step(errorText(e), 'error');
      if (e instanceof WalletSignatureMismatch && r.def) {
        // The wallet's record of Kreivo's metadata is not one it signs with: offer it again.
        forget(remembered(a.wallet, r.def));
        step('Use “Offer Kreivo’s metadata to the wallets again” above, accept it in the wallet, then redeem again.', 'muted');
      }
      if (e instanceof UnclearOutcome) {
        const p: Pending = { redeemer: snap.redeemer.account, reference: snap.reference, txHash: e.txHash, validUntil: e.validUntil, network: t.n.key, networkLabel: t.n.label, kreivo: t.n.kreivo.network, snapshot: snap };
        const res = keep(p, nets());
        r.restored = res.kind === 'other';
        r.watch = undefined;
        if (res.kind === 'other') {
          // Another tab holds a lock for this redeemer: that one is the lock, and nothing here overwrites it.
          r.pending = res.existing;
          r.unkept = false;
          r.reference = res.existing.reference;
          step(`Another tab already holds a lock for this redeemer (the redemption ${res.existing.txHash}), so that lock is kept and this redemption, ${e.txHash}, is not stored: keep its hash, and check it on Kreivo.`, 'error');
        } else {
          r.pending = p;
          r.unkept = res.kind === 'failed';
          r.reference = snap.reference;
          if (r.unkept) step('This browser could not store the lock, so a reload or closing the page would lose it: do not reload or close this page until it settles.', 'error');
        }
        void watchPending(step);
      }
    } finally {
      r.locked = undefined;
      host.setBusy(false);
      void readFacts();
    }
  }

  /** Follows an unclear redemption until it settles, then frees its reference. */
  async function watchPending(step: (t: string, kind?: 'muted' | 'ok' | 'error') => void) {
    const u = r.pending;
    if (!u) return;
    step(`Reading Kreivo for the redemption with this reference${u.validUntil === undefined ? '' : `, until block ${u.validUntil} is final`}; the reference stays locked meanwhile. Do not reload or close this page until it settles.`);
    const chain = () => {
      const t = contract();
      return typeof t === 'string' || t.n.key !== u.network ? undefined : chainOf(t.k, t.n, t.c);
    };
    const onRead = (x: SettleRead) => {
      if (r.pending !== u) return;
      r.watch = x.ok ? `last checked at block ${x.head} (finalized), ${clock()}` : `last read failed at ${clock()} (${x.error}); trying again`;
      render();
    };
    const out = await settle(u, chain, { sleep, stopped: () => r.pending !== u, onRead });
    if (!out || r.pending !== u) return;
    r.pending = undefined;
    r.unkept = false;
    r.restored = false;
    r.watch = undefined;
    settled.add(u.txHash);
    drop(u);
    try {
      const s = u.snapshot;
      if (out.kind === 'recorded') {
        const red = out.redemption;
        const same = sameRedemption(red, s);
        step(
          same
            ? `Kreivo records the redemption (final by block ${out.at}): ${formatAmount(red.amount, s.asset.decimals)} ${s.asset.symbol}, paid to ${s.payout}. bezent pays it out on Polygon.`
            : `Kreivo records a redemption with this reference that differs from what you reviewed (${formatAmount(red.amount, s.asset.decimals)} ${s.asset.symbol} of Here(${red.asset}), paid to ${red.payout.kind === 'address' ? getAddress(red.payout.address) : red.payout.commitment}). bezent acts on what Kreivo recorded. Keep the transaction hash.`,
          same ? 'ok' : 'error',
        );
        const link = document.createElement('a');
        link.href = host.statusLink(s.redeemer.address, s.reference);
        link.textContent = 'Follow this redemption’s status';
        $('redeem-link').replaceChildren(link);
        r.reference = freshReference();
      } else {
        step(`Block ${out.at} is final, past the transaction’s mortality, and Kreivo records no redemption with this reference: nothing was redeemed. The reference is free again.`, 'ok');
      }
    } catch (e) {
      // The lock is already cleared: a message that cannot be formed must not leave an unhandled rejection.
      step(`The redemption ${u.txHash} settled, but its outcome could not be shown (${errorText(e)}). Check it on Kreivo.`, 'error');
    }
    restore(step);
    render();
  }

  /** Reads the stored records and takes them into account: the ones it cannot read lock Redeem (announced once per
   * set, and listed with a button to clear each), and an unclear redemption a reload (or another tab) left behind is
   * read back and followed again (one at a time; the next is read once this settles). Never one already seen to
   * settle. Before anything can enable Redeem: the page starts with it locked. */
  function restore(given?: (t: string, kind?: 'muted' | 'ok' | 'error') => void) {
    const k = kept(nets());
    r.storageDown = k.down;
    r.unreadable = k.unreadable;
    const keys = k.unreadable.map((u) => u.key).sort().join('\n');
    const say_ = (t: string, kind: 'muted' | 'ok' | 'error') => (given ?? progressList($('redeem-progress')))(t, kind);
    if (keys !== r.announced) {
      r.announced = keys;
      if (keys) say_(`${k.unreadable.length} stored redemption record(s) can’t be read; Redeem is off until you check ${k.unreadable.length === 1 ? 'it' : 'them'}.`, 'error');
    }
    if (r.pending) return;
    for (const x of k.found) if (settled.has(x.txHash)) drop(x); // Its removal failed before: try again, never lock.
    const p = k.found.find((x) => !settled.has(x.txHash));
    if (!p) return;
    const step = given ?? progressList($('redeem-progress'));
    r.pending = p;
    r.restored = true;
    r.unkept = false;
    r.watch = undefined;
    r.reference = p.reference;
    step(`A redemption sent before this page was reloaded may still be included (${p.txHash}, ${p.networkLabel}); its reference is locked again.`, 'error');
    void watchPending(step);
  }

  /** Removes an unreadable record, after the user confirms it, naming its network and redeemer. */
  function clearRecord(u: Unreadable) {
    const { kreivo, redeemer } = keyNames(u.key, nets());
    const ok = window.confirm(
      `Clear the stored redemption record for ${redeemer} on ${kreivo}?\n\nIt can’t be read (${u.why}). Clearing it turns Redeem back on; if that redemption is still being included, this page will not watch it. Check the redeemer on Kreivo first.`,
    );
    if (!ok) return;
    try {
      if (localStorage.getItem(u.key) === u.raw) localStorage.removeItem(u.key); // Only what was shown.
    } catch {
      // Storage blocked: the record stays, and so does the lock.
    }
    restore();
    render();
  }

  // Another tab's lock takes effect at once.
  window.addEventListener('storage', (e) => {
    if (e.key !== null && !e.key.startsWith(PENDING_PREFIX)) return;
    restore();
    render();
  });

  // A pending record: leaving the page ends the watch (the lock itself is kept in storage when it can be).
  window.addEventListener('beforeunload', (e) => {
    if (!r.pending) return;
    e.preventDefault();
    e.returnValue = '';
  });
  restore();
  render();

  return {
    render,
    /** The network or Kreivo changed: what was read is for another chain. */
    reset() {
      r.gen++;
      // An unclear redemption stays locked across a change of network: its watcher keeps waiting (its chain
      // reads nothing while another network is selected) and settles when its network is back. Clearing it here
      // would let a switch away and back sign a second redemption under a fresh reference.
      r.account = undefined;
      r.facts = undefined;
      r.reading = false;
      facts($('redeem-facts'), []);
      $('redeem-accounts').replaceChildren();
      say($('redeem-wallet-status'), '');
      onAmount();
    },
    /** The EVM account changed: the payout's default follows it unless the user typed one. */
    onEvmAccount: () => {
      if (!host.busy()) onPayout();
    },
  };
}
