import { Files, LogOut } from "lucide-react";
import { useLayoutEffect, useRef } from "react";
import { useTranslation } from "react-i18next";

import type { User } from "../api";
import { NavIcon } from "./NavIcon";
import { RAIL_MAIN, RAIL_MORE, SCREEN_TITLE_KEY, navScreen, type NavEntry, type ScreenId } from "./nav";
import { ScreenLink, hrefOf } from "./navigation";

/** The desktop rail: the places, the account row. The ink plate slides to the active section. */
export function Rail({ active, user, signingOut, onSignOut }: {
  /** The screen on top of the stack; null for an address that is no screen, so nothing is lit. */
  active: ScreenId | null;
  user: User | null;
  signingOut: boolean;
  onSignOut: () => void;
}) {
  const { t } = useTranslation();
  const nav = useRef<HTMLElement>(null);
  const placed = useRef(false);
  const current = active === null ? null : navScreen(active);

  useLayoutEffect(() => {
    const el = nav.current;
    if (el === null) return;
    const place = () => {
      const on = el.querySelector<HTMLElement>(".rail-a.on");
      const ink = el.querySelector<HTMLElement>(".rail-ink");
      if (ink === null) return;
      if (on === null) {
        ink.style.opacity = "0";
        return;
      }
      // On a phone the rail is not drawn and has no size to place the plate by.
      if (on.offsetHeight === 0) return;
      // The first placement jumps; only a change of section slides.
      const jump = !placed.current;
      placed.current = true;
      if (jump) ink.style.transition = "none";
      ink.style.opacity = "1";
      ink.style.height = `${on.offsetHeight}px`;
      ink.style.transform = `translateY(${on.offsetTop}px)`;
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

  const link = (entry: NavEntry) => (
    <ScreenLink
      key={entry.screen}
      to={hrefOf(entry.screen)}
      mode="tab"
      className={entry.screen === current ? "rail-a on" : "rail-a"}
      aria-current={entry.screen === current ? "page" : undefined}
    >
      <NavIcon name={entry.icon} />
      <span>{t(SCREEN_TITLE_KEY[entry.screen])}</span>
    </ScreenLink>
  );

  return (
    <aside className="rail">
      <ScreenLink to={hrefOf("archive")} mode="tab" className="brand" aria-label={t("nav.archive")}>
        <Files aria-hidden="true" /><span>paperjam</span>
      </ScreenLink>
      <nav ref={nav} className="rail-nav" aria-label={t("nav.main")}>
        <span className="rail-ink" />
        {RAIL_MAIN.map(link)}
        <div className="rail-rule" />
        {RAIL_MORE.map(link)}
      </nav>
      <div className="account-row">
        <span className="account-name" title={user?.display_name}>{user?.display_name ?? ""}</span>
        <button className="icon-button" type="button" onClick={onSignOut} disabled={signingOut || user === null} title={t("app.signOut")}>
          <LogOut aria-hidden="true" /><span className="sr-only">{t("app.signOut")}</span>
        </button>
      </div>
    </aside>
  );
}
