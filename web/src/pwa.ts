/**
 * PWA installation and offline service worker integration.
 */

export async function registerServiceWorker(
  nav: Navigator = navigator,
  isDev: boolean = import.meta.env.DEV,
): Promise<ServiceWorkerRegistration | undefined> {
  // Never register the production shell worker during development with MSW.
  if (isDev) {
    return undefined;
  }

  if (!("serviceWorker" in nav) || typeof nav.serviceWorker?.register !== "function") {
    return undefined;
  }

  try {
    return await nav.serviceWorker.register("/sw.js", { scope: "/" });
  } catch {
    // Gracefully ignore registration errors (e.g. file:// origins or disabled cookies).
    return undefined;
  }
}

/**
 * Subscribes to browser online/offline connectivity events.
 * Returns an unsubscription cleanup callback.
 */
export function monitorOnlineStatus(
  onChange: (online: boolean) => void,
  win: Window = window,
): () => void {
  const handleOnline = () => onChange(true);
  const handleOffline = () => onChange(false);

  win.addEventListener("online", handleOnline);
  win.addEventListener("offline", handleOffline);

  return () => {
    win.removeEventListener("online", handleOnline);
    win.removeEventListener("offline", handleOffline);
  };
}

/** The app's own static files: the shell, its hashed bundles, fonts and icons. */
const STATIC_PATHS = new Set(["/", "/index.html", "/manifest.webmanifest", "/favicon.svg", "/icon.svg", "/icon-maskable.svg"]);

function isStaticPath(pathname: string): boolean {
  return STATIC_PATHS.has(pathname) || pathname.startsWith("/assets/") || pathname.startsWith("/fonts/");
}

/**
 * Deletes everything the service worker cached except the app's static files, so no page a
 * signed-in user opened stays in the browser profile after sign-out. Never throws.
 */
export async function clearCachedPages(storage: CacheStorage | undefined = globalThis.caches): Promise<void> {
  if (!storage) return;
  try {
    for (const name of await storage.keys()) {
      if (!name.startsWith("paperjam-shell-")) continue;
      const cache = await storage.open(name);
      for (const request of await cache.keys()) {
        if (!isStaticPath(new URL(request.url).pathname)) await cache.delete(request);
      }
    }
  } catch {
    /* storage refused or unavailable */
  }
}
