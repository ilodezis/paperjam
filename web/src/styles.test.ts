import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

// Vitest stubs CSS imports, so read the stylesheet itself (tests run from web/).
const css = readFileSync(resolve(process.cwd(), "src/styles.css"), "utf8");

function block(selector: string): Record<string, string> {
  const start = css.indexOf(`${selector} {`);
  if (start < 0) throw new Error(`missing ${selector}`);
  const body = css.slice(start, css.indexOf("}", start));
  return Object.fromEntries([...body.matchAll(/--([a-z-]+):\s*(#[0-9a-f]{6})/gi)].map((m) => [m[1], m[2]]));
}

function luminance(hex: string): number {
  const [r, g, b] = [1, 3, 5].map((i) => {
    const v = parseInt(hex.slice(i, i + 2), 16) / 255;
    return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

const light = block(":root");
const dark = { ...light, ...block(':root[data-theme="dark"]') };
const surfaces = ["paper", "paper-raised", "sidebar", "accent-soft"];

// Every text token on every surface it is used on. Faint text is never set on accent-soft.
const pairs: Array<[string, string[]]> = [
  ["ink", surfaces],
  ["muted", surfaces],
  ["accent", surfaces],
  ["danger", surfaces],
  ["faint", ["paper", "paper-raised", "sidebar"]],
  ["on-accent", ["accent"]],
];

describe("text tokens", () => {
  for (const [theme, tokens] of [["light", light], ["dark", dark]] as const) {
    for (const [text, backgrounds] of pairs) {
      for (const background of backgrounds) {
        it(`${theme}: --${text} on --${background} meets WCAG AA`, () => {
          expect(contrast(tokens[text], tokens[background])).toBeGreaterThanOrEqual(4.5);
        });
      }
    }
  }
});

describe("type scale", () => {
  it("has no text below the badge size", () => {
    const sizes = [...css.matchAll(/font-size:\s*(\d+(?:\.\d+)?)px/g)].map((m) => Number(m[1]));
    expect(sizes.filter((size) => size < 11)).toEqual([]);
  });
});

describe("touch screens", () => {
  const coarse = css.slice(css.indexOf("@media (pointer: coarse) {"));

  it("keeps text fields at 16px on coarse pointers so iOS does not zoom", () => {
    expect(coarse).toMatch(/:is\(input, select, textarea\)[^{]*\{[^}]*font-size:\s*16px/);
  });

  it("turns the tap highlight off", () => {
    expect(css).toContain("-webkit-tap-highlight-color: transparent");
  });

  it("widens the result chip tap area without growing the chip", () => {
    expect(css).toMatch(/\.text-chip::after\s*\{[^}]*inset:\s*-\d+px/);
  });

  it("provides :active touch feedback on interactive tabs, rows, pills, items, summaries, and toggles", () => {
    for (const selector of [
      ".taxonomy-tab:active",
      ".taxonomy-list-item > button:active",
      ".segmented-pill:active",
      ".select-dropdown-item:active",
      "details > summary:active",
      ".toggle-row:active",
    ]) {
      expect(css).toContain(selector);
    }
  });

  it("raises compact controls to 44px touch targets on coarse pointers", () => {
    expect(coarse).toMatch(/\.icon-button,\s*\.search-clear\s*\{[^}]*min-height:\s*44px/);
    expect(coarse).toMatch(/\.upload-file-heading \.icon-button\s*\{[^}]*min-height:\s*44px/);
    expect(coarse).toMatch(/\.alias-list \.icon-button::after\s*\{[^}]*inset:\s*-\d+px/);
    expect(coarse).toMatch(/\.toast-dismiss\s*\{[^}]*min-height:\s*44px/);
    expect(coarse).toMatch(/\.segmented-pill\s*\{[^}]*min-height:\s*44px/);
    expect(coarse).toMatch(/\.page-reference::after\s*\{[^}]*inset:\s*-\d+px/);
  });

  it("keeps mobile bar backdrop-filter lightweight and confirm sheet-up snappy", () => {
    expect(css).not.toContain("blur(18px)");
    expect(css).not.toContain("blur(20px)");
    expect(css).toMatch(/\.confirm-dialog\s*\{[^}]*animation:\s*sheet-up\s+250ms/);
  });

  it("fades settings tabs dynamically by overflow edge", () => {
    expect(css).toContain(".settings-tabs[data-fade-left]");
    expect(css).toContain(".settings-tabs[data-fade-right]");
    expect(css).toContain(".settings-tabs[data-fade-left][data-fade-right]");
  });
});
