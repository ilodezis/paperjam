import { describe, expect, it } from "vitest";

import en from "../locales/en.json";
import ru from "../locales/ru.json";

import { BAR_TABS, destOf, documentIdOf, documentPath, screenForPath, tabOf } from "./nav";

describe("addresses", () => {
  it("maps each address to its screen", () => {
    expect(screenForPath("/")).toBe("archive");
    expect(screenForPath("/upload")).toBe("upload");
    expect(screenForPath("/taxonomy/")).toBe("taxonomy");
    expect(screenForPath("/documents/abc")).toBe("document");
    expect(screenForPath("/documents")).toBeNull();
    expect(screenForPath("/nope")).toBeNull();
  });

  it("round-trips a document id through its address", () => {
    const id = "doc 1/ü";
    expect(documentIdOf(documentPath(id))).toBe(id);
    expect(documentIdOf("/documents/a/b")).toBeNull();
  });

  it("builds a stack destination from a location", () => {
    expect(destOf("/documents/x/", "?page=2")).toEqual({ id: "document", path: "/documents/x", search: "?page=2" });
    expect(destOf("/nope", "")).toBeNull();
  });
});

describe("bottom bar", () => {
  it("has four tabs and no trash: the trash lives under Settings on a phone", () => {
    expect(BAR_TABS.map((tab) => tab.screen)).toEqual(["archive", "upload", "taxonomy", "settings"]);
  });

  it("labels every tab with one short word, so none is cut off on a 402 px phone", () => {
    for (const catalog of [en, ru]) {
      for (const tab of BAR_TABS) {
        const label = tab.barKey.split(".").reduce<unknown>((node, key) => (node as Record<string, unknown>)[key], catalog) as string;
        expect(label, tab.barKey).not.toMatch(/\s/);
        expect(label.length, label).toBeLessThanOrEqual(12);
      }
    }
  });

  it("keeps a tab lit for the screens below it", () => {
    expect(tabOf("document")).toBe("archive");
    expect(tabOf("trash")).toBe("settings");
    expect(tabOf("upload")).toBe("upload");
  });

  it("lights no tab on an address that is no screen", () => {
    expect(tabOf(null)).toBeNull();
  });
});
