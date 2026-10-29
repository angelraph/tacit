// Tacit's service worker: makes the app installable and lets it open
// without a connection. Pages and code are fetched fresh when online, so a
// deploy is never shadowed by an old copy; the cache is only a fallback.
// Coordinator, chain and price calls are never cached.

const CACHE = "tacit-v2";
const SHELL = ["./", "./index.html", "./styles.css", "./extra.css", "./config.js", "./logo.svg", "./manifest.webmanifest"];

self.addEventListener("install", (e) => {
  e.waitUntil(caches.open(CACHE).then((c) => c.addAll(SHELL)).then(() => self.skipWaiting()));
});

self.addEventListener("activate", (e) => {
  e.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (e) => {
  const req = e.request;
  const url = new URL(req.url);
  if (req.method !== "GET" || url.origin !== self.location.origin || url.pathname.startsWith("/v1/")) return;
  // The server sends no cache headers, so the browser would happily reuse a
  // stale module after a deploy. "no-cache" asks the server every time; an
  // unchanged file costs one 304. A navigation can't take a RequestInit, so it
  // is refetched by URL.
  const fresh = req.mode === "navigate" ? fetch(req.url, { cache: "no-cache", credentials: "same-origin" }) : fetch(req, { cache: "no-cache" });
  e.respondWith(
    fresh
      .then((res) => {
        if (res.ok && res.type === "basic") {
          const copy = res.clone();
          caches.open(CACHE).then((c) => c.put(req, copy));
        }
        return res;
      })
      .catch(() =>
        caches.match(req).then((hit) => hit || (req.mode === "navigate" ? caches.match("./index.html") : Response.error())),
      ),
  );
});
