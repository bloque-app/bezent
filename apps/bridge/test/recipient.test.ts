// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// T-018-14 (amendment 0029, Open 7): a recipient that cannot receive bUSDC is refused before signing.

import { describe, expect, it } from 'vitest';
import { assessRecipient, type RecipientFacts } from '../src/lib/recipient';

const facts = (over: Partial<RecipientFacts> = {}): RecipientFacts => ({
  account: { providers: 1, consumers: 0, sufficients: 0, free: 10n ** 10n },
  asset: { status: 'Live', isSufficient: false, minBalance: 1n },
  assetAccount: undefined,
  amount: 1_000_000n,
  existentialDeposit: 333_333_333n,
  nativeDecimals: 12,
  nativeSymbol: 'KSM',
  assetSymbol: 'bUSDC',
  assetDecimals: 6,
  refused: false,
  maxConsumers: 16,
  ...over,
});

describe('assessRecipient', () => {
  it('refuses an account already at MaxConsumers, which cannot take the consumer reference a first mint adds', () => {
    const at = (consumers: number) => assessRecipient(facts({ account: { providers: 1, consumers, sufficients: 0, free: 10n ** 10n } }));
    expect(at(15).ok).toBe(true);
    const a = at(16);
    expect(a.ok).toBe(false);
    expect(!a.ok && a.reason).toMatch(/16 consumer references, Kreivo's limit \(16\)/);
    // An account that already holds the asset takes no new reference, and a sufficient asset takes none.
    expect(assessRecipient(facts({ account: { providers: 1, consumers: 16, sufficients: 0, free: 1n }, assetAccount: { status: 'Liquid', balance: 1n } })).ok).toBe(true);
    expect(assessRecipient(facts({ account: { providers: 1, consumers: 16, sufficients: 0, free: 1n }, asset: { status: 'Live', isSufficient: true, minBalance: 1n } })).ok).toBe(true);
  });

  it('accepts an account with a provider', () => {
    expect(assessRecipient(facts()).ok).toBe(true);
  });

  it('refuses an account with no provider, and says to send it the existential deposit of KSM', () => {
    const a = assessRecipient(facts({ account: { providers: 0, consumers: 0, sufficients: 0, free: 0n } }));
    expect(a.ok).toBe(false);
    expect(!a.ok && a.reason).toMatch(/cannot receive bUSDC yet: it holds no KSM/);
    expect(!a.ok && a.advice).toMatch(/0\.000333333333 KSM/);
  });

  it('accepts an account that already holds the asset, provider or not', () => {
    const a = assessRecipient(
      facts({ account: { providers: 0, consumers: 1, sufficients: 0, free: 0n }, assetAccount: { status: 'Liquid', balance: 5n } }),
    );
    expect(a.ok).toBe(true);
    // A frozen asset account can still receive; only a blocked one cannot.
    expect(assessRecipient(facts({ assetAccount: { status: 'Frozen', balance: 5n } })).ok).toBe(true);
    expect(assessRecipient(facts({ assetAccount: { status: 'Blocked', balance: 5n } })).ok).toBe(false);
  });

  it('refuses when the asset is missing or not live', () => {
    expect(assessRecipient(facts({ asset: undefined })).ok).toBe(false);
    expect(assessRecipient(facts({ asset: { status: 'Frozen', isSufficient: false, minBalance: 1n } })).ok).toBe(false);
    expect(assessRecipient(facts({ asset: { status: 'Destroying', isSufficient: false, minBalance: 1n } })).ok).toBe(false);
  });

  it('refuses a first deposit below the asset’s minimum balance', () => {
    const a = assessRecipient(facts({ asset: { status: 'Live', isSufficient: false, minBalance: 2_000_000n } }));
    expect(a.ok).toBe(false);
    expect(!a.ok && a.reason).toMatch(/at least 2 bUSDC/);
  });

  it('accepts a sufficient asset with no provider (should bUSDC become sufficient)', () => {
    const a = assessRecipient(
      facts({ account: { providers: 0, consumers: 0, sufficients: 0, free: 0n }, asset: { status: 'Live', isSufficient: true, minBalance: 1n } }),
    );
    expect(a.ok).toBe(true);
  });

  it('refuses an account the core refuses', () => {
    expect(assessRecipient(facts({ refused: true })).ok).toBe(false);
  });
});
