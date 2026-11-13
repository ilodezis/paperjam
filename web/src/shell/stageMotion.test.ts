import { describe, expect, it, vi } from "vitest";

import { stageMountedEntries } from "./Stage";
import { fade, PARALLAX, SHADE, sheetCloses, slide, swipeCommits, swipeFrame, whenPresent } from "./stageMotion";

describe("whenPresent", () => {
  it("returns at once when the element is already there", async () => {
    await expect(whenPresent(() => "here")).resolves.toBe("here");
  });

  it("waits for an element that shows up a moment later", async () => {
    vi.useFakeTimers();
    let calls = 0;
    const found = whenPresent(() => (++calls > 3 ? "late" : null));
    await vi.advanceTimersByTimeAsync(100);
    await expect(found).resolves.toBe("late");
    vi.useRealTimers();
  });

  it("gives up after the timeout so a navigation never hangs on it", async () => {
    vi.useFakeTimers();
    const found = whenPresent(() => null, 100, 16);
    await vi.advanceTimersByTimeAsync(200);
    await expect(found).resolves.toBeNull();
    vi.useRealTimers();
  });
});

describe("edge swipe", () => {
  it("commits past 35% of the width, however slowly", () => {
    expect(swipeCommits(0.36, 0)).toBe(true);
    expect(swipeCommits(0.34, 0.1)).toBe(false);
  });

  it("commits on a fast flick once it has moved a little", () => {
    expect(swipeCommits(0.1, 0.6)).toBe(true);
    expect(swipeCommits(0.05, 0.9)).toBe(false);
    expect(swipeCommits(0.2, 0.4)).toBe(false);
  });

  it("moves the top screen with the finger and the one below by the parallax", () => {
    expect(swipeFrame(0, 400)).toEqual({ top: 0, under: -PARALLAX * 400, shade: SHADE });
    expect(swipeFrame(1, 400)).toEqual({ top: 400, under: -0, shade: 0 });
    const half = swipeFrame(0.5, 400);
    expect(half.top).toBe(200);
    expect(half.under).toBeCloseTo(-56, 5);
    expect(half.shade).toBeCloseTo(0.16, 5);
  });
});

describe("sheet drag", () => {
  it("closes past 28% of its height or faster than 0.6 px/ms", () => {
    expect(sheetCloses(300, 1000, 0)).toBe(true);
    expect(sheetCloses(270, 1000, 0.1)).toBe(false);
    expect(sheetCloses(20, 1000, 0.7)).toBe(true);
  });
});

describe("stage motions and DOM order", () => {
  function makeMockElement(): HTMLElement {
    return {
      hidden: false,
      style: {} as Record<string, string>,
      animate: vi.fn(() => ({ finished: Promise.resolve(), cancel: vi.fn() })),
      getAnimations: vi.fn(() => []),
    } as unknown as HTMLElement;
  }

  it("hides node a after fade() so it cannot blink over b", async () => {
    const a = makeMockElement();
    const b = makeMockElement();
    b.hidden = true;
    b.style.display = "none";

    await fade(a, b);

    expect(a.hidden).toBe(true);
    expect(a.style.display).toBe("none");
    expect(b.hidden).toBe(false);
    expect(b.style.display).toBe("");
  });

  it("hides the departing node after a slide, push and pop", async () => {
    const a = makeMockElement();
    const b = makeMockElement();
    const shade = makeMockElement();

    await slide(a, b, shade, 400, 1);
    expect(a.hidden).toBe(true);
    expect(a.style.display).toBe("none");
    expect(b.hidden).toBe(false);
    expect(b.style.display).toBe("");

    a.hidden = false;
    a.style.display = "";
    await slide(a, b, shade, 400, -1);
    expect(a.hidden).toBe(true);
    expect(a.style.display).toBe("none");
  });

  it("a push animates the new screen in over 540ms and the one below by the parallax", async () => {
    const a = makeMockElement();
    const b = makeMockElement();
    const shade = makeMockElement();
    await slide(a, b, shade, 400, 1);
    const [keyframes, options] = (b.animate as ReturnType<typeof vi.fn>).mock.calls[0] as [Keyframe[], KeyframeAnimationOptions];
    expect(keyframes).toEqual([{ transform: "translateX(400px)" }, { transform: "translateX(0px)" }]);
    expect(options.duration).toBe(540);
    expect(options.easing).toBe("cubic-bezier(.32,.72,0,1)");
    const [underKeyframes] = (a.animate as ReturnType<typeof vi.fn>).mock.calls[0] as [Keyframe[]];
    expect(underKeyframes[1]).toEqual({ transform: `translateX(${-PARALLAX * 400}px)` });
  });

  it("orders leaving entries before logical entries in DOM tree order", () => {
    const logical = [{ id: "document" as const, key: 2, back: "archive" as const, path: "/documents/a", search: "" }];
    const leaving = [{ id: "archive" as const, key: 1, back: null, path: "/", search: "" }];

    const mounted = stageMountedEntries(logical, leaving);
    expect(mounted.map((e) => e.id)).toEqual(["archive", "document"]);
  });
});
