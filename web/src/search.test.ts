import { describe, expect, it } from "vitest";

import * as search from "./search";

describe("search query helpers", () => {
  it("adds and removes exact structured filters", () => {
    expect(search.toggleQueryFilter).toBeTypeOf("function");
    expect(search.toggleQueryFilter("contract", "type:contract")).toBe("contract type:contract");
    expect(search.toggleQueryFilter("passport type:passport", "type:passport")).toBe("passport");
    expect(search.toggleQueryFilter("type:passport person:testov", "type:passport")).toBe("person:testov");
  });

  it("quotes values with whitespace without corrupting existing queries", () => {
    expect(search.queryFilter("person", 'Alex "Example"')).toBe('person:"Alex \\"Example\\""');
    expect(search.toggleQueryFilter("", "person", 'Alex "Example"')).toBe('person:"Alex \\"Example\\""');
  });

  it("preserves free text and OR expressions when adding a filter", () => {
    expect(search.toggleQueryFilter("rent OR lease", "tag:warranty")).toBe("rent OR lease tag:warranty");
  });
});

describe("filter tokens", () => {
  it("tells filters from words", () => {
    expect(search.hasFilterTokens("passport mom")).toBe(false);
    expect(search.hasFilterTokens("")).toBe(false);
    expect(search.hasFilterTokens("passport type:passport")).toBe(true);
    expect(search.hasFilterTokens("due:<2027-06")).toBe(true);
    expect(search.hasFilterTokens("has:expiring")).toBe(true);
    expect(search.hasFilterTokens("-has:tags")).toBe(true);
    expect(search.hasFilterTokens('person:"Alex Example"')).toBe(true);
    expect(search.hasFilterTokens("date:>=2024 rent")).toBe(true);
    expect(search.hasFilterTokens("field.expiry_date<2037")).toBe(true);
    // A colon in plain words, or a filter-looking text inside quotes, is no filter.
    expect(search.hasFilterTokens("note: call back")).toBe(false);
    expect(search.hasFilterTokens('"type:passport"')).toBe(false);
  });
});

describe("safe snippets", () => {
  it("turns only mark tags into highlighted segments", () => {
    expect(search.parseSnippet('A <mark>passport</mark> &lt;copy&gt;')).toEqual([
      { text: "A ", mark: false },
      { text: "passport", mark: true },
      { text: " <copy>", mark: false },
    ]);
  });

  it("does not interpret arbitrary HTML", () => {
    expect(search.parseSnippet('<img src=x onerror=alert(1)> <mark>safe</mark>')).toEqual([
      { text: "<img src=x onerror=alert(1)> ", mark: false },
      { text: "safe", mark: true },
    ]);
  });
});

describe("filter tokens", () => {
  it("matches a whole token, not a piece of another word", () => {
    expect(search.hasQueryToken("rent type:passport", "type:passport")).toBe(true);
    expect(search.hasQueryToken("type:passport_copy", "type:passport")).toBe(false);
    expect(search.hasQueryToken("", "has:attention")).toBe(false);
  });
});

describe("calm snippets", () => {
  const text = (parts: search.SnippetPart[]) => parts.map((part) => part.text).join("");

  it("lowercases a snippet captured in capitals and keeps the highlight", () => {
    const calm = search.calmSnippet(search.parseSnippet("THE AGREEMENT <mark>BEGINS</mark> ON 15 SEPTEMBER"), "en");
    expect(text(calm)).toBe("The agreement begins on 15 september");
    expect(calm.filter((part) => part.mark).map((part) => part.text)).toEqual(["begins"]);
  });

  it("leaves ordinary mixed-case text alone", () => {
    const original = search.parseSnippet("Two-year warranty on the <mark>Receipt</mark> from ACME");
    expect(search.calmSnippet(original, "en")).toEqual(original);
  });

  it("collapses OCR filler and line breaks", () => {
    expect(text(search.calmSnippet([{ text: "P<UTOERIKSSON<<<<<<ANNA\nMARIA_____ok", mark: false }], "en"))).toBe("P<utoeriksson anna maria ok");
  });
});
