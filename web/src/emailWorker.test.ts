// @vitest-environment node
import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

/** The Cloudflare Email Worker lives only as a snippet in `docs/email-intake.md`; this runs its
 *  `extractAddresses` on the header cases the server's parser is checked against
 *  (`src/email/parse.rs`), so the two never disagree on who sent a message. */
const doc = readFileSync(new URL("../../docs/email-intake.md", import.meta.url), "utf8");
const source = doc.slice(doc.indexOf("// Splits a From:"), doc.indexOf("export default {"));
const extractAddresses = new Function(`${source}; return extractAddresses;`)() as (
  header: string,
) => string[] | null;

const cases = JSON.parse(
  readFileSync(new URL("../../tests/data/email_from_headers.json", import.meta.url), "utf8"),
) as { header: string; worker: string[] | null }[];

describe("email worker sender parsing", () => {
  it("finds the snippet in the docs", () => {
    expect(source).toContain("function extractAddresses");
  });

  it.each(cases)("$header", ({ header, worker }) => {
    expect(extractAddresses(header)).toEqual(worker);
  });
});
