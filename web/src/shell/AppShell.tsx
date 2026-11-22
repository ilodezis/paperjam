import { useQueryClient } from "@tanstack/react-query";
import { WifiOff } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type { User } from "../api";
import { signOut } from "../app/session";
import { ToastHost } from "../components/controls/ToastHost";
import { toast } from "../components/controls/toast";
import { monitorOnlineStatus } from "../pwa";
import { searchQuery, taxonomyQuery } from "../queries";
import { BottomNav } from "./BottomNav";
import { DESKTOP_MIN_WIDTH, LayoutContext } from "./layout";
import { destOf, type ScreenId } from "./nav";
import { Rail } from "./Rail";
import { preloadScreen } from "./screens";
import { Stage, type StackInfo } from "./Stage";
import { useRouter, useRouterState } from "@tanstack/react-router";
import { NotFoundPage } from "../pages/NotFoundPage";

/** The frame around every screen: the rail (desktop), the stage, the bottom bar (phone) and the
 *  toasts. The host is the container the layout rules ask about; the shell tells the few parts that
 *  are drawn in script whether it is a desktop yet. `user` is null for the moment between the first
 *  paint and the session's answer: the frame is drawn at once, without a name in it. */
export function AppShell({ user }: { user: User | null }) {
  const { t } = useTranslation();
  const router = useRouter();
  const queryClient = useQueryClient();
  const host = useRef<HTMLDivElement>(null);
  const [desktop, setDesktop] = useState(() => window.innerWidth >= DESKTOP_MIN_WIDTH);
  const [online, setOnline] = useState(() => navigator.onLine);
  const [signingOut, setSigningOut] = useState(false);
  // An address that is no screen (a mistyped link) is shown as not found, inside the same frame.
  const unknownAddress = useRouterState({ select: (state) => destOf(state.location.pathname, state.location.searchStr) === null });
  const [stack, setStack] = useState<StackInfo>(() => {
    const first: ScreenId = destOf(router.state.location.pathname, router.state.location.searchStr)?.id ?? "archive";
    return { root: first, top: first, depth: 1 };
  });

  useLayoutEffect(() => {
    const el = host.current;
    if (el === null) return;
    const read = () => setDesktop(el.clientWidth >= DESKTOP_MIN_WIDTH);
    read();
    const observer = new ResizeObserver(read);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  useEffect(
    () =>
      monitorOnlineStatus((value) => {
        setOnline(value);
        if (!value) toast(t("shell.offlineToast"), { icon: "alert" });
      }),
    [t],
  );

  // Once the app is idle: the other screens' code and the reads the archive starts from, so the
  // first tap on a tab finds them already here.
  useEffect(() => {
    const warm = () => {
      void queryClient.prefetchQuery(taxonomyQuery);
      void queryClient.prefetchQuery(searchQuery("", "relevance"));
      for (const id of ["archive", "document", "upload", "taxonomy", "trash", "settings"] as const) {
        void preloadScreen(id).catch(() => undefined);
      }
    };
    if (typeof requestIdleCallback === "function") {
      const id = requestIdleCallback(warm, { timeout: 3000 });
      return () => cancelIdleCallback(id);
    }
    const id = window.setTimeout(warm, 1500);
    return () => window.clearTimeout(id);
  }, [queryClient]);

  async function leave() {
    setSigningOut(true);
    try {
      await signOut();
    } catch {
      /* the server was unreachable; this device has forgotten the user anyway */
    } finally {
      setSigningOut(false);
    }
  }

  return (
    <LayoutContext value={{ desktop }}>
      <div ref={host} className="app-host">
        <div className="vt">
          <Rail active={unknownAddress ? null : stack.top} user={user} signingOut={signingOut} onSignOut={() => void leave()} />
          {unknownAddress ? <NotFoundPage /> : <Stage onStack={setStack} />}
          {!online ? (
            <aside className="offline-banner" role="status">
              <WifiOff aria-hidden="true" />
              <span>{t("shell.offlineNotice")}</span>
            </aside>
          ) : null}
          <BottomNav root={unknownAddress ? null : stack.root} depth={stack.depth} />
          <ToastHost />
        </div>
      </div>
    </LayoutContext>
  );
}
