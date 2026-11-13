import { describe, expect, it } from "vitest";

import { fieldFromInput, hasPageImages, toggleId } from "./document";

describe("page images", () => {
  const view = (hasPages: boolean) => ({ file: { has_pages: hasPages } }) as Parameters<typeof hasPageImages>[0];

  it("has page images when the server says the document has pages", () => {
    expect(hasPageImages(view(true))).toBe(true);
  });

  it("has none for a text-only or never-rendered document", () => {
    expect(hasPageImages(view(false))).toBe(false);
  });
});

describe("document field editing", () => {
  it("builds typed values without losing numeric precision", () => {
    expect(fieldFromInput("identifier", " P00012345 ")).toEqual({ kind: "identifier", value: "P00012345" });
    expect(fieldFromInput("number", "12345678901234567890")).toEqual({ kind: "number", value: "12345678901234567890" });
    expect(fieldFromInput("money", "1250.50", " eur ")).toEqual({ kind: "money", value: { amount: "1250.50", currency: "EUR" } });
  });

  it("splits lists and clears empty values", () => {
    expect(fieldFromInput("list", "one, two, ,three")).toEqual({ kind: "list", value: ["one", "two", "three"] });
    expect(fieldFromInput("text", "  ")).toBeNull();
    expect(fieldFromInput("money", "", "EUR")).toBeNull();
  });

  it("replaces people and tags by id without mutating the previous list", () => {
    const original = ["person-a"];
    expect(toggleId(original, "person-b", true)).toEqual(["person-a", "person-b"]);
    expect(toggleId(original, "person-a", false)).toEqual([]);
    expect(original).toEqual(["person-a"]);
  });
});
