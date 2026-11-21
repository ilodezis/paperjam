import { lazy, type ComponentType } from "react";

import { wait } from "../lib/motion";
import type { ScreenId } from "./nav";

/** Each screen is its own chunk. The loaders are called ahead of a tap (a hover, a route's
 *  preload) so the screen is already here when the motion starts. */
const loaders: Record<ScreenId, () => Promise<{ default: ComponentType }>> = {
  archive: () => import("../pages/ArchivePage").then((m) => ({ default: m.ArchivePage })),
  document: () => import("../pages/DocumentPage").then((m) => ({ default: m.DocumentPage })),
  upload: () => import("../pages/UploadPage").then((m) => ({ default: m.UploadPage })),
  taxonomy: () => import("../pages/TaxonomyPage").then((m) => ({ default: m.TaxonomyPage })),
  trash: () => import("../pages/TrashPage").then((m) => ({ default: m.TrashPage })),
  settings: () => import("../pages/SettingsPage").then((m) => ({ default: m.SettingsPage })),
};

/** The screens whose code has arrived, as components. */
const loaded = new Map<ScreenId, ComponentType>();

/** Fetch a screen's code without drawing it. Cheap to call again: modules load once. */
export function preloadScreen(id: ScreenId): Promise<unknown> {
  return loaders[id]().then((mod) => {
    loaded.set(id, mod.default);
    return mod;
  });
}

const components = new Map<ScreenId, ComponentType>();

/** The component that draws a screen.
 *
 *  A screen whose code is already here is drawn as it is. `lazy` would still hold its first render
 *  back for a tick and show the empty fallback, and the screen's motion would start on that empty
 *  frame; only a screen asked for before its code arrived (a pasted link, the first paint) goes
 *  through `lazy`. The choice is made once per screen so a mounted screen never swaps its type. */
export function screenComponent(id: ScreenId): ComponentType {
  let component = components.get(id);
  if (component === undefined) {
    component = loaded.get(id) ?? lazy(loaders[id]);
    components.set(id, component);
  }
  return component;
}

/** How long a tap waits for a screen's data, once its code is here, before the screen opens without it. */
const DATA_WAIT_MS = 250;

export interface OpenOptions {
  /** How long to wait for the reads once the code is here; 0 opens the screen on its own loading state. */
  waitMs?: number;
}

/** Everything a screen needs to open filled in: its code and its reads (`ensureQueryData` calls the
 *  route has already started). Data already on the device, even an old copy, is taken as it is and
 *  refreshed behind the screen; a slow read does not hold the tap past `waitMs` (`DATA_WAIT_MS`) —
 *  the screen opens and fills in when it arrives. With `waitMs: 0` only the code is awaited; the
 *  reads keep running (a failure is the screen's to show, not an unhandled rejection). */
export async function openScreen(id: ScreenId, reads: Promise<unknown>[] = [], { waitMs = DATA_WAIT_MS }: OpenOptions = {}): Promise<void> {
  const settled = Promise.allSettled(reads);
  await preloadScreen(id);
  if (waitMs > 0) await Promise.race([settled, wait(waitMs)]);
}

/** The screens the stage has mounted right now, published by it for the routes: a screen that is
 *  already drawn opens at once, whatever its data is doing. */
let mounted: ReadonlySet<ScreenId> = new Set();

export function setMountedScreens(ids: Iterable<ScreenId>): void {
  mounted = new Set(ids);
}

export function isScreenMounted(id: ScreenId): boolean {
  return mounted.has(id);
}
