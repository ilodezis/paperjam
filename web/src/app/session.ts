import { api, type User } from "../api";
import { sessionQuery } from "../queries";
import { persister } from "./persist";
import { queryClient } from "./queryClient";

/* "Is this device signed in?" kept as a plain flag. It says nothing about who, and holds no data:
   it only lets the app draw its frame at once on a start with no cache yet, instead of waiting for
   the session to answer behind a loading screen. The session query has the last word. */

const HINT = "paperjam:signed-in";

export function hasSessionHint(): boolean {
  try {
    return window.localStorage.getItem(HINT) === "1";
  } catch {
    return false;
  }
}

function setSessionHint(on: boolean): void {
  try {
    if (on) window.localStorage.setItem(HINT, "1");
    else window.localStorage.removeItem(HINT);
  } catch {
    /* storage refused */
  }
}

/** Everything cached for a user other than the one signed in now: all but the session itself. */
function dropUserData(): void {
  queryClient.removeQueries({ predicate: (query) => query.queryKey[0] !== sessionQuery.queryKey[0] });
}

/** A sign-in (password or passkey) succeeded. */
export function signedIn(user: User): void {
  dropUserData();
  setSessionHint(true);
  queryClient.setQueryData(sessionQuery.queryKey, { user });
}

/** Nobody is signed in: forget what the device was told about them, in memory and on disk. */
export async function forgetUser(): Promise<void> {
  setSessionHint(false);
  dropUserData();
  await persister.removeClient();
}

export async function signOut(): Promise<void> {
  await api.logout();
  await forgetUser();
  queryClient.setQueryData(sessionQuery.queryKey, { user: null });
}
