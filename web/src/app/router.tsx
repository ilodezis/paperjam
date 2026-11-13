import type { QueryClient } from "@tanstack/react-query";
import { createRootRouteWithContext, createRoute, createRouter } from "@tanstack/react-router";

import { api } from "../api";
import { hasPageImages } from "../document";
import { NotFoundPage } from "../pages/NotFoundPage";
import { preloadImage } from "../lib/preloadImage";
import { documentQuery, ensureFresh, searchQuery, statsQuery, taxonomyQuery, tokensQuery, trashQuery, usersQuery } from "../queries";
import type { SearchSort } from "../queries";
import { openScreen } from "../shell/screens";
import { queryClient } from "./queryClient";
import { Root } from "./Root";
import { hasSessionHint } from "./session";

export interface RouterContext {
  queryClient: QueryClient;
}

/** The archive's address carries its search; everything else has no query string. */
export interface ArchiveSearch {
  q?: string;
  sort?: Exclude<SearchSort, "relevance">;
}

/** The address as a plain query string, not JSON-in-the-URL: `?q=type:passport&sort=date_desc`. */
const parseSearch = (search: string): Record<string, unknown> => Object.fromEntries(new URLSearchParams(search));

function stringifySearch(search: Record<string, unknown>): string {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(search)) {
    if (value !== undefined && value !== null && value !== "") params.set(key, String(value));
  }
  const text = params.toString();
  return text === "" ? "" : `?${text}`;
}

/** A route's reads, started only on a device that has been signed in: before the sign-in they
 *  would only come back 401. */
const reads = (...calls: Array<() => Promise<unknown>>): Promise<unknown>[] => (hasSessionHint() ? calls.map((call) => call()) : []);

// The screens themselves are drawn by the stage; the routes hold the addresses and have each
// screen's code and data ready before the navigation commits, so the motion never starts on an
// empty screen. With the data already on the device (even an old copy) a route does not wait for
// the network at all: the screen opens on it and the fresh copy replaces it as it arrives.
/** How long a filter change waits for a fresh search before it shows the copy kept on the device. */
const SEARCH_FRESH_WAIT_MS = 600;

const rootRoute = createRootRouteWithContext<RouterContext>()({
  component: Root,
  // An address that is no screen of the app is shown as such, not as the archive.
  notFoundComponent: NotFoundPage,
});

const archiveRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  validateSearch: (search: Record<string, unknown>): ArchiveSearch => ({
    q: typeof search.q === "string" && search.q !== "" ? search.q : undefined,
    sort: search.sort === "date_asc" || search.sort === "date_desc" ? search.sort : undefined,
  }),
  loaderDeps: ({ search }) => ({ q: search.q ?? "", sort: (search.sort ?? "relevance") as SearchSort }),
  loader: async ({ context, deps }) => {
    await openScreen(
      "archive",
      ...reads(
        () => context.queryClient.ensureQueryData(taxonomyQuery),
        // An old copy of a search kept from an earlier visit would draw first and be swapped a
        // moment later (the list blinks); a short wait draws the current answer once.
        () => ensureFresh(context.queryClient, searchQuery(deps.q, deps.sort), SEARCH_FRESH_WAIT_MS),
      ),
    );
  },
});

const documentRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/documents/$id",
  loader: async ({ context, params }) => {
    await openScreen(
      "document",
      ...reads(
        () => context.queryClient.ensureQueryData(documentQuery(params.id, 1)),
        () => context.queryClient.ensureQueryData(taxonomyQuery),
        // Only a document with page images has a first page to preload.
        () => context.queryClient.ensureQueryData(documentQuery(params.id, 1)).then((document) => (hasPageImages(document) ? preloadImage(api.pageImageUrl(params.id, 1)) : undefined)),
      ),
    );
  },
});

const uploadRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/upload",
  loader: async () => {
    await openScreen("upload");
  },
});

const taxonomyRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/taxonomy",
  loader: async ({ context }) => {
    await openScreen("taxonomy", ...reads(() => context.queryClient.ensureQueryData(taxonomyQuery)));
  },
});

const trashRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/trash",
  loader: async ({ context }) => {
    await openScreen("trash", ...reads(() => context.queryClient.ensureQueryData(trashQuery)));
  },
});

const settingsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/settings",
  loader: async ({ context }) => {
    await openScreen(
      "settings",
      ...reads(
        () => context.queryClient.ensureQueryData(statsQuery),
        () => context.queryClient.ensureQueryData(tokensQuery),
        () => context.queryClient.ensureQueryData(usersQuery),
      ),
    );
  },
});

const routeTree = rootRoute.addChildren([archiveRoute, documentRoute, uploadRoute, taxonomyRoute, trashRoute, settingsRoute]);

export const router = createRouter({
  routeTree,
  context: { queryClient },
  parseSearch,
  stringifySearch,
  // The stage scrolls its own screens; the window never scrolls.
  scrollRestoration: false,
  defaultPreload: "intent",
  // Query owns freshness: loaders always run and ensureQueryData decides.
  defaultPreloadStaleTime: 0,
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
