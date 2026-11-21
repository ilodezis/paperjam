import { afterEach, describe, expect, it, vi } from "vitest";

import { dueState, EXPIRING_DAYS, formatFileSize, formatIsoDate } from "./format";
import en from "./locales/en.json";
import ru from "./locales/ru.json";

describe("formatIsoDate", () => {
  it("formats an ISO date for Russian and English", () => {
    expect(formatIsoDate("2031-07-11", "ru")).toBe("11 июл. 2031 г.");
    expect(formatIsoDate("2031-07-11", "en")).toBe("Jul 11, 2031");
  });

  it("does not shift the day, including on the first and last day of a month", () => {
    expect(formatIsoDate("2031-07-01", "en")).toBe("Jul 1, 2031");
    expect(formatIsoDate("2031-01-31", "en")).toBe("Jan 31, 2031");
  });

  it("returns invalid or non-ISO strings unchanged", () => {
    expect(formatIsoDate("2031-02-30", "en")).toBe("2031-02-30");
    expect(formatIsoDate("11.07.2031", "ru")).toBe("11.07.2031");
    expect(formatIsoDate("not a date", "en")).toBe("not a date");
  });
});

const ATTENTION_FLAGS = [
  "missing_fields",
  "mrz_mismatch",
  "checksum_failed",
  "ocr_llm_disagree",
  "illegible",
  "llm_failed",
  "llm_budget",
  "processing_failed",
] as const;

describe("attention flag translations", () => {
  it.each([
    ["en", en],
    ["ru", ru],
  ])("has a non-empty text for every flag in %s", (_language, catalog) => {
    const flags = (catalog as { document: { attentionFlags: Record<string, string> } }).document.attentionFlags;
    for (const flag of ATTENTION_FLAGS) {
      expect(typeof flags[flag], flag).toBe("string");
      expect(flags[flag].trim().length, flag).toBeGreaterThan(0);
    }
  });
});

describe("formatFileSize", () => {
  it("uses kilobytes below a megabyte and megabytes above, always with one decimal", () => {
    expect(formatFileSize(340 * 1024, "ru")).toBe("340,0\u00a0КБ");
    expect(formatFileSize(179_609, "en")).toBe("175.4\u00a0KB");
    expect(formatFileSize(1_258_291, "ru")).toBe("1,2\u00a0МБ");
    expect(formatFileSize(1_258_291, "en")).toBe("1.2\u00a0MB");
  });
});

describe("formatter caching", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("builds one date formatter per language and reuses it", () => {
    const spy = vi.spyOn(Intl, "DateTimeFormat");
    const outputs = [1, 2, 3].map(() => formatIsoDate("2031-07-11", "de"));
    expect(new Set(outputs).size).toBe(1);
    expect(spy).toHaveBeenCalledTimes(1);
    formatIsoDate("2031-07-11", "fr");
    expect(spy).toHaveBeenCalledTimes(2);
  });

  it("builds one size formatter per language and reuses it", () => {
    const spy = vi.spyOn(Intl, "NumberFormat");
    const outputs = [1, 2, 3].map(() => formatFileSize(1_258_291, "de"));
    expect(new Set(outputs).size).toBe(1);
    expect(spy).toHaveBeenCalledTimes(1);
    formatFileSize(1_258_291, "fr");
    expect(spy).toHaveBeenCalledTimes(2);
  });
});

describe("dueState", () => {
  const today = new Date(Date.UTC(2026, 10, 7, 23, 30));

  it("counts whole days from today, a UTC day like the server's", () => {
    expect(dueState("2026-11-07", today)).toEqual({ state: "soon", days: 0 });
    expect(dueState("2026-12-07", today)).toEqual({ state: "soon", days: 30 });
  });

  it("is overdue once the day has passed", () => {
    expect(dueState("2026-11-06", today)).toEqual({ state: "overdue", days: -1 });
  });

  it("is soon up to the server's horizon and later after it", () => {
    const edge = new Date(Date.UTC(2026, 10, 7 + EXPIRING_DAYS)).toISOString().slice(0, 10);
    const after = new Date(Date.UTC(2026, 10, 8 + EXPIRING_DAYS)).toISOString().slice(0, 10);
    expect(dueState(edge, today)?.state).toBe("soon");
    expect(dueState(after, today)?.state).toBe("later");
  });

  it("is null for what is not an ISO date", () => {
    expect(dueState("soon", today)).toBeNull();
  });
});
