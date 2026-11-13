/* The screen stack, as pure logic. A phone keeps the screens you came through mounted
   under the current one (their scroll position with them) so "back" is a slide and not a
   reload; a desktop shows one screen at a time. The Stage component runs the plan this
   returns: mount, animate, then unmount what was dropped.

   Unlike a fixed list of screens, a document screen is one screen id with many addresses, so a
   screen is told apart by its path; its query string (the archive's search) travels with the
   entry and changes in place. */

import type { ScreenId } from "./nav";

export interface Dest {
  id: ScreenId;
  /** The address without its query string: what makes two screens the same screen. */
  path: string;
  /** The query string, with its leading "?" or empty. */
  search: string;
}

export interface StackEntry extends Dest {
  /** Unique for the life of the app: a screen opened twice is two entries, never one. */
  key: number;
  /** The screen "back" returns to, fixed when the entry is made; null at the bottom of a stack. */
  back: ScreenId | null;
}

/** How a navigation was asked for. `push` goes deeper; `replace` swaps the top screen and
 *  keeps what is below (a section tab); `tab` starts the stack over. */
export type NavMode = "push" | "replace" | "tab";

export type Motion = "none" | "push" | "pop" | "fade";

export interface StackState {
  entries: StackEntry[];
  nextKey: number;
}

export interface NavPlan {
  state: StackState;
  motion: Motion;
  /** The screen leaving and the screen arriving; both null when nothing moves. */
  from: StackEntry | null;
  to: StackEntry | null;
  /** Entries to unmount once the motion has finished. */
  drop: StackEntry[];
}

export const initialStack = (dest: Dest): StackState => ({ entries: [{ ...dest, key: 0, back: null }], nextKey: 1 });

export const topOf = (state: StackState): StackEntry => state.entries[state.entries.length - 1] as StackEntry;

/** The screen a "back" goes to, for the top bar's label. */
export function backTarget(state: StackState): ScreenId | null {
  const below = state.entries[state.entries.length - 2];
  return below === undefined ? null : below.id;
}

const NONE = (state: StackState): NavPlan => ({ state, motion: "none", from: null, to: null, drop: [] });

/** The same stack with the query string of the entry at `path` replaced (the archive's search
 *  changing in place). The entry keeps its key, so nothing remounts. */
export function withSearch(state: StackState, path: string, search: string): StackState {
  let changed = false;
  const entries = state.entries.map((entry) => {
    if (entry.path !== path || entry.search === search) return entry;
    changed = true;
    return { ...entry, search };
  });
  return changed ? { ...state, entries } : state;
}

/** Turn "go to `dest`" into the next stack and the motion that gets there.
 *  - the same screen: nothing moves (the caller scrolls to the top);
 *  - a screen already under the top, on a phone: pop back to it, dropping what was between;
 *  - `push`: slide the new screen in over the current one;
 *  - anything else: cross-fade; `replace` keeps what was below the top, `tab` keeps nothing;
 *  - the desktop always cross-fades and always keeps nothing. */
export function planGo(
  state: StackState,
  dest: Dest,
  { mode, desktop }: { mode: NavMode; desktop: boolean },
): NavPlan {
  const top = topOf(state);
  if (top.path === dest.path) return NONE(withSearch(state, dest.path, dest.search));

  if (!desktop) {
    const at = state.entries.findIndex((e) => e.path === dest.path);
    if (at !== -1 && at < state.entries.length - 1) {
      const to = { ...(state.entries[at] as StackEntry), search: dest.search };
      return {
        state: { ...state, entries: [...state.entries.slice(0, at), to] },
        motion: "pop",
        from: top,
        to,
        drop: state.entries.slice(at + 1),
      };
    }
  }

  const nextKey = state.nextKey + 1;

  if (!desktop && mode === "push") {
    const to: StackEntry = { ...dest, key: state.nextKey, back: top.id };
    return {
      state: { entries: [...state.entries, to], nextKey },
      motion: "push",
      from: top,
      to,
      drop: [],
    };
  }

  const keep = !desktop && mode === "replace" ? state.entries.slice(0, -1) : [];
  const to: StackEntry = { ...dest, key: state.nextKey, back: keep[keep.length - 1]?.id ?? null };
  return {
    state: { entries: [...keep, to], nextKey },
    motion: "fade",
    from: top,
    to,
    drop: state.entries.filter((e) => !keep.includes(e)),
  };
}

/** Pop the top screen (the back button, the edge swipe, the browser's back). */
export function planPop(state: StackState): NavPlan {
  if (state.entries.length < 2) return NONE(state);
  const from = topOf(state);
  const to = state.entries[state.entries.length - 2] as StackEntry;
  return { state: { ...state, entries: state.entries.slice(0, -1) }, motion: "pop", from, to, drop: [from] };
}
