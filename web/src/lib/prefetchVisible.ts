import { useEffect, useRef, type RefObject } from "react";

/** Marks an element inside the list whose id is worth warming. */
export const PREFETCH_ATTRIBUTE = "data-prefetch-id";

/** Touch screens have no hover to warm anything before the tap. */
const touchOnly = (): boolean => typeof matchMedia === "function" && matchMedia("(hover: none)").matches;

/** Run `task` when the browser is idle; the returned function cancels it. */
function whenIdle(task: () => void): () => void {
  if (typeof requestIdleCallback === "function") {
    const handle = requestIdleCallback(task, { timeout: 2000 });
    return () => cancelIdleCallback(handle);
  }
  const handle = setTimeout(task, 200);
  return () => clearTimeout(handle);
}

/** On touch devices, calls `prefetch(id)` once per id for every `[data-prefetch-id]` element of
 *  `list` that scrolls into view (a little early), in idle time, so a tap finds its data cached.
 *  Does nothing on hover-capable devices, without IntersectionObserver, or while `enabled` is off.
 *  `items` changes whenever the list's elements do. */
export function usePrefetchVisible(list: RefObject<HTMLElement | null>, enabled: boolean, items: unknown, prefetch: (id: string) => void): void {
  const seen = useRef(new Set<string>());
  const pending = useRef(new Map<string, () => void>());

  useEffect(() => {
    const root = list.current;
    if (!enabled || !root || typeof IntersectionObserver !== "function" || !touchOnly()) return;
    const scheduled = pending.current;
    const done = seen.current;
    const observer = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        const id = entry.target.getAttribute(PREFETCH_ATTRIBUTE);
        observer.unobserve(entry.target);
        if (!id || done.has(id)) continue;
        done.add(id);
        scheduled.set(id, whenIdle(() => {
          scheduled.delete(id);
          prefetch(id);
        }));
      }
    }, { rootMargin: "200px 0px" });
    for (const element of root.querySelectorAll(`[${PREFETCH_ATTRIBUTE}]`)) observer.observe(element);
    return () => observer.disconnect();
  }, [enabled, items, list, prefetch]);

  useEffect(() => {
    const scheduled = pending.current;
    const done = seen.current;
    return () => {
      for (const cancel of scheduled.values()) cancel();
      scheduled.clear();
      done.clear();
    };
  }, []);
}
