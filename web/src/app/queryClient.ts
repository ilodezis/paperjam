import { QueryClient } from "@tanstack/react-query";

import { watchIngest } from "../queries";
import { CACHE_MAX_AGE_MS } from "./persist";

// One client for the app's lifetime: the router's loaders and the screens share the same
// cache, and the device keeps it (see persist.ts).
export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      // A query outlives its last screen for as long as the device keeps the cache; a shorter
      // gcTime would drop it from memory before it could be written out.
      gcTime: CACHE_MAX_AGE_MS,
      refetchOnWindowFocus: false,
      retry: 1,
      networkMode: "offlineFirst",
    },
    mutations: { networkMode: "offlineFirst" },
  },
});

// A document that finishes processing brings names, types and counts the cached lists lack.
watchIngest(queryClient);
