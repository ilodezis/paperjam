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
  vi.unstubAllGlobals();
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

/** The page as an installed app (the edge swipe is ours) or as a browser tab (the system's is). */
function stubStandalone(standalone: boolean) {
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: standalone && query.includes("standalone"),
    media: query,
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
  }));
}

/** A touch of the stage; the stage is 400px wide in the tests that drag. */
function touch(type: "pointerdown" | "pointermove" | "pointerup", x: number, y = 300) {
  const stage = container?.querySelector<HTMLElement>(".stage") as HTMLElement;
  stage.dispatchEvent(new PointerEvent(type, { bubbles: true, pointerId: 7, pointerType: "touch", clientX: x, clientY: y, button: 0 }));
}

function sizeStage(width: number) {
  const stage = container?.querySelector<HTMLElement>(".stage") as HTMLElement;
  Object.defineProperty(stage, "offsetWidth", { configurable: true, value: width });
  stage.setPointerCapture = vi.fn();
}

const screenEl = (id: string) => container?.querySelector<HTMLElement>(`[data-screen="${id}"]`) as HTMLElement;

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

  it("interrupts an in-flight transition cleanly when a newer navigation arrives", async () => {
    const { go, screens } = await mount("/");
    let finishFirst: () => void = () => undefined;
    const firstHeld = new Promise<void>((resolve) => { finishFirst = resolve; });
    const firstCancel = vi.fn();
    let batch = 0;
    const animate = vi.mocked(Element.prototype.animate);
    animate.mockImplementation(() => {
      if (batch === 0) return { finished: firstHeld, cancel: firstCancel } as never;
      return { finished: Promise.resolve(), cancel: vi.fn() } as never;
    });
    try {
      await go("/documents/a", "push");
      // First push is still running; immediately switch to settings tab.
      batch = 1;
      await go("/settings", "tab");
      expect(firstCancel).toHaveBeenCalled();
      expect(screens()).toEqual(["settings"]);
      // Late settlement of the interrupted animation must not disturb the current screen.
      await act(async () => finishFirst());
      expect(screens()).toEqual(["settings"]);
    } finally {
      animate.mockImplementation(() => ({ finished: Promise.resolve(), cancel: vi.fn() }) as never);
    }
  });

  describe("swipe back from the left edge", () => {
    it("leaves a tap on the back button alone, so the address and the stack still agree", async () => {
      stubStandalone(true);
      const { go, screens, router } = await mount("/");
      await go("/documents/a", "push");
      // A swipe that began on the touch would hold the stage busy for its settle motion; the
      // button's click arrives meanwhile.
      let release: () => void = () => undefined;
      const held = new Promise<void>((resolve) => (release = resolve));
      const animate = vi.mocked(Element.prototype.animate);
      animate.mockImplementation(() => ({ finished: held, cancel: vi.fn() }) as never);
      try {
        await act(async () => {
          touch("pointerdown", 10);
          touch("pointerup", 10);
          router.history.back();
        });
        await act(async () => release());
      } finally {
        animate.mockImplementation(() => ({ finished: Promise.resolve(), cancel: vi.fn() }) as never);
      }
      expect(screens()).toEqual(["archive"]);
      expect(router.state.location.pathname).toBe("/");
    });

    it("follows the finger from the edge when installed, then pops and takes the address along", async () => {
      stubStandalone(true);
      const { go, screens, router } = await mount("/");
      await go("/documents/a", "push");
      sizeStage(400);
      await act(async () => {
        touch("pointerdown", 10);
        touch("pointermove", 14);
      });
      // Under the slop it is still a tap.
      expect(screenEl("document").style.transform).toBe("");
      await act(async () => touch("pointermove", 200));
      expect(screenEl("document").style.transform).toMatch(/^translateX\(.+px\)$/);
      expect(screenEl("archive").hidden).toBe(false);
      await act(async () => touch("pointerup", 200));
      await act(async () => undefined);
      expect(screens()).toEqual(["archive"]);
      expect(router.state.location.pathname).toBe("/");
    });

    it("does not start in a browser tab, where the system's back gesture owns the edge", async () => {
      stubStandalone(false);
      const { go, screens } = await mount("/");
      await go("/documents/a", "push");
      sizeStage(400);
      await act(async () => {
        touch("pointerdown", 10);
        touch("pointermove", 200);
        touch("pointerup", 200);
      });
      expect(screens()).toEqual(["archive(hidden)", "document"]);
      expect(screenEl("document").style.transform).toBe("");
    });

    it("lets a mostly vertical move from the edge scroll instead", async () => {
      stubStandalone(true);
      const { go, screens } = await mount("/");
      await go("/documents/a", "push");
      sizeStage(400);
      await act(async () => {
        touch("pointerdown", 10, 100);
        touch("pointermove", 14, 140);
        // It drifts right later; the gesture was already given up.
        touch("pointermove", 200, 145);
      });
      expect(screenEl("document").style.transform).toBe("");
      expect(screenEl("archive").hidden).toBe(true);
      await act(async () => touch("pointerup", 200, 145));
      expect(screens()).toEqual(["archive(hidden)", "document"]);
    });
  });
});
