/// <reference lib="webworker" />

// v3: earlier versions stored shared-document and OAuth pages; activating
// this one deletes those caches.
const CACHE_NAME = "paperjam-shell-v3";

const PRECACHE_ASSETS = [
  "/",
  "/index.html",
  "/manifest.webmanifest",
  "/favicon.svg",
  "/icon.svg",
  "/icon-maskable.svg",
];

/** Paths served only from the live server: the API, shared-document pages, the OAuth screens and
 *  calendar feeds carry a document or a grant and must never land in the browser profile. */
const NETWORK_ONLY_PREFIXES = ["/api/", "/s/", "/oauth/", "/calendar/", "/.well-known/"];

function networkOnly(pathname) {
  return pathname === "/healthz" || NETWORK_ONLY_PREFIXES.some((prefix) => pathname.startsWith(prefix));
}

/** A response the server lets a shared cache keep: a plain same-origin 200 without `no-store` or
 *  `private`. */
function storable(response) {
  if (!response.ok || response.status !== 200 || response.type !== "basic") return false;
  const cacheControl = (response.headers.get("Cache-Control") || "").toLowerCase();
  return !/\b(no-store|private)\b/.test(cacheControl);
}

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(CACHE_NAME)
      .then((cache) => cache.addAll(PRECACHE_ASSETS))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(
          keys
            .filter((key) => key.startsWith("paperjam-shell-") && key !== CACHE_NAME)
            .map((key) => caches.delete(key)),
        ),
      )
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("message", (event) => {
  if (event.data && event.data.type === "SKIP_WAITING") {
    void self.skipWaiting();
  }
});

self.addEventListener("fetch", (event) => {
  const { request } = event;

  // Never touch non-GET requests (uploads, logins, edits, deletes).
  if (request.method !== "GET") {
    return;
  }

  const url = new URL(request.url);

  // Only handle same-origin HTTP/HTTPS requests.
  if (url.origin !== self.location.origin) {
    return;
  }

  // STRICT RULE: never cache API calls, shared documents, OAuth or calendar feeds.
  // Sensitive documents and state must only come from the live server.
  if (networkOnly(url.pathname)) {
    return;
  }

  // Navigation requests: network-first, falling back to the cached app shell. Only the shell
  // itself (`/`) is stored; other pages are never kept, so the offline shell is always the app.
  if (request.mode === "navigate") {
    event.respondWith(
      fetch(request)
        .then((networkResponse) => {
          if (url.pathname === "/" && !url.search && storable(networkResponse)) {
            const responseToCache = networkResponse.clone();
            void caches.open(CACHE_NAME).then((cache) => {
              void cache.put("/", responseToCache.clone());
              void cache.put("/index.html", responseToCache);
            });
          }
          return networkResponse;
        })
        .catch(async () => {
          const cache = await caches.open(CACHE_NAME);
          const cachedShell = (await cache.match("/index.html")) || (await cache.match("/"));
          if (cachedShell) {
            return cachedShell;
          }
          return new Response("Offline", {
            status: 503,
            statusText: "Service Unavailable",
            headers: { "Content-Type": "text/plain; charset=utf-8" },
          });
        }),
    );
    return;
  }

  // Static assets (hashed chunks in /assets/, fonts, icons, manifest):
  // Cache-first with network fallback and dynamic caching of newly requested assets.
  event.respondWith(
    caches.match(request).then((cachedResponse) => {
      if (cachedResponse) {
        return cachedResponse;
      }

      return fetch(request).then((networkResponse) => {
        if (storable(networkResponse)) {
          const responseToCache = networkResponse.clone();
          void caches.open(CACHE_NAME).then((cache) => {
            void cache.put(request, responseToCache);
          });
        }
        return networkResponse;
      });
    }),
  );
});
