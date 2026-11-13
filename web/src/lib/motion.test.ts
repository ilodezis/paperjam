import { afterEach, describe, expect, it, vi } from "vitest";

import { animate, EASE_OUT, EASE_SHEET, prefersReducedMotion } from "./motion";

function node() {
  return { animate: vi.fn(() => ({ finished: Promise.resolve() })) } as unknown as Element & { animate: ReturnType<typeof vi.fn> };
}

function reduced(on: boolean) {
  vi.stubGlobal("matchMedia", (query: string) => ({ matches: on && query.includes("reduce"), addEventListener() {}, removeEventListener() {} }));
}

afterEach(() => vi.unstubAllGlobals());

describe("motion", () => {
  it("names the two easings the CSS tokens name", () => {
    expect(EASE_OUT).toBe("cubic-bezier(.16,1,.3,1)");
    expect(EASE_SHEET).toBe("cubic-bezier(.32,.72,0,1)");
  });

  it("runs an animation as asked, holding its end state, with the sheet easing by default", () => {
    reduced(false);
    const el = node();
    animate(el, [{ opacity: 0 }, { opacity: 1 }], { duration: 320, delay: 40 });
    expect(el.animate).toHaveBeenCalledWith(expect.any(Array), { fill: "both", easing: EASE_SHEET, duration: 320, delay: 40 });
  });

  it("is a cut under reduced motion: one millisecond, no delay", () => {
    reduced(true);
    expect(prefersReducedMotion()).toBe(true);
    const el = node();
    animate(el, [{ opacity: 0 }, { opacity: 1 }], { duration: 540, delay: 70 });
    expect(el.animate).toHaveBeenCalledWith(expect.any(Array), expect.objectContaining({ duration: 1, delay: 0 }));
  });
});
