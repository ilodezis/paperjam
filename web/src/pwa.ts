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
