// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// A deadline on a read that may never answer (a light client still syncing, or with no peer): the read fails
// closed with a clear message instead of leaving the page waiting.

export class Timeout extends Error {}

/** Runs `read` with an AbortSignal that fires after `ms`, and rejects with `Timeout(message)` then, whether or
 * not `read` honours the signal. */
export async function withTimeout<T>(ms: number, message: string, read: (signal: AbortSignal) => Promise<T>): Promise<T> {
  const ctl = new AbortController();
  let timer: ReturnType<typeof setTimeout> | undefined;
  const late = new Promise<never>((_, reject) => {
    timer = setTimeout(() => {
      ctl.abort();
      reject(new Timeout(message));
    }, ms);
  });
  try {
    return await Promise.race([read(ctl.signal), late]);
  } finally {
    clearTimeout(timer);
  }
}
