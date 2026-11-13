import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nextProvider } from "react-i18next";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { createI18n } from "../i18n";
import { announceNavigation } from "./navIntent";
import { Stage, type StackInfo } from "./Stage";
import { preloadScreen } from "./screens";
import { useScreen } from "./ScreenFrame";

vi.mock("../pages/ArchivePage", () => ({ ArchivePage: () => <p data-testid="archive">archive</p> }));
vi.mock("../pages/DocumentPage", () => ({ DocumentPage: () => <p data-testid="document">document {useScreen().path}</p> }));
vi.mock("../pages/UploadPage", () => ({ UploadPage: () => <p>upload</p> }));
vi.mock("../pages/TaxonomyPage", () => ({ TaxonomyPage: () => <p>taxonomy</p> }));
vi.mock("../pages/TrashPage", () => ({ TrashPage: () => <p>trash</p> }));
vi.mock("../pages/SettingsPage", () => ({ SettingsPage: () => <p>settings</p> }));

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

beforeAll(async () => {
  // jsdom draws nothing: a motion is a promise that is already over.
  Element.prototype.animate = vi.fn(() => ({ finished: Promise.resolve(), cancel: vi.fn() })) as never;
  Element.prototype.getAnimations = () => [];
  Element.prototype.scrollTo = () => undefined;
  window.scrollTo = () => undefined;
  await Promise.all(["archive", "document", "upload", "taxonomy", "trash", "settings"].map((id) => preloadScreen(id as never)));
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
  const info: StackInfo[] = [];
  const rootRoute = createRootRoute({ component: () => <Stage onStack={(value) => info.push(value)} /> });
  const routes = ["/", "/documents/$id", "/upload", "/settings", "/trash"].map((p) => createRoute({ getParentRoute: () => rootRoute, path: p }));
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
        <I18nextProvider i18n={createI18n("en")}><RouterProvider router={router} /></I18nextProvider>
      </QueryClientProvider>,
    );
  });
  const go = async (to: string, mode: "push" | "tab") => {
    announceNavigation({ mode });
    await act(async () => {
      await router.navigate({ to: to as never, resetScroll: false });
    });
  };
  const screens = () => Array.from(container?.querySelectorAll<HTMLElement>(".screen") ?? []).map((el) => `${el.dataset.screen}${el.hidden ? "(hidden)" : ""}`);
  return { router, go, screens, info };
}

describe("Stage", () => {
  it("draws the screen the address names", async () => {
    const { screens } = await mount("/");
    expect(screens()).toEqual(["archive"]);
  });

  it("keeps the previous screen mounted, hidden, under a pushed one", async () => {
    const { go, screens, info } = await mount("/");
    await go("/documents/a", "push");
    expect(screens()).toEqual(["archive(hidden)", "document"]);
    expect(container?.textContent).toContain("document /documents/a");
    expect(info.at(-1)).toEqual({ root: "archive", top: "document", depth: 2 });
  });

  it("pops back to the screen underneath instead of stacking a copy", async () => {
    const { go, screens, router } = await mount("/");
    await go("/documents/a", "push");
    await go("/", "push");
    expect(screens()).toEqual(["archive"]);
    expect(router.state.location.pathname).toBe("/");
  });

  it("a tab starts the stack over", async () => {
    const { go, screens, info } = await mount("/");
    await go("/documents/a", "push");
    await go("/settings", "tab");
    expect(screens()).toEqual(["settings"]);
    expect(info.at(-1)).toEqual({ root: "settings", top: "settings", depth: 1 });
  });

  it("the browser's back, which nobody announced, pops too", async () => {
    const { go, screens, router } = await mount("/");
    await go("/documents/a", "push");
    await act(async () => {
      router.history.back();
    });
    expect(screens()).toEqual(["archive"]);
  });

  it("opens a pasted link as the only screen, with nothing to go back to", async () => {
    const { screens, info } = await mount("/documents/xyz");
    expect(screens()).toEqual(["document"]);
    expect(info.at(-1)).toEqual({ root: "document", top: "document", depth: 1 });
  });
});
