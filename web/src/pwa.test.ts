import { afterEach, describe, expect, it, vi } from "vitest";

import manifest from "../public/manifest.webmanifest?raw";
import swScript from "../public/sw.js?raw";
import { monitorOnlineStatus, registerServiceWorker } from "./pwa";

describe("PWA registration", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("skips registration in development mode to avoid intercepting mock API worker", async () => {
    const registerMock = vi.fn();
    const navMock = {
      serviceWorker: { register: registerMock },
    } as unknown as Navigator;

    const result = await registerServiceWorker(navMock, true);
    expect(result).toBeUndefined();
    expect(registerMock).not.toHaveBeenCalled();
  });

  it("registers sw.js with root scope when running in production", async () => {
    const fakeRegistration = {} as ServiceWorkerRegistration;
    const registerMock = vi.fn().mockResolvedValue(fakeRegistration);
    const navMock = {
      serviceWorker: { register: registerMock },
    } as unknown as Navigator;

    const result = await registerServiceWorker(navMock, false);
    expect(result).toBe(fakeRegistration);
    expect(registerMock).toHaveBeenCalledWith("/sw.js", { scope: "/" });
  });

  it("handles missing serviceWorker support gracefully", async () => {
    const navMock = {} as Navigator;
    const result = await registerServiceWorker(navMock, false);
    expect(result).toBeUndefined();
  });

  it("swallows registration rejections without unhandled exceptions", async () => {
    const registerMock = vi.fn().mockRejectedValue(new Error("SecurityError"));
    const navMock = {
      serviceWorker: { register: registerMock },
    } as unknown as Navigator;

    const result = await registerServiceWorker(navMock, false);
    expect(result).toBeUndefined();
  });
});

describe("connectivity monitoring", () => {
  it("notifies listeners on online and offline events and cleans up listeners", () => {
    const listeners: Record<string, EventListener> = {};
    const winMock = {
      addEventListener: vi.fn((event: string, handler: EventListener) => {
        listeners[event] = handler;
      }),
      removeEventListener: vi.fn((event: string, handler: EventListener) => {
        if (listeners[event] === handler) delete listeners[event];
      }),
    } as unknown as Window;

    const onStatusChange = vi.fn();
    const cleanup = monitorOnlineStatus(onStatusChange, winMock);

    expect(winMock.addEventListener).toHaveBeenCalledWith("online", expect.any(Function));
    expect(winMock.addEventListener).toHaveBeenCalledWith("offline", expect.any(Function));

    listeners.offline(new Event("offline"));
    expect(onStatusChange).toHaveBeenLastCalledWith(false);

    listeners.online(new Event("online"));
    expect(onStatusChange).toHaveBeenLastCalledWith(true);

    cleanup();
    expect(winMock.removeEventListener).toHaveBeenCalledWith("online", expect.any(Function));
    expect(winMock.removeEventListener).toHaveBeenCalledWith("offline", expect.any(Function));
  });
});

describe("manifest and service worker contracts", () => {
  it("provides a valid, installable Web App Manifest matching design system colors", () => {
    const parsed = JSON.parse(manifest);
    expect(parsed.name).toBe("paperjam");
    expect(parsed.short_name).toBe("paperjam");
    expect(parsed.start_url).toBe("/");
    expect(parsed.display).toBe("standalone");
    expect(parsed.theme_color).toBe("#315e4c");
    expect(parsed.background_color).toBe("#f6f5f1");
    expect(Array.isArray(parsed.icons)).toBe(true);
    expect(parsed.icons.length).toBeGreaterThanOrEqual(2);

    const standardIcon = parsed.icons.find((i: { purpose?: string }) => i.purpose === "any");
    const maskableIcon = parsed.icons.find((i: { purpose?: string }) => i.purpose === "maskable");
    expect(standardIcon).toBeDefined();
    expect(maskableIcon).toBeDefined();
  });

  it("enforces strict no-API-caching in the service worker", () => {
    // The service worker MUST explicitly bypass /api/ and non-GET requests
    expect(swScript).toContain('url.pathname.startsWith("/api/")');
    expect(swScript).toContain('request.method !== "GET"');
    expect(swScript).toContain('request.mode === "navigate"');
    expect(swScript).toContain("/manifest.webmanifest");
    expect(swScript).toContain("/index.html");
  });
});
