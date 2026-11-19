// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Wallet discovery with EIP-6963 (MetaMask, Rabby and others announce themselves); no `window.ethereum`
// guessing, and no wallet SDK or third-party script.

import type { EIP1193Provider } from 'viem';

export interface WalletInfo {
  uuid: string;
  name: string;
  /** A data URI (EIP-6963 requires one), shown as an image; the CSP allows `data:` images only. */
  icon: string;
  rdns: string;
}

export interface Wallet {
  info: WalletInfo;
  provider: EIP1193Provider;
}

interface AnnounceEvent extends Event {
  detail: { info: WalletInfo; provider: EIP1193Provider };
}

/** Listens for wallets and calls `onChange` with every wallet announced so far, then asks them to announce. */
export function discoverWallets(onChange: (wallets: Wallet[]) => void): () => void {
  const wallets = new Map<string, Wallet>();
  const onAnnounce = (e: Event) => {
    const { info, provider } = (e as AnnounceEvent).detail ?? {};
    if (!info?.uuid || !provider || typeof provider.request !== 'function') return;
    const icon = typeof info.icon === 'string' && info.icon.startsWith('data:image/') ? info.icon : '';
    wallets.set(info.uuid, { info: { uuid: info.uuid, name: String(info.name), icon, rdns: String(info.rdns) }, provider });
    onChange([...wallets.values()]);
  };
  window.addEventListener('eip6963:announceProvider', onAnnounce);
  window.dispatchEvent(new Event('eip6963:requestProvider'));
  return () => window.removeEventListener('eip6963:announceProvider', onAnnounce);
}
