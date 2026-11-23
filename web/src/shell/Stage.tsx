import { Suspense, createElement, memo, useCallback, useEffect, useEffectEvent, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useRouter, useRouterState } from "@tanstack/react-router";

import { animate, prefersReducedMotion, settle } from "../lib/motion";
import { useLayout } from "./layout";
import { SCROLL_TOP_EVENT } from "./navigation";
import { destOf, type ScreenId } from "./nav";
import { takeNavigationIntent } from "./navIntent";
import { screenComponent, setMountedScreens } from "./screens";
import { ScreenBoundary } from "./ScreenBoundary";
import { ScreenFrame, ScreenProvider, type ScreenInfo } from "./ScreenFrame";
import { initialStack, planGo, planPop, topOf, type Dest, type NavPlan, type StackEntry, type StackState } from "./stack";
import { fade, sharedMorph, slide, SLIDE_MS, swipeCommits, swipeFrame, whenPresent } from "./stageMotion";

/** What is mounted right now: the stack itself, plus screens that are leaving and still
 *  drawn until their motion is over; `moving` are the two the motion is running on. */
interface StageView {
  logical: StackState;
  leaving: StackEntry[];
  moving: number[];
  /** While a motion runs, the screens that answer the keyboard: the one that did before it and a
   *  screen mounted for it. A screen already drawn learns it became (or stopped being) the top only
   *  once the motion is over, so the commit that starts the motion redraws no page. Unset: the top. */
  held?: number[];
}

interface Job {
  plan: NavPlan;
  shared: HTMLElement | null;
}

/** A touch that began at the left edge and has not yet shown which way it goes. */
interface Candidate {
  pointer: number;
  x0: number;
  y0: number;
  t0: number;
}

interface Swipe {
  pointer: number;
  x0: number;
  t0: number;
  top: HTMLElement;
  under: HTMLElement;
  width: number;
  dx: number;
}

/** What the frame around the stage needs: which screen is on top, which one the stack started
 *  with (the bottom bar's lit tab) and how deep it is. */
export interface StackInfo {
  root: ScreenId;
  top: ScreenId;
  depth: number;
}

/** Pointer travel from the left edge that starts a swipe back, in pixels. */
const EDGE = 28;

/** Horizontal travel that turns an edge touch into a swipe, in pixels; less is still a tap. */
const SLOP = 10;

/** Installed to the home screen. In a browser tab the system's own back gesture owns the edge. */
const standalone = (): boolean =>
  typeof matchMedia === "function" &&
  (matchMedia("(display-mode: standalone)").matches || (navigator as { standalone?: boolean }).standalone === true);

const ARCHIVE: Dest = { id: "archive", path: "/", search: "" };

/** The screens, and how they hand over. The router owns the address; this owns the stack: a
 *  change of address becomes a plan (`planGo`) and the plan becomes a motion. A phone keeps the
 *  screens you came through mounted underneath; a desktop shows one. */
export function Stage({ onStack }: { onStack: (info: StackInfo) => void }) {
  const router = useRouter();
  // The address the router has finished loading: a screen's code and data are in before it moves.
  const location = useRouterState({ select: (s) => s.resolvedLocation ?? s.location });
  const { desktop } = useLayout();

  const [view, setView] = useState<StageView>(() => ({
    logical: initialStack(destOf(router.state.location.pathname, router.state.location.searchStr) ?? ARCHIVE),
    leaving: [],
    moving: [],
  }));
  const viewRef = useRef(view);
  const nodes = useRef(new Map<number, HTMLElement>());
  const stage = useRef<HTMLDivElement>(null);
  const shade = useRef<HTMLDivElement>(null);
  const busy = useRef(false);
  const job = useRef<Job | null>(null);
  const activeMotion = useRef<AbortController | null>(null);
  const candidate = useRef<Candidate | null>(null);
  const swipe = useRef<Swipe | null>(null);

  // One function for every screen, so a screen's props stay the same from one commit to the next.
  const registerNode = useCallback((key: number, node: HTMLElement | null) => {
    if (node === null) nodes.current.delete(key);
    else nodes.current.set(key, node);
  }, []);

  const commit = (next: StageView) => {
    viewRef.current = next;
    setView(next);
  };

  const scrollToTop = () => {
    const top = nodes.current.get(topOf(viewRef.current.logical).key);
    top?.scrollTo({ top: 0, behavior: prefersReducedMotion() ? "auto" : "smooth" });
  };

  const abortMotion = () => {
    if (activeMotion.current !== null) {
      activeMotion.current.abort();
      activeMotion.current = null;
    }
    for (const el of nodes.current.values()) {
      settle(el);
      el.style.zIndex = "";
      el.style.boxShadow = "";
      el.style.transform = "";
    }
    if (shade.current !== null) {
      settle(shade.current);
      shade.current.style.zIndex = "";
      shade.current.style.opacity = "";
    }
    busy.current = false;
  };

  const run = useEffectEvent(async ({ plan, shared }: Job) => {
    const controller = new AbortController();
    activeMotion.current = controller;
    const { signal } = controller;
    const from = plan.from === null ? undefined : nodes.current.get(plan.from.key);
    const to = plan.to === null ? undefined : nodes.current.get(plan.to.key);
    const stageEl = stage.current;
    const shadeEl = shade.current;
    try {
      if (from !== undefined && to !== undefined && stageEl !== null && shadeEl !== null) {
        const target =
          shared !== null && plan.motion !== "pop"
            ? await whenPresent(() => to.querySelector<HTMLElement>("[data-shared-target]"), 32, 16, signal)
            : null;
        if (signal.aborted) return;
        if (shared !== null && target !== null) {
          await sharedMorph(from, to, shared, target, stageEl.parentElement ?? stageEl, signal);
        } else if (plan.motion === "push") {
          await slide(from, to, shadeEl, stageEl.offsetWidth, 1, 0, signal);
        } else if (plan.motion === "pop") {
          await slide(from, to, shadeEl, stageEl.offsetWidth, -1, 0, signal);
        } else {
          await fade(from, to, signal);
        }
      }
    } finally {
      if (!signal.aborted) {
        if (activeMotion.current === controller) activeMotion.current = null;
        commit({ logical: viewRef.current.logical, leaving: [], moving: [] });
        busy.current = false;
      }
    }
  });

  const sync = useEffectEvent((dest: Dest) => {
    // A navigation that arrives mid-motion interrupts the active animation cleanly so rapid taps
    // never queue behind an outgoing transition.
    const interrupted = busy.current;
    if (interrupted) {
      abortMotion();
      const currentTop = nodes.current.get(topOf(viewRef.current.logical).key);
      if (currentTop !== undefined) {
        currentTop.hidden = false;
        currentTop.style.display = "";
      }
    }
    const intent = takeNavigationIntent();
    // Nobody announced it (the browser's back or forward, a pasted link): a screen already under
    // the top pops back to; anything else starts over.
    const plan = planGo(viewRef.current.logical, dest, { mode: intent?.mode ?? "tab", desktop });
    if (plan.motion === "none" || plan.from === null || plan.to === null) {
      if (interrupted || plan.state !== viewRef.current.logical) {
        // Only the query string moved, or an interrupted motion returned to the current screen.
        commit({ logical: plan.state, leaving: [], moving: [] });
      } else {
        scrollToTop();
      }
      return;
    }
    busy.current = true;
    job.current = { plan, shared: intent?.shared ?? null };
    const before = viewRef.current;
    const toKey = plan.to.key;
    const fresh = !before.logical.entries.some((entry) => entry.key === toKey);
    const held = [topOf(before.logical).key, ...(fresh ? [toKey] : [])];
    commit({ logical: plan.state, leaving: plan.drop, moving: [plan.from.key, toKey], held });
  });

  useLayoutEffect(() => {
    const dest = destOf(location.pathname, location.searchStr);
    if (dest !== null) sync(dest);
  }, [location.pathname, location.searchStr]);

  // The frame around the stage follows the stack.
  const report = useEffectEvent(onStack);
  useLayoutEffect(() => {
    const entries = view.logical.entries;
    report({ root: (entries[0] as StackEntry).id, top: topOf(view.logical).id, depth: entries.length });
  }, [view.logical]);

  useEffect(() => {
    window.addEventListener(SCROLL_TOP_EVENT, scrollToTop);
    return () => {
      window.removeEventListener(SCROLL_TOP_EVENT, scrollToTop);
      activeMotion.current?.abort();
      activeMotion.current = null;
    };
  }, []);

  // The routes ask what is on the stage before the next tap's loader runs, so this is current by then.
  useLayoutEffect(() => {
    setMountedScreens(stageMountedEntries(view.logical.entries, view.leaving).map((entry) => entry.id));
  }, [view]);
  useLayoutEffect(() => () => setMountedScreens([]), []);

  // The plan is run once its screens are in the DOM and before they are painted.
  useLayoutEffect(() => {
    const next = job.current;
    if (next === null) return;
    job.current = null;
    void run(next);
  }, [view]);

  // Growing into a desktop window: only the screen on top stays.
  useEffect(() => {
    const v = viewRef.current;
    if (desktop && v.logical.entries.length > 1) {
      if (busy.current) abortMotion();
      commit({ logical: { ...v.logical, entries: [topOf(v.logical)] }, leaving: [], moving: [] });
    }
  }, [desktop]);

  /* ---------- Swipe back from the left edge (phone) ---------- */
  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    const stageEl = stage.current;
    candidate.current = null;
    if (desktop || busy.current || swipe.current !== null || viewRef.current.logical.entries.length < 2 || stageEl === null) return;
    if (e.pointerType === "mouse" && e.button !== 0) return;
    if (!standalone() || e.clientX - stageEl.getBoundingClientRect().left > EDGE) return;
    // Only a candidate: a tap on the bar's back button starts here too and must reach its click untouched.
    candidate.current = { pointer: e.pointerId, x0: e.clientX, y0: e.clientY, t0: performance.now() };
  };

  const startSwipe = (c: Candidate): boolean => {
    const stageEl = stage.current;
    const shadeEl = shade.current;
    const v = viewRef.current;
    if (busy.current || stageEl === null || shadeEl === null) return false;
    const [top, under] = [v.logical.entries[v.logical.entries.length - 1], v.logical.entries[v.logical.entries.length - 2]];
    const topEl = top === undefined ? undefined : nodes.current.get(top.key);
    const underEl = under === undefined ? undefined : nodes.current.get(under.key);
    if (top === undefined || under === undefined || topEl === undefined || underEl === undefined) return false;
    // Bring the screen below out of hiding for the length of the gesture.
    commit({ ...v, moving: [top.key, under.key] });
    underEl.style.zIndex = "1";
    shadeEl.style.zIndex = "2";
    topEl.style.zIndex = "3";
    topEl.style.boxShadow = "-24px 0 48px -18px rgba(0,0,0,.55)";
    swipe.current = { pointer: c.pointer, x0: c.x0, t0: c.t0, top: topEl, under: underEl, width: stageEl.offsetWidth, dx: 0 };
    try {
      stageEl.setPointerCapture(c.pointer);
    } catch {
      /* the pointer is already gone */
    }
    return true;
  };

  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const c = candidate.current;
    if (c !== null && e.pointerId === c.pointer && swipe.current === null) {
      const dx = e.clientX - c.x0;
      const dy = e.clientY - c.y0;
      if (Math.abs(dx) < SLOP && Math.abs(dy) < SLOP) return;
      candidate.current = null;
      // Rightward and mostly horizontal starts the swipe; anything else was a scroll or a drift.
      if (dx < SLOP || dx <= Math.abs(dy) || !startSwipe(c)) return;
    }
    const s = swipe.current;
    if (s === null || e.pointerId !== s.pointer) return;
    s.dx = Math.min(Math.max(e.clientX - s.x0, 0), s.width);
    const frame = swipeFrame(s.dx / s.width, s.width);
    s.top.style.transform = `translateX(${frame.top}px)`;
    s.under.style.transform = `translateX(${frame.under}px)`;
    if (shade.current !== null) shade.current.style.opacity = String(frame.shade);
  };

  const endSwipe = async (e: React.PointerEvent<HTMLDivElement>) => {
    if (candidate.current?.pointer === e.pointerId) candidate.current = null;
    const s = swipe.current;
    const shadeEl = shade.current;
    if (s === null || e.pointerId !== s.pointer || shadeEl === null) return;
    swipe.current = null;
    const progress = s.dx / s.width;
    const velocity = s.dx / Math.max(1, performance.now() - s.t0);
    s.top.style.transform = "";
    s.under.style.transform = "";
    shadeEl.style.opacity = "";
    busy.current = true;
    const controller = new AbortController();
    activeMotion.current = controller;
    const { signal } = controller;
    const v = viewRef.current;
    if (swipeCommits(progress, velocity)) {
      const plan = planPop(v.logical);
      commit({ logical: plan.state, leaving: plan.drop, moving: v.moving, held: [topOf(v.logical).key] });
      await slide(s.top, s.under, shadeEl, s.width, -1, progress, signal);
      if (signal.aborted) return;
      if (activeMotion.current === controller) activeMotion.current = null;
      commit({ logical: plan.state, leaving: [], moving: [] });
      busy.current = false;
      // The address follows the stack; the change finds the stack already there and does nothing.
      router.history.back();
    } else {
      const frame = swipeFrame(progress, s.width);
      const anims = [
        animate(s.top, [{ transform: `translateX(${frame.top}px)` }, { transform: "none" }], { duration: SLIDE_MS }),
        animate(s.under, [{ transform: `translateX(${frame.under}px)` }, { transform: `translateX(${-s.width * 0.28}px)` }], { duration: SLIDE_MS }),
        animate(shadeEl, [{ opacity: frame.shade }, { opacity: 0.32 }], { duration: SLIDE_MS }),
      ];
      let onAbort: (() => void) | undefined;
      const aborted = new Promise<void>((resolve) => {
        onAbort = () => {
          for (const animation of anims) animation.cancel();
          resolve();
        };
        signal.addEventListener("abort", onAbort, { once: true });
      });
      await Promise.race([
        Promise.all(anims.map((animation) => animation.finished)).then(() => undefined, () => undefined),
        aborted,
      ]);
      if (onAbort) signal.removeEventListener("abort", onAbort);
      if (signal.aborted) return;
      if (activeMotion.current === controller) activeMotion.current = null;
      settle(s.top, s.under, shadeEl);
      for (const n of [s.top, s.under, shadeEl]) n.style.zIndex = "";
      s.top.style.boxShadow = "";
      commit({ ...viewRef.current, moving: [] });
      busy.current = false;
    }
  };

  const top = topOf(view.logical);
  const mounted: StackEntry[] = stageMountedEntries(view.logical.entries, view.leaving);
  return (
    <div
      ref={stage}
      className="stage"
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={endSwipe}
      onPointerCancel={endSwipe}
    >
      <div ref={shade} className="screen-shade" />
      {mounted.map((entry) => (
        <StageScreen
          key={entry.key}
          entryKey={entry.key}
          id={entry.id}
          path={entry.path}
          search={entry.search}
          back={desktop ? null : entry.back}
          active={view.held ? view.held.includes(entry.key) : entry.key === top.key}
          hidden={entry.key !== top.key && !view.moving.includes(entry.key)}
          registerNode={registerNode}
        />
      ))}
    </div>
  );
}

interface StageScreenProps {
  entryKey: number;
  id: ScreenId;
  path: string;
  search: string;
  back: ScreenId | null;
  active: boolean;
  hidden: boolean;
  registerNode: (key: number, node: HTMLElement | null) => void;
}

/** One mounted screen. Primitives only, so a commit that does not touch this screen (a hidden one
 *  under a motion) does not draw it again. */
const StageScreen = memo(function StageScreen({ entryKey, id, path, search, back, active, hidden, registerNode }: StageScreenProps) {
  const register = useCallback((node: HTMLElement | null) => registerNode(entryKey, node), [registerNode, entryKey]);
  const info = useMemo<ScreenInfo>(() => ({ id, path, search, back, active }), [id, path, search, back, active]);
  return (
    <ScreenFrame id={id} hidden={hidden} register={register}>
      <ScreenProvider info={info}>
        <ScreenBoundary id={id}>
          <Suspense fallback={null}>
            <ScreenBody id={id} />
          </Suspense>
        </ScreenBoundary>
      </ScreenProvider>
    </ScreenFrame>
  );
});

/** Draws only when the screen changes; what the page inside reads from its context reaches it on its own. */
const ScreenBody = memo(function ScreenBody({ id }: { id: ScreenId }) {
  return createElement(screenComponent(id));
});

export function stageMountedEntries(logical: StackEntry[], leaving: StackEntry[]): StackEntry[] {
  return [...leaving, ...logical];
}
