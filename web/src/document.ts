import type { DocumentView, FieldKind, FieldValue } from "./api";

/** Whether the document has page images (and so a thumbnail) to ask for. The server says so: a
 *  plain-text file, and a file that failed before its pages were rendered, have none. */
export function hasPageImages(document: Pick<DocumentView, "file">): boolean {
  return document.file.has_pages;
}

export function fieldFromInput(kind: FieldKind, input: string, currency = "EUR"): FieldValue | null {
  const value = input.trim();
  if (!value) return null;
  if (kind === "list") return { kind, value: value.split(",").map((item) => item.trim()).filter(Boolean) };
  if (kind === "money") return { kind, value: { amount: value, currency: currency.trim().toUpperCase() || "EUR" } };
  return { kind, value } as FieldValue;
}

export function toggleId(ids: string[], id: string, checked: boolean): string[] {
  if (checked) return ids.includes(id) ? ids : [...ids, id];
  return ids.filter((current) => current !== id);
}
