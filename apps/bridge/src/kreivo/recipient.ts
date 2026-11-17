// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Reads what the recipient pre-check needs from Kreivo (lib/recipient.ts decides): the account
// (`System.Account`), the asset (`Assets.Asset(Here(id))`), the recipient's asset account
// (`Assets.Account(Here(id), who)`) and the existential deposit, at the finalized block.

import type { Kreivo } from './client';
import type { KreivoNetwork } from '../config/networks';
import type { RecipientFacts } from '../lib/recipient';
import { encodeSs58 } from '../lib/ss58';
import { fromHex, type Hex } from '../lib/bytes';

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
): Promise<RecipientFacts> {
  const api = k.api as Narrow;
  const who = encodeSs58(fromHex(account), n.ss58Prefix);
  const asset = { type: 'Here', value: n.asset.id };
  const [acc, a, aa, ed] = await Promise.all([
    api.query.System.Account.getValue(who),
    api.query.Assets.Asset.getValue(asset),
    api.query.Assets.Account.getValue(asset, who),
    api.constants.Balances.ExistentialDeposit(),
  ]);
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
  };
}
