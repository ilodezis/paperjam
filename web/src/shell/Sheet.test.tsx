import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nextProvider } from "react-i18next";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { createI18n } from "../i18n";
import { Sheet } from "./Sheet";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let container: HTMLDivElement | null = null;

/** Tracks live animations per element so tests can verify `settle()` releases WAAPI fill. */
const activeAnimations = new WeakMap<Element, Set<Animation>>();

function installAnimationStub() {
  Element.prototype.animate = (function (this: Element) {
    let set = activeAnimations.get(this);
    if (!set) {
      set = new Set();
      activeAnimations.set(this, set);
    }
    const anim = {
      finished: Promise.resolve(),
      cancel: vi.fn(() => {
        set?.delete(anim as unknown as Animation);
      }),
    } as unknown as Animation;
    set.add(anim);
    return anim;
  }) as unknown as Element["animate"];
  Element.prototype.getAnimations = function (this: Element) {
    return Array.from(activeAnimations.get(this) ?? []);
  };
}

beforeAll(() => {
  installAnimationStub();
});

afterEach(async () => {
  installAnimationStub();
  if (root) await act(async () => root?.unmount());
  container?.remove();
  root = null;
  container = null;
});

function Harness() {
  const [open, setOpen] = useState(false);
  return (
    <>
      <main>
        <button type="button" id="opener" onClick={() => setOpen(true)}>Open</button>
        <button type="button" id="behind">Behind</button>
      </main>
      <Sheet open={open} title="Actions" onClose={() => setOpen(false)}>
        <button type="button" id="first">First</button>
        <button type="button" id="second">Second</button>
        <button type="button" id="last">Last</button>
      </Sheet>
    </>
  );
}

async function mount() {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () => {
    root?.render(<I18nextProvider i18n={createI18n("en")}><Harness /></I18nextProvider>);
  });
  const opener = document.getElementById("opener") as HTMLButtonElement;
  opener.focus();
  await act(async () => {
    opener.click();
  });
  return opener;
}

async function press(key: string, init: KeyboardEventInit = {}) {
  const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...init });
  await act(async () => {
    (document.activeElement ?? document.body).dispatchEvent(event);
  });
  return event;
}

const dialog = () => document.querySelector<HTMLElement>('[role="dialog"]');

describe("Sheet", () => {
  it("takes focus when it opens and makes the page behind inert", async () => {
    await mount();
    expect(document.activeElement).toBe(dialog());
    expect(document.getElementById("opener")?.closest("[inert]")).not.toBeNull();
    expect(dialog()?.closest("[inert]")).toBeNull();
    expect(document.querySelector(".scrim")?.closest("[inert]")).toBeNull();
  });

  it("cycles Tab and Shift+Tab inside the dialog", async () => {
    await mount();
    const first = document.getElementById("first");
    const last = document.getElementById("last");

    const forward = await press("Tab");
    expect(forward.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(first);

    const backward = await press("Tab", { shiftKey: true });
    expect(backward.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(last);

    const wrap = await press("Tab");
    expect(wrap.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(first);

    // Between the ends the browser moves focus itself; the trap must not interfere.
    document.getElementById("second")?.focus();
    expect((await press("Tab")).defaultPrevented).toBe(false);
  });

  it("pulls stray focus back into the dialog", async () => {
    await mount();
    await act(async () => {
      document.getElementById("behind")?.focus();
    });
    expect(dialog()?.contains(document.activeElement)).toBe(true);
  });

  it("closes on Escape, lifts the inert background and returns focus to the opener", async () => {
    const opener = await mount();
    await press("Escape");
    await act(async () => {
      await Promise.resolve();
    });

    expect(dialog()).toBeNull();
    expect(document.querySelector("[inert]")).toBeNull();
    expect(document.activeElement).toBe(opener);
  });

  it("returns focus to the opener after the parent re-rendered while open", async () => {
    const opener = await mount();
    // A re-render hands the sheet a fresh onClose; that must not forget who opened it.
    await act(async () => {
      document.getElementById("first")?.focus();
    });
    await act(async () => {
      root?.render(<I18nextProvider i18n={createI18n("en")}><Harness /></I18nextProvider>);
    });
    await press("Escape");
    await act(async () => {
      await Promise.resolve();
    });
    expect(document.activeElement).toBe(opener);
  });

  it("settles the open WAAPI fill so dragging the grab handle moves and dismisses the sheet", async () => {
    await mount();
    // Wait for the open animation to settle.
    await act(async () => {
      await Promise.resolve();
    });
    const el = dialog()!;
    const scrim = document.querySelector<HTMLElement>(".scrim")!;
    expect(el.getAnimations()).toHaveLength(0);
    expect(scrim.getAnimations()).toHaveLength(0);

    // Simulate a lingering animation before pointerdown to verify defensive settle too.
    el.animate([], { duration: 260, fill: "both" });
    expect(el.getAnimations()).toHaveLength(1);

    Object.defineProperty(el, "offsetHeight", { configurable: true, value: 400 });
    const grab = el.querySelector<HTMLElement>(".sheet-grab")!;
    grab.setPointerCapture = vi.fn();

    await act(async () => {
      grab.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, pointerId: 3, pointerType: "touch", clientY: 100 }));
    });
    expect(el.getAnimations()).toHaveLength(0);

    await act(async () => {
      grab.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerId: 3, pointerType: "touch", clientY: 260 }));
    });
    expect(el.style.transform).toBe("translateY(160px)");

    await act(async () => {
      grab.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, pointerId: 3, pointerType: "touch", clientY: 260 }));
      await Promise.resolve();
    });
    expect(dialog()).toBeNull();
  });

  it("hands off focus, inert background, and scrim cleanly across consecutive sheets", async () => {
    function HandoffHarness() {
      const [first, setFirst] = useState(false);
      const [second, setSecond] = useState(false);
      return (
        <>
          <main>
            <button type="button" id="open-first" onClick={() => setFirst(true)}>Open actions</button>
          </main>
          <Sheet open={first} title="Actions" onClose={() => setFirst(false)}>
            <button
              type="button"
              id="open-second"
              onClick={() => {
                setFirst(false);
                setSecond(true);
              }}
            >
              Share
            </button>
          </Sheet>
          <Sheet open={second} title="Share" onClose={() => setSecond(false)}>
            <button type="button" id="share-action">Create link</button>
          </Sheet>
        </>
      );
    }

    // Hold exit animations to simulate a real 200ms close animation overlapping the second sheet.
    const pendingExit = new Promise<void>(() => undefined);
    let call = 0;
    Element.prototype.animate = (function (this: Element) {
      call += 1;
      const anim = {
        finished: call > 2 && call <= 4 ? pendingExit : Promise.resolve(),
        cancel: vi.fn(),
      } as unknown as Animation;
      return anim;
    }) as unknown as Element["animate"];

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    await act(async () => {
      root?.render(<I18nextProvider i18n={createI18n("en")}><HandoffHarness /></I18nextProvider>);
    });

    const opener = document.getElementById("open-first") as HTMLButtonElement;
    opener.focus();
    await act(async () => {
      opener.click();
    });
    expect(document.querySelectorAll('[role="dialog"]')).toHaveLength(1);

    await act(async () => {
      document.getElementById("open-second")?.click();
    });

    // Only Share sheet and one scrim remain; background stays inert and focus is in Share sheet.
    const dialogs = document.querySelectorAll<HTMLElement>('[role="dialog"]');
    expect(dialogs).toHaveLength(1);
    expect(dialogs[0]?.getAttribute("aria-label")).toBe("Share");
    expect(document.querySelectorAll(".scrim")).toHaveLength(1);
    expect(opener.closest("[inert]")).not.toBeNull();
    expect(dialogs[0]?.contains(document.activeElement)).toBe(true);

    // Closing Share returns focus to the page opener and lifts inert.
    installAnimationStub();
    await press("Escape");
    await act(async () => {
      await Promise.resolve();
    });
    expect(document.querySelector('[role="dialog"]')).toBeNull();
    expect(document.querySelector("[inert]")).toBeNull();
    expect(document.activeElement).toBe(opener);
  });
});
