import { useCallback, type ComponentProps, type MouseEvent } from "react";
import { useRouter, type AnyRouter } from "@tanstack/react-router";

import { announceNavigation } from "./navIntent";
import { FALLBACK_BACK, SCREEN_PATH, screenForPath, type ScreenId } from "./nav";
import { useScreen } from "./ScreenFrame";
import type { NavMode } from "./stack";
import { preloadScreen } from "./screens";

/** The address a static screen lives at — for a link's `href`. */
export const hrefOf = (screen: ScreenId): string => SCREEN_PATH[screen];

const SCROLL_TOP = "paperjam:scroll-top";

/** Ask the stage to scroll its current screen to the top (a tap on where you already are). */
export function scrollCurrentToTop(): void {
  window.dispatchEvent(new Event(SCROLL_TOP));
}
export const SCROLL_TOP_EVENT = SCROLL_TOP;

function split(href: string): { path: string; query: string } {
  const at = href.indexOf("?");
  return at === -1 ? { path: href, query: "" } : { path: href.slice(0, at), query: href.slice(at + 1) };
}

const searchOf = (query: string): Record<string, string> => Object.fromEntries(new URLSearchParams(query));

/** Fetch what an address needs — the screen's code and the route's reads — before it is asked for. */
export function preloadHref(router: AnyRouter, href: string): void {
  const { path, query } = split(href);
  const screen = screenForPath(path);
  if (screen !== null) void preloadScreen(screen).catch(() => undefined);
  void router.preloadRoute({ to: path as never, search: searchOf(query) as never }).catch(() => undefined);
}

export interface GoOptions {
  mode?: NavMode;
  /** The element that flies to the next screen's page. */
  shared?: HTMLElement | null;
  /** Replace the history entry instead of adding one. */
  replace?: boolean;
}

/** Go to an address. `mode` says how it should feel: `push` slides in deeper, `replace` swaps the
 *  current screen for a sibling, `tab` starts over. */
export function useGo() {
  const router = useRouter();
  return useCallback(
    (href: string, options: GoOptions = {}) => {
      const { path, query } = split(href);
      const mode = options.mode ?? "push";
      const here = router.state.location;
      if (here.pathname === path && here.searchStr === (query === "" ? "" : `?${query}`)) {
        scrollCurrentToTop();
        return;
      }
      announceNavigation({ mode, shared: options.shared });
      void router.navigate({ to: path as never, search: searchOf(query) as never, replace: options.replace ?? mode === "replace", resetScroll: false });
    },
    [router],
  );
}

/** The browser's own back, so the address and the stack stay one thing. `steps` goes further. */
export function useBack() {
  const router = useRouter();
  return useCallback((steps = 1) => router.history.go(-steps), [router]);
}

/** What the top bar's back does on this screen: slide back to the screen below, or — for a screen
 *  opened from a pasted link, with nothing below — go to its parent. */
export function useScreenBack() {
  const { id, back } = useScreen();
  const goBack = useBack();
  const go = useGo();
  return useCallback(() => {
    if (back !== null) {
      goBack();
      return;
    }
    const parent = FALLBACK_BACK[id];
    if (parent !== undefined) go(hrefOf(parent), { mode: "tab" });
  }, [back, goBack, go, id]);
}

type ScreenLinkProps = {
  to: string;
  mode?: NavMode;
  /** Fly the element matching this selector, inside the link's card, to the next screen's page. */
  sharedSelector?: string;
} & Omit<ComponentProps<"a">, "href">;

/** A link to a screen: a real anchor (open in a new tab works), driven by the app's motion. */
export function ScreenLink({ to, mode, sharedSelector, onClick, onPointerEnter, onPointerDown, onFocus, children, ...rest }: ScreenLinkProps) {
  const go = useGo();
  const router = useRouter();
  return (
    <a
      href={to}
      {...rest}
      onClick={(e: MouseEvent<HTMLAnchorElement>) => {
        onClick?.(e);
        if (e.defaultPrevented || e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
        e.preventDefault();
        const shared = sharedSelector ? e.currentTarget.closest("li, article")?.querySelector<HTMLElement>(sharedSelector) ?? null : null;
        go(to, { mode, shared });
      }}
      onPointerEnter={(e) => {
        onPointerEnter?.(e);
        preloadHref(router, to);
      }}
      // On touch, pointerenter fires with the tap itself; pointerdown buys the ~100 ms before click.
      onPointerDown={(e) => {
        onPointerDown?.(e);
        preloadHref(router, to);
      }}
      onFocus={(e) => {
        onFocus?.(e);
        preloadHref(router, to);
      }}
    >
      {children}
    </a>
  );
}
