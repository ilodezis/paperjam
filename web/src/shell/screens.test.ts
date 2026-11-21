import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { isScreenMounted, openScreen, preloadScreen, setMountedScreens } from "./screens";

vi.mock("../pages/ArchivePage", () => ({ ArchivePage: () => null }));
vi.mock("../pages/DocumentPage", () => ({ DocumentPage: () => null }));
vi.mock("../pages/UploadPage", () => ({ UploadPage: () => null }));
vi.mock("../pages/TaxonomyPage", () => ({ TaxonomyPage: () => null }));
vi.mock("../pages/TrashPage", () => ({ TrashPage: () => null }));
vi.mock("../pages/SettingsPage", () => ({ SettingsPage: () => null }));

const never = () => new Promise<never>(() => undefined);

beforeAll(async () => {
  // The code of a screen is in before the timers are faked.
  await preloadScreen("archive");
});

afterEach(() => {
  vi.useRealTimers();
  setMountedScreens([]);
});

describe("openScreen", () => {
  it("with waitMs 0 opens on the code alone, even when a read never settles", async () => {
    vi.useFakeTimers();
    let opened = false;
    void openScreen("archive", [never()], { waitMs: 0 }).then(() => {
      opened = true;
    });
    // Only microtasks run: no timer has advanced.
    await vi.advanceTimersByTimeAsync(0);
    expect(opened).toBe(true);
  });

  it("with waitMs 0 leaves a failing read to the screen", async () => {
    const unhandled = vi.fn();
    process.on("unhandledRejection", unhandled);
    await openScreen("archive", [Promise.reject(new Error("offline"))], { waitMs: 0 });
    await new Promise((resolve) => setTimeout(resolve, 10));
    process.off("unhandledRejection", unhandled);
    expect(unhandled).not.toHaveBeenCalled();
  });

  it("by default waits for a slow read, but no longer than 250 ms", async () => {
    vi.useFakeTimers();
    let opened = false;
    void openScreen("archive", [never()]).then(() => {
      opened = true;
    });
    await vi.advanceTimersByTimeAsync(249);
    expect(opened).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    expect(opened).toBe(true);
  });

  it("opens as soon as the reads are in", async () => {
    vi.useFakeTimers();
    let opened = false;
    void openScreen("archive", [Promise.resolve(1)]).then(() => {
      opened = true;
    });
    await vi.advanceTimersByTimeAsync(0);
    expect(opened).toBe(true);
  });

  it("takes its own waitMs", async () => {
    vi.useFakeTimers();
    let opened = false;
    void openScreen("archive", [never()], { waitMs: 600 }).then(() => {
      opened = true;
    });
    await vi.advanceTimersByTimeAsync(599);
    expect(opened).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    expect(opened).toBe(true);
  });
});

describe("mounted screens", () => {
  it("reflects what the stage published", () => {
    expect(isScreenMounted("archive")).toBe(false);
    setMountedScreens(["archive", "document"]);
    expect(isScreenMounted("archive")).toBe(true);
    expect(isScreenMounted("document")).toBe(true);
    expect(isScreenMounted("settings")).toBe(false);
    setMountedScreens(new Set(["settings"] as const));
    expect(isScreenMounted("archive")).toBe(false);
    expect(isScreenMounted("settings")).toBe(true);
  });
});
