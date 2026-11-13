import { useLayoutEffect, useRef } from "react";
import { useRouter } from "@tanstack/react-router";
import { useTranslation } from "react-i18next";

import { NavIcon } from "./NavIcon";
import { BAR_TABS, tabOf, type ScreenId } from "./nav";
import { hrefOf, preloadHref, scrollCurrentToTop, useBack, useGo } from "./navigation";

/** The phone's bottom bar: four tabs, one short word each. The ink line slides to the tab the
 *  current stack belongs to. */
export function BottomNav({ root, depth }: { root: ScreenId | null; depth: number }) {
  const { t } = useTranslation();
  const go = useGo();
  const back = useBack();
  const router = useRouter();
  const bar = useRef<HTMLElement>(null);
  const barWidth = useRef(0);
  const current = tabOf(root);

  useLayoutEffect(() => {
    const el = bar.current;
    if (el === null) return;
    const place = () => {
      const on = el.querySelector<HTMLElement>("a.on");
      const ink = el.querySelector<HTMLElement>(".ink");
      if (ink === null || el.offsetWidth === 0) return;
      // No tab is lit (an unknown address): the ink has nothing to point at.
      if (on === null) {
        ink.style.opacity = "0";
        return;
      }
      ink.style.opacity = "";
      // The first placement and a change of the bar's width (a rotation, a resize) jump;
      // only a change of tab slides.
      const jump = barWidth.current !== el.offsetWidth;
      barWidth.current = el.offsetWidth;
      if (jump) ink.style.transition = "none";
      ink.style.transform = `translateX(${on.offsetLeft + on.offsetWidth / 2 - 10}px)`;
      if (jump) {
        void ink.offsetWidth;
        ink.style.transition = "";
      }
    };
    place();
    const observer = new ResizeObserver(place);
    observer.observe(el);
    return () => observer.disconnect();
  }, [current]);

  return (
    <nav ref={bar} className="bnav" aria-label={t("nav.main")}>
      <i className="ink" />
      {BAR_TABS.map((item) => (
        <a
          key={item.screen}
          href={hrefOf(item.screen)}
          className={item.screen === current ? "on" : undefined}
          aria-current={item.screen === current ? "page" : undefined}
          onClick={(e) => {
            e.preventDefault();
            // Tapping the tab you are in goes back to its first screen; on the first, it scrolls up.
            if (item.screen === current) {
              if (depth > 1) back(depth - 1);
              else scrollCurrentToTop();
              return;
            }
            go(hrefOf(item.screen), { mode: "tab" });
          }}
          onPointerEnter={() => preloadHref(router, hrefOf(item.screen))}
        >
          <NavIcon name={item.icon} />
          <span>{t(item.barKey)}</span>
        </a>
      ))}
    </nav>
  );
}
