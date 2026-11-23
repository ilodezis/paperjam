import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { stageMountedEntries } from "./Stage";
import { fade, PARALLAX, SHADE, SLIDE_MS, sharedMorph, sheetCloses, slide, swipeCommits, swipeFrame, whenPresent } from "./stageMotion";

describe("whenPresent", () => {
  it("returns at once when the element is already there", async () => {
    await expect(whenPresent(() => "here")).resolves.toBe("here");
  });

  it("waits for an element that shows up a frame or two later", async () => {
    vi.useFakeTimers();
    let calls = 0;
    const found = whenPresent(() => (++calls > 2 ? "late" : null));
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

describe("whenPresent default budget", () => {
  it("gives up after about two frames (32 ms), not half a second", async () => {
    vi.useFakeTimers();
    let calls = 0;
    let done = false;
    const found = whenPresent(() => {
      calls++;
      return null;
    }).then((v) => {
      done = true;
      return v;
    });
    await vi.advanceTimersByTimeAsync(31);
    expect(done).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    await expect(found).resolves.toBeNull();
    expect(calls).toBe(3);
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

  it("a push animates the new screen in over SLIDE_MS and the one below by the parallax", async () => {
    const a = makeMockElement();
    const b = makeMockElement();
    const shade = makeMockElement();
    await slide(a, b, shade, 400, 1);
    const [keyframes, options] = (b.animate as ReturnType<typeof vi.fn>).mock.calls[0] as [Keyframe[], KeyframeAnimationOptions];
    expect(keyframes).toEqual([{ transform: "translateX(400px)" }, { transform: "translateX(0px)" }]);
    expect(options.duration).toBe(SLIDE_MS);
    expect(options.delay ?? 0).toBe(0);
    expect(SLIDE_MS).toBeLessThanOrEqual(280);
    expect(options.easing).toBe("cubic-bezier(.32,.72,0,1)");
    const [underKeyframes] = (a.animate as ReturnType<typeof vi.fn>).mock.calls[0] as [Keyframe[]];
    expect(underKeyframes[1]).toEqual({ transform: `translateX(${-PARALLAX * 400}px)` });
  });

  it("fade runs crisply with no startup delay", async () => {
    const a = makeMockElement();
    const b = makeMockElement();
    await fade(a, b);
    for (const node of [a, b]) {
      const [, options] = (node.animate as ReturnType<typeof vi.fn>).mock.calls[0] as [Keyframe[], KeyframeAnimationOptions];
      expect(options.delay ?? 0).toBe(0);
      expect(Number(options.duration)).toBeLessThanOrEqual(150);
    }
  });

  it("cancels animations and leaves departing node visible when aborted", async () => {
    const cancel = vi.fn();
    const pending = new Promise<void>(() => undefined);
    const a = makeMockElement();
    const b = makeMockElement();
    a.animate = vi.fn(() => ({ finished: pending, cancel })) as unknown as Element["animate"];
    b.animate = vi.fn(() => ({ finished: pending, cancel })) as unknown as Element["animate"];
    const controller = new AbortController();
    const done = fade(a, b, controller.signal);
    controller.abort();
    await done;
    expect(cancel).toHaveBeenCalledTimes(2);
    expect(a.hidden).toBe(false);
    expect(a.style.display).not.toBe("none");
  });

  describe("sharedMorph", () => {
    const rect = (left: number, top: number, width: number, height: number) =>
      ({ left, top, width, height, right: left + width, bottom: top + height, x: left, y: top, toJSON: () => ({}) }) as DOMRect;
    const flyerAnimate = vi.fn(() => ({ finished: Promise.resolve(), cancel: vi.fn() }));
    const proto = Element.prototype as unknown as { animate?: unknown };
    const hadAnimate = "animate" in proto;
    const prevAnimate = proto.animate;

    beforeEach(() => {
      flyerAnimate.mockClear();
      proto.animate = flyerAnimate;
    });
    afterEach(() => {
      if (hadAnimate) proto.animate = prevAnimate;
      else delete proto.animate;
    });

    async function morph() {
      const host = document.createElement("div");
      const source = document.createElement("div");
      const target = document.createElement("div");
      host.append(source);
      document.body.append(host);
      host.getBoundingClientRect = () => rect(0, 0, 800, 900);
      // A 100x140 thumbnail at (20, 50); the canvas is 400 wide, so the page lands 4x as large.
      source.getBoundingClientRect = () => rect(20, 50, 100, 140);
      target.getBoundingClientRect = () => rect(200, 40, 400, 700);
      const a = makeMockElement();
      const b = makeMockElement();
      b.hidden = true;
      await sharedMorph(a, b, source, target, host);
      host.remove();
      return { a, b };
    }

    it("flies the clone by transform alone: no width or height in its keyframes", async () => {
      await morph();
      expect(flyerAnimate).toHaveBeenCalledTimes(1);
      const [keyframes] = flyerAnimate.mock.calls[0] as unknown as [Keyframe[]];
      expect(keyframes).toHaveLength(2);
      for (const frame of keyframes) expect(Object.keys(frame)).toEqual(["transform"]);
      expect(keyframes[0]?.transform).toBe("none");
      // Landing rect: 400 x 560 at (200, 110) -> scale 4, translate (180, 60) from the source box.
      expect(keyframes[1]?.transform).toBe("translate(180px, 60px) scale(4)");
    });

    it("keeps the clone at the source box, scaling from its top-left corner", async () => {
      const host = document.createElement("div");
      const source = document.createElement("div");
      host.append(source);
      document.body.append(host);
      host.getBoundingClientRect = () => rect(10, 10, 800, 900);
      source.getBoundingClientRect = () => rect(20, 50, 100, 140);
      const target = document.createElement("div");
      target.getBoundingClientRect = () => rect(200, 40, 400, 700);
      let flyer: HTMLElement | null = null;
      flyerAnimate.mockImplementationOnce(() => {
        flyer = host.querySelector<HTMLElement>(".flyer");
        return { finished: Promise.resolve(), cancel: vi.fn() };
      });
      await sharedMorph(makeMockElement(), makeMockElement(), source, target, host);
      host.remove();
      const style = (flyer as HTMLElement | null)?.style;
      expect(style?.width).toBe("100px");
      expect(style?.height).toBe("140px");
      expect(style?.left).toBe("10px");
      expect(style?.top).toBe("40px");
      expect(style?.transformOrigin).toBe("0 0");
    });

    it("stays crisp with no startup delay and every animation within 280 ms", async () => {
      const { a, b } = await morph();
      const calls = [
        ...flyerAnimate.mock.calls,
        ...(a.animate as ReturnType<typeof vi.fn>).mock.calls,
        ...(b.animate as ReturnType<typeof vi.fn>).mock.calls,
      ] as unknown as Array<[Keyframe[], KeyframeAnimationOptions]>;
      expect(calls).toHaveLength(3);
      for (const [, options] of calls) {
        expect(options.delay ?? 0).toBe(0);
        expect(Number(options.duration)).toBeLessThanOrEqual(280);
      }
    });
  });

  it("orders leaving entries before logical entries in DOM tree order", () => {
    const logical = [{ id: "document" as const, key: 2, back: "archive" as const, path: "/documents/a", search: "" }];
    const leaving = [{ id: "archive" as const, key: 1, back: null, path: "/", search: "" }];

    const mounted = stageMountedEntries(logical, leaving);
    expect(mounted.map((e) => e.id)).toEqual(["archive", "document"]);
  });
});
