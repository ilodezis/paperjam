import { afterEach, describe, expect, it, vi } from "vitest";

import * as theme from "./theme";

afterEach(() => {
  vi.unstubAllGlobals();
  window.localStorage.clear();
  document.documentElement.removeAttribute("data-theme");
});

describe("theme", () => {
  it("resolves the stored preference and system appearance", async () => {
    expect(theme.resolveTheme).toBeTypeOf("function");
    const matchMedia = vi.fn().mockReturnValue({ matches: true });
    vi.stubGlobal("matchMedia", matchMedia);

    expect(theme.resolveTheme("dark", matchMedia)).toBe("dark");
    expect(theme.resolveTheme("light", matchMedia)).toBe("light");
    expect(theme.resolveTheme("system", matchMedia)).toBe("dark");
    expect(theme.resolveTheme("system", vi.fn().mockReturnValue({ matches: false }))).toBe("light");
  });

  it("defaults invalid stored values to system", () => {
    expect(theme.readThemePreference()).toBe("system");
    window.localStorage.setItem("paperjam:theme", "unexpected");
    expect(theme.readThemePreference()).toBe("system");
    window.localStorage.setItem("paperjam:theme", "dark");
    expect(theme.readThemePreference()).toBe("dark");
  });

  it("persists and applies a manual preference", () => {
    vi.stubGlobal("matchMedia", vi.fn().mockReturnValue({ matches: false }));
    theme.applyThemePreference("dark");
    expect(window.localStorage.getItem("paperjam:theme")).toBe("dark");
    expect(document.documentElement.dataset.theme).toBe("dark");
  });
});
