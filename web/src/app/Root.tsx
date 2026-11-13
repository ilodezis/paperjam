import { useQuery } from "@tanstack/react-query";
import { useEffect } from "react";
import { useTranslation } from "react-i18next";

import { api } from "../api";
import { AuthPage } from "../pages/AuthPage";
import { sessionQuery } from "../queries";
import { AppShell } from "../shell/AppShell";
import { queryClient } from "./queryClient";
import { forgetUser, hasSessionHint, signedIn } from "./session";

/** What the address bar shows before anything else: the page, signed in or not. The frame is
 *  drawn from the first paint on a device that has been signed in; the session's answer only
 *  fills in the name, or — if nobody is signed in any more — replaces the frame with the sign-in. */
export function Root() {
  const session = useQuery(sessionQuery);
  const user = session.data?.user;
  const { i18n } = useTranslation();
  const language = i18n.resolvedLanguage ?? "en";

  useEffect(() => {
    if (user === null) void forgetUser();
  }, [user]);

  // The account's language is what the interface shows. An account that still holds the default
  // while the interface is in another language is brought in line, so that settings and the
  // titles of new documents agree with what the person reads.
  const accountLocale = user?.locale;
  useEffect(() => {
    if (accountLocale === undefined || accountLocale === language) return;
    api.setLocale(language)
      .then((updated) => queryClient.setQueryData(sessionQuery.queryKey, updated))
      .catch(() => undefined);
  }, [accountLocale, language]);

  if (user === null) return <AuthPage onUser={signedIn} />;
  if (user === undefined && !hasSessionHint()) {
    // Never signed in here, or signed out: the answer is a moment away, and the page it ends in
    // is the sign-in. Nothing worth drawing meanwhile, and no loading screen.
    return session.isError ? <AuthPage onUser={signedIn} /> : <div className="splash" aria-hidden="true" />;
  }
  return <AppShell user={user ?? null} />;
}
