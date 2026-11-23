/* Every read the screens make, as query options: the router's loaders prefetch them, the screens
   subscribe to them, and the cache that holds them is kept on the device (app/persist.ts). */

import { keepPreviousData, queryOptions, type QueryClient, type QueryKey } from "@tanstack/react-query";

import { ApiError, api, type DocumentStatus, type DocumentView, type SearchOptions, type User } from "./api";

export type SessionData = { user: User | null };

/** Who is signed in. An answer of "nobody" is data (the sign-in page follows from it); a network
 *  failure is not, so a device that is offline keeps the user it had. */
export const sessionQuery = queryOptions({
  queryKey: ["session"],
  queryFn: async (): Promise<SessionData> => {
    try {
      return await api.session();
    } catch (cause) {
      if (cause instanceof ApiError && cause.status === 401) return { user: null };
      throw cause;
    }
  },
  staleTime: 60_000,
});

/* The taxonomy and the stats are counts of what the archive holds, and the device keeps them across
   reloads (app/persist.ts). A document finishing, a person or a type being added elsewhere (an
   agent, another device) changes them without this device being told, so they are short-lived: a
   reload after half a minute asks again (the router waits for the answer), and what this device
   itself changes invalidates them at once (`invalidateArchive`). */
export const taxonomyQuery = queryOptions({
  queryKey: ["taxonomy"],
  queryFn: api.taxonomy,
  staleTime: 30_000,
});

export const statsQuery = queryOptions({
  queryKey: ["stats"],
  queryFn: api.stats,
  staleTime: 30_000,
});

export const connectionsQuery = queryOptions({
  queryKey: ["connections"],
  queryFn: api.connections,
  staleTime: 60_000,
});

export const tokensQuery = queryOptions({
  queryKey: ["tokens"],
  queryFn: api.tokens,
  staleTime: 5 * 60_000,
});

/** The user list is the administrators'; everyone else gets a 403, which is "no list", not a failure. */
export const usersQuery = queryOptions({
  queryKey: ["users"],
  queryFn: async () => {
    try {
      return await api.users();
    } catch (cause) {
      if (cause instanceof ApiError && cause.status === 403) return null;
      throw cause;
    }
  },
  staleTime: 5 * 60_000,
});

export const calendarFeedQuery = queryOptions({
  queryKey: ["calendar-feed"],
  queryFn: api.calendarFeed,
  staleTime: 5 * 60_000,
});

export const emailSettingsQuery = queryOptions({
  queryKey: ["email-settings"],
  queryFn: async () => {
    try {
      return await api.emailSettings();
    } catch (cause) {
      if (cause instanceof ApiError && cause.status === 403) return null;
      throw cause;
    }
  },
  staleTime: 60_000,
});

export const passkeysQuery = queryOptions({
  queryKey: ["passkeys"],
  queryFn: api.passkeys,
  staleTime: 5 * 60_000,
});

/** Every read that is made of what the archive holds: lists, counts, names, people, types. A change
 *  to a document, or to the taxonomy it is described with, makes all of them stale together, so a
 *  screen never shows a list that disagrees with the one beside it. Only what is on screen is
 *  fetched again at once; the rest is fetched when it is next opened. Resolves when the taxonomy
 *  is fresh again, which is what a screen that selects what it just created waits for. */
export function invalidateArchive(client: QueryClient): Promise<void> {
  for (const key of ["search", "stats", "trash", "document", "document-events"]) {
    void client.invalidateQueries({ queryKey: [key] });
  }
  return client.invalidateQueries({ queryKey: ["taxonomy"] });
}

/** Calls `invalidateArchive` when a document the cache has been following settles (queued or
 *  processing → ready or failed): that is the moment its people, organizations and type appear in
 *  the archive's names and counts. Covers every screen that polls a document, not only the upload page. */
export function watchIngest(client: QueryClient): () => void {
  const seen = new Map<string, DocumentStatus>();
  return client.getQueryCache().subscribe((event) => {
    const { query } = event;
    if (event.type !== "updated" || query.queryKey[0] !== "document") return;
    const status = (query.state.data as DocumentView | undefined)?.status;
    if (!status) return;
    const id = String(query.queryKey[1]);
    const before = seen.get(id);
    seen.set(id, status);
    if ((before === "queued" || before === "processing") && (status === "ready" || status === "failed")) invalidateArchive(client);
  });
}

export type SearchSort = NonNullable<SearchOptions["sort"]>;

export const searchQuery = (query: string, sort: SearchSort) =>
  queryOptions({
    queryKey: ["search", query, sort],
    queryFn: () => api.search({ query, sort }),
    staleTime: 60_000,
    // A new filter keeps the list it replaces on screen (dimmed) until its own answer is in,
    // instead of dropping to skeletons for a moment and drawing the list again.
    placeholderData: keepPreviousData,
  });

/** Have a read ready before a screen draws it. Nothing cached: fetch and wait. A fresh copy: use it.
 *  An old copy (one kept on the device from an earlier visit): give the network `waitMs` to answer,
 *  so the screen is drawn once with the current answer instead of drawing the old one and swapping
 *  it a moment later; a slow or failed refresh falls back to the old copy. */
export async function ensureFresh<T, K extends QueryKey>(
  client: QueryClient,
  options: { queryKey: K; queryFn?: unknown; staleTime?: unknown },
  waitMs: number,
): Promise<void> {
  const typed = options as Parameters<QueryClient["fetchQuery"]>[0] & { queryKey: K };
  const query = client.getQueryCache().find<T>({ queryKey: typed.queryKey, exact: true });
  if (query?.state.data === undefined) {
    await client.ensureQueryData(typed);
    return;
  }
  const staleTime = typeof typed.staleTime === "number" ? typed.staleTime : 0;
  if (!query.isStaleByTime(staleTime)) return;
  let timer: ReturnType<typeof setTimeout> | undefined;
  await Promise.race([
    client.fetchQuery(typed).then(() => undefined, () => undefined),
    new Promise<void>((resolve) => { timer = setTimeout(resolve, waitMs); }),
  ]);
  clearTimeout(timer);
}

export const trashQuery = queryOptions({
  queryKey: ["trash"],
  queryFn: async (): Promise<DocumentView[]> => {
    const page = await api.search({ query: "", sort: "date_desc", includeTrashed: true });
    const details = await Promise.all(page.hits.map((hit) => api.document(hit.id)));
    return details.filter((document) => Boolean(document.trashed_at));
  },
  staleTime: 60_000,
});

/** A document, with the text of one page. A document still being processed refreshes itself. */
export const documentQuery = (id: string, page: number) =>
  queryOptions({
    queryKey: ["document", id, page],
    queryFn: () => api.document(id, page),
    staleTime: 30_000,
    refetchInterval: (query) => {
      const status = query.state.data?.status;
      return status === "queued" || status === "processing" ? 1500 : false;
    },
  });

export const documentEventsQuery = (id: string) =>
  queryOptions({
    queryKey: ["document-events", id],
    queryFn: () => api.documentEvents(id),
    staleTime: 30_000,
  });
