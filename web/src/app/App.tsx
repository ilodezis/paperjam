import { useIsRestoring } from "@tanstack/react-query";
import { PersistQueryClientProvider } from "@tanstack/react-query-persist-client";
import { RouterProvider } from "@tanstack/react-router";

import { CACHE_BUSTER, CACHE_MAX_AGE_MS, persister, shouldPersist } from "./persist";
import { queryClient } from "./queryClient";
import { router } from "./router";

export function App() {
  return (
    <PersistQueryClientProvider
      client={queryClient}
      persistOptions={{
        persister,
        maxAge: CACHE_MAX_AGE_MS,
        buster: CACHE_BUSTER,
        dehydrateOptions: { shouldDehydrateQuery: shouldPersist },
      }}
    >
      <Routed />
    </PersistQueryClientProvider>
  );
}

/** The router's first loaders read the cache, so they wait for it to come back from the device:
 *  a few milliseconds, and the app opens on what it last saw instead of on the network. */
function Routed() {
  return useIsRestoring() ? null : <RouterProvider router={router} />;
}
