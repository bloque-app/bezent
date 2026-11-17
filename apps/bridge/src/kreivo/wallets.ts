// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Kreivo's browser-extension wallets (Talisman, SubWallet, Polkadot.js) through the injected-extension interface
// (`window.injectedWeb3`), for the redeem (F-018, T-018-15). polkadot-api's `connectInjectedExtension` hides the
// extension's `metadata` interface, through which the page registers Kreivo's custom signed extensions with the
// wallet before signing (kreivo/extensions.ts), so the app enables the extension itself and keeps both.

import type { MetadataDef, SignPayload } from './extensions';
import { pjsSigner } from './extensions';
import type { PolkadotSigner } from 'polkadot-api/signer';
import { decodeSs58 } from '../lib/ss58';
import { fromHex, type Hex } from '../lib/bytes';

interface InjectedAccount {
  address: string;
  genesisHash?: string | null;
  name?: string;
  type?: string;
}

interface Enabled {
  accounts: { get: () => Promise<InjectedAccount[]> };
  signer: { signPayload?: SignPayload };
  metadata?: { provide: (def: MetadataDef) => Promise<boolean> };
}

type InjectedWeb3 = Record<string, { enable?: (dapp: string) => Promise<Enabled> } | undefined>;

export interface KreivoAccount {
  wallet: string;
  name: string | undefined;
  /** The account's 32 bytes. */
  account: Hex;
  /** As the wallet wrote it. */
  address: string;
  signer: PolkadotSigner;
}

export interface KreivoWallet {
  name: string;
  accounts: KreivoAccount[];
  /** Whether the wallet takes metadata from the page; one that does not must know Kreivo's extensions itself. */
  takesMetadata: boolean;
  /** Gives the wallet Kreivo's initialisation metadata, with the custom signed extensions registered. Always asks:
   * a wallet's record of this runtime may come from another page that did not register them, and `metadata.get()`
   * does not say. main.ts asks once per wallet and runtime. */
  provideMetadata(def: MetadataDef): Promise<'provided' | 'declined' | 'unsupported'>;
}

const injected = (): InjectedWeb3 => ((globalThis as { injectedWeb3?: InjectedWeb3 }).injectedWeb3 ?? {}) as InjectedWeb3;

/** The extensions that announced themselves. */
export const kreivoWalletNames = (): string[] => Object.keys(injected());

/** Accounts the app can sign with: Substrate keys (not Ethereum ones), and not bound to another chain. */
const usable = (a: InjectedAccount, genesis: Hex) =>
  (a.type === undefined || a.type === 'sr25519' || a.type === 'ed25519' || a.type === 'ecdsa') && (!a.genesisHash || a.genesisHash === genesis);

/** Enables extension `name` for the page and lists its usable accounts, each with a signer. */
export async function enableKreivoWallet(name: string, dapp: string, genesis: Hex): Promise<KreivoWallet> {
  const entry = injected()[name];
  if (!entry?.enable) throw new Error(`${name} is not available in this browser.`);
  const ext = await entry.enable(dapp);
  const sign = ext.signer.signPayload?.bind(ext.signer);
  if (!sign) throw new Error(`${name} cannot sign a transaction payload.`);
  const accounts: KreivoAccount[] = [];
  for (const a of await ext.accounts.get()) {
    if (!usable(a, genesis)) continue;
    const d = a.address.startsWith('0x') ? { account: a.address as Hex } : decodeSs58(a.address);
    if (!d || fromHex(d.account).length !== 32) continue;
    accounts.push({ wallet: name, name: a.name, account: d.account, address: a.address, signer: pjsSigner(a.address, fromHex(d.account), sign) });
  }
  const meta = ext.metadata;
  return {
    name,
    accounts,
    takesMetadata: !!meta,
    async provideMetadata(def) {
      if (!meta) return 'unsupported';
      return (await meta.provide(def)) ? 'provided' : 'declined';
    },
  };
}
