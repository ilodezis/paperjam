import { useEffect, useState } from "react";

import { api, type User } from "../api";
import { LoginPage } from "./LoginPage";
import { SetupPage } from "./SetupPage";

/** What a visitor without a session sees: the sign-in, or — on an instance that has no user yet —
 *  the setup that creates the first one. The answer is a moment away; until it comes nothing is
 *  drawn, so the sign-in never flashes before the setup. A server that cannot be asked gets the
 *  sign-in, which reports the trouble itself. */
export function AuthPage({ onUser }: { onUser: (user: User) => void }) {
  const [needed, setNeeded] = useState<boolean | null>(null);

  useEffect(() => {
    let live = true;
    api.setupStatus()
      .then((status) => live && setNeeded(status.needed))
      .catch(() => live && setNeeded(false));
    return () => {
      live = false;
    };
  }, []);

  if (needed === null) return <div className="splash" aria-hidden="true" />;
  return needed ? <SetupPage onUser={onUser} /> : <LoginPage onUser={onUser} />;
}
