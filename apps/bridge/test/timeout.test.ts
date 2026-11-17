// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// T-018-14: a Kreivo read that never answers (a light client still syncing, or with no peer) fails closed after
// its deadline, with a clear message, and aborts the reads it started.

import { describe, expect, it } from 'vitest';
import { Timeout, withTimeout } from '../src/lib/timeout';
import { readRecipient } from '../src/kreivo/recipient';
import { KREIVO_TESTBED } from '../src/config/networks';
import type { Kreivo } from '../src/kreivo/client';

const ALICE = '0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d';

describe('withTimeout', () => {
  it('passes an answer through in time', async () => {
    await expect(withTimeout(1_000, 'late', async () => 7)).resolves.toBe(7);
  });

  it('rejects with the message, and aborts, when the read never answers', async () => {
    let signal: AbortSignal | undefined;
    const never = (s: AbortSignal) => {
      signal = s;
      return new Promise<never>(() => undefined);
    };
    await expect(withTimeout(10, 'Kreivo did not answer.', never)).rejects.toThrow(Timeout);
    expect(signal?.aborted).toBe(true);
  });
});

describe('readRecipient', () => {
  it('fails closed when the light client does not answer, saying so', async () => {
    const signals: AbortSignal[] = [];
    const hang = {
      getValue: (...args: unknown[]) => {
        signals.push((args.at(-1) as { signal: AbortSignal }).signal);
        return new Promise(() => undefined);
      },
    };
    const k = {
      client: undefined,
      close: () => undefined,
      api: {
        query: { System: { Account: hang }, Assets: { Asset: hang, Account: hang } },
        constants: { Balances: { ExistentialDeposit: () => new Promise(() => undefined) } },
      },
    } as unknown as Kreivo;
    await expect(readRecipient(k, KREIVO_TESTBED, ALICE, 1n, 10)).rejects.toThrow(/did not answer within .* Nothing was signed/);
    expect(signals).toHaveLength(3);
    expect(signals.every((s) => s.aborted)).toBe(true);
  });
});
