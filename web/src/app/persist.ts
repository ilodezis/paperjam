import { del, get, set } from "idb-keyval";
import type { PersistedClient, Persister } from "@tanstack/react-query-persist-client";
import type { Query } from "@tanstack/react-query";

/** How long the device keeps what it was told. Also the cache's `gcTime`, which may not be shorter. */
export const CACHE_MAX_AGE_MS = 24 * 60 * 60 * 1000;

/** Bump when the shape of anything cached changes: an older build's cache is then dropped. */
export const CACHE_BUSTER = "paperjam-cache-3";

const KEY = "paperjam-query-cache";

/** How often the cache is written out at most. The provider asks on every change a query goes
 *  through, and every write copies the whole cache into storage on the main thread. */
export const PERSIST_THROTTLE_MS = 1000;

/** Queries that never leave memory: credentials' metadata and the administrators' lists. */
const MEMORY_ONLY = new Set(["tokens", "users", "passkeys"]);

export const shouldPersist = (query: Query): boolean =>
  query.state.status === "success" && !MEMORY_ONLY.has(String(query.queryKey[0]));

interface Storage {
  set: (key: string, value: PersistedClient) => Promise<void>;
  get: (key: string) => Promise<PersistedClient | undefined>;
  del: (key: string) => Promise<void>;
}

/** The query cache in IndexedDB. Storage that is missing or refuses (a private window, a full
 *  disk) is not an error: the app then simply opens from the network.
 *
 *  Writes are coalesced: the newest state is written at most once per `throttleMs`, and at once
 *  when the page is hidden, so an app swiped away keeps what it had just been told. */
export function createPersister(storage: Storage = { set, get, del }, throttleMs = PERSIST_THROTTLE_MS): Persister {
  let pending: PersistedClient | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;

  const write = async () => {
    if (timer !== null) clearTimeout(timer);
    timer = null;
    const client = pending;
    pending = null;
    if (client === null) return;
    try {
      await storage.set(KEY, client);
    } catch {
      /* nothing to keep it in */
    }
  };

  if (typeof document !== "undefined") {
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "hidden") void write();
    });
  }

  return {
    persistClient: (client: PersistedClient) => {
      pending = client;
      if (timer === null) timer = setTimeout(() => void write(), throttleMs);
    },
    restoreClient: async () => {
      try {
        return await storage.get(KEY);
      } catch {
        return undefined;
      }
    },
    removeClient: async () => {
      // A write still waiting would bring back what is being dropped.
      if (timer !== null) clearTimeout(timer);
      timer = null;
      pending = null;
      try {
        await storage.del(KEY);
      } catch {
        /* already gone */
      }
    },
  };
}

export const persister: Persister = createPersister();
