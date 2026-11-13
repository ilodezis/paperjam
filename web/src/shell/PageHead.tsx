import { ChevronLeft } from "lucide-react";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";

import { FALLBACK_BACK, SCREEN_TITLE_KEY } from "./nav";
import { useScreenBack } from "./navigation";
import { useScreen } from "./ScreenFrame";

/** The bar at the top of a phone screen: back with the previous screen's name, the compact
 *  title that fades in as the large one scrolls away, and room for an action. On a desktop the
 *  bar is hidden and the page's own heading carries the title. */
export function TopBar({ title, right }: { title: string; right?: ReactNode }) {
  const { t } = useTranslation();
  const { id, back } = useScreen();
  const goBack = useScreenBack();
  const backTo = back ?? FALLBACK_BACK[id] ?? null;
  return (
    <div className={backTo === null && right === undefined ? "topbar bare" : "topbar"}>
      <div className="topbar-l">
        {backTo !== null ? (
          <button type="button" className="back" onClick={goBack}>
            <ChevronLeft aria-hidden="true" />
            <span>{t(SCREEN_TITLE_KEY[backTo])}</span>
          </button>
        ) : null}
      </div>
      <div className="topbar-title" aria-hidden="true">{title}</div>
      <div className="topbar-r">{right}</div>
    </div>
  );
}
