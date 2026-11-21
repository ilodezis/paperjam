const ISO_DATE = /^(\d{4})-(\d{2})-(\d{2})$/;

// Formatters are costly to construct, so keep one per language instead of one per call.
const dateFormatters = new Map<string, Intl.DateTimeFormat>();
const sizeFormatters = new Map<string, Intl.NumberFormat>();

function dateFormatter(language: string): Intl.DateTimeFormat {
  let formatter = dateFormatters.get(language);
  if (!formatter) {
    formatter = new Intl.DateTimeFormat(language, { dateStyle: "medium", timeZone: "UTC" });
    dateFormatters.set(language, formatter);
  }
  return formatter;
}

function sizeFormatter(language: string): Intl.NumberFormat {
  let formatter = sizeFormatters.get(language);
  if (!formatter) {
    formatter = new Intl.NumberFormat(language, { maximumFractionDigits: 1, minimumFractionDigits: 1 });
    sizeFormatters.set(language, formatter);
  }
  return formatter;
}

/**
 * Formats a calendar date ("YYYY-MM-DD") for display in the given language.
 * The date is treated as a calendar day in UTC, so the day never shifts with the
 * viewer's time zone. Anything that is not a valid ISO calendar date is returned unchanged.
 */
export function formatIsoDate(value: string, language: string): string {
  const match = ISO_DATE.exec(value);
  if (!match) return value;
  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  const date = new Date(Date.UTC(year, month - 1, day));
  if (date.getUTCFullYear() !== year || date.getUTCMonth() !== month - 1 || date.getUTCDate() !== day) return value;
  return dateFormatter(language).format(date);
}

/** A file size for display: kilobytes below a megabyte, megabytes above, always with one decimal. */
export function formatFileSize(bytes: number, language: string): string {
  const megabyte = 1024 * 1024;
  const ru = language.startsWith("ru");
  if (bytes >= megabyte) {
    const value = sizeFormatter(language).format(bytes / megabyte);
    return `${value}\u00a0${ru ? "МБ" : "MB"}`;
  }
  const value = sizeFormatter(language).format(bytes / 1024);
  return `${value}\u00a0${ru ? "КБ" : "KB"}`;
}

/** A document is "expiring" for the archive when it is due within this many days (or past due): the server's `has:expiring`. */
export const EXPIRING_DAYS = 270;

export interface DueState {
  /** `overdue`: the date has passed; `soon`: within `EXPIRING_DAYS`; `later`: further away. */
  state: "overdue" | "soon" | "later";
  /** Whole days from today to the date; negative once it has passed. */
  days: number;
}

/** Where a due date ("YYYY-MM-DD") stands relative to today (a UTC day, like the server's). `null` for anything that is not an ISO date. */
export function dueState(value: string, today: Date = new Date()): DueState | null {
  const match = ISO_DATE.exec(value);
  if (!match) return null;
  const due = Date.UTC(Number(match[1]), Number(match[2]) - 1, Number(match[3]));
  const days = Math.round((due - Date.UTC(today.getUTCFullYear(), today.getUTCMonth(), today.getUTCDate())) / 86_400_000);
  return { state: days < 0 ? "overdue" : days <= EXPIRING_DAYS ? "soon" : "later", days };
}
