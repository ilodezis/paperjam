import { createBrowserHistory, createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { useState, type ReactNode } from "react";

import type { ScreenId } from "./nav";
import { ScreenProvider } from "./ScreenFrame";

// jsdom has no layout to scroll; the router asks anyway.
window.scrollTo = () => undefined;

/** A screen outside the stage, for tests: the context a screen reads (its place in the stack)
 *  and a router of its own to navigate with. Browser history is opt-in for beforeunload coverage. */
export function ScreenHarness({ id = "archive", path = "/", search = "", browserHistory = false, children }: {
  id?: ScreenId;
  path?: string;
  search?: string;
  browserHistory?: boolean;
  children: ReactNode;
}) {
  const [router] = useState(() => {
    const root = createRootRoute();
    const index = createRoute({
      getParentRoute: () => root,
      path: "/",
      component: () => (
        <ScreenProvider info={{ id, path, search, back: null, active: true }}>{children}</ScreenProvider>
      ),
    });
    const history = browserHistory ? createBrowserHistory() : createMemoryHistory({ initialEntries: ["/"] });
    return createRouter({ routeTree: root.addChildren([index]), scrollRestoration: false, history });
  });
  return <RouterProvider router={router} />;
}
