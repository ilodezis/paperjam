import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nextProvider } from "react-i18next";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { createI18n } from "../i18n";
import { AppShell } from "./AppShell";

vi.mock("./Stage", () => ({ Stage: () => <p data-testid="stage">stage</p> }));
vi.mock("../app/session", () => ({ signOut: vi.fn() }));
vi.mock("../pwa", () => ({ monitorOnlineStatus: () => () => undefined }));
vi.mock("./screens", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./screens")>()),
  preloadScreen: () => Promise.resolve(),
}));

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

beforeAll(() => {
  // jsdom has no layout; the shell watches its own size to pick the rail or the bottom bar.
  if (!("ResizeObserver" in globalThis)) {
    (globalThis as { ResizeObserver?: unknown }).ResizeObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
    };
  }
  window.scrollTo = () => undefined;
});

let root: Root | null = null;
let container: HTMLDivElement | null = null;

afterEach(async () => {
  if (root) await act(async () => root?.unmount());
  container?.remove();
  root = null;
  container = null;
});

async function mount(path: string) {
  const rootRoute = createRootRoute({ component: () => <AppShell user={null} /> });
  const routes = ["/", "/upload", "/taxonomy", "/settings"].map((p) => createRoute({ getParentRoute: () => rootRoute, path: p }));
  const router = createRouter({
    routeTree: rootRoute.addChildren(routes),
    history: createMemoryHistory({ initialEntries: [path] }),
    scrollRestoration: false,
  });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () => {
    root?.render(
      <QueryClientProvider client={new QueryClient()}>
        <I18nextProvider i18n={createI18n("en")}>
          <RouterProvider router={router} />
        </I18nextProvider>
      </QueryClientProvider>,
    );
  });
}

/** The tabs and rail items that are lit, by their class and by aria-current. */
function lit() {
  const classed = Array.from(container?.querySelectorAll(".bnav a.on, .rail-a.on") ?? []);
  const current = Array.from(container?.querySelectorAll('[aria-current="page"]') ?? []);
  return { classed: classed.length, current: current.length };
}

describe("AppShell navigation", () => {
  it("lights the archive on the archive address", async () => {
    await mount("/");
    expect(container?.querySelector("[data-testid=stage]")).toBeTruthy();
    expect(lit()).toEqual({ classed: 2, current: 2 });
    expect(container?.querySelector(".bnav a.on")?.textContent).toContain("Archive");
  });

  it("lights nothing on an address that is no screen, and shows not found in its place", async () => {
    await mount("/nope");
    expect(container?.querySelector(".not-found-page")).toBeTruthy();
    expect(container?.querySelector("[data-testid=stage]")).toBeNull();
    expect(lit()).toEqual({ classed: 0, current: 0 });
  });
});
