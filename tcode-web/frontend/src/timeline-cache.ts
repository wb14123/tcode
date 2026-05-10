const DB_NAME = 'tcode-timeline-cache';
const DB_VERSION = 1;
const STORE_NAME = 'entries';
const MAX_ENTRIES = 50;

export interface TimelineCacheEntry {
  sessionId: string;
  events: string[];
  byteOffset: number;
  lastLineNumber: number;
  updatedAt: number;
}

function isIndexedDBAvailable(): boolean {
  try {
    return typeof indexedDB !== 'undefined' && indexedDB !== null;
  } catch {
    return false;
  }
}

function openDB(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    if (!isIndexedDBAvailable()) {
      reject(new Error('IndexedDB not available'));
      return;
    }

    const request = indexedDB.open(DB_NAME, DB_VERSION);

    request.onupgradeneeded = () => {
      const db = request.result;
      if (!db.objectStoreNames.contains(STORE_NAME)) {
        db.createObjectStore(STORE_NAME);
      }
    };

    request.onsuccess = () => {
      resolve(request.result);
    };

    request.onerror = () => {
      reject(request.error ?? new Error('Failed to open IndexedDB'));
    };
  });
}

function cacheKey(sessionId: string, subagentId?: string): string {
  if (subagentId !== undefined && subagentId.length > 0) {
    return `${sessionId}::subagent::${subagentId}`;
  }
  return `${sessionId}-display`;
}

async function evictOldest(
  db: IDBDatabase,
  excludeSessionId: string,
  neededSlots: number,
): Promise<void> {
  const tx = db.transaction(STORE_NAME, 'readwrite');
  const store = tx.objectStore(STORE_NAME);

  const allEntries: { key: string; updatedAt: number }[] = await new Promise(
    (resolve, reject) => {
      const items: { key: string; updatedAt: number }[] = [];
      const cursorReq = store.openCursor();

      cursorReq.onsuccess = () => {
        const cursor = cursorReq.result;
        if (cursor) {
          const entry = cursor.value as TimelineCacheEntry;
          if (entry.sessionId !== excludeSessionId) {
            items.push({ key: cursor.key as string, updatedAt: entry.updatedAt });
          }
          cursor.continue();
        } else {
          resolve(items);
        }
      };

      cursorReq.onerror = () => {
        reject(cursorReq.error ?? new Error('Cursor iteration failed'));
      };
    },
  );

  // Sort by updatedAt ascending (oldest first) and evict neededSlots
  allEntries.sort((a, b) => a.updatedAt - b.updatedAt);
  const toEvict = allEntries.slice(0, neededSlots);

  for (const item of toEvict) {
    store.delete(item.key);
  }

  await new Promise<void>((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error ?? new Error('Eviction transaction failed'));
  });
}

async function enforceCapacity(
  db: IDBDatabase,
  currentSessionId: string,
): Promise<void> {
  const count = await new Promise<number>((resolve, reject) => {
    const tx = db.transaction(STORE_NAME, 'readonly');
    const store = tx.objectStore(STORE_NAME);
    const req = store.count();
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error ?? new Error('Count failed'));
  });

  const excess = count - MAX_ENTRIES;
  if (excess > 0) {
    await evictOldest(db, currentSessionId, excess);
  }
}

export const timelineCache = {
  async load(
    sessionId: string,
    subagentId?: string,
  ): Promise<TimelineCacheEntry | null> {
    if (!isIndexedDBAvailable()) {
      return null;
    }

    let db: IDBDatabase;
    try {
      db = await openDB();
    } catch {
      return null;
    }

    try {
      const key = cacheKey(sessionId, subagentId);
      const entry = await new Promise<TimelineCacheEntry | undefined>(
        (resolve, reject) => {
          const tx = db.transaction(STORE_NAME, 'readonly');
          const store = tx.objectStore(STORE_NAME);
          const req = store.get(key);
          req.onsuccess = () => resolve(req.result as TimelineCacheEntry | undefined);
          req.onerror = () => reject(req.error ?? new Error('Get failed'));
        },
      );

      return entry ?? null;
    } catch {
      return null;
    } finally {
      db.close();
    }
  },

  async save(
    sessionId: string,
    subagentId: string | undefined,
    entry: TimelineCacheEntry,
  ): Promise<void> {
    if (!isIndexedDBAvailable()) {
      return;
    }

    let db: IDBDatabase;
    try {
      db = await openDB();
    } catch {
      return;
    }

    try {
      const key = cacheKey(sessionId, subagentId);
      await new Promise<void>((resolve, reject) => {
        const tx = db.transaction(STORE_NAME, 'readwrite');
        const store = tx.objectStore(STORE_NAME);
        const req = store.put(entry, key);
        req.onsuccess = () => resolve();
        req.onerror = () => reject(req.error ?? new Error('Put failed'));
      });

      await enforceCapacity(db, entry.sessionId);
    } catch {
      // Silently ignore save failures
    } finally {
      db.close();
    }
  },

  async remove(sessionId: string, subagentId?: string): Promise<void> {
    if (!isIndexedDBAvailable()) {
      return;
    }

    let db: IDBDatabase;
    try {
      db = await openDB();
    } catch {
      return;
    }

    try {
      const key = cacheKey(sessionId, subagentId);
      await new Promise<void>((resolve, reject) => {
        const tx = db.transaction(STORE_NAME, 'readwrite');
        const store = tx.objectStore(STORE_NAME);
        const req = store.delete(key);
        req.onsuccess = () => resolve();
        req.onerror = () => reject(req.error ?? new Error('Delete failed'));
      });
    } catch {
      // Silently ignore remove failures
    } finally {
      db.close();
    }
  },
};
