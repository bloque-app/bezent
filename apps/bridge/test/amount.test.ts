// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// T-018-14 (REQ-LM-1, REQ-LM-2): the amount is exactly what was typed, and inside the contract's limits.

import { describe, expect, it } from 'vitest';
import { checkAmount, formatAmount, parseAmount } from '../src/lib/amount';

// The route's limits as deployed (plan part 1): 1 USDC to 50 000 USDC, six decimals.
const USDC = { minimum: 1_000_000n, maximum: 50_000_000_000n, decimals: 6, symbol: 'USDC' };

describe('parseAmount', () => {
  it.each([
    ['1', 1_000_000n],
    ['1.5', 1_500_000n],
    ['0.000001', 1n],
    ['.5', 500_000n],
    ['50000', 50_000_000_000n],
    ['007.10', 7_100_000n],
    [' 2 ', 2_000_000n],
    ['3.', 3_000_000n],
  ])('%s is %s units', (text, units) => {
    expect(parseAmount(text, 6)).toEqual({ ok: true, units });
  });

  it.each(['', '-1', '+1', '1e6', '1,000', '1.0000001', 'abc', '1.2.3', '.', '0x10', 'Infinity'])('refuses %j', (text) => {
    expect(parseAmount(text, 6).ok).toBe(false);
  });

  it('takes no fraction for a token without decimals', () => {
    expect(parseAmount('5', 0)).toEqual({ ok: true, units: 5n });
    expect(parseAmount('5.1', 0).ok).toBe(false);
  });
});

describe('checkAmount', () => {
  it('accepts the minimum and the maximum themselves', () => {
    expect(checkAmount(1_000_000n, USDC)).toEqual({ ok: true });
    expect(checkAmount(50_000_000_000n, USDC)).toEqual({ ok: true });
  });

  it('refuses below the minimum and above the maximum, saying the limit', () => {
    const below = checkAmount(999_999n, USDC);
    expect(below.ok).toBe(false);
    expect(!below.ok && below.reason).toMatch(/minimum deposit is 1 USDC/);
    const above = checkAmount(50_000_000_001n, USDC);
    expect(above.ok).toBe(false);
    expect(!above.ok && above.reason).toMatch(/maximum deposit is 50000 USDC/);
  });

  it('refuses more than the balance', () => {
    expect(checkAmount(2_000_000n, USDC, 1_999_999n).ok).toBe(false);
    expect(checkAmount(2_000_000n, USDC, 2_000_000n).ok).toBe(true);
  });
});

describe('formatAmount', () => {
  it('drops trailing zeros', () => {
    expect(formatAmount(1_500_000n, 6)).toBe('1.5');
    expect(formatAmount(1n, 6)).toBe('0.000001');
    expect(formatAmount(0n, 6)).toBe('0');
    expect(formatAmount(10n ** 12n, 12)).toBe('1');
  });
});
