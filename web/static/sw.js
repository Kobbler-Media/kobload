const CACHE_VERSION = 'v1';
const CACHE_NAME = `kobload-${CACHE_VERSION}`;

// ── Lifecycle ──

self.addEventListener('install', () => {
  self.skipWaiting();
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(
          keys
            .filter((k) => k.startsWith('kobload-') && k !== CACHE_NAME)
            .map((k) => caches.delete(k))
        )
      )
      .then(() => self.clients.claim())
  );
});

// ── Fetch strategy ──

self.addEventListener('fetch', (event) => {
  const url = new URL(event.request.url);

  // Only handle http(s) — skip chrome-extension:// and other unsupported schemes
  if (url.protocol !== 'http:' && url.protocol !== 'https:') return;

  // API calls and event streams: network only
  if (url.pathname.startsWith('/api/')) return;

  // SvelteKit immutable assets (content-hashed): cache-first
  if (url.pathname.startsWith('/_app/immutable/')) {
    event.respondWith(
      caches.match(event.request).then(
        (cached) =>
          cached ||
          fetch(event.request).then((resp) => {
            const clone = resp.clone();
            caches.open(CACHE_NAME).then((cache) => cache.put(event.request, clone));
            return resp;
          })
      )
    );
    return;
  }

  // Navigation & other static assets: network-first, cache fallback
  if (event.request.mode === 'navigate' || url.pathname.match(/\.(css|js|png|svg|ico|woff2?)$/)) {
    event.respondWith(
      fetch(event.request)
        .then((resp) => {
          const clone = resp.clone();
          caches.open(CACHE_NAME).then((cache) => cache.put(event.request, clone));
          return resp;
        })
        .catch(() => caches.match(event.request))
    );
    return;
  }

  // Google Fonts: cache-first after first load
  if (url.hostname === 'fonts.googleapis.com' || url.hostname === 'fonts.gstatic.com') {
    event.respondWith(
      caches.match(event.request).then(
        (cached) =>
          cached ||
          fetch(event.request).then((resp) => {
            const clone = resp.clone();
            caches.open(CACHE_NAME).then((cache) => cache.put(event.request, clone));
            return resp;
          })
      )
    );
    return;
  }
});
