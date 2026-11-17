// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The bridge web app's page (F-018, T-018-14): a deposit of USDC on the origin network through DirectedDeposit,
// naming a Kreivo recipient. Plain DOM, no framework. The seams for what comes next:
// - redeem on Kreivo (T-018-15) reuses kreivo/client.ts and lib/ss58.ts, and adds its own section;
// - the status page (T-018-17) starts from `DepositResult.arrival` (the coordinate) and lib/refs.ts.

import { DEPLOYMENT, NETWORKS, REVISION } from './config/build';
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

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

function facts(dl: HTMLElement, rows: [string, string][]) {
  dl.replaceChildren(
    ...rows.flatMap(([k, v]) => {
      const dt = document.createElement('dt');
      dt.textContent = k;
      const dd = document.createElement('dd');
      dd.textContent = v;
      return [dt, dd];
    }),
  );
}

function say(el: HTMLElement, text: string, kind: 'muted' | 'error' | 'ok' = 'muted') {
  el.textContent = text;
  el.className = kind;
}

const errorText = (e: unknown) =>
  (e as { shortMessage?: string }).shortMessage ?? (e instanceof Error ? e.message : String(e));

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
  busy: boolean;
}
const state: State = { busy: false };

// 1. Network.
const select = $<HTMLSelectElement>('network');
for (const n of NETWORKS) select.add(new Option(n.label, n.key));
$('build').textContent = `${DEPLOYMENT} build · ${REVISION}`;
select.addEventListener('change', () => void chooseNetwork());

async function chooseNetwork() {
  state.kreivo?.close();
  Object.assign(state, { network: undefined, session: undefined, contract: undefined, kreivo: undefined, check: undefined });
  const n = NETWORKS.find((x) => x.key === select.value)!;
  const err = $('network-error');
  err.hidden = true;
  facts($('network-facts'), [
    ['bezent network', n.network],
    ['Chain id', String(n.chainId)],
    ['DirectedDeposit', n.directedDeposit ?? 'not set in this build'],
    ['Token', n.token ?? 'not set in this build'],
    ['Mints on', `${n.kreivo.label}, ${n.kreivo.asset.symbol} (Here(${n.kreivo.asset.id}))`],
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
      b.addEventListener('click', () => void useWallet(w));
      return b;
    }),
  );
});
setTimeout(() => {
  if (!$('wallets').querySelector('button')) {
    $('wallets').innerHTML = '<p class="muted">No EVM wallet announced itself. Install or unlock one (MetaMask, Rabby…), then reload.</p>';
  }
}, 1500);

const hooked = new WeakSet<object>();

async function useWallet(w: Wallet) {
  state.wallet = w;
  if (!hooked.has(w.provider)) {
    hooked.add(w.provider);
    // Another account or chain in the wallet: read everything again rather than trust what was read.
    w.provider.on?.('accountsChanged', () => void (state.wallet === w && useWallet(w)));
    w.provider.on?.('chainChanged', () => void (state.wallet === w && useWallet(w)));
  }
  const n = state.network;
  if (!n) return;
  const out = $('wallet-facts');
  facts(out, [['Wallet', `${w.info.name}: connecting…`]]);
  try {
    const s = await connect(w.provider, n);
    state.session = s;
    state.contract = await readContract(s);
    state.balance = await balanceOf(s);
    const c = state.contract;
    facts(out, [
      ['Wallet', w.info.name],
      ['Depositor', s.account],
      ['Balance', `${formatAmount(state.balance, c.decimals)} ${c.symbol}`],
      ['Limits', `${formatAmount(c.minimum, c.decimals)} to ${formatAmount(c.maximum, c.decimals)} ${c.symbol}, from the contract`],
      ['Permit domain', `${c.tokenName} / ${c.tokenVersion} / ${n.chainId}`],
    ]);
  } catch (e) {
    state.session = undefined;
    facts(out, [['Wallet', `${w.info.name}: ${errorText(e)}`]]);
  }
  onAmount();
  render();
}

// 3. Recipient.
const recipientInput = $<HTMLInputElement>('recipient');
recipientInput.addEventListener('input', onRecipient);
function onRecipient() {
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
  const box = $('kreivo-accounts');
  const { getInjectedExtensions, connectInjectedExtension } = await import('polkadot-api/pjs-signer');
  const names = getInjectedExtensions();
  if (names.length === 0) {
    box.innerHTML = '<p class="muted">No Kreivo wallet (Talisman, SubWallet, Polkadot.js) is installed in this browser.</p>';
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
        b.addEventListener('click', () => {
          recipientInput.value = address;
          onRecipient();
        });
        buttons.push(b);
      }
    } catch {
      // The user declined this extension; the others may still answer.
    }
  }
  if (buttons.length) box.replaceChildren(...buttons);
  else box.innerHTML = '<p class="muted">No Kreivo account was shared.</p>';
}

/** The recipient pre-check (amendment 0029, Open 7): refuses, before signing, an account that cannot
 * receive the bridged asset. Returns the latest assessment. */
async function precheck(): Promise<Assessment | undefined> {
  const { recipient: r, kreivo: k, network: n } = state;
  if (!r || !k || !n) return undefined;
  const amount = state.amount ?? 1n;
  try {
    const f = await readRecipient(k, n.kreivo, r.account, amount);
    if (state.recipient?.account !== r.account) return undefined;
    const a = assessRecipient(f);
    state.check = a;
    say($('recipient-status'), a.ok ? a.note : `${a.reason} ${a.advice}`, a.ok ? 'ok' : 'error');
    render();
    return a;
  } catch (e) {
    state.check = undefined;
    say($('recipient-status'), `The account could not be read on Kreivo: ${errorText(e)}`, 'error');
    render();
    return undefined;
  }
}

// 4. Amount.
const amountInput = $<HTMLInputElement>('amount');
amountInput.addEventListener('input', onAmount);
function onAmount() {
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
  const { network: n, session: s, recipient: r, amount, contract: c, check } = state;
  const rows: [string, string][] = [];
  if (n) rows.push(['Network', `${n.label} (chain ${n.chainId})`], ['Contract', n.directedDeposit]);
  if (s) rows.push(['From', s.account], ['Refund to', `${s.account} (the depositor)`]);
  if (r && n) rows.push(['Recipient (Kreivo)', r.address], ['Recipient (32 bytes)', r.account]);
  if (amount !== undefined && c) rows.push(['Amount', `${formatAmount(amount, c.decimals)} ${c.symbol}`]);
  facts($('review'), rows);
  $<HTMLButtonElement>('deposit').disabled = state.busy || !(n && s && c && r && amount !== undefined && check?.ok);
}

$('deposit').addEventListener('click', () => void onDeposit());
async function onDeposit() {
  const { session: s, contract: c, recipient: r, amount } = state;
  if (!s || !c || !r || amount === undefined) return;
  const list = $('progress');
  list.replaceChildren();
  const step = (t: string, kind: 'muted' | 'error' | 'ok' = 'muted') => {
    const li = document.createElement('li');
    say(li, t, kind);
    list.append(li);
  };
  state.busy = true;
  render();
  try {
    step('Checking the recipient on Kreivo again.');
    const a = await precheck();
    if (!a?.ok) throw new Error('The recipient cannot receive the deposit; nothing was signed.');
    const res = await deposit(s, c, { amount, recipient: r.account }, (t) => step(t));
    step(`Deposited: ${res.hash}.`, 'ok');
    const rows: [string, string][] = [
      ['Transaction', res.hash],
      ['Signed with', res.method === 'permit' ? 'a permit (EIP-2612)' : 'an approval'],
    ];
    if (res.arrival) {
      const co = res.arrival.coordinate;
      rows.push(
        ['Coordinate', `${co.network} · block ${co.blockNumber} (${co.blockHash}) · log ${co.positionIndex}`],
        ['Bridge reference', depositReference(co)],
        ['Mint reference', mintReference(co)],
      );
      step('bezent mints once Polygon makes this block final. The references above hold for this block.');
    } else {
      step('The receipt has no directed arrival for this deposit; check the transaction.', 'error');
    }
    facts($('result'), rows);
    state.balance = await balanceOf(s);
  } catch (e) {
    step(errorText(e), 'error');
  } finally {
    state.busy = false;
    render();
  }
}

void chooseNetwork();
