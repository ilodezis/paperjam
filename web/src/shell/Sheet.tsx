import { useEffect, useLayoutEffect, useRef, useState, type PointerEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";

import { animate, settle } from "../lib/motion";
import { useLayout } from "./layout";
import { sheetCloses } from "./stageMotion";

interface DragState {
  pointer: number;
  y0: number;
  t0: number;
  dy: number;
}

const closedTransform = (desktop: boolean): string => (desktop ? "translateY(24px) scale(.97)" : "translateY(105%)");

const FOCUSABLE = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled]):not([type=hidden])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "[tabindex]:not([tabindex='-1'])",
].join(",");

const focusables = (root: HTMLElement): HTMLElement[] =>
  Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE)).filter((node) => node.closest("[hidden], [inert]") === null);

/** Ref-counts `inert` per background node so overlapping or consecutive sheets never lift `inert`
 *  while another sheet still needs the background locked. */
const inertCounts = new WeakMap<Element, number>();

interface ActiveSheet {
  panel: HTMLElement;
  scrim: HTMLElement | null;
  opener: HTMLElement | null;
}

/** Stack of currently open sheets (bottom to top), used for focus handoff and Escape routing. */
const openSheets: ActiveSheet[] = [];

/** Callbacks that immediately finish any sheet currently playing its closing animation. */
const closingSheets = new Set<() => void>();

/** Marks everything outside the dialog `inert` (no focus, no clicks, hidden from assistive
 *  technology) by walking up from the panel and marking each ancestor's other children. The scrim
 *  stays live: it is the click-outside-to-close target. Returns the undo, which decrements each
 *  node's ref-count and leaves anything that was already inert outside Sheet untouched. */
function makeBackgroundInert(panel: HTMLElement, keep: HTMLElement | null): () => void {
  const marked: Element[] = [];
  for (let node: HTMLElement = panel; node.parentElement !== null; node = node.parentElement) {
    for (const sibling of Array.from(node.parentElement.children)) {
      if (sibling === node || sibling === keep) continue;
      if (["HEAD", "SCRIPT", "STYLE", "LINK"].includes(sibling.tagName)) continue;
      const count = inertCounts.get(sibling) ?? 0;
      if (count === 0 && sibling.hasAttribute("inert")) continue;
      inertCounts.set(sibling, count + 1);
      sibling.setAttribute("inert", "");
      marked.push(sibling);
    }
  }
  return () => {
    for (const node of marked) {
      const next = (inertCounts.get(node) ?? 1) - 1;
      if (next <= 0) {
        inertCounts.delete(node);
        node.removeAttribute("inert");
      } else {
        inertCounts.set(node, next);
      }
    }
  };
}

function resolveOpener(): HTMLElement | null {
  const active = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  if (active !== null) {
    const hostSheet = openSheets.find((entry) => entry.panel.contains(active));
    if (hostSheet) return hostSheet.opener;
  }
  return active;
}

/** A bottom sheet with a grab handle: on a phone it rises from the bottom edge and follows the
 *  finger down to close (past 28% of its height, or fast); on a desktop it is a small centred
 *  panel. The backdrop, Escape and the handle all close it. While it is open focus stays inside
 *  (Tab and Shift+Tab cycle), the page behind is inert, and closing returns focus to the opener. */
export function Sheet({ open, title, onClose, children, headerAction }: {
  open: boolean;
  title: string;
  onClose: () => void;
  children: ReactNode;
  /** An control at the end of the header, such as a close button for sheets without a footer. */
  headerAction?: ReactNode;
}) {
  const { t } = useTranslation();
  const { desktop } = useLayout();
  const [present, setPresent] = useState(open);
  const panel = useRef<HTMLDivElement>(null);
  const scrim = useRef<HTMLDivElement>(null);
  const drag = useRef<DragState | null>(null);

  if (open && !present) setPresent(true);

  useLayoutEffect(() => {
    const el = panel.current;
    const back = scrim.current;
    if (el === null || back === null) return;
    if (open) {
      let cancelled = false;
      const handedOff = closingSheets.size > 0;
      if (handedOff) {
        for (const finish of Array.from(closingSheets)) finish();
      }
      settle(el, back);
      const opening = [
        animate(el, [{ transform: closedTransform(desktop), opacity: desktop ? 0 : 1 }, { transform: "none", opacity: 1 }], { duration: 260 }),
      ];
      if (!handedOff) {
        opening.push(animate(back, [{ opacity: 0 }, { opacity: 1 }], { duration: 200 }));
      }
      void Promise.all(opening.map((a) => a.finished)).then(
        () => {
          if (!cancelled) settle(el, back);
        },
        () => {
          /* cancelled by drag or close */
        },
      );
      return () => {
        cancelled = true;
      };
    }

    // Another sheet is already open (e.g. Actions -> Share): hide immediately without a second
    // fading scrim overlapping the new sheet's scrim, then unmount on the next microtask.
    if (openSheets.length > 0) {
      let cancelled = false;
      settle(el, back);
      el.hidden = true;
      back.hidden = true;
      queueMicrotask(() => {
        if (!cancelled) setPresent(false);
      });
      return () => {
        cancelled = true;
      };
    }

    let cancelled = false;
    const finishNow = () => {
      if (cancelled) return;
      cancelled = true;
      closingSheets.delete(finishNow);
      settle(el, back);
      el.hidden = true;
      back.hidden = true;
      queueMicrotask(() => setPresent(false));
    };
    closingSheets.add(finishNow);
    const leaving = [
      animate(el, [{ transform: el.style.transform || "none", opacity: 1 }, { transform: closedTransform(desktop), opacity: desktop ? 0 : 1 }], { duration: 200 }),
      animate(back, [{ opacity: 1 }, { opacity: 0 }], { duration: 180 }),
    ];
    void Promise.all(leaving.map((a) => a.finished)).then(
      () => {
        closingSheets.delete(finishNow);
        if (!cancelled) {
          settle(el, back);
          setPresent(false);
        }
      },
      () => {
        closingSheets.delete(finishNow);
      },
    );
    return () => {
      cancelled = true;
      closingSheets.delete(finishNow);
    };
  }, [open, present, desktop]);

  // The latest onClose, so a parent that passes a fresh closure on every render does not re-run
  // the effect below and lose track of the element that opened the sheet.
  const close = useRef(onClose);
  useEffect(() => {
    close.current = onClose;
  });

  useEffect(() => {
    if (!open) return;
    const el = panel.current;
    if (el === null) return;
    const opener = resolveOpener();
    const entry: ActiveSheet = { panel: el, scrim: scrim.current, opener };
    openSheets.push(entry);
    const releaseBackground = makeBackgroundInert(el, scrim.current);
    el.focus();

    const isTopmost = () => openSheets[openSheets.length - 1] === entry;

    function onKeyDown(event: KeyboardEvent) {
      if (!isTopmost()) return;
      if (event.key === "Escape") {
        event.stopPropagation();
        close.current();
        return;
      }
      if (event.key !== "Tab" || el === null) return;
      const items = focusables(el);
      const active = document.activeElement;
      if (items.length === 0) {
        event.preventDefault();
        el.focus();
      } else if (!el.contains(active) || active === el) {
        event.preventDefault();
        items[event.shiftKey ? items.length - 1 : 0].focus();
      } else if (event.shiftKey && active === items[0]) {
        event.preventDefault();
        items[items.length - 1].focus();
      } else if (!event.shiftKey && active === items[items.length - 1]) {
        event.preventDefault();
        items[0].focus();
      }
    }
    function onFocusIn(event: FocusEvent) {
      if (!isTopmost()) return;
      if (el !== null && event.target instanceof Node && !el.contains(event.target)) el.focus();
    }
    window.addEventListener("keydown", onKeyDown, true);
    document.addEventListener("focusin", onFocusIn);
    return () => {
      window.removeEventListener("keydown", onKeyDown, true);
      document.removeEventListener("focusin", onFocusIn);
      const index = openSheets.indexOf(entry);
      if (index !== -1) openSheets.splice(index, 1);
      releaseBackground();
      if (openSheets.length === 0 && opener?.isConnected && opener.closest("[inert]") === null) {
        opener.focus();
      }
    };
  }, [open]);

  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    if (event.pointerType === "mouse" && event.button !== 0) return;
    settle(panel.current, scrim.current);
    drag.current = { pointer: event.pointerId, y0: event.clientY, t0: performance.now(), dy: 0 };
    try {
      event.currentTarget.setPointerCapture(event.pointerId);
    } catch {
      /* the pointer is already gone */
    }
  };

  const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    const el = panel.current;
    if (d === null || el === null || event.pointerId !== d.pointer) return;
    d.dy = Math.max(0, event.clientY - d.y0);
    el.style.transform = `translateY(${d.dy}px)`;
  };

  const onPointerUp = (event: PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    const el = panel.current;
    if (d === null || el === null || event.pointerId !== d.pointer) return;
    drag.current = null;
    const velocity = d.dy / Math.max(1, performance.now() - d.t0);
    if (sheetCloses(d.dy, el.offsetHeight, velocity)) {
      onClose();
      return;
    }
    const from = d.dy;
    el.style.transform = "";
    settle(el);
    void animate(el, [{ transform: `translateY(${from}px)` }, { transform: "none" }], { duration: 240 }).finished.then(
      () => settle(el),
      () => undefined,
    );
  };

  if (!present) return null;
  const host = document.querySelector(".vt") ?? document.body;
  return createPortal(
    <>
      <div ref={scrim} className="scrim" onClick={onClose} />
      <div ref={panel} className="sheet" role="dialog" aria-modal="true" aria-label={title} tabIndex={-1}>
        <div
          className="sheet-grab"
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
        >
          <i aria-hidden="true" />
          <span className="sr-only">{t("common.close")}</span>
        </div>
        <div className={headerAction ? "sheet-head sheet-head--action" : "sheet-head"}><h3>{title}</h3>{headerAction}</div>
        <div className="sheet-body">{children}</div>
      </div>
    </>,
    host,
  );
}
