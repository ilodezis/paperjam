import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nextProvider } from "react-i18next";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { createI18n } from "../i18n";
import { BottomNav } from "./BottomNav";
import type { ScreenId } from "./nav";
import * as screens from "./screens";

vi.mock("./screens", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./screens")>()),
  preloadScreen: vi.fn(() => Promise.resolve()),
}));

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

beforeAll(() => {
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
  vi.clearAllMocks();
  if (root) await act(async () => root?.unmount());
  container?.remove();
  root = null;
  container = null;
});

async function mountNav(options: { holdSettings?: () => Promise<void>; blockSettings?: boolean } = {}) {
  let setRootScreen!: (id: ScreenId) => void;
  function Host() {
    const [screen, setScreen] = useState<ScreenId>("archive");
    setRootScreen = setScreen;
    return <BottomNav root={screen} depth={1} />;
  }

  const rootRoute = createRootRoute({ component: Host });
  const archive = createRoute({ getParentRoute: () => rootRoute, path: "/" });
  const upload = createRoute({ getParentRoute: () => rootRoute, path: "/upload" });
  const taxonomy = createRoute({ getParentRoute: () => rootRoute, path: "/taxonomy" });
  const settings = createRoute({
    getParentRoute: () => rootRoute,
    path: "/settings",
    beforeLoad: () => {
      if (options.blockSettings) throw new Error("blocked");
    },
    loader: async () => {
      await options.holdSettings?.();
    },
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([archive, upload, taxonomy, settings]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
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

  const nav = container.querySelector<HTMLElement>(".bnav")!;
  Object.defineProperty(nav, "offsetWidth", { configurable: true, value: 400 });
  const links = Array.from(nav.querySelectorAll<HTMLAnchorElement>("a"));
  links.forEach((link, idx) => {
    Object.defineProperty(link, "offsetLeft", { configurable: true, value: idx * 100 });
    Object.defineProperty(link, "offsetWidth", { configurable: true, value: 100 });
  });

  return { router, nav, links, setRootScreen };
}

describe("BottomNav", () => {
  it("preloads the target route on pointerdown", async () => {
    const { router, links } = await mountNav();
    const preloadRoute = vi.spyOn(router, "preloadRoute").mockResolvedValue(undefined as never);
    const settingsLink = links.find((a) => a.getAttribute("href") === "/settings")!;

    await act(async () => {
      settingsLink.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, pointerType: "touch" }));
    });

    expect(screens.preloadScreen).toHaveBeenCalledWith("settings");
    expect(preloadRoute).toHaveBeenCalledWith(expect.objectContaining({ to: "/settings" }));
  });

  it("highlights the tapped tab and slides the ink bar optimistically before navigation finishes", async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    const { nav, links, setRootScreen } = await mountNav({ holdSettings: () => gate });
    const settingsLink = links.find((a) => a.getAttribute("href") === "/settings")!;
    const ink = nav.querySelector<HTMLElement>(".ink")!;

    await act(async () => {
      settingsLink.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    });

    // Active class, aria-current, and ink position update immediately even while the route loader is pending.
    expect(settingsLink.classList.contains("on")).toBe(true);
    expect(settingsLink.getAttribute("aria-current")).toBe("page");
    expect(ink.style.transform).toBe("translateX(340px)");

    await act(async () => {
      release();
      setRootScreen("settings");
    });
    expect(settingsLink.classList.contains("on")).toBe(true);
  });

  it("reverts optimistic highlight if navigation is cancelled and stays on the current tab", async () => {
    const { router, links } = await mountNav();
    vi.spyOn(router, "navigate").mockResolvedValue(undefined);
    const archiveLink = links.find((a) => a.getAttribute("href") === "/")!;
    const settingsLink = links.find((a) => a.getAttribute("href") === "/settings")!;

    await act(async () => {
      settingsLink.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
      await Promise.resolve();
    });

    expect(archiveLink.classList.contains("on")).toBe(true);
    expect(settingsLink.classList.contains("on")).toBe(false);
  });
});
