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

/** Marks everything outside the dialog `inert` (no focus, no clicks, hidden from assistive
 *  technology) by walking up from the panel and marking each ancestor's other children. The scrim
 *  stays live: it is the click-outside-to-close target. Returns the undo, which leaves anything that
 *  was already inert exactly as it found it. */
function makeBackgroundInert(panel: HTMLElement, keep: HTMLElement | null): () => void {
  const marked: Element[] = [];
  for (let node: HTMLElement = panel; node.parentElement !== null; node = node.parentElement) {
    for (const sibling of Array.from(node.parentElement.children)) {
      if (sibling === node || sibling === keep || sibling.hasAttribute("inert")) continue;
      if (["HEAD", "SCRIPT", "STYLE", "LINK"].includes(sibling.tagName)) continue;
      sibling.setAttribute("inert", "");
      marked.push(sibling);
    }
  }
  return () => {
    for (const node of marked) node.removeAttribute("inert");
  };
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
      animate(el, [{ transform: closedTransform(desktop), opacity: desktop ? 0 : 1 }, { transform: "none", opacity: 1 }], { duration: 520 });
      animate(back, [{ opacity: 0 }, { opacity: 1 }], { duration: 320 });
      return;
    }
    let cancelled = false;
    const leaving = [
      animate(el, [{ transform: el.style.transform || "none", opacity: 1 }, { transform: closedTransform(desktop), opacity: desktop ? 0 : 1 }], { duration: 320 }),
      animate(back, [{ opacity: 1 }, { opacity: 0 }], { duration: 280 }),
    ];
    void Promise.all(leaving.map((a) => a.finished)).then(() => {
      if (!cancelled) setPresent(false);
    });
    return () => {
      cancelled = true;
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
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const releaseBackground = makeBackgroundInert(el, scrim.current);
    el.focus();

    function onKeyDown(event: KeyboardEvent) {
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
      if (el !== null && event.target instanceof Node && !el.contains(event.target)) el.focus();
    }
    window.addEventListener("keydown", onKeyDown, true);
    document.addEventListener("focusin", onFocusIn);
    return () => {
      window.removeEventListener("keydown", onKeyDown, true);
      document.removeEventListener("focusin", onFocusIn);
      releaseBackground();
      if (opener?.isConnected) opener.focus();
    };
  }, [open]);

  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    if (event.pointerType === "mouse" && event.button !== 0) return;
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
    animate(el, [{ transform: `translateY(${from}px)` }, { transform: "none" }], { duration: 360 });
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
