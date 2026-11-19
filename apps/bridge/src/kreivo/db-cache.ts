// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The light client's database, kept in the browser (F-018 v0.5.1): smoldot's own `chainHead_unstable_finalizedDatabase`
// (a finalized block, the consensus authorities and some peers) is saved to IndexedDB under the SHA-256 of the
// chain specification it came from, and given back as `databaseContent` on the next visit, so the page does not
// warp-sync the relay chain from the specification's old checkpoint every time. It is a cache of public chain
// data: nothing about the user is stored, and a different specification never finds another's entry.
//
// Every access to storage is guarded: a private window, blocked site data or a full disk only means a cold start.

/** What is kept per chain at most, in bytes: smoldot's database for one chain is a few hundred KiB. */
export const DB_MAX_BYTES = 2 * 1024 * 1024;
/** How often a synced chain's database is saved. */
export const SAVE_EVERY_MS = 60_000;

const NAME = 'bezent-bridge-smoldot';
const STORE = 'database';

interface Entry {
  /** The chain specification's SHA-256 (also the entry's key). */
  spec: string;
  content: string;
}

const bytes = (s: string) => new TextEncoder().encode(s).length;

function open(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = globalThis.indexedDB.open(NAME, 1);
    req.onupgradeneeded = () => req.result.createObjectStore(STORE, { keyPath: 'spec' });
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error ?? new Error('IndexedDB did not open'));
    req.onblocked = () => reject(new Error('IndexedDB is blocked'));
  });
}

function done<T>(req: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error ?? new Error('IndexedDB failed'));
  });
}

/** The saved database for the specification with this SHA-256, or `undefined`: nothing saved, storage that cannot
 * be used, or an entry that is not what this code writes. Never throws. */
export async function loadDatabase(spec: string): Promise<string | undefined> {
  try {
    const db = await open();
    try {
      const e = (await done(db.transaction(STORE, 'readonly').objectStore(STORE).get(spec))) as Entry | undefined;
      if (!e || e.spec !== spec || typeof e.content !== 'string' || bytes(e.content) > DB_MAX_BYTES) return undefined;
      return e.content;
    } finally {
      db.close();
    }
  } catch {
    return undefined;
  }
}

/** Saves `content` for the specification with this SHA-256, unless it is empty or over `DB_MAX_BYTES`. Returns
 * whether it was saved. Never throws. */
export async function saveDatabase(spec: string, content: string): Promise<boolean> {
  try {
    if (typeof content !== 'string' || content === '' || bytes(content) > DB_MAX_BYTES) return false;
    const db = await open();
    try {
      const tx = db.transaction(STORE, 'readwrite');
      const finished = new Promise<void>((resolve, reject) => {
        tx.oncomplete = () => resolve();
        tx.onerror = () => reject(tx.error ?? new Error('IndexedDB failed'));
        tx.onabort = () => reject(tx.error ?? new Error('IndexedDB aborted'));
      });
      tx.objectStore(STORE).put({ spec, content } satisfies Entry);
      await finished;
      return true;
    } finally {
      db.close();
    }
  } catch {
    return false;
  }
}

export interface Persisted {
  /** The chain specification's SHA-256. */
  spec: string;
  /** Asks the light client for the chain's database (at most `DB_MAX_BYTES`). */
  dump(): Promise<unknown>;
}

/** Saves each chain's database now, every `everyMs`, and when the page is hidden or closed (`pagehide`), until
 * the returned function is called. A chain whose dump fails is skipped. */
export function persistDatabases(chains: Persisted[], everyMs: number = SAVE_EVERY_MS): () => void {
  const saveAll = async () => {
    await Promise.all(
      chains.map(async (c) => {
        try {
          const content = await c.dump();
          if (typeof content === 'string') await saveDatabase(c.spec, content);
        } catch {
          // The light client may be shutting down: the next save tries again.
        }
      }),
    );
  };
  void saveAll();
  const timer = setInterval(() => void saveAll(), everyMs);
  const onHide = () => void saveAll();
  globalThis.addEventListener?.('pagehide', onHide);
  return () => {
    clearInterval(timer);
    globalThis.removeEventListener?.('pagehide', onHide);
  };
}
