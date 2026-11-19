// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Whether a Kreivo account can receive the mint (F-018, T-018-14; amendment 0029, Open 7, ruled: bUSDC stays
// not sufficient, and the app refuses a recipient that cannot receive it, before signing).
//
// pallet-assets creates the recipient's asset account on the first mint. For an asset that is not sufficient
// it takes a consumer reference on the account, which frame_system gives only to an account that has a
// provider (`UnavailableConsumer` otherwise; research/03): in practice, an account holding at least the
// existential deposit of KSM. The first mint must also reach the asset's minimum balance. An account that
// already holds the asset needs neither. That consumer reference also fails on an account already at
// frame_system's `MaxConsumers` (`TooManyConsumers`). A blocked asset account receives nothing, and an asset that is not
// live mints nothing.

import { formatAmount } from './amount';

export interface AccountRead {
  /** `System.Account`: what frame_system counts on the account (all zero when it does not exist). */
  providers: number;
  consumers: number;
  sufficients: number;
  free: bigint;
}

export interface AssetRead {
  /** `Assets.Asset(id)`'s status: `Live`, `Frozen` or `Destroying`. */
  status: string;
  isSufficient: boolean;
  minBalance: bigint;
}

export interface AssetAccountRead {
  /** `Assets.Account(id, who)`'s status: `Liquid`, `Frozen` or `Blocked`. */
  status: string;
  balance: bigint;
}

export interface RecipientFacts {
  account: AccountRead;
  /** `undefined` when the asset does not exist on this network. */
  asset: AssetRead | undefined;
  /** `undefined` when the recipient holds no account of the asset yet. */
  assetAccount: AssetAccountRead | undefined;
  /** The amount the deposit would mint, in the asset's minor units. */
  amount: bigint;
  /** For the advice: the native token's existential deposit and symbol. */
  existentialDeposit: bigint;
  nativeDecimals: number;
  nativeSymbol: string;
  assetSymbol: string;
  assetDecimals: number;
  refused: boolean;
  /** frame_system's `MaxConsumers` on this network. */
  maxConsumers: number;
}

export type Assessment = { ok: true; note: string } | { ok: false; reason: string; advice: string };

export function assessRecipient(f: RecipientFacts): Assessment {
  if (f.refused) {
    return {
      ok: false,
      reason: 'bezent does not mint to this account: a deposit naming it would be returned.',
      advice: 'Name your own Kreivo account.',
    };
  }
  if (!f.asset) {
    return { ok: false, reason: `${f.assetSymbol} does not exist on this Kreivo network.`, advice: 'Check that you chose the right network.' };
  }
  if (f.asset.status !== 'Live') {
    return {
      ok: false,
      reason: `${f.assetSymbol} is ${f.asset.status.toLowerCase()} on Kreivo, so nothing can be minted now.`,
      advice: 'Try again later.',
    };
  }
  if (f.assetAccount) {
    if (f.assetAccount.status === 'Blocked') {
      return { ok: false, reason: `This account is blocked from holding ${f.assetSymbol}.`, advice: 'Name another Kreivo account.' };
    }
    return { ok: true, note: `This account already holds ${f.assetSymbol}.` };
  }
  if (f.amount < f.asset.minBalance) {
    return {
      ok: false,
      reason: `A first deposit to this account must be at least ${formatAmount(f.asset.minBalance, f.assetDecimals)} ${f.assetSymbol}.`,
      advice: 'Deposit more.',
    };
  }
  if (f.asset.isSufficient) return { ok: true, note: `${f.assetSymbol} can open its own account.` };
  if (f.account.providers === 0) {
    const ed = `${formatAmount(f.existentialDeposit, f.nativeDecimals)} ${f.nativeSymbol}`;
    return {
      ok: false,
      reason: `This Kreivo account cannot receive ${f.assetSymbol} yet: it holds no ${f.nativeSymbol}. ${f.assetSymbol} is not a sufficient asset, so the account must exist on Kreivo first.`,
      advice: `Send it at least ${ed} (the existential deposit), plus a little for fees if you will redeem later, then check again.`,
    };
  }
  if (f.account.consumers >= f.maxConsumers) {
    return {
      ok: false,
      reason: `This Kreivo account cannot receive ${f.assetSymbol}: it already holds ${f.account.consumers} consumer references, Kreivo's limit (${f.maxConsumers}), and a first ${f.assetSymbol} needs one more.`,
      advice: 'Name another Kreivo account, or free one of its references (for example, close an asset account it no longer uses) and check again.',
    };
  }
  return { ok: true, note: `The account exists on Kreivo, so it can receive ${f.assetSymbol}.` };
}
