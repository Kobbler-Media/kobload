# Kobload web UI

SvelteKit admin dashboard, built as a static SPA (`adapter-static`) and served by nginx in the `web` image, which also proxies `/api/*` to the API.

- Install from the repo root with `make install` (needs the `../koblibs` sibling checkout for `@koblibs/ui`).
- `npm run dev` serves it with hot reload and proxies `/api` to `VITE_PROXY_TARGET` (default `http://localhost:7080`); the dev compose stack runs this for you.
- `make web` from the repo root runs format, lint, `svelte-check` and the production build; `svelte-check` must report zero warnings.
