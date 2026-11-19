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
  /** A second origin network in the build, to select away from a record's own. */
  other: undefined as unknown,
  /** Runs when the contract's record is read: where a test lets another tab act mid-watch. */
  onRedemption: undefined as (() => void) | undefined,
  /** The code at any address, as the EVM wallet reads it. */
  code: '0x' as string,
  provided: 0,
  /** What the contract records for the reference, and the finalized head, for an unclear outcome. */
  recorded: undefined as unknown,
  head: 1,
  /** The finalized head cannot be read. */
  fail: false,
  /** The Web Locks held, shared by every page of a test as a browser shares them across its tabs. */
  held: new Set<string>(),
  /** The names of the Web Locks the page asked for, in order. */
  asked: [] as string[],
  /** Kreivo's light client has no finalized block until the test says so (`load({ hold: true })`). */
  hold: false,
  sync: undefined as { resolve: () => void; reject: (e: unknown) => void } | undefined,
  connects2: 0,
}));

vi.mock('../src/config/build', async () => {
  const { network } = await import('./mock-wallet');
  fake.network = network;
  const other = { ...network, key: 'other-net', label: 'Other', network: 'other-net', chainId: 80003 };
  fake.other = other;
  return { DEPLOYMENT: 'test', REVISION: 'test-revision', STATUS_ROUTE: null, NETWORKS: [network, other], CHAIN_SPEC_SHA256: { kreivo: '', kusama: '' } };
});

vi.mock('../src/kreivo/client', () => ({
  connectKreivo: async () => {
    fake.connects2++;
    return {
      client: {},
      api: {},
      synced: fake.hold ? new Promise<void>((resolve, reject) => (fake.sync = { resolve, reject })) : Promise.resolve(),
      close: () => undefined,
    };
  },
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
      redemption: async () => {
        fake.onRedemption?.();
        return fake.recorded;
      },
      finalized: async () => {
        if (fake.fail) throw new Error('the light client is syncing');
        return fake.head;
      },
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

/** The page's storage, kept across `load({ keep: true })` as a browser keeps it across a reload. */
let store = new Map<string, string>();

/** A Web Locks manager over `fake.held`: `ifAvailable` answers `null` while another holds the name. */
const webLocks = {
  request: async (name: string, opts: { ifAvailable?: boolean }, cb: (lock: { name: string; mode: string } | null) => Promise<unknown>) => {
    fake.asked.push(name);
    if (fake.held.has(name)) {
      if (!opts.ifAvailable) throw new Error('the fake waits for no one');
      return cb(null);
    }
    fake.held.add(name);
    try {
      return await cb({ name, mode: 'exclusive' });
    } finally {
      fake.held.delete(name);
    }
  },
};

async function load(o: { keep?: boolean; blocked?: boolean; stuck?: boolean; unreadableKey?: string; noLocks?: boolean; full?: boolean; hold?: boolean } = {}) {
  vi.resetModules();
  fake.hold = !!o.hold;
  fake.sync = undefined;
  fake.connects2 = 0;
  fake.connects.length = 0;
  fake.deposits.length = 0;
  fake.redeems.length = 0;
  fake.account = A1;
  fake.code = '0x';
  fake.provided = 0;
  fake.recorded = undefined;
  fake.head = 1;
  fake.fail = false;
  fake.onRedemption = undefined;
  fake.held.clear();
  fake.asked.length = 0;
  // happy-dom has no Web Locks (`navigator.locks` is null): the page gets the fake, or none.
  Object.defineProperty(navigator, 'locks', { configurable: true, get: () => (o.noLocks ? null : webLocks) });
  // A storage of the test's own: Node's global localStorage, where it has one, is not the page's.
  if (!o.keep) store = new Map<string, string>();
  const refused = () => {
    throw new DOMException('blocked', 'SecurityError');
  };
  vi.stubGlobal(
    'localStorage',
    o.blocked
      ? { getItem: refused, setItem: refused, removeItem: refused, clear: refused, key: refused, get length(): number { return refused(); } }
      : {
          getItem: (k: string) => {
            if (k === o.unreadableKey) throw new DOMException('unreadable', 'UnknownError');
            return store.get(k) ?? null;
          },
          setItem: (k: string, v: string) => {
            if (o.full && k.startsWith('bezent.bridge.pending/')) throw new DOMException('full', 'QuotaExceededError');
            store.set(k, String(v));
          },
          removeItem: (k: string) => {
            if (o.stuck && k.startsWith('bezent.bridge.pending/')) throw new DOMException('stuck', 'QuotaExceededError');
            store.delete(k);
          },
          clear: () => store.clear(),
          key: (i: number) => [...store.keys()][i] ?? null,
          get length() {
            return store.size;
          },
        },
  );
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

// The page registers its listeners on `window` and never removes them; a page of an earlier test must not hear this
// test's events (a `storage` event would wake it), so they are removed with the test.
const registered: [string, EventListenerOrEventListenerObject][] = [];
const addEventListener = window.addEventListener.bind(window);
beforeEach(() => {
  vi.useRealTimers();
  window.addEventListener = ((type: string, fn: EventListenerOrEventListenerObject, o?: boolean | AddEventListenerOptions) => {
    registered.push([type, fn]);
    addEventListener(type, fn, o);
  }) as typeof window.addEventListener;
});
afterEach(() => {
  window.addEventListener = addEventListener;
  for (const [type, fn] of registered.splice(0)) window.removeEventListener(type, fn);
  document.body.innerHTML = '';
  vi.unstubAllGlobals();
  delete (navigator as unknown as Record<string, unknown>).locks;
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

describe('leaving the page', () => {
  it('is not warned about when nothing is pending', async () => {
    await load();
    const e = new Event('beforeunload', { cancelable: true });
    window.dispatchEvent(e);
    expect(e.defaultPrevented).toBe(false);
  });
});

describe('a signature over another payload, and an unclear outcome', () => {
  const metadataKey = `bezent.bridge.metadata/polkadot-js/0x${'c7'.repeat(32)}/136/PassAuthenticate,DynamicMaxBlockWeight`;
  const pendingKey = `bezent.bridge.pending/v1/kreivo-test/${ALICE}`;
  async function redeeming(o: Parameters<typeof load>[0] = {}) {
    await load(o);
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
    expect($('redeem-review').textContent).toMatch(/locked: the redemption 0x7{64} .* may still be included/);
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

  it('keeps an unclear redemption locked across a change of network (L6)', async () => {
    const { reference, UnclearOutcome } = await redeeming();
    fake.head = 1_000;
    fake.redeems[0]!.reject(new UnclearOutcome('The redemption 0x77… was broadcast, but following it failed.', `0x${'77'.repeat(32)}`, 1_064));
    await flush();
    // Switching networks re-reads everything, but must not free the reference or allow a second redemption.
    $<HTMLSelectElement>('network').dispatchEvent(new Event('change'));
    await flush();
    expect($('redeem-reference').textContent).toBe(reference);
    expect($<HTMLButtonElement>('redeem').disabled).toBe(true);
    expect($<HTMLButtonElement>('redeem-new-reference').disabled).toBe(true);
  });

  async function pend(o: Parameters<typeof load>[0] & { fail?: boolean } = {}) {
    const ctx = await redeeming(o);
    fake.head = 1_000;
    fake.fail = !!o.fail;
    fake.redeems[0]!.reject(new ctx.UnclearOutcome('The redemption 0x77… was broadcast, but following it failed.', `0x${'77'.repeat(32)}`, 1_064));
    await flush();
    return ctx;
  }
  const lockRow = () => $('redeem-review').textContent ?? '';

  it('keeps the lock across a reload: the record is stored, and a fresh page with the same storage starts with Redeem off', async () => {
    const { reference } = await pend();
    const stored = JSON.parse(store.get(pendingKey) ?? 'null');
    expect(stored).toMatchObject({ v: 1, txHash: `0x${'77'.repeat(32)}`, reference, validUntil: 1_064, snapshot: { amount: '2000000' } });
    // A reload: new modules, new DOM, the same storage.
    await load({ keep: true });
    expect($<HTMLButtonElement>('redeem').disabled).toBe(true);
    expect($<HTMLButtonElement>('redeem-new-reference').disabled).toBe(true);
    expect($('redeem-reference').textContent).toBe(reference);
    expect(lockRow()).toMatch(/locked: the redemption 0x7{64} .* may still be included/);
    expect(lockRow()).toMatch(/Do not reload or close this page/);
    expect($('redeem-progress').textContent).toMatch(/sent before this page was reloaded may still be included/);
    // Connecting an account and filling the form does not enable it.
    $<HTMLButtonElement>('redeem-connect').click();
    await flush();
    ($('redeem-accounts').querySelector('button') as HTMLButtonElement).click();
    await flush();
    type('redeem-amount', '2');
    await flush();
    expect($<HTMLButtonElement>('redeem').disabled).toBe(true);
    expect(store.has(pendingKey)).toBe(true);
  });

  it('warns on leaving while a redemption is pending, and says not to reload or close the page', async () => {
    const { UnclearOutcome } = await pend();
    const e = new Event('beforeunload', { cancelable: true });
    window.dispatchEvent(e);
    expect(e.defaultPrevented).toBe(true);
    expect($('redeem-progress').textContent).toMatch(/Do not reload or close this page until it settles/);
    expect(UnclearOutcome).toBeDefined();
  });

  it('says the lock would be lost on a reload when storage is unavailable', async () => {
    await pend({ blocked: true });
    expect($('redeem-progress').textContent).toMatch(/could not store the lock, so a reload or closing the page would lose it/);
    expect(lockRow()).toMatch(/could not store the lock, so a reload or closing the page would lose it/);
    expect($<HTMLButtonElement>('redeem').disabled).toBe(true);
    expect(store.size).toBe(0);
  });

  it('names the network the redemption belongs to, and says to select it again when another is selected', async () => {
    const other = fake.other as { chainId: number };
    store = new Map();
    store.set(pendingKey, record({ network: 'other-net', networkLabel: 'Other' }, { backend: { kind: 'evm', chainId: String(other.chainId) } }));
    await load({ keep: true });
    expect(lockRow()).toMatch(/\(.*, Other\) may still be included/);
    expect(lockRow()).toMatch(/belongs to Other, which is not selected: select it again to settle it/);
    expect($<HTMLButtonElement>('redeem').disabled).toBe(true);
  });

  it('shows what the watcher last read, and when its read failed', async () => {
    await pend();
    expect(lockRow()).toMatch(/last checked at block 1000 \(finalized\), \d\d:\d\d:\d\d UTC/);
    // A wedged watcher is visible: its read fails.
    await pend({ fail: true });
    expect(lockRow()).toMatch(/last read failed at \d\d:\d\d:\d\d UTC \(the light client is syncing\); trying again/);
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
    // Settled: the stored lock is cleared.
    expect(store.has(pendingKey)).toBe(false);
  });
  /** A stored record as this build writes one for the mock network, with the given fields replaced. */
  const record = (over: Record<string, unknown> = {}, snap: Record<string, unknown> = {}) => {
    const n = fake.network as { key: string; label: string; chainId: number };
    return JSON.stringify({
      v: 1,
      network: n.key,
      networkLabel: n.label,
      kreivo: 'kreivo-test',
      redeemer: ALICE,
      reference: `0x${'12'.repeat(32)}`,
      txHash: `0x${'88'.repeat(32)}`,
      validUntil: 5_000,
      snapshot: {
        redeemer: { account: ALICE, address: ALICE_SS58 },
        asset: { id: 101, symbol: 'bUSDC', decimals: 6 },
        amount: '2000000',
        whole: false,
        backend: { kind: 'evm', chainId: String(n.chainId) },
        payout: A1,
        reference: `0x${'12'.repeat(32)}`,
        ...snap,
      },
      ...over,
    });
  };
  async function connectAccount() {
    $<HTMLButtonElement>('redeem-connect').click();
    await flush();
    ($('redeem-accounts').querySelector('button') as HTMLButtonElement).click();
    await flush();
    type('redeem-amount', '2');
    await flush();
  }
  const clearButton = () => $('redeem-unreadable').querySelector('button') as HTMLButtonElement | null;

  describe('an unreadable record', () => {
    const wrongKey = `bezent.bridge.pending/v1/kreivo-test/0x${'ee'.repeat(32)}`;
    const bad: [string, () => string, string?][] = [
      ['an unknown version', () => record({ v: 2 })],
      ['malformed text', () => 'not json'],
      ['a body that is not its key’s', () => record(), wrongKey],
      ['a negative validUntil', () => record({ validUntil: -5 })],
      ['a fractional validUntil', () => record({ validUntil: 1.5 })],
      ['a validUntil that is text', () => record({ validUntil: 'soon' })],
      ['decimals out of range', () => record({}, { asset: { id: 101, symbol: 'bUSDC', decimals: 99 } })],
      ['decimals that are not the network’s', () => record({}, { asset: { id: 101, symbol: 'bUSDC', decimals: 18 } })],
      ['an asset id that is not a u32', () => record({}, { asset: { id: 2 ** 32, symbol: 'bUSDC', decimals: 6 } })],
      ['an asset id that is not the network’s', () => record({}, { asset: { id: 7, symbol: 'bUSDC', decimals: 6 } })],
      ['an address that is not the account’s', () => record({}, { redeemer: { account: ALICE, address: 'HNZata7iMYWmk5RvZRTiAsSDhV8366zq2YGb3tLH5Upf74G' } })],
      ['a backend on another chain', () => record({}, { backend: { kind: 'evm', chainId: '1' } })],
      ['an attested backend', () => record({}, { backend: { kind: 'attested', id: 3 } })],
      ['another Kreivo network', () => record({ kreivo: 'kreivo' })],
      ['a network the build does not carry', () => record({ network: 'polygon' })],
    ];

    it.each(bad)('locks Redeem, and is read as unreadable, for %s', async (_what, body, key) => {
      store = new Map();
      store.set(key ?? pendingKey, body());
      await load({ keep: true });
      expect($<HTMLButtonElement>('redeem').disabled).toBe(true);
      expect(lockRow()).toMatch(/a stored redemption record can’t be read \(.*\); Redeem is off until you check it/);
      expect($('redeem-progress').textContent).toMatch(/1 stored redemption record\(s\) can’t be read; Redeem is off until you check it/);
      // Not locked as a pending redemption: nothing is watched, and no reference is held.
      expect(lockRow()).not.toMatch(/locked: the redemption/);
      await connectAccount();
      expect($<HTMLButtonElement>('redeem').disabled).toBe(true);
      expect(clearButton()).not.toBeNull();
    });

    it('does not print the notice again on each connect, and clears only on confirmation, naming the network and redeemer', async () => {
      store = new Map();
      store.set(pendingKey, 'not json');
      await load({ keep: true });
      const notices = () => ($('redeem-progress').textContent ?? '').split('can’t be read').length - 1;
      expect(notices()).toBe(1);
      await connectAccount();
      await connectAccount();
      expect(notices()).toBe(1);
      expect($<HTMLButtonElement>('redeem').disabled).toBe(true);
      const confirmed = vi.fn(() => false);
      vi.stubGlobal('confirm', confirmed);
      window.confirm = confirmed;
      clearButton()!.click();
      await flush();
      expect(confirmed).toHaveBeenCalledTimes(1);
      const asked = (confirmed.mock.calls[0] as unknown as string[])[0]!;
      expect(asked).toContain('kreivo-test');
      expect(asked).toContain(ALICE_SS58);
      // Declined: the record stays, and Redeem stays off.
      expect(store.get(pendingKey)).toBe('not json');
      expect($<HTMLButtonElement>('redeem').disabled).toBe(true);
      confirmed.mockReturnValue(true);
      clearButton()!.click();
      await flush();
      expect(store.has(pendingKey)).toBe(false);
      expect(clearButton()).toBeNull();
      expect(lockRow()).not.toMatch(/can’t be read/);
      expect($<HTMLButtonElement>('redeem').disabled).toBe(false);
    });
  });

  it('says so, in the lock row, when storage cannot be read, and leaves Redeem usable', async () => {
    await load({ blocked: true });
    expect(lockRow()).toMatch(/storage unavailable: a lock from an earlier visit can’t be checked/);
    await connectAccount();
    expect($<HTMLButtonElement>('redeem').disabled).toBe(false);
    expect(lockRow()).toMatch(/storage unavailable/);
  });

  it('counts a record whose read throws as unreadable: it locks Redeem, it does not fail open', async () => {
    store = new Map();
    store.set(pendingKey, record());
    await load({ keep: true, unreadableKey: pendingKey });
    expect(lockRow()).toMatch(/a stored redemption record can’t be read \(reading it failed: .*\); Redeem is off until you check it/);
    expect(lockRow()).not.toMatch(/storage unavailable/);
    await connectAccount();
    expect($<HTMLButtonElement>('redeem').disabled).toBe(true);
    $<HTMLButtonElement>('redeem').disabled = false;
    $<HTMLButtonElement>('redeem').click();
    await flush();
    expect(fake.redeems).toHaveLength(0);
    // Cleared on confirmation, while its read still throws.
    vi.stubGlobal('confirm', vi.fn(() => true));
    window.confirm = () => true;
    clearButton()!.click();
    await flush();
    expect(store.has(pendingKey)).toBe(false);
    expect(lockRow()).not.toMatch(/can’t be read/);
  });

  it('says nothing about storage when it is readable', async () => {
    await load();
    expect(lockRow()).not.toMatch(/storage unavailable/);
  });

  describe('two tabs', () => {
    it('removes a settled record only if it is still the settled one (compare-and-delete)', async () => {
      const { reference, UnclearOutcome } = await redeeming();
      fake.recorded = { redeemer: ALICE, asset: 101, amount: 2_000_000n, backend: { kind: 'evm', chainId: 80002n }, payout: { kind: 'address', address: A1 }, refunded: false };
      // Another tab replaces the record for this redeemer between this tab's write and its removal.
      const theirs = record({ txHash: `0x${'99'.repeat(32)}` });
      const rec = fake.recorded;
      let reads = 0;
      // The first read finds the redemption recorded, and the other tab's record lands as it does; the next finds none.
      Object.defineProperty(fake, 'recorded', {
        configurable: true,
        get: () => {
          if (reads++ === 0) store.set(pendingKey, theirs);
          return reads === 1 ? rec : undefined;
        },
        set: (v) => Object.defineProperty(fake, 'recorded', { value: v, writable: true, configurable: true, enumerable: true }),
      });
      fake.redeems[0]!.reject(new UnclearOutcome('The redemption 0x77… was broadcast, but following it failed.', `0x${'77'.repeat(32)}`, 1_064));
      await flush();
      expect($('redeem-progress').textContent).toMatch(/Kreivo records the redemption/);
      expect($('redeem-reference').textContent).not.toBe(reference);
      expect(store.get(pendingKey)).toBe(theirs); // Not this tab's to remove.
      // And it is taken as the lock, not lost.
      expect(lockRow()).toMatch(/locked: the redemption 0x9{64}/);
    });

    it('removes the record when it is the settled one', async () => {
      const { UnclearOutcome } = await redeeming();
      fake.recorded = { redeemer: ALICE, asset: 101, amount: 2_000_000n, backend: { kind: 'evm', chainId: 80002n }, payout: { kind: 'address', address: A1 }, refunded: false };
      fake.redeems[0]!.reject(new UnclearOutcome('x', `0x${'77'.repeat(32)}`, 1_064));
      await flush();
      expect(store.has(pendingKey)).toBe(false);
    });

    it('does not overwrite another tab’s record: it becomes the lock, and the unstored hash is named', async () => {
      const { UnclearOutcome } = await redeeming();
      const theirs = record({ txHash: `0x${'99'.repeat(32)}`, reference: `0x${'34'.repeat(32)}` }, { reference: `0x${'34'.repeat(32)}` });
      store.set(pendingKey, theirs);
      fake.redeems[0]!.reject(new UnclearOutcome('x', `0x${'77'.repeat(32)}`, 1_064));
      await flush();
      expect(store.get(pendingKey)).toBe(theirs);
      expect(lockRow()).toMatch(/locked: the redemption 0x9{64}/);
      expect($('redeem-reference').textContent).toBe(`0x${'34'.repeat(32)}`);
      expect($('redeem-progress').textContent).toMatch(/Another tab already holds a lock for this redeemer .*0x9{64}.*0x7{64}, is not stored/);
      expect($<HTMLButtonElement>('redeem').disabled).toBe(true);
    });

    it('watches this tab’s unstored redemption as a second lock: Redeem stays off after the adopted lock settles, until its own does', async () => {
      const { UnclearOutcome } = await redeeming();
      // The adopted record can no longer be included at head 2 000; this tab's own redemption, valid to 5 000, can.
      fake.head = 2_000;
      const theirs = record({ txHash: `0x${'99'.repeat(32)}`, reference: `0x${'34'.repeat(32)}`, validUntil: 1_500 }, { reference: `0x${'34'.repeat(32)}` });
      store.set(pendingKey, theirs);
      fake.redeems[0]!.reject(new UnclearOutcome('x', `0x${'77'.repeat(32)}`, 5_000));
      await flush();
      // The adopted lock settled (nothing recorded, past its mortality) and was removed from storage.
      expect($('redeem-progress').textContent).toMatch(/Block 2000 is final, past the transaction’s mortality/);
      expect(store.has(pendingKey)).toBe(false);
      expect(lockRow()).not.toMatch(/locked: the redemption 0x9{64}/);
      // This tab's own is still held in memory, watched, and keeps Redeem off.
      expect(lockRow()).toMatch(/Also unclear.*this tab’s redemption 0x7{64} .* may still be included\. It is not stored/);
      expect(lockRow()).toMatch(/Its watcher last checked at block 2000 \(finalized\)/);
      expect($<HTMLButtonElement>('redeem').disabled).toBe(true);
      const e = new Event('beforeunload', { cancelable: true });
      window.dispatchEvent(e);
      expect(e.defaultPrevented).toBe(true);
      // A click that reaches the handler anyway signs nothing.
      $<HTMLButtonElement>('redeem').disabled = false;
      $<HTMLButtonElement>('redeem').click();
      await flush();
      expect(fake.redeems).toHaveLength(1);
      expect($('redeem-progress').textContent).toMatch(/The redemption 0x7{64} may still be included, so no other is sent.* Nothing was signed/);
    });

    it('frees this tab’s unstored redemption once it settles, and only then', async () => {
      const { UnclearOutcome } = await redeeming();
      fake.head = 2_000;
      const theirs = record({ txHash: `0x${'99'.repeat(32)}`, reference: `0x${'34'.repeat(32)}`, validUntil: 5_000 }, { reference: `0x${'34'.repeat(32)}` });
      store.set(pendingKey, theirs);
      // This tab's own can no longer be included at head 2 000: it settles at once, the adopted lock stays.
      fake.redeems[0]!.reject(new UnclearOutcome('x', `0x${'77'.repeat(32)}`, 1_064));
      await flush();
      expect($('redeem-progress').textContent).toMatch(/past the mortality of this tab’s redemption 0x7{64}.*nothing was redeemed by it/);
      expect(lockRow()).not.toMatch(/Also unclear/);
      expect(lockRow()).toMatch(/locked: the redemption 0x9{64}/);
      expect(store.get(pendingKey)).toBe(theirs);
      expect($<HTMLButtonElement>('redeem').disabled).toBe(true);
    });

    it('runs the redemption inside the Web Lock bezent.bridge.redeem, and holds it until the redemption ends', async () => {
      await redeeming();
      expect(fake.asked).toEqual(['bezent.bridge.redeem']);
      expect(fake.held.has('bezent.bridge.redeem')).toBe(true);
      fake.redeems[0]!.reject(new Error('the wallet declined'));
      await flush();
      expect(fake.held.has('bezent.bridge.redeem')).toBe(false);
    });

    /** What a second tab of the browser gets when it asks for the redeem lock as its click does. */
    const secondTab = () => webLocks.request('bezent.bridge.redeem', { ifAvailable: true }, async (lock) => lock);
    /** The watcher's 12 s wait runs in 40 ms (undone with the test's globals), so a test can let it settle. */
    function fastWatcher() {
      const real = globalThis.setTimeout;
      vi.stubGlobal('setTimeout', ((fn: () => void, ms?: number, ...rest: unknown[]) => real(fn, ms === 12_000 ? 40 : ms, ...rest)) as typeof setTimeout);
    }
    /** The head moves past the transaction's mortality, and the watcher reads it. */
    async function settleNow() {
      fake.head = 2_000;
      await new Promise((r) => setTimeout(r, 200));
      await flush();
    }

    it('keeps the Web Lock after an unclear outcome whose record could not be stored, until it settles', async () => {
      fastWatcher();
      await pend({ full: true });
      expect(store.has(pendingKey)).toBe(false);
      expect(lockRow()).toMatch(/could not store the lock/);
      // A second tab reads no record, and is refused the lock: it signs nothing.
      expect(fake.held.has('bezent.bridge.redeem')).toBe(true);
      expect(await secondTab()).toBeNull();
      await settleNow();
      expect($('redeem-progress').textContent).toMatch(/nothing was redeemed/);
      expect(fake.held.has('bezent.bridge.redeem')).toBe(false);
      expect(await secondTab()).not.toBeNull();
    });

    it('keeps the Web Lock while this tab’s unclear redemption is only in memory (another tab’s record is the lock), until it settles', async () => {
      fastWatcher();
      const { UnclearOutcome } = await redeeming();
      fake.head = 1_000;
      store.set(pendingKey, record({ txHash: `0x${'99'.repeat(32)}`, reference: `0x${'34'.repeat(32)}`, validUntil: 1_064 }, { reference: `0x${'34'.repeat(32)}` }));
      fake.redeems[0]!.reject(new UnclearOutcome('x', `0x${'77'.repeat(32)}`, 1_064));
      await flush();
      expect(lockRow()).toMatch(/Also unclear/);
      expect(fake.held.has('bezent.bridge.redeem')).toBe(true);
      expect(await secondTab()).toBeNull();
      await settleNow();
      expect(lockRow()).not.toMatch(/Also unclear/);
      expect(fake.held.has('bezent.bridge.redeem')).toBe(false);
    });

    it('lets go of the Web Lock at once when the unclear outcome is stored', async () => {
      await pend();
      expect(store.has(pendingKey)).toBe(true);
      expect(fake.held.has('bezent.bridge.redeem')).toBe(false);
    });

    it('signs nothing while another tab holds the redeem lock (two tabs clicking Redeem together)', async () => {
      await load();
      await connectAccount();
      fake.held.add('bezent.bridge.redeem'); // Another tab is signing.
      $<HTMLButtonElement>('redeem').click();
      await flush();
      expect(fake.redeems).toHaveLength(0);
      expect($('redeem-progress').textContent).toMatch(/Another tab of this page is redeeming right now.* Nothing was signed/);
      expect($<HTMLButtonElement>('redeem').disabled).toBe(false);
      // Once it lets go, this tab redeems.
      fake.held.delete('bezent.bridge.redeem');
      $<HTMLButtonElement>('redeem').click();
      await flush();
      expect(fake.redeems).toHaveLength(1);
    });

    it('refuses to redeem in a browser without Web Locks', async () => {
      await load({ noLocks: true });
      await connectAccount();
      $<HTMLButtonElement>('redeem').click();
      await flush();
      expect(fake.redeems).toHaveLength(0);
      expect($('redeem-progress').textContent).toMatch(/does not offer Web Locks.* Nothing was signed/);
    });

    it('signs nothing when another tab left a lock since this page read the storage', async () => {
      await load();
      await connectAccount();
      expect($<HTMLButtonElement>('redeem').disabled).toBe(false);
      store.set(pendingKey, record());
      $<HTMLButtonElement>('redeem').click();
      await flush();
      expect(fake.redeems).toHaveLength(0);
      expect($<HTMLButtonElement>('redeem').disabled).toBe(true);
      expect(lockRow()).toMatch(/locked: the redemption 0x8{64}/);
    });

    it('takes another tab’s lock at once, on a storage event', async () => {
      await load();
      await connectAccount();
      expect($<HTMLButtonElement>('redeem').disabled).toBe(false);
      store.set(pendingKey, record());
      window.dispatchEvent(new StorageEvent('storage', { key: pendingKey }));
      await flush();
      expect($<HTMLButtonElement>('redeem').disabled).toBe(true);
      expect($<HTMLButtonElement>('redeem-new-reference').disabled).toBe(true);
      expect(lockRow()).toMatch(/locked: the redemption 0x8{64}/);
      expect($('redeem-reference').textContent).toBe(`0x${'12'.repeat(32)}`);
    });

    it('takes another tab’s unreadable record at once, and ignores the storage events of other keys', async () => {
      await load();
      await connectAccount();
      window.dispatchEvent(new StorageEvent('storage', { key: 'unrelated' }));
      await flush();
      expect($<HTMLButtonElement>('redeem').disabled).toBe(false);
      store.set(pendingKey, 'not json');
      window.dispatchEvent(new StorageEvent('storage', { key: pendingKey }));
      await flush();
      expect($<HTMLButtonElement>('redeem').disabled).toBe(true);
      expect(lockRow()).toMatch(/can’t be read/);
    });
  });

  it('does not lock a settled redemption again when its record could not be removed', async () => {
    const { reference, UnclearOutcome } = await redeeming({ stuck: true });
    fake.recorded = { redeemer: ALICE, asset: 101, amount: 2_000_000n, backend: { kind: 'evm', chainId: 80002n }, payout: { kind: 'address', address: A1 }, refunded: false };
    let reads = 0;
    fake.onRedemption = () => void reads++;
    fake.redeems[0]!.reject(new UnclearOutcome('x', `0x${'77'.repeat(32)}`, 1_064));
    await flush();
    await flush();
    // The record is still there (its removal failed), but it settled once and was not locked again.
    expect(store.has(pendingKey)).toBe(true);
    expect(reads).toBe(1);
    expect(lockRow()).not.toMatch(/locked: the redemption/);
    expect($('redeem-reference').textContent).not.toBe(reference);
    expect($<HTMLButtonElement>('redeem-new-reference').disabled).toBe(false);
    expect($<HTMLButtonElement>('redeem').disabled).toBe(false);
    // Another tab's event does not lock it again either.
    window.dispatchEvent(new StorageEvent('storage', { key: pendingKey }));
    await flush();
    expect(reads).toBe(1);
    expect($<HTMLButtonElement>('redeem').disabled).toBe(false);
  });

  it('does not leave an unhandled rejection when the settled message cannot be formed', async () => {
    const { UnclearOutcome } = await redeeming();
    // A record whose payout is not an address makes the message throw once the lock is cleared.
    fake.recorded = { redeemer: ALICE, asset: 101, amount: 2_000_000n, backend: { kind: 'evm', chainId: 80002n }, payout: { kind: 'address', address: 'not an address' }, refunded: false };
    const unhandled = vi.fn();
    process.on('unhandledRejection', unhandled);
    fake.redeems[0]!.reject(new UnclearOutcome('x', `0x${'77'.repeat(32)}`, 1_064));
    await flush();
    process.off('unhandledRejection', unhandled);
    expect(unhandled).not.toHaveBeenCalled();
    expect($('redeem-progress').textContent).toMatch(/settled, but its outcome could not be shown/);
    expect(lockRow()).not.toMatch(/locked: the redemption/);
    expect(store.has(pendingKey)).toBe(false);
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

describe('Kreivo before the light client has a finalized block', () => {
  // The mock network is the local test bed's (a loopback node); these tests give it the light client Kreivo's live
  // networks use, and put the test bed back.
  type Net = { kreivo: { connection: unknown } };
  let loopback: unknown;
  beforeEach(() => {
    const net = fake.network as Net;
    loopback = net.kreivo.connection;
    net.kreivo.connection = { kind: 'light-client', relay: 'kusama' };
  });
  afterEach(() => {
    (fake.network as Net).kreivo.connection = loopback;
  });

  it('says it is syncing, refuses the Kreivo actions in plain words, and reads Kreivo once it has synced', async () => {
    await load({ hold: true });
    expect($('kreivo-status').textContent).toMatch(/Connecting to .* \(light client\): syncing Kusama, about a minute on a first visit/);
    expect($('kreivo-status').textContent).not.toMatch(/Reading/);

    type('recipient', ALICE_SS58);
    type('amount', '2.5');
    await flush();
    expect(depositButton().disabled).toBe(true);
    expect($('recipient-status').textContent).toMatch(/still connecting .*Nothing was signed/);

    $('redeem-connect').click();
    await flush();
    expect($('redeem-wallet-status').textContent).toMatch(/still connecting .*Nothing was signed/);

    fake.sync!.resolve();
    await flush();
    expect($('kreivo-status').textContent).toMatch(/^Reading .* through the light client\./);
    expect(depositButton().disabled).toBe(false);
  });

  it('shows an error with a retry when the sync fails, and tries again on it', async () => {
    await load({ hold: true });
    fake.sync!.reject(new Error('Kreivo did not sync within 3 minutes. Nothing was signed.'));
    await flush();
    expect($('kreivo-status').textContent).toMatch(/Kreivo is not reachable: Kreivo did not sync within 3 minutes/);
    const retry = $('kreivo-status').querySelector('button') as HTMLButtonElement;
    expect(retry.textContent).toBe('Retry');
    expect(fake.connects2).toBe(1);
    retry.click();
    await flush();
    expect(fake.connects2).toBe(2);
    expect($('kreivo-status').textContent).toMatch(/syncing Kusama/);
    fake.sync!.resolve();
    await flush();
    expect($('kreivo-status').textContent).toMatch(/^Reading/);
  });
});
