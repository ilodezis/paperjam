// This origin used to serve the paperjam web app, which registered a service
// worker at /sw.js. Browsers that still have it fetch this file on their next
// update check: it clears the old app's caches and unregisters itself, so the
// landing page is served straight from the network from then on.
self.addEventListener("install", () => {
  self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.map((key) => caches.delete(key))))
      .then(() => self.registration.unregister()),
  );
});
