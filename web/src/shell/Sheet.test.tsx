import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nextProvider } from "react-i18next";
import { afterEach, beforeAll, describe, expect, it } from "vitest";

import { createI18n } from "../i18n";
import { Sheet } from "./Sheet";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let container: HTMLDivElement | null = null;

beforeAll(() => {
  // jsdom has no Web Animations: a sheet that animates out is gone as soon as it resolves.
  Element.prototype.animate = (() => ({ finished: Promise.resolve(), cancel: () => undefined })) as unknown as Element["animate"];
});

afterEach(async () => {
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
});
