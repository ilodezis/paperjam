/// <reference lib="webworker" />

const CACHE_NAME = "paperjam-shell-v2";

const PRECACHE_ASSETS = [
  "/",
  "/index.html",
  "/manifest.webmanifest",
  "/favicon.svg",
  "/icon.svg",
  "/icon-maskable.svg",
];

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

  // STRICT RULE: Never cache API calls or healthcheck endpoints.
  // Sensitive documents and state must only come from the live server.
  if (url.pathname.startsWith("/api/") || url.pathname === "/healthz") {
    return;
  }

  // Navigation requests: Network-first, fallback to cached offline app shell.
  if (request.mode === "navigate") {
    event.respondWith(
      fetch(request)
        .then((networkResponse) => {
          if (
            networkResponse.ok &&
            networkResponse.status === 200 &&
            networkResponse.type === "basic"
          ) {
            const responseToCache = networkResponse.clone();
            void caches.open(CACHE_NAME).then((cache) => {
              void cache.put(request, responseToCache);
              void cache.put("/index.html", responseToCache.clone());
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
        if (
          networkResponse.ok &&
          networkResponse.status === 200 &&
          networkResponse.type === "basic"
        ) {
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
