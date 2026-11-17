// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The redeem section of the page (F-018, T-018-15; US-B5): connect a Kreivo extension wallet with Kreivo's custom
// signed extensions registered, show the redeemer's bUSDC and the contract's minimum and pauses, take the amount (or
// the whole balance) and the payout address, pick a fresh random reference, and run the locked redeem (src/redeem.ts).
// The result names the redemption's on-chain reference (the redeemer and the reference) and links to its status.

import { getAddress, type Address } from 'viem';
import type { ResolvedNetwork } from '../config/networks';
import { bezentContract } from '../config/networks';
import type { Kreivo } from '../kreivo/client';
import { backendText, type Backend } from '../kreivo/bezent';
import { KREIVO_USER_EXTENSIONS, type MetadataDef } from '../kreivo/extensions';
import { kreivoRuntime, redeemChain, type RedeemChain, type RedeemFacts } from '../kreivo/redeem';
import { enableKreivoWallet, kreivoWalletNames, type KreivoAccount, type KreivoWallet } from '../kreivo/wallets';
import { formatAmount, parseAmount } from '../lib/amount';
import { encodeSs58 } from '../lib/ss58';
import { fromHex, type Hex } from '../lib/bytes';
import { payoutReference, withdrawalReference } from '../lib/refs';
import { freshReference, parsePayout, redeemRows, refusal, runRedeem, takeRedeemSnapshot, type RedeemSnapshot } from '../redeem';
import { $, errorText, facts, note, progressList, say } from './dom';

/** The redeem's controls the lock disables, by id, and the containers whose buttons it disables. */
export const REDEEM_LOCKED_IDS = ['redeem-connect', 'redeem-amount', 'redeem-whole', 'redeem-payout', 'redeem-new-reference'] as const;
export const REDEEM_LOCKED_CONTAINERS = ['redeem-accounts'] as const;

/** What the redeem needs from the rest of the page. */
export interface RedeemHost {
  network(): ResolvedNetwork | undefined;
  kreivo(): Kreivo | undefined;
  /** The connected EVM account, the payout address's default. */
  evmAccount(): Address | undefined;
  busy(): boolean;
  /** Locks or unlocks the whole page (one value-moving action at a time) and draws it again. */
  setBusy(busy: boolean): void;
  /** Where a redemption's status is shown. */
  statusLink(redeemer: string, reference: Hex): string;
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

export function initRedeem(host: RedeemHost) {
  const chainOf = host.chain ?? ((k, n, c) => redeemChain(k, n.kreivo, c));
  const enable = host.enable ?? enableKreivoWallet;
  const names = host.walletNames ?? kreivoWalletNames;
  const runtimeOf = host.runtime ?? kreivoRuntime;

  const r: {
    account?: KreivoAccount;
    facts?: RedeemFacts;
    reading: boolean;
    gen: number;
    amount?: bigint;
    payout?: Address;
    payoutEdited: boolean;
    reference: Hex;
    locked?: RedeemSnapshot;
  } = { reading: false, gen: 0, payoutEdited: false, reference: freshReference() };

  const amountInput = $<HTMLInputElement>('redeem-amount');
  const wholeInput = $<HTMLInputElement>('redeem-whole');
  const payoutInput = $<HTMLInputElement>('redeem-payout');
  const button = $<HTMLButtonElement>('redeem');

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
    for (const name of found) {
      let w: KreivoWallet;
      try {
        w = await enable(name, 'bezent bridge', rt.def.genesisHash as Hex);
      } catch {
        continue; // The user declined this extension; the others may still answer.
      }
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

  async function useAccount(a: KreivoAccount) {
    if (host.busy()) return;
    r.account = a;
    r.facts = undefined;
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
        ['KSM', `${formatAmount(f.ksm, 12)} KSM, for the fee and the storage deposit`],
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
    if (!r.payoutEdited) {
      const evm = host.evmAccount();
      payoutInput.value = evm ?? '';
    }
    r.payout = undefined;
    if (!payoutInput.value.trim()) {
      say(out, 'The connected EVM account is used when there is one.');
      return render();
    }
    const p = parsePayout(payoutInput.value);
    if (p.ok) {
      r.payout = p.address;
      say(out, `Paid to ${p.address}${host.evmAccount() === p.address ? ' (the connected EVM account)' : ''}.`, 'ok');
    } else say(out, p.reason, 'error');
    render();
  }

  // The reference.
  $('redeem-new-reference').addEventListener('click', () => {
    if (host.busy()) return;
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
    facts($('redeem-review'), rows);
    button.disabled = host.busy() || r.reading || !n || !r.facts || !live();
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
    const t = contract();
    const a = r.account;
    // The snapshot: from here on the redemption uses only this, never the form.
    const snap = live();
    if (typeof t === 'string' || !a || !snap) return;
    const step = progressList($('redeem-progress'));
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
    } finally {
      r.locked = undefined;
      host.setBusy(false);
      void readFacts();
    }
  }

  return {
    render,
    /** The network or Kreivo changed: what was read is for another chain. */
    reset() {
      r.gen++;
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
