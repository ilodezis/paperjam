export interface SnippetPart {
  text: string;
  mark: boolean;
}

export function queryFilter(prefix: string, value: string): string {
  const escaped = value.replaceAll("\\", "\\\\").replaceAll('"', '\\"');
  return /[\s"]/.test(value) ? `${prefix}:"${escaped}"` : `${prefix}:${value}`;
}

export function toggleQueryFilter(query: string, prefix: string, value?: string): string {
  const token = value === undefined ? prefix : queryFilter(prefix, value);
  const escaped = token.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const pattern = new RegExp(`(?:^|\\s)${escaped}(?=\\s|$)`);
  if (pattern.test(query)) return query.replace(pattern, " ").replace(/\s+/g, " ").trim();
  return [query.trim(), token].filter(Boolean).join(" ");
}

export function parseSnippet(snippet: string): SnippetPart[] {
  const parts: SnippetPart[] = [];
  const marks = /<mark>([\s\S]*?)<\/mark>/g;
  const decode = (text: string) => text
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&quot;", '"')
    .replaceAll("&#39;", "'")
    .replaceAll("&amp;", "&");
  let offset = 0;
  for (const match of snippet.matchAll(marks)) {
    const index = match.index;
    if (index > offset) parts.push({ text: decode(snippet.slice(offset, index)), mark: false });
    parts.push({ text: decode(match[1]), mark: true });
    offset = index + match[0].length;
  }
  if (offset < snippet.length) parts.push({ text: decode(snippet.slice(offset)), mark: false });
  return parts;
}

/** The archive's query and sort, read from a screen's query string (`?q=type:passport&sort=date_desc`). */
export function archiveSearchOf(search: string): { query: string; sort: "relevance" | "date_desc" | "date_asc" } {
  const params = new URLSearchParams(search);
  const sort = params.get("sort");
  return { query: params.get("q") ?? "", sort: sort === "date_asc" || sort === "date_desc" ? sort : "relevance" };
}

/** Whether the query carries this exact filter token (`has:attention`, `type:passport`), as
 *  opposed to merely containing the text inside another word. */
export function hasQueryToken(query: string, token: string): boolean {
  return query.split(/\s+/).includes(token);
}

/** Whether the query carries any filter of the language (`type:passport`, `-has:tags`,
 *  `date:>=2024`, `field.expiry_date<2037`), as opposed to words alone. */
export function hasFilterTokens(query: string): boolean {
  const unquoted = query.replace(/"[^"]*"/g, " ").replace(/"[^"]*$/, " ");
  return unquoted.split(/\s+/).some((token) => /^-?(?:type|from|person|tag|has|date|added|due|local|field\.[^\s:<>=]+)[:<>=]/i.test(token));
}

/** A snippet made quiet enough to scan: runs of OCR filler (`<<<<<`, `_____`, `-----`) and line
 *  breaks collapse to one space, and text captured in capitals (a scanned form, an MRZ line) is
 *  lowercased so a result is not a wall of uppercase. Highlighted parts keep their place; only the
 *  case of the letters changes. */
export function calmSnippet(parts: SnippetPart[], language: string): SnippetPart[] {
  const tidy = (text: string) => text.replace(/[<_=\-.~|]{3,}/g, " ").replace(/\s+/g, " ");
  const tidied = parts.map((part) => ({ ...part, text: tidy(part.text) }));
  const letters = tidied.map((part) => part.text).join("").replace(/[^\p{L}]/gu, "");
  const upper = letters.replace(/[^\p{Lu}]/gu, "");
  const shouting = letters.length >= 12 && upper.length / letters.length >= 0.7;
  if (!shouting) return tidied;
  let atStart = true;
  return tidied.map((part) => {
    const lower = part.text.toLocaleLowerCase(language);
    const first = lower.search(/\p{L}/u);
    if (!atStart || first < 0) return { ...part, text: lower };
    atStart = false;
    return { ...part, text: lower.slice(0, first) + lower.charAt(first).toLocaleUpperCase(language) + lower.slice(first + 1) };
  });
}
