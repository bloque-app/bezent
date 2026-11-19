// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Reads what the recipient pre-check needs from Kreivo (lib/recipient.ts decides): the account
// (`System.Account`), the asset (`Assets.Asset(Here(id))`), the recipient's asset account
// (`Assets.Account(Here(id), who)`) and the existential deposit, at the finalized block. Every read has a
// deadline (`READ_TIMEOUT_MS`): a light client still syncing, or with no peer, fails closed with a clear
// message instead of leaving the page waiting.

import type { Kreivo } from './client';
import type { KreivoNetwork } from '../config/networks';
import type { RecipientFacts } from '../lib/recipient';
import { encodeSs58 } from '../lib/ss58';
import { fromHex, type Hex } from '../lib/bytes';
import { withTimeout } from '../lib/timeout';

/** How long a recipient check may take before it is given up. */
export const READ_TIMEOUT_MS = 45_000;

interface Value<T> {
  getValue(...args: unknown[]): Promise<T>;
}
interface Narrow {
  query: {
    System: { Account: Value<{ providers: number; consumers: number; sufficients: number; data: { free: bigint } }> };
    Assets: {
      Asset: Value<{ status: { type: string }; is_sufficient: boolean; min_balance: bigint } | undefined>;
      Account: Value<{ status: { type: string }; balance: bigint } | undefined>;
    };
  };
  constants: { Balances: { ExistentialDeposit: () => Promise<bigint> } };
}

export async function readRecipient(
  k: Kreivo,
  n: KreivoNetwork,
  account: Hex,
  amount: bigint,
  timeoutMs: number = READ_TIMEOUT_MS,
): Promise<RecipientFacts> {
  const api = k.api as Narrow;
  // The deadline is for the read, not for the light client's first sync: that has its own (kreivo/client.ts).
  await k.synced;
  const who = encodeSs58(fromHex(account), n.ss58Prefix);
  const asset = { type: 'Here', value: n.asset.id };
  const [acc, a, aa, ed] = await withTimeout(
    timeoutMs,
    `${n.label} did not answer within ${Math.round(timeoutMs / 1000)} s (the light client may still be syncing). Nothing was signed; try again in a moment.`,
    (signal) =>
      Promise.all([
        api.query.System.Account.getValue(who, { signal }),
        api.query.Assets.Asset.getValue(asset, { signal }),
        api.query.Assets.Account.getValue(asset, who, { signal }),
        api.constants.Balances.ExistentialDeposit(),
      ]),
  );
  return {
    account: { providers: acc.providers, consumers: acc.consumers, sufficients: acc.sufficients, free: acc.data.free },
    asset: a ? { status: a.status.type, isSufficient: a.is_sufficient, minBalance: a.min_balance } : undefined,
    assetAccount: aa ? { status: aa.status.type, balance: aa.balance } : undefined,
    amount,
    existentialDeposit: ed,
    nativeDecimals: 12,
    nativeSymbol: 'KSM',
    assetSymbol: n.asset.symbol,
    assetDecimals: n.asset.decimals,
    refused: n.refusedRecipients.includes(account),
    maxConsumers: n.maxConsumers,
  };
}
