# Contributing to Kobload

Pull requests are welcome — bug fixes, indexer handlers, download clients, parsing improvements, docs, all of it. Nothing here is a gate you need permission to pass; open an issue first only if you want to check a design direction before writing code.

## Licence

Kobload is licensed under the **GNU Affero General Public License v3.0** ([LICENSE](LICENSE)). By submitting a contribution you agree that it is licensed under AGPL-3.0 on the same terms, and that you have the right to submit it.

## Getting set up

You need Docker (Compose v2, BuildKit), a recent stable Rust toolchain, and Node.js 24+.

```bash
git clone https://github.com/Kobbler-Media/kobload.git
git clone https://github.com/Kobbler-Media/koblibs.git   # sibling checkout, required
cd kobload
cp .env.example .env
make tools        # one-off: builds the kobload-tools base image
npm --prefix web ci
```

`koblibs` is a shared Rust + Svelte workspace that is not published to crates.io or npm yet. `api/Cargo.toml` depends on its crates by relative path (`../../koblibs/*`) and `web/package.json` references it as `file:../../koblibs`, so the sibling checkout is mandatory for local builds. `KOBLIBS_DIR` in `.env` relocates it for Docker builds only.

## Build and test

```bash
make        # fmt, lint, check, build, test — run this before opening a PR
make quick  # everything except tests, for the inner loop
make web    # frontend only: format, lint, svelte-check, build
```

`make` must be clean. Warnings are errors (`RUSTFLAGS="-D warnings"`, `clippy -D warnings`), and `svelte-check` must report zero warnings.

Individual targets:

| Target | What it does |
|---|---|
| `make test` | Starts a throwaway Postgres via `docker-compose.test.yml`, runs the Rust test suite, tears it down |
| `make sqlx-check` | Verifies the committed `.sqlx/` offline query cache matches the code |
| `make sqlx-prepare` | Regenerates `.sqlx/` — run after changing any SQL or migration |
| `make tools` | Rebuilds the `kobload-tools` base image (FFmpeg, chromaprint, unrar/7z) |
| `make docker-push` | Maintainers only; needs push rights on the release registry |

All Rust targets run with `SQLX_OFFLINE=true`, so you can build without a database. `make test`, `make sqlx-check` and `make sqlx-prepare` need Docker because they bring up a real Postgres.

## Bug fixes are test-first

If you are fixing a bug or changing behaviour, **write a failing test first**:

1. Add an integration test in `api/tests/integration/` (a module plus its `mod` line in `main.rs`) that reproduces the bug. Run it and watch it fail.
2. Fix the bug.
3. Run it again and watch it pass.

Prefer integration tests over unit tests, and cover every entry point into the broken logic — if the same code path is reachable from a job handler and an HTTP route, test both. A PR that fixes a bug without a test that would have caught it will be asked for one.

## Code conventions

- **Keep the core generic.** Domain types are `Release`, not `ProwlarrRelease`. Indexer- and client-specific code converts into the generic types at the edge; `api/src/watch/types.rs::Release` is canonical.
- **Writes go through repositories.** All INSERT/UPDATE/DELETE lives in `api/src/repositories/`.
- **Background work goes through jobs.** Add handlers in `api/src/jobs/handlers/` rather than calling `tokio::spawn`. The continuous pollers in `polling.rs` and `watch/service.rs` are the deliberate exceptions.
- **HTTP clients** come from `crate::http::new_client()` so timeouts and TLS settings stay consistent.
- **SQL** uses the compile-time-checked `sqlx::query!` / `query_as!` macros where possible. Run `make sqlx-prepare` after any change and commit the `.sqlx/` diff.
- **New tables** need a matching update in `api/src/backup.rs`.
- **Navigation** is duplicated between `web/src/lib/components/Sidebar.svelte` and `MobileMenu.svelte` — change both.
- **Comments explain why, not what.** The codebase is deliberately light on comments; if code needs a comment to be readable, prefer renaming or refactoring it.
- **No compatibility shims or commented-out code.** Delete what you replace; version control is the history.

## Pull requests

- One logical change per PR, with a description of what broke and why the fix is the right shape.
- `make` clean, tests included for behaviour changes.
- Migrations are append-only once released: add a new file in `api/migrations/` rather than editing an existing one.
- Don't commit `.env`, credentials, or anything from `data/`.

## Machine-specific notes

Anything about *your* machine, your deployment, your paths or your agent tooling belongs in `CLAUDE.local.md` / `AGENTS.local.md`, which are gitignored. Keep `CLAUDE.md` / `AGENTS.md` to conventions that apply to everyone.
