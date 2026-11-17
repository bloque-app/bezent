// @vitest-environment happy-dom
// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// T-018-14 and T-018-15 (follow-ups from #135's review): the page's wiring, main.ts, in a DOM. The chain-facing modules
// are replaced by controllable fakes, so the tests decide when each wallet read and each send answers:
//
// - Deposit stays disabled while a wallet read is in flight, and a click then sends nothing;
// - an older wallet read that answers after a newer one began is dropped;
// - a wallet event during a deposit is kept and replayed once it ends, and Deposit stays disabled until that read
//   answers;
// - a wallet read in flight when a redemption starts drops its answer, and is read again once it ends;
// - while anything is in flight every control that changes what is sent is locked, the network among them;
// - when the chain recorded something else than the snapshot (the sender-mismatch branch), the page says so loudly,
//   shows what was recorded, and offers no status link;
// - a status link fills the status form;
// - from #138's review: a payout to one of bezent's own contracts is refused, and one to a contract is flagged; a
//   default payout that follows a new EVM account says so; a signature over another payload forgets the wallet's
//   metadata and the page offers it again; an unclear outcome keeps its reference locked until it settles.

import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Address, EIP1193Provider } from 'viem';
import type { Hex } from '../src/lib/bytes';

const ALICE: Hex = '0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d';
const ALICE_SS58 = 'HNZata7iMYWmk5RvZRTiAsSDhV8366zq2YGb3tLH5Upf74F';
const A1 = '0x1111111111111111111111111111111111111111' as Address;
const A2 = '0x2222222222222222222222222222222222222222' as Address;

const fake = vi.hoisted(() => ({
  connects: [] as { account: string; resolve: (s: unknown) => void }[],
  deposits: [] as { args: unknown[]; resolve: (r: unknown) => void; reject: (e: unknown) => void }[],
  redeems: [] as { resolve: (r: unknown) => void; reject: (e: unknown) => void }[],
  account: '0x1111111111111111111111111111111111111111',
  network: undefined as unknown,
  /** The code at any address, as the EVM wallet reads it. */
  code: '0x' as string,
  provided: 0,
  /** What the contract records for the reference, and the finalized head, for an unclear outcome. */
  recorded: undefined as unknown,
  head: 1,
}));

vi.mock('../src/config/build', async () => {
  const { network } = await import('./mock-wallet');
  fake.network = network;
  return { DEPLOYMENT: 'test', REVISION: 'test-revision', STATUS_ROUTE: null, NETWORKS: [network], CHAIN_SPEC_SHA256: { kreivo: '', kusama: '' } };
});

vi.mock('../src/kreivo/client', () => ({
  connectKreivo: async () => ({ client: {}, api: {}, close: () => undefined }),
}));

vi.mock('../src/kreivo/recipient', () => ({ readRecipient: async () => ({}) }));
vi.mock('../src/lib/recipient', () => ({ assessRecipient: () => ({ ok: true, note: 'The account can receive bUSDC.' }) }));

vi.mock('../src/evm/deposit', async (original) => {
  const real = await original<typeof import('../src/evm/deposit')>();
  return {
    ...real,
    // Each connect waits for the test; it answers with the account the wallet had when it was asked.
    connect: (_p: unknown, network: unknown) => {
      const account = fake.account;
      const pub = { getChainId: async () => (network as { chainId: number }).chainId, getCode: async () => fake.code };
      return new Promise((resolve) => fake.connects.push({ account, resolve: () => resolve({ network, account, chain: {}, wallet: {}, public: pub }) }));
    },
    readContract: async () => ({
      token: '0x41E94Eb019C0762f9Bfcf9Fb1E58725BfB0e7582',
      reserve: '0x000000000000000000000000000000000000Ba5e',
      minimum: 1_000_000n,
      maximum: 50_000_000_000n,
      tokenName: 'USDC',
      tokenVersion: '2',
      symbol: 'USDC',
      decimals: 6,
    }),
    balanceOf: async () => 100_000_000n,
    deposit: (...args: unknown[]) => new Promise((resolve, reject) => fake.deposits.push({ args, resolve, reject })),
  };
});

vi.mock('../src/kreivo/wallets', () => ({
  kreivoWalletNames: () => ['polkadot-js'],
  enableKreivoWallet: async () => ({
    name: 'polkadot-js',
    takesMetadata: true,
    provideMetadata: async () => (fake.provided++, 'provided'),
    accounts: [{ wallet: 'polkadot-js', name: 'Alice', account: ALICE, address: ALICE_SS58, signer: { publicKey: new Uint8Array(32) } }],
  }),
}));

vi.mock('../src/kreivo/redeem', async (original) => {
  const real = await original<typeof import('../src/kreivo/redeem')>();
  return {
    ...real,
    kreivoRuntime: async () => ({ extensions: ['PassAuthenticate', 'DynamicMaxBlockWeight'], def: { genesisHash: `0x${'c7'.repeat(32)}`, specVersion: 136 } }),
    redeemChain: () => ({
      facts: async () => ({
        bounds: { supplyCeiling: 1n << 60n, windowAmount: 1n << 50n, windowBlocks: 10, perTransferMax: 1n << 40n, redeemMinimum: 1_000_000n },
        backends: [{ kind: 'evm', chainId: 80002n }],
        pauses: [],
        balance: 10_000_000n,
        assetStatus: 'Liquid',
        ksm: 10n ** 12n,
        approval: undefined,
        approvalDeposit: 6_700_000_000n,
        at: { number: 1, hash: `0x${'00'.repeat(32)}` },
      }),
      redemption: async () => fake.recorded,
      finalized: async () => fake.head,
    }),
  };
});

vi.mock('../src/redeem', async (original) => {
  const real = await original<typeof import('../src/redeem')>();
  return { ...real, runRedeem: () => new Promise((resolve, reject) => fake.redeems.push({ resolve, reject })) };
});

// The DOM environment serves the test from a page URL, not a file: the page is read from the app's root, where the
// tests run.
const html = readFileSync(resolve(process.cwd(), 'index.html'), 'utf8');
const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const flush = async () => {
  for (let i = 0; i < 10; i++) await new Promise((r) => setTimeout(r, 0));
};
const type = (id: string, value: string) => {
  const el = $<HTMLInputElement>(id);
  el.value = value;
  el.dispatchEvent(new Event('input'));
};

let listeners: Record<string, (() => void)[]>;
/** Announces one EIP-6963 wallet, whose `accountsChanged` the test fires. */
function announce() {
  listeners = {};
  const provider = {
    request: async () => null,
    on: (e: string, f: () => void) => (listeners[e] ??= []).push(f),
  } as unknown as EIP1193Provider;
  window.dispatchEvent(Object.assign(new Event('eip6963:announceProvider'), { detail: { info: { uuid: 'u1', name: 'Mock', icon: '', rdns: 'test.mock' }, provider } }));
}
const walletChanged = (account: Address) => {
  fake.account = account;
  for (const f of listeners.accountsChanged ?? []) f();
};
/** Answers the oldest wallet read still waiting. */
const answerRead = async (i = 0) => {
  fake.connects.splice(i, 1)[0]!.resolve(undefined);
  await flush();
};

async function load() {
  vi.resetModules();
  fake.connects.length = 0;
  fake.deposits.length = 0;
  fake.redeems.length = 0;
  fake.account = A1;
  fake.code = '0x';
  fake.provided = 0;
  fake.recorded = undefined;
  fake.head = 1;
  // A storage of the test's own: Node's global localStorage, where it has one, is not the page's.
  const store = new Map<string, string>();
  vi.stubGlobal('localStorage', {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, String(v)),
    removeItem: (k: string) => void store.delete(k),
    clear: () => store.clear(),
  });
  document.body.innerHTML = html.slice(html.indexOf('<body>') + 6, html.indexOf('<script'));
  await import('../src/main');
  await flush();
  announce();
  ($('wallets').querySelector('button') as HTMLButtonElement).click();
  await flush();
  await answerRead();
}

const depositButton = () => $<HTMLButtonElement>('deposit');
const depositor = () => $('wallet-facts').textContent ?? '';

async function readyToDeposit() {
  await load();
  type('recipient', ALICE_SS58);
  type('amount', '2.5');
  await flush();
  expect(depositButton().disabled).toBe(false);
}

beforeEach(() => {
  vi.useRealTimers();
});
afterEach(() => {
  document.body.innerHTML = '';
  vi.unstubAllGlobals();
});

describe('the page’s wallet reads', () => {
  it('keeps Deposit disabled while a wallet read is in flight, and a click then sends nothing', async () => {
    await readyToDeposit();
    walletChanged(A2);
    await flush();
    expect(fake.connects).toHaveLength(1);
    expect(depositButton().disabled).toBe(true);
    depositButton().click();
    await flush();
    expect(fake.deposits).toHaveLength(0);
    await answerRead();
    expect(depositButton().disabled).toBe(false);
    expect(depositor()).toContain(A2);
  });

  it('drops an older read that answers after a newer one began', async () => {
    await readyToDeposit();
    walletChanged(A2);
    walletChanged(A1);
    await flush();
    expect(fake.connects.map((c) => c.account)).toEqual([A2, A1]);
    await answerRead(1); // The newer read, for A1, answers first.
    expect(depositor()).toContain(A1);
    expect(depositButton().disabled).toBe(false);
    await answerRead(0); // The older one, for A2, answers last: dropped.
    expect(depositor()).toContain(A1);
    expect(depositor()).not.toContain(A2);
  });
});

describe('a deposit in flight', () => {
  it('locks every control that changes what is sent, the network among them, and unlocks them after', async () => {
    await readyToDeposit();
    depositButton().click();
    await flush();
    expect(fake.deposits).toHaveLength(1);
    for (const id of ['network', 'recipient', 'amount', 'from-kreivo-wallet', 'redeem-connect', 'redeem-amount', 'redeem-whole', 'redeem-payout', 'redeem-new-reference', 'deposit', 'redeem']) {
      expect($<HTMLButtonElement>(id).disabled, id).toBe(true);
    }
    expect(($('wallets').querySelector('button') as HTMLButtonElement).disabled).toBe(true);
    // The status page reads only: it stays usable.
    expect($<HTMLButtonElement>('status-deposit').disabled).toBe(false);
    fake.deposits[0]!.reject(new Error('The user declined.'));
    await flush();
    expect($<HTMLSelectElement>('network').disabled).toBe(false);
    expect($('progress').textContent).toContain('The user declined.');
  });

  it('keeps a wallet event for after the deposit, and Deposit stays disabled until that read answers', async () => {
    await readyToDeposit();
    depositButton().click();
    await flush();
    walletChanged(A2);
    await flush();
    expect(fake.connects).toHaveLength(0); // Not read mid-deposit.
    fake.deposits[0]!.reject(new Error('The user declined.'));
    await flush();
    expect(fake.connects.map((c) => c.account)).toEqual([A2]); // Replayed.
    expect(depositButton().disabled).toBe(true);
    await answerRead();
    expect(depositor()).toContain(A2);
    expect(depositButton().disabled).toBe(false);
  });

  it('says loudly when the chain recorded another sender, shows what it recorded, and offers no status link', async () => {
    await readyToDeposit();
    depositButton().click();
    await flush();
    const network = fake.network as { network: string };
    const arrival = {
      coordinate: { network: network.network, blockNumber: 100n, blockHash: `0x${'ab'.repeat(32)}`, positionKind: 'log', positionIndex: 41 },
      sender: A2,
      amount: 2_500_000n,
      recipient: ALICE,
      refund: A1,
    };
    fake.deposits[0]!.resolve({ hash: `0x${'12'.repeat(32)}`, receipt: {}, arrival: undefined, arrivals: [arrival], directedLogs: 1, method: 'permit', reusedAllowance: false });
    await flush();
    const steps = $('progress').textContent ?? '';
    expect(steps).toContain(`sender ${A2}, not ${A1}`);
    expect(steps).toContain('bezent acts on what the chain recorded');
    expect($('progress').lastElementChild?.className).toBe('error');
    expect($('result').textContent).toContain('Recorded refund');
    expect($('deposit-link').textContent).toBe('');
  });

  it('links a deposit that matches its review to its status', async () => {
    await readyToDeposit();
    depositButton().click();
    await flush();
    const network = fake.network as { network: string };
    const arrival = {
      coordinate: { network: network.network, blockNumber: 100n, blockHash: `0x${'ab'.repeat(32)}`, positionKind: 'log', positionIndex: 41 },
      sender: A1,
      amount: 2_500_000n,
      recipient: ALICE,
      refund: A1,
    };
    fake.deposits[0]!.resolve({ hash: `0x${'12'.repeat(32)}`, receipt: {}, arrival, arrivals: [arrival], directedLogs: 1, method: 'permit', reusedAllowance: false });
    await flush();
    expect($('deposit-link').querySelector('a')?.getAttribute('href')).toBe(`#status=deposit&tx=0x${'12'.repeat(32)}&log=41`);
  });
});

describe('a redemption in flight', () => {
  async function readyToRedeem() {
    await load();
    $<HTMLButtonElement>('redeem-connect').click();
    await flush();
    ($('redeem-accounts').querySelector('button') as HTMLButtonElement).click();
    await flush();
    type('redeem-amount', '2');
    await flush();
    expect($<HTMLInputElement>('redeem-payout').value).toBe(A1); // The connected EVM account, by default.
    expect($<HTMLButtonElement>('redeem').disabled).toBe(false);
  }

  it('drops the answer of a wallet read in flight when it starts, and reads the wallet again once it ends', async () => {
    await readyToRedeem();
    walletChanged(A2);
    await flush();
    expect(fake.connects).toHaveLength(1);
    $<HTMLButtonElement>('redeem').click();
    await flush();
    expect(fake.redeems).toHaveLength(1);
    await answerRead(); // Answers mid-redemption: dropped.
    expect(depositor()).not.toContain(A2);
    expect($<HTMLSelectElement>('network').disabled).toBe(true);
    fake.redeems[0]!.reject(new Error('The wallet did not sign.'));
    await flush();
    expect(fake.connects.map((c) => c.account)).toEqual([A2]); // Read again.
    await answerRead();
    expect(depositor()).toContain(A2);
    expect($('redeem-progress').textContent).toContain('The wallet did not sign.');
  });

  it('keeps the payout address the user typed when the EVM account changes', async () => {
    await readyToRedeem();
    type('redeem-payout', A2);
    walletChanged(A1);
    await flush();
    await answerRead();
    expect($<HTMLInputElement>('redeem-payout').value).toBe(A2);
  });
});

describe('the payout address', () => {
  async function connected() {
    await load();
    $<HTMLButtonElement>('redeem-connect').click();
    await flush();
    ($('redeem-accounts').querySelector('button') as HTMLButtonElement).click();
    await flush();
    type('redeem-amount', '2');
    await flush();
  }
  const network = () => fake.network as { reserve: Address; token: Address; directedDeposit: Address; emitter: Address };

  it('refuses bezent’s own contracts, in any case', async () => {
    await connected();
    for (const a of [network().reserve, network().token.toLowerCase(), network().directedDeposit, network().emitter.toLowerCase()]) {
      type('redeem-payout', a);
      await flush();
      expect($('redeem-payout-status').textContent).toMatch(/not yours: USDC paid to it would not reach you/);
      expect($<HTMLButtonElement>('redeem').disabled).toBe(true);
    }
    type('redeem-payout', A2);
    await flush();
    expect($<HTMLButtonElement>('redeem').disabled).toBe(false);
  });

  it('warns, without refusing, when the address is a contract', async () => {
    await connected();
    fake.code = '0x6080';
    type('redeem-payout', A2);
    await flush();
    expect($('redeem-payout-warning').textContent).toMatch(/is a contract on .*not a plain account/);
    expect($<HTMLButtonElement>('redeem').disabled).toBe(false);
    fake.code = '0x';
    type('redeem-payout', A1);
    await flush();
    expect($('redeem-payout-warning').textContent).toBe('');
  });

  it('says so when the default follows a new EVM account', async () => {
    await connected();
    expect($<HTMLInputElement>('redeem-payout').value).toBe(A1);
    walletChanged(A2);
    await flush();
    await answerRead();
    expect($<HTMLInputElement>('redeem-payout').value).toBe(A2);
    expect($('redeem-payout-status').textContent).toBe(`The payout address changed to ${A2}: it follows the connected EVM account, which changed (it was ${A1}). Check it before you redeem.`);
    expect($('redeem-payout-status').className).toBe('warn');
  });
});

describe('a signature over another payload, and an unclear outcome', () => {
  const metadataKey = `bezent.bridge.metadata/polkadot-js/0x${'c7'.repeat(32)}/136/PassAuthenticate,DynamicMaxBlockWeight`;
  async function redeeming() {
    await load();
    $<HTMLButtonElement>('redeem-connect').click();
    await flush();
    ($('redeem-accounts').querySelector('button') as HTMLButtonElement).click();
    await flush();
    type('redeem-amount', '2');
    await flush();
    const reference = $('redeem-reference').textContent!;
    $<HTMLButtonElement>('redeem').click();
    await flush();
    expect(fake.redeems).toHaveLength(1);
    // main.ts's modules, as this load imported them.
    return { reference, ...(await import('../src/kreivo/redeem')), ...(await import('../src/kreivo/extensions')) };
  }

  it('forgets that the wallet took the metadata, and offers it again on request', async () => {
    const { WalletSignatureMismatch } = await redeeming();
    expect(localStorage.getItem(metadataKey)).toBe('1');
    expect(fake.provided).toBe(1);
    fake.redeems[0]!.reject(new WalletSignatureMismatch('The wallet’s signature does not verify. Nothing was sent.'));
    await flush();
    expect(localStorage.getItem(metadataKey)).toBeNull();
    expect($('redeem-progress').textContent).toContain('Offer Kreivo’s metadata to the wallets again');
    $<HTMLButtonElement>('redeem-offer-metadata').click();
    await flush();
    expect(fake.provided).toBe(2);
    expect(localStorage.getItem(metadataKey)).toBe('1');
    expect($('redeem-wallet-status').textContent).toBe('polkadot-js took Kreivo’s metadata.');
  });

  it('keeps the reference locked while the redemption may still be included, with no “pick another”', async () => {
    const { reference, UnclearOutcome } = await redeeming();
    fake.head = 1_000;
    fake.redeems[0]!.reject(new UnclearOutcome('The redemption 0x77… was broadcast, but following it failed.', `0x${'77'.repeat(32)}`, 1_064));
    await flush();
    expect($('redeem-reference').textContent).toBe(reference);
    expect($<HTMLButtonElement>('redeem-new-reference').disabled).toBe(true);
    $<HTMLButtonElement>('redeem-new-reference').click();
    await flush();
    expect($('redeem-reference').textContent).toBe(reference);
    expect($('redeem-review').textContent).toMatch(/locked: the redemption 0x7{64} may still be included/);
    expect($('redeem-progress').textContent).toMatch(/until block 1064 is final; the reference stays locked meanwhile/);
  });

  it('keeps Redeem off, and refuses a second redemption, until the first settles (L6)', async () => {
    const { UnclearOutcome } = await redeeming();
    fake.head = 1_000;
    fake.redeems[0]!.reject(new UnclearOutcome('The redemption 0x77… was broadcast, but following it failed.', `0x${'77'.repeat(32)}`, 1_064));
    await flush();
    expect($<HTMLButtonElement>('redeem').disabled).toBe(true);
    expect($('redeem-review').textContent).toMatch(/Redeem is off/);
    // A click that reaches the handler anyway (the attribute is not the lock) starts nothing.
    $<HTMLButtonElement>('redeem').disabled = false;
    $<HTMLButtonElement>('redeem').click();
    await flush();
    expect(fake.redeems).toHaveLength(1);
    expect($('redeem-progress').textContent).toMatch(/no other is sent until Kreivo records it.* Nothing was signed/);
  });

  it('frees the reference once Kreivo records the redemption, and links to its status', async () => {
    const { reference, UnclearOutcome } = await redeeming();
    fake.recorded = {
      redeemer: ALICE,
      asset: 101,
      amount: 2_000_000n,
      backend: { kind: 'evm', chainId: 80002n },
      payout: { kind: 'address', address: A1 },
      refunded: false,
    };
    fake.redeems[0]!.reject(new UnclearOutcome('The redemption 0x77… was broadcast, but following it failed.', `0x${'77'.repeat(32)}`, 1_064));
    await flush();
    expect($('redeem-progress').textContent).toMatch(/Kreivo records the redemption \(final by block 1\): 2 bUSDC, paid to 0x1{40}/);
    expect($('redeem-link').querySelector('a')?.getAttribute('href')).toBe(`#status=withdrawal&redeemer=${ALICE_SS58}&reference=${reference}`);
    expect($('redeem-reference').textContent).not.toBe(reference);
    expect($<HTMLButtonElement>('redeem-new-reference').disabled).toBe(false);
  });
});

describe('status links', () => {
  it('fill the status form from the page’s fragment', async () => {
    await load();
    const reference = `0x${'ab'.repeat(32)}`;
    location.hash = `#status=withdrawal&redeemer=${ALICE_SS58}&reference=${reference}`;
    window.dispatchEvent(new Event('hashchange'));
    await flush();
    expect($<HTMLInputElement>('status-redeemer').value).toBe(ALICE_SS58);
    expect($<HTMLInputElement>('status-reference').value).toBe(reference);
  });
});
