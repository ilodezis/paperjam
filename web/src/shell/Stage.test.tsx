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

// The archive reads its screen context, as the real one does, and counts how often it is drawn.
const archive = vi.hoisted(() => ({ renders: 0 }));
vi.mock("../pages/ArchivePage", () => ({
  ArchivePage: () => {
    useScreen();
    archive.renders += 1;
    return <p data-testid="archive">archive</p>;
  },
}));
const documentPage = vi.hoisted(() => ({ renders: 0 }));
vi.mock("../pages/DocumentPage", () => ({
  DocumentPage: () => {
    const { path } = useScreen();
    documentPage.renders += 1;
    return <p data-testid="document">document {path}</p>;
  },
}));
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

  it("does not redraw a hidden screen on every commit", async () => {
    const { go, screens } = await mount("/");
    archive.renders = 0;
    await go("/documents/a", "push");
    expect(screens()).toEqual(["archive(hidden)", "document"]);
    // Its own `active` flip is the only change it is told about; the stage's commits and the
    // router's updates around the motion leave it alone.
    expect(archive.renders).toBe(1);
  });

  it("starts a pop's motion before redrawing either page", async () => {
    const { go, screens, router } = await mount("/");
    await go("/documents/a", "push");
    archive.renders = 0;
    documentPage.renders = 0;
    // What each page had drawn by the time the first frame of the motion was asked for.
    let atMotion: { archive: number; document: number } | null = null;
    const animate = vi.mocked(Element.prototype.animate);
    animate.mockImplementation(() => {
      atMotion ??= { archive: archive.renders, document: documentPage.renders };
      return { finished: Promise.resolve(), cancel: vi.fn() } as never;
    });
    try {
      await act(async () => {
        router.history.back();
      });
    } finally {
      animate.mockImplementation(() => ({ finished: Promise.resolve(), cancel: vi.fn() }) as never);
    }
    expect(screens()).toEqual(["archive"]);
    expect(atMotion).toEqual({ archive: 0, document: 0 });
    // The archive learns it is on top once the motion is over; the leaving document is never redrawn.
    expect(archive.renders).toBe(1);
    expect(documentPage.renders).toBe(0);
  });
});
