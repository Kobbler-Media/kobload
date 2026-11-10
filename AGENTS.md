# Kobload — engineering conventions

Generic download orchestration service: indexer search, Transmission/SABnzbd/debrid downloads, webhook and NATS notifications to whatever asked for the file.

Machine-specific notes — your paths, your deployment, your local tooling — belong in the gitignored `CLAUDE.local.md` / `AGENTS.local.md`, not here.

## Keep the core generic

Core logic uses generic types (`Release`, not `ProwlarrRelease`). Indexer-specific code converts into the generic types at the edge. `api/src/watch/types.rs::Release` is canonical.

## Build

- `make` — fmt, lint, check, build, tests. Required before calling anything done.
- `make quick` — no tests. Use during development.
- `make sqlx-prepare` — regenerate the `.sqlx/` offline cache after changing queries or migrations.

Warnings are errors. Clippy `collapsible_if`: use let-chains, not nested `if let`.

`make sqlx-check` and `make test` start a throwaway Postgres via Docker. A stale cache means `make sqlx-prepare`.

koblibs must be checked out as a sibling directory; `api/Cargo.toml` depends on its crates by relative path (`../../koblibs/*`) and `web/package.json` pulls `@koblibs/ui` from `file:../../koblibs`. `KOBLIBS_DIR` relocates it for Docker builds only.

## Patterns

- Tests in `api/tests/integration/`, one test binary: add a module and its `mod` line in `main.rs`, never a new top-level `api/tests/*.rs`. Filter with `cargo test --test integration <module>::`. Anything needing the process to itself (a custom global allocator) is a bench in `api/benches/`.
- Bug fixes are test-first — reproduce, then fix.
- INSERT/UPDATE/DELETE via `api/src/repositories/`.
- Background work via `api/src/jobs/handlers/`, not `tokio::spawn`. Exceptions: `polling.rs` and `watch/service.rs` are continuous pollers.
- HTTP via `crate::http::new_client()`.
- sqlx: prefer the `query!` / `query_as!` macros.
- New tables: update `api/src/backup.rs`.
- Navigation is duplicated in `Sidebar.svelte` and `MobileMenu.svelte` — change both.
- Frontend: `svelte-check` must report zero warnings.
- Comments explain non-obvious *why*. No docstrings restating what the code does.

## Debrid streaming (Real-Debrid)

RD removed `/torrents/instantAvailability` — it returns error_code 37 ("Disabled endpoint"). There is **no** bulk cached-check API. Availability comes from two places, in trust order:

1. **Our own probes** — cache warming adds a magnet, reads the status, and (for everything it won't stream) deletes it again. Outcomes are persisted to `debrid_availability_cache`. Warming submissions **must** be deleted; each holds an active-torrent slot, and a full pool makes every later add fail.
2. **Torrentio markers** — with an RD key in the URL, `name` carries `[RD+]` (cached) / `[RD download]` / `[RD+ Download]` (torrent cached, file isn't). Check "download" **before** the `+` suffix or the third form reads as cached. This is Torrentio's own shared store (14-day TTL, any user's unrestrict), not a check against our account — a hint, never a promise.

In debrid mode Torrentio **drops `infoHash`/`fileIdx`** (recover both from the `/resolve/...` URL) — but it also returns far more content: the keyless endpoint is hard-capped at 50 streams vs 108 for the same movie, and the extras include already-cached 4K. So the debrid response is the primary source and the keyless one is merged in behind it as a fallback; both are fetched concurrently.

Match RD errors on `error_code`, never the `error` string (not reliably a slug): 33 = no free slot (transient), 35 = infringing — usually **not about the release**: after about four adds in three minutes RD pauses the account's adds for 21 s to ~5 min and answers 35 to every add, known-good hashes included. Only names carrying a few tags (`debrid/rd_names.rs`) are refused every time. So the throttle holds the account's adds and retries once before a 35 counts, warming only adds on a quiet account, and a refusal demotes, never hides (copies already on the account keep serving, and the account is checked before adding), 36 = fair usage, 37 = disabled endpoint. Rate limit: 250 req/min.

RD does **not** dedup: re-adding a present hash often creates a duplicate entry rather than erroring, so hash-level dedup has to live here.

## Ecosystem

Kobload sits alongside [kobbler](https://github.com/Kobbler-Media/kobbler) (media server) and [koblibs](https://github.com/Kobbler-Media/koblibs) (shared crates and UI components).
