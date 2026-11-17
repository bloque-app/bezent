// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Amounts (F-018, T-018-14): a decimal string the user types, in the token's minor units, inside the
// contract's immutable `minimum` and `maximum` (REQ-LM-1, REQ-LM-2), which the app reads from the contract.

export type Parsed = { ok: true; units: bigint } | { ok: false; reason: string };

/** Parses `text` (`12`, `12.5`, `0.000001`) into minor units of a token with `decimals` decimals. No sign,
 * exponent, grouping or more fractional digits than the token has: the amount signed is exactly the amount
 * typed. */
export function parseAmount(text: string, decimals: number): Parsed {
  const t = text.trim();
  const m = /^(\d+)(?:\.(\d*))?$/.exec(t) ?? /^()\.(\d+)$/.exec(t);
  if (!m) return { ok: false, reason: 'Enter an amount such as 25 or 25.50.' };
  const whole = m[1] || '0';
  const frac = m[2] ?? '';
  if (frac.length > decimals) return { ok: false, reason: `At most ${decimals} decimal places.` };
  const units = BigInt(whole) * 10n ** BigInt(decimals) + BigInt(frac.padEnd(decimals, '0') || '0');
  return { ok: true, units };
}

/** `units` as a decimal string with `decimals` decimals, without trailing zeros. */
export function formatAmount(units: bigint, decimals: number): string {
  const neg = units < 0n;
  const v = neg ? -units : units;
  const base = 10n ** BigInt(decimals);
  const whole = (v / base).toString();
  const frac = decimals > 0 ? (v % base).toString().padStart(decimals, '0').replace(/0+$/, '') : '';
  return (neg ? '-' : '') + whole + (frac ? `.${frac}` : '');
}

export interface Bounds {
  minimum: bigint;
  maximum: bigint;
  decimals: number;
  symbol: string;
}

/** Checks `units` against the contract's limits and, when known, the depositor's balance. The contract
 * reverts outside its limits (`BelowMinimum`, `AboveMaximum`); the app refuses before signing. */
export function checkAmount(units: bigint, b: Bounds, balance?: bigint): { ok: true } | { ok: false; reason: string } {
  const f = (u: bigint) => `${formatAmount(u, b.decimals)} ${b.symbol}`;
  if (units < b.minimum) return { ok: false, reason: `The minimum deposit is ${f(b.minimum)}.` };
  if (units > b.maximum) return { ok: false, reason: `The maximum deposit is ${f(b.maximum)}; deposit more in several transactions.` };
  if (balance !== undefined && units > balance) return { ok: false, reason: `This account holds ${f(balance)}.` };
  return { ok: true };
}
