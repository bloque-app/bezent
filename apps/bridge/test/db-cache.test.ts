// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// F-018 v0.5.1: the light client's database cache (kreivo/db-cache.ts): a round trip through IndexedDB, keyed by the
// chain specification's hash, capped, and a cold start (never an error) when storage cannot be used.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { DB_MAX_BYTES, loadDatabase, persistDatabases, saveDatabase } from '../src/kreivo/db-cache';

/** The smallest IndexedDB the cache needs: one database, one object store keyed by `spec`. */
function fakeIndexedDb(rows: Map<string, unknown>) {
  const request = <T>(run: () => T) => {
    const req = {} as { result: T; error: null; onsuccess?: () => void; onerror?: () => void };
    queueMicrotask(() => {
      req.result = run();
      req.onsuccess?.();
    });
    return req;
  };
  const db = {
    createObjectStore: () => undefined,
    close: () => undefined,
    transaction: () => {
      const tx = { oncomplete: undefined as undefined | (() => void), onerror: undefined, onabort: undefined, error: null } as Record<string, unknown> & { oncomplete?: () => void };
      tx.objectStore = () => ({
        get: (k: string) => request(() => rows.get(k)),
        put: (v: { spec: string }) => {
          rows.set(v.spec, v);
          queueMicrotask(() => queueMicrotask(() => tx.oncomplete?.()));
        },
      });
      return tx;
    },
  };
  return {
    open: () => {
      const req = { result: db } as { result: typeof db; onupgradeneeded?: () => void; onsuccess?: () => void };
      queueMicrotask(() => {
        req.onupgradeneeded?.();
        req.onsuccess?.();
      });
      return req;
    },
  };
}

const A = 'a'.repeat(64);
const B = 'b'.repeat(64);

describe('the database cache', () => {
  let rows: Map<string, unknown>;
  beforeEach(() => {
    rows = new Map();
    vi.stubGlobal('indexedDB', fakeIndexedDb(rows));
  });
  afterEach(() => vi.unstubAllGlobals());

  it('gives back what was saved, under the specification’s hash and no other', async () => {
    expect(await loadDatabase(A)).toBeUndefined();
    expect(await saveDatabase(A, '{"chain":1}')).toBe(true);
    expect(await loadDatabase(A)).toBe('{"chain":1}');
    expect(await loadDatabase(B)).toBeUndefined();
  });

  it('refuses to save empty or oversized content, and ignores a stored entry that is not what it writes', async () => {
    expect(await saveDatabase(A, '')).toBe(false);
    expect(await saveDatabase(A, 'x'.repeat(DB_MAX_BYTES + 1))).toBe(false);
    expect(rows.size).toBe(0);
    rows.set(A, { spec: A, content: 42 });
    rows.set(B, { spec: A, content: 'another entry under this key' });
    expect(await loadDatabase(A)).toBeUndefined();
    expect(await loadDatabase(B)).toBeUndefined();
  });

  it('falls back to a cold start when IndexedDB throws', async () => {
    vi.stubGlobal('indexedDB', {
      open: () => {
        throw new DOMException('blocked', 'SecurityError');
      },
    });
    await expect(loadDatabase(A)).resolves.toBeUndefined();
    await expect(saveDatabase(A, '{}')).resolves.toBe(false);
  });

  it('falls back to a cold start when there is no IndexedDB, or its open request errors', async () => {
    vi.stubGlobal('indexedDB', undefined);
    await expect(loadDatabase(A)).resolves.toBeUndefined();
    await expect(saveDatabase(A, '{}')).resolves.toBe(false);
    vi.stubGlobal('indexedDB', {
      open: () => {
        const req = {} as { onerror?: () => void; error: Error };
        req.error = new Error('denied');
        queueMicrotask(() => req.onerror?.());
        return req;
      },
    });
    await expect(loadDatabase(A)).resolves.toBeUndefined();
  });
});

describe('persistDatabases', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.stubGlobal('indexedDB', fakeIndexedDb(new Map()));
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it('saves now, every interval and on pagehide, skips a chain that fails, and stops when told', async () => {
    const saved: string[] = [];
    const dump = vi.fn(async () => `{"n":${saved.length}}`);
    const stop = persistDatabases(
      [
        { spec: A, dump },
        {
          spec: B,
          dump: async () => {
            throw new Error('the light client is gone');
          },
        },
      ],
      1_000,
    );
    await vi.advanceTimersByTimeAsync(0);
    expect(dump).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(2_000);
    expect(dump).toHaveBeenCalledTimes(3);
    globalThis.dispatchEvent?.(new Event('pagehide'));
    await vi.advanceTimersByTimeAsync(0);
    expect(dump.mock.calls.length).toBeGreaterThanOrEqual(3);
    stop();
    const n = dump.mock.calls.length;
    await vi.advanceTimersByTimeAsync(5_000);
    expect(dump).toHaveBeenCalledTimes(n);
    expect(await loadDatabase(B)).toBeUndefined();
    expect(await loadDatabase(A)).toMatch(/^\{"n":/);
  });
});
