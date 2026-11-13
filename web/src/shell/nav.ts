/* The navigation model: which screens exist, where each lives, and which ones the rail and the
   bottom bar show. */

import type { Dest } from "./stack";

export type ScreenId = "archive" | "document" | "upload" | "taxonomy" | "trash" | "settings";

export type IconName = "archive" | "upload" | "taxonomy" | "trash" | "settings";

/** Where each screen lives. The document is matched by prefix (`/documents/<id>`). */
export const SCREEN_PATH: Record<ScreenId, string> = {
  archive: "/",
  document: "/documents",
  upload: "/upload",
  taxonomy: "/taxonomy",
  trash: "/trash",
  settings: "/settings",
};

/** The i18n key of each screen's name: the rail, the top bar's title and the back label. */
export const SCREEN_TITLE_KEY: Record<ScreenId, string> = {
  archive: "nav.archive",
  document: "screen.document",
  upload: "nav.upload",
  taxonomy: "nav.taxonomy",
  trash: "nav.trash",
  settings: "nav.settings",
};

/** Screens that live below another one: the rail and the bar keep the parent lit. */
const PARENT: Partial<Record<ScreenId, ScreenId>> = {
  document: "archive",
};

export const navScreen = (id: ScreenId): ScreenId => PARENT[id] ?? id;

/** What a screen's "back" does when the stack has nothing below it (a pasted link). */
export const FALLBACK_BACK: Partial<Record<ScreenId, ScreenId>> = {
  document: "archive",
  trash: "settings",
};

/** The address of a document. */
export const documentPath = (id: string): string => `/documents/${encodeURIComponent(id)}`;

/** The document a path points at, or null. */
export function documentIdOf(path: string): string | null {
  const match = /^\/documents\/([^/]+)\/?$/.exec(path);
  return match === null ? null : decodeURIComponent(match[1] as string);
}

function normalized(pathname: string): string {
  return pathname.length > 1 ? pathname.replace(/\/+$/, "") : pathname;
}

export function screenForPath(pathname: string): ScreenId | null {
  const path = normalized(pathname);
  if (documentIdOf(path) !== null) return "document";
  for (const id of Object.keys(SCREEN_PATH) as ScreenId[]) {
    if (id !== "document" && SCREEN_PATH[id] === path) return id;
  }
  return null;
}

/** Where a location lands in the stack, or null for an address that is not a screen. */
export function destOf(pathname: string, searchStr: string): Dest | null {
  const path = normalized(pathname);
  const id = screenForPath(path);
  return id === null ? null : { id, path, search: searchStr };
}

/** A screen shown in the rail and the bottom bar. */
export interface NavEntry {
  screen: ScreenId;
  icon: IconName;
  /** The i18n key of the bottom bar's label, short enough for a quarter of a phone's width. */
  barKey: string;
}

/** The rail's main group, in order. */
export const RAIL_MAIN: readonly NavEntry[] = [
  { screen: "archive", icon: "archive", barKey: "nav.archive" },
  { screen: "upload", icon: "upload", barKey: "nav.add" },
  { screen: "taxonomy", icon: "taxonomy", barKey: "nav.taxonomyShort" },
];

/** The rail's lower group. The trash is one level down on a phone (in Settings), not a tab. */
export const RAIL_MORE: readonly NavEntry[] = [
  { screen: "trash", icon: "trash", barKey: "nav.trash" },
  { screen: "settings", icon: "settings", barKey: "nav.settings" },
];

/** The phone's bottom bar: four tabs, each label one short word. */
export const BAR_TABS: readonly NavEntry[] = [
  ...RAIL_MAIN,
  { screen: "settings", icon: "settings", barKey: "nav.settings" },
];

/** Which bar tab a root screen belongs to; null when there is no screen (an unknown address), so no tab is lit. */
export function tabOf(root: ScreenId | null): ScreenId | null {
  if (root === null) return null;
  const screen = navScreen(root);
  return screen === "trash" ? "settings" : screen;
}
