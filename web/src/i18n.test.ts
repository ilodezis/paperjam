import { describe, expect, it } from "vitest";

import { createI18n, pickLanguage } from "./i18n";
import en from "./locales/en.json";
import ru from "./locales/ru.json";

function keys(value: unknown, prefix = ""): string[] {
  if (!value || typeof value !== "object") return [prefix];
  return Object.entries(value as Record<string, unknown>).flatMap(([key, child]) =>
    keys(child, prefix ? `${prefix}.${key}` : key),
  );
}

describe("plurals", () => {
  it("plural_ru_uses_few_and_many", () => {
    const t = createI18n("ru").t;
    expect(t("search.results", { count: 1 })).toBe("1 документ");
    expect(t("search.results", { count: 3 })).toBe("3 документа");
    expect(t("search.results", { count: 5 })).toBe("5 документов");
    expect(t("search.results", { count: 21 })).toBe("21 документ");
    expect(t("search.results", { count: 11 })).toBe("11 документов");
  });

  it("english uses one and other", () => {
    const t = createI18n("en").t;
    expect(t("search.results", { count: 1 })).toBe("1 document");
    expect(t("search.results", { count: 2 })).toBe("2 documents");
  });
});

describe("pickLanguage", () => {
  it("takes the first supported language", () => {
    expect(pickLanguage(["de-DE", "ru-RU", "en"])).toBe("ru");
    expect(pickLanguage(["fr"])).toBe("en");
    expect(pickLanguage([])).toBe("en");
  });
});

describe("locale catalogs", () => {
  it("keeps English and Russian keys in sync while allowing language-specific plural forms", () => {
    const canonical = (catalog: unknown) => [...new Set(keys(catalog).map((key) =>
      key.replace(/_(?:one|few|many|other)$/, ""),
    ))].sort();
    expect(canonical(ru)).toEqual(canonical(en));
  });

  it("provides every Russian plural form", () => {
    const russian = keys(ru);
    for (const key of keys(en).filter((key) => key.endsWith("_other"))) {
      const base = key.slice(0, -"_other".length);
      for (const form of ["one", "few", "many", "other"]) {
        expect(russian).toContain(`${base}_${form}`);
      }
    }
  });
});
