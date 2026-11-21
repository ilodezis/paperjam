import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ScreenFrame } from "./ScreenFrame";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let container: HTMLDivElement | null = null;
const original = Object.getOwnPropertyDescriptor(Element.prototype, "scrollTop");
/** What each scroller reports, and every value written to it. */
const reported = new WeakMap<Element, number>();
const writes = vi.fn<(value: number) => void>();

beforeEach(() => {
  writes.mockClear();
  // jsdom has no scrolling: a scroller reports what the test says and records what is written to it.
  Object.defineProperty(Element.prototype, "scrollTop", {
    configurable: true,
    get(this: Element) {
      return reported.get(this) ?? 0;
    },
    set(this: Element, value: number) {
      writes(value);
      reported.set(this, value);
    },
  });
});

afterEach(async () => {
  if (original) Object.defineProperty(Element.prototype, "scrollTop", original);
  if (root) await act(async () => root?.unmount());
  container?.remove();
  root = null;
  container = null;
});

async function mount(hidden: boolean) {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  const register = vi.fn();
  const draw = async (isHidden: boolean) => {
    await act(async () => {
      root?.render(<ScreenFrame id="archive" hidden={isHidden} register={register}>x</ScreenFrame>);
    });
  };
  await draw(hidden);
  const section = container.querySelector<HTMLElement>(".screen") as HTMLElement;
  return { draw, section, register };
}

describe("ScreenFrame", () => {
  it("does not write a scroll position when it mounts", async () => {
    await mount(false);
    expect(writes).not.toHaveBeenCalled();
  });

  it("does not write one when a screen that never scrolled comes back", async () => {
    const { draw, section } = await mount(false);
    await draw(true);
    await draw(false);
    expect(section.style.display).toBe("");
    expect(writes).not.toHaveBeenCalled();
  });

  it("puts a scroll position back once when the screen returns", async () => {
    const { draw, section } = await mount(false);
    reported.set(section, 120);
    await act(async () => {
      section.dispatchEvent(new Event("scroll"));
    });
    await draw(true);
    // A hidden scroller has no layout and reads 0.
    reported.set(section, 0);
    await draw(false);
    expect(writes).toHaveBeenCalledTimes(1);
    expect(writes).toHaveBeenCalledWith(120);
  });

  it("keeps the same register callback across redraws", async () => {
    const { draw, register } = await mount(false);
    await draw(true);
    await draw(false);
    // Bound once on mount; a callback that changed every render would be called off and on again.
    expect(register.mock.calls).toEqual([[expect.any(HTMLElement)]]);
  });
});
