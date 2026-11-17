// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The bridge web app's page (F-018): a deposit of USDC on the origin network through DirectedDeposit, naming a Kreivo
// recipient (T-018-14); a redemption of bUSDC on Kreivo (T-018-15, ui/redeem.ts); and the status of either, from
// chain data (T-018-17, ui/status.ts). Plain DOM, no framework.
//
// One value-moving action at a time: while a deposit or a redemption is in flight (`state.busy`) every control that
// changes what is sent is locked, and wallet events are kept and replayed once it ends. A wallet read is tagged with
// a generation, so an answer that arrives after the page moved on (another wallet event, or a deposit that started)
// is dropped, and Deposit stays disabled while a read is in flight.

import { DEPLOYMENT, NETWORKS, REVISION, STATUS_ROUTE } from './config/build';
import { resolveNetwork, type ResolvedNetwork } from './config/networks';
import { discoverWallets, type Wallet } from './evm/eip6963';
import { balanceOf, connect, deposit, readContract, type ContractFacts, type Session } from './evm/deposit';
import { checkAmount, formatAmount, parseAmount } from './lib/amount';
import { assessRecipient, type Assessment } from './lib/recipient';
import { depositReference, mintReference } from './lib/refs';
import { decodeSs58, encodeSs58, parseRecipient } from './lib/ss58';
import { fromHex, type Hex } from './lib/bytes';
import { connectKreivo, type Kreivo } from './kreivo/client';
import { readRecipient } from './kreivo/recipient';
import { LOCKED_CONTAINERS, LOCKED_IDS, lockControls, reviewRows, runDeposit, takeSnapshot, type Lockable, type Snapshot } from './review';
import { $, errorText, facts, note, progressList, say } from './ui/dom';
import { initRedeem, REDEEM_LOCKED_CONTAINERS, REDEEM_LOCKED_IDS } from './ui/redeem';
import { depositLink, initStatus, withdrawalLink } from './ui/status';

interface State {
  network?: ResolvedNetwork;
  wallet?: Wallet;
  session?: Session;
  contract?: ContractFacts;
  balance?: bigint;
  recipient?: { account: Hex; address: string };
  amount?: bigint;
  check?: Assessment;
  kreivo?: Kreivo;
  /** While a deposit is in flight: what was reviewed, frozen at the click. Every input is locked meanwhile. */
  locked?: Snapshot;
  /** A wallet event that arrived during a deposit or a redemption, or a read that was dropped for one; replayed once
   * it ends. */
  walletChanged?: Wallet;
  /** A deposit or a redemption is in flight. */
  busy: boolean;
  /** A wallet read is in flight: its answer is not in yet, so Deposit waits for it. */
  reading: boolean;
  /** The generation of the latest wallet read; an answer of an older one is dropped. */
  walletGen: number;
}
const state: State = { busy: false, reading: false, walletGen: 0 };

/** Every control that changes what is deposited or redeemed: the network, the wallets, the recipient, the amounts,
 * the payout address and the reference. */
function controls(): Lockable[] {
  return [
    ...[...LOCKED_IDS, ...REDEEM_LOCKED_IDS].map((id) => $<HTMLInputElement | HTMLSelectElement | HTMLButtonElement>(id)),
    ...[...LOCKED_CONTAINERS, ...REDEEM_LOCKED_CONTAINERS].flatMap((id) => [...$(id).querySelectorAll('button')]),
  ];
}

// The redeem (section 6) and the status page (section 7), wired first: the deposit's code draws them too.
const redeem = initRedeem({
  network: () => state.network,
  kreivo: () => state.kreivo,
  evmAccount: () => state.session?.account,
  busy: () => state.busy,
  setBusy: (b) => setBusy(b),
  statusLink: withdrawalLink,
});
const status = initStatus({ network: () => state.network, kreivo: () => state.kreivo, session: () => state.session, route: STATUS_ROUTE });
window.addEventListener('hashchange', () => status.follow(location.hash));

// 1. Network.
const select = $<HTMLSelectElement>('network');
for (const n of NETWORKS) {
  const o = document.createElement('option');
  o.value = n.key;
  o.textContent = n.label;
  select.append(o);
}
$('build').textContent = `${DEPLOYMENT} build · ${REVISION}`;
select.addEventListener('change', () => void chooseNetwork());

async function chooseNetwork() {
  if (state.busy) return;
  state.kreivo?.close();
  state.walletGen++;
  Object.assign(state, { network: undefined, session: undefined, contract: undefined, kreivo: undefined, check: undefined, reading: false });
  redeem.reset();
  const n = NETWORKS.find((x) => x.key === select.value)!;
  const err = $('network-error');
  err.hidden = true;
  facts($('network-facts'), [
    ['bezent network', n.network],
    ['Chain id', String(n.chainId)],
    ['DirectedDeposit', n.directedDeposit ?? 'not set in this build'],
    ['Token', n.token ?? 'not set in this build'],
    ['Reserve', n.reserve ?? 'not set in this build'],
    ['Code hash', n.codeHash ?? 'not set in this build'],
    ['Mints on', n.kreivo ? `${n.kreivo.label}, ${n.kreivo.asset.symbol} (Here(${n.kreivo.asset.id}))` : 'not set in this build'],
    ['bezent contract (Kreivo)', n.kreivo?.bezent ? encodeSs58(fromHex(n.kreivo.bezent), n.kreivo.ss58Prefix) : 'not set in this build'],
    ['Reference emitter', n.emitter ?? 'not set in this build'],
    ['Status route', STATUS_ROUTE ?? 'not set in this build: status from chain data alone'],
  ]);
  try {
    state.network = resolveNetwork(n);
  } catch (e) {
    err.textContent = errorText(e);
    err.hidden = false;
    render();
    return;
  }
  void startKreivo(state.network);
  if (state.wallet) await useWallet(state.wallet);
  render();
}

async function startKreivo(n: ResolvedNetwork) {
  const status = $('kreivo-status');
  const how = n.kreivo.connection.kind === 'light-client' ? 'light client' : 'local node';
  say(status, `Connecting to ${n.kreivo.label} (${how})…`);
  try {
    const k = await connectKreivo(n.kreivo);
    if (state.network !== n) return k.close();
    state.kreivo = k;
    say(status, `Reading ${n.kreivo.label} through the ${how}.`);
    void precheck();
    render();
  } catch (e) {
    say(status, `Kreivo is not reachable: ${errorText(e)}`, 'error');
  }
}

// 2. EVM wallet.
discoverWallets((wallets) => {
  const box = $('wallets');
  box.replaceChildren(
    ...wallets.map((w) => {
      const b = document.createElement('button');
      b.type = 'button';
      if (w.info.icon) {
        const img = document.createElement('img');
        img.src = w.info.icon;
        img.alt = '';
        b.append(img);
      }
      b.append(w.info.name);
      b.disabled = state.busy;
      b.addEventListener('click', () => void useWallet(w));
      return b;
    }),
  );
});
setTimeout(() => {
  if (!$('wallets').querySelector('button')) {
    $('wallets').replaceChildren(note('No EVM wallet announced itself. Install or unlock one (MetaMask, Rabby…), then reload.'));
  }
}, 1500);

const hooked = new WeakSet<object>();

async function useWallet(w: Wallet) {
  if (state.busy) {
    // Mid-deposit or mid-redemption: it runs on what was reviewed; read the wallet again once it ends.
    state.walletChanged = w;
    return;
  }
  state.wallet = w;
  if (!hooked.has(w.provider)) {
    hooked.add(w.provider);
    // Another account or chain in the wallet: read everything again rather than trust what was read.
    w.provider.on?.('accountsChanged', () => void (state.wallet === w && useWallet(w)));
    w.provider.on?.('chainChanged', () => void (state.wallet === w && useWallet(w)));
  }
  const n = state.network;
  if (!n) return render();
  const gen = ++state.walletGen;
  // Dropped when a newer read started, or when a deposit or a redemption started meanwhile: then it is read again
  // once that ends.
  const stale = () => {
    if (gen !== state.walletGen) return true;
    if (state.busy) {
      state.walletChanged = w;
      return true;
    }
    return false;
  };
  state.reading = true;
  render();
  const out = $('wallet-facts');
  facts(out, [['Wallet', `${w.info.name}: connecting…`]]);
  try {
    const s = await connect(w.provider, n);
    if (stale()) return;
    const c = await readContract(s);
    if (stale()) return;
    const balance = await balanceOf(s);
    if (stale()) return;
    Object.assign(state, { session: s, contract: c, balance });
    facts(out, [
      ['Wallet', w.info.name],
      ['Depositor', s.account],
      ['Balance', `${formatAmount(balance, c.decimals)} ${c.symbol}`],
      ['Limits', `${formatAmount(c.minimum, c.decimals)} to ${formatAmount(c.maximum, c.decimals)} ${c.symbol}, from the contract`],
      ['Permit domain', `${c.tokenName} / ${c.tokenVersion} / ${n.chainId}`],
    ]);
  } catch (e) {
    if (stale()) return;
    state.session = undefined;
    facts(out, [['Wallet', `${w.info.name}: ${errorText(e)}`]]);
  } finally {
    if (gen === state.walletGen) state.reading = false;
  }
  if (gen !== state.walletGen) return;
  onAmount();
  redeem.onEvmAccount();
  render();
}

// 3. Recipient.
const recipientInput = $<HTMLInputElement>('recipient');
recipientInput.addEventListener('input', onRecipient);
function onRecipient() {
  if (state.busy) return;
  const prefix = state.network?.kreivo.ss58Prefix ?? 2;
  const r = parseRecipient(recipientInput.value, prefix);
  state.check = undefined;
  if (r.ok) {
    state.recipient = { account: r.account, address: r.address };
    say($('recipient-status'), 'Checking the account on Kreivo…');
    void precheck();
  } else {
    state.recipient = undefined;
    say($('recipient-status'), recipientInput.value.trim() ? r.reason : '', 'error');
  }
  render();
}

$('from-kreivo-wallet').addEventListener('click', () => void kreivoAccounts());
async function kreivoAccounts() {
  if (state.busy) return;
  const box = $('kreivo-accounts');
  const { getInjectedExtensions, connectInjectedExtension } = await import('polkadot-api/pjs-signer');
  const names = getInjectedExtensions();
  if (names.length === 0) {
    box.replaceChildren(note('No Kreivo wallet (Talisman, SubWallet, Polkadot.js) is installed in this browser.'));
    return;
  }
  const prefix = state.network?.kreivo.ss58Prefix ?? 2;
  const buttons: HTMLElement[] = [];
  for (const name of names) {
    try {
      const ext = await connectInjectedExtension(name, 'bezent bridge');
      for (const a of ext.getAccounts()) {
        const d = decodeSs58(a.address);
        if (!d) continue;
        const address = encodeSs58(fromHex(d.account), prefix);
        const b = document.createElement('button');
        b.type = 'button';
        b.textContent = `${a.name ?? name}: ${address.slice(0, 8)}…${address.slice(-6)}`;
        b.disabled = state.busy;
        b.addEventListener('click', () => {
          if (state.busy) return;
          recipientInput.value = address;
          onRecipient();
        });
        buttons.push(b);
      }
    } catch {
      // The user declined this extension; the others may still answer.
    }
  }
  box.replaceChildren(...(buttons.length ? buttons : [note('No Kreivo account was shared.')]));
}

/** The recipient pre-check (amendment 0029, Open 7): refuses, before signing, an account that cannot
 * receive the bridged asset. On the live form, as the user types; the deposit checks its snapshot instead
 * (`recheck`). */
async function precheck(): Promise<Assessment | undefined> {
  const { recipient: r, network: n } = state;
  if (!r || !n || state.busy) return undefined;
  const amount = state.amount ?? 1n;
  // The form may move on while Kreivo answers: then this answer is for another account or amount, and dropped.
  const current = () => !state.busy && state.recipient?.account === r.account && (state.amount ?? 1n) === amount;
  const a = await recheck(r.account, amount, current);
  if (!current()) return undefined;
  state.check = a;
  render();
  return a;
}

/** Checks exactly `account` for exactly `amount` on Kreivo, and says what it found while `current()`. Never
 * reads the form. A read that times out fails closed (`undefined`), with the reason shown. */
async function recheck(account: Hex, amount: bigint, current: () => boolean = () => true): Promise<Assessment | undefined> {
  const { kreivo: k, network: n } = state;
  const out = $('recipient-status');
  const tell = (text: string, kind: 'muted' | 'error' | 'ok') => current() && say(out, text, kind);
  if (!k || !n) {
    tell('Kreivo is not connected yet, so the account cannot be checked.', 'error');
    return undefined;
  }
  try {
    const a = assessRecipient(await readRecipient(k, n.kreivo, account, amount));
    tell(a.ok ? a.note : `${a.reason} ${a.advice}`, a.ok ? 'ok' : 'error');
    return a;
  } catch (e) {
    tell(`The account could not be read on Kreivo: ${errorText(e)}`, 'error');
    return undefined;
  }
}

// 4. Amount.
const amountInput = $<HTMLInputElement>('amount');
amountInput.addEventListener('input', onAmount);
function onAmount() {
  if (state.busy) return;
  const c = state.contract;
  const out = $('amount-status');
  state.amount = undefined;
  if (!amountInput.value.trim()) {
    say(out, c ? `Between ${formatAmount(c.minimum, c.decimals)} and ${formatAmount(c.maximum, c.decimals)} ${c.symbol}.` : '');
  } else if (!c) {
    say(out, 'Connect a wallet first: the limits are read from the contract.');
  } else {
    const p = parseAmount(amountInput.value, c.decimals);
    const ok = p.ok ? checkAmount(p.units, c, state.balance) : p;
    if (p.ok && ok.ok) {
      state.amount = p.units;
      say(out, `${formatAmount(p.units, c.decimals)} ${c.symbol} becomes ${formatAmount(p.units, c.decimals)} ${state.network?.kreivo.asset.symbol} on Kreivo.`);
      void precheck();
    } else if (!ok.ok) {
      say(out, ok.reason, 'error');
    }
  }
  render();
}

// 5. Review and deposit.
function render() {
  const { network: n, session: s, recipient: r, amount, contract: c, check, locked } = state;
  lockControls(controls(), state.busy);
  const rows: [string, string][] = [];
  if (n) rows.push(['Network', `${n.label} (chain ${n.chainId})`], ['Contract', n.directedDeposit]);
  if (locked && c) {
    // The deposit in flight: what was reviewed at the click, not the form.
    rows.push(...reviewRows(locked, c), ['Review', 'Locked until this deposit ends.']);
  } else {
    if (s) rows.push(['From', s.account], ['Refund to', `${s.account} (the depositor)`]);
    if (r && n) rows.push(['Recipient (Kreivo)', encodeSs58(fromHex(r.account), n.kreivo.ss58Prefix)], ['Recipient (32 bytes)', r.account]);
    if (amount !== undefined && c) rows.push(['Amount', `${formatAmount(amount, c.decimals)} ${c.symbol}`]);
  }
  facts($('review'), rows);
  $<HTMLButtonElement>('deposit').disabled = state.busy || state.reading || !(n && s && c && r && amount !== undefined && check?.ok);
  redeem.render();
}

$('deposit').addEventListener('click', () => void onDeposit());
async function onDeposit() {
  if (state.busy || state.reading) return;
  const { session: s, contract: c, network: n } = state;
  if (!s || !c || !n) return;
  const prefix = n.kreivo.ss58Prefix;
  // The snapshot: from here on the deposit uses only this, never the form.
  const snap = takeSnapshot(state, prefix);
  if (!snap) return;
  const step = progressList($('progress'));
  $('result').replaceChildren();
  $('deposit-link').replaceChildren();
  setBusy(true, snap);
  try {
    const { res, comparison: cmp } = await runDeposit(snap, s, c, prefix, { recheck: (a, m) => recheck(a, m), deposit }, (t) => step(t));
    step(`Sent and included: ${res.hash}.`, cmp.ok ? 'ok' : 'muted');
    if (res.reusedAllowance) step('An allowance already in place for the contract was used; no new approval was sent.');
    const rows: [string, string][] = [
      ['Transaction', res.hash],
      ['Signed with', res.method === 'permit' ? 'a permit (EIP-2612)' : 'an approval'],
    ];
    if (cmp.recorded) {
      rows.push(
        ['Recorded recipient (Kreivo)', cmp.recorded.recipient],
        ['Recorded amount', `${formatAmount(cmp.recorded.amount, c.decimals)} ${c.symbol}`],
        ['Recorded refund', cmp.recorded.refund],
      );
    }
    if (cmp.ok) {
      const co = cmp.arrival.coordinate;
      rows.push(
        ['Coordinate', `${co.network} · block ${co.blockNumber} (${co.blockHash}) · log ${co.positionIndex}`],
        ['Bridge reference', depositReference(co)],
        ['Mint reference', mintReference(co)],
      );
      step('The Directed log matches what you reviewed. bezent mints once Polygon makes this block final; the references above hold for this block.', 'ok');
      const link = document.createElement('a');
      link.href = depositLink(res.hash, co.positionIndex);
      link.textContent = 'Follow this deposit’s status';
      $('deposit-link').replaceChildren(link);
    } else {
      step(`${cmp.reason} bezent acts on what the chain recorded, shown above, not on what you reviewed. Keep the transaction hash.`, 'error');
    }
    facts($('result'), rows);
    state.balance = await balanceOf(s);
  } catch (e) {
    step(errorText(e), 'error');
  } finally {
    setBusy(false);
  }
}

/** Locks or unlocks the page for a deposit (with its snapshot) or a redemption. On unlock, a wallet event kept
 * meanwhile is replayed, and Deposit stays disabled until that read answers. */
function setBusy(busy: boolean, snap?: Snapshot) {
  if (busy) {
    // Any wallet read in flight is for the form, not for what is sent: drop its answer, and read again after.
    if (state.reading && state.wallet) state.walletChanged = state.wallet;
    state.walletGen++;
    state.reading = false;
    state.busy = true;
    state.locked = snap;
    render();
    return;
  }
  state.busy = false;
  state.locked = undefined;
  const w = state.walletChanged;
  state.walletChanged = undefined;
  if (w) void useWallet(w);
  else render();
}


void chooseNetwork().then(() => status.follow(location.hash));
