# Kobload

Kobload is a self-hosted download orchestration service: it searches torrent and usenet indexers, hands releases to a download client, parses the results into structured media metadata, and notifies whatever asked for them. It is the acquisition half of [Kobbler](https://github.com/Kobbler-Media/kobbler), but it has no Kobbler-specific code and can be driven by anything that speaks its HTTP API.

Licensed under [AGPL-3.0](LICENSE). Contributions welcome — see [CONTRIBUTING.md](CONTRIBUTING.md).

## Lawful use

Kobload is a general-purpose indexer/download orchestrator, the same category as
Sonarr, Radarr or Prowlarr. It ships with no indexer credentials, no bundled
content, and no index of copyrighted material. What you point it at, and what you
do with what it retrieves, is your responsibility under the laws that apply to you.

## What it does

- **Pluggable indexers** — Prowlarr, direct Newznab/Torznab, MyAnonaMouse, TorrentDay, and Cardigann YAML definitions (auto-downloaded from Prowlarr's definition repo)
- **Pluggable download clients** — Transmission (torrents), SABnzbd (usenet), and debrid providers (Real-Debrid, AllDebrid, Premiumize, TorBox)
- **Release interpretation** — parses release names and file paths into structured metadata for TV, movies, books, audiobooks, music, games, software and sports
- **Quality profiles** — resolution, codec, source, audio and language preferences with scoring and upgrade rules
- **Watch patterns** — standing searches that fire when a matching release appears
- **Jobs and events** — async work over NATS, real-time SSE streams, and HTTP webhook callbacks
- **Web UI** — SvelteKit admin dashboard for configuration and monitoring

## Prerequisites

- Docker with Compose v2 and BuildKit
- [koblibs](https://github.com/Kobbler-Media/koblibs) cloned as a **sibling directory** of this repo. It is a shared Rust + Svelte workspace that Kobload depends on and it is not published to crates.io or npm yet, so the checkout is required:

  ```bash
  git clone https://github.com/Kobbler-Media/kobload.git
  git clone https://github.com/Kobbler-Media/koblibs.git
  # you should end up with ./kobload and ./koblibs next to each other
  ```

  If you keep koblibs somewhere else, set `KOBLIBS_DIR` in `.env`.

- For building outside Docker: a recent stable Rust toolchain and Node.js 24+.

## Quickstart

```bash
cd kobload
cp .env.example .env        # edit it — at minimum set ADMIN_PASSWORD and NATS_TOKEN
make up                     # builds the kobload-tools base image first if missing, then starts everything
```

`make up` is `make tools` (builds the `kobload-tools` base image — FFmpeg, chromaprint, archive tools) then `docker compose up -d --build`. Doing that second step alone on a fresh clone fails with a "pull access denied" error that looks like a registry problem — it's really just the missing base image; run `make tools` (or `make up`) first.

Then open the web UI at <http://localhost:5174>. If you left `ADMIN_PASSWORD` blank, the generated one is in the logs:

```bash
docker compose logs api | grep -i password
```

To stop: `docker compose down` (add `-v` to also drop the database).

Postgres only reads `POSTGRES_PASSWORD` when it first initializes an empty
database — changing it in `.env` after the first `up` leaves the old password
in the `db` volume, and `api` crash-loops with `password authentication
failed`. `docker compose down -v` to wipe and reinit if that happens (deletes
your data).

### Development stack

Hot reload for both the API (cargo watch) and the web UI (vite):

```bash
docker compose -f ../koblibs/docker-compose.nats.yml up -d   # shared NATS + kob_dev network
cp env.dev.template .env.dev                                 # optional, fixes the dev admin password
docker compose -f docker-compose.dev.yml up -d --build
```

`./scripts/run_dev.sh` wraps all of the above and prints the admin credentials when it is done.

| Service | URL |
|---|---|
| Web UI | http://localhost:5174 |
| API | http://localhost:7080 |

## How it relates to Kobbler

Kobbler is the media server; Kobload is the thing that goes and fetches what Kobbler is missing. The two talk in three ways:

1. **Kobbler → Kobload over HTTP.** Kobbler calls this service at `KOBLOAD_PUBLIC_URL` and authenticates with `KOBLOAD_API_KEY`. Set the same key on both sides. `KOBLOAD_PUBLIC_URL` must be reachable from Kobbler, because Kobload also uses it to build the file-download links it hands back.
2. **Kobload → Kobbler over HTTP.** Callers supply a `callback_url` per download or want request; Kobload POSTs completion notifications there. Nothing about Kobbler's address is configured in Kobload.
3. **Both over NATS.** Watch matches, ingest-ready events and ingest results travel over a shared NATS instance. In a combined deployment, run one NATS (see `../koblibs/docker-compose.nats.yml`), comment out the bundled `nats` service here, and point `NATS_URL` at the shared one.

Kobload also needs **filesystem access to completed downloads** so it can stream files onward — download clients do not expose a download API. Point `KOBLOAD_DOWNLOADS` at the directory your torrent/usenet client writes to; it is mounted at `/downloads` in the API container.

Neither side is required: Kobload runs standalone and its API is generic.

## Configuration

Copy `.env.example` to `.env`. It documents every variable the compose files consume; the ones you will actually care about:

| Variable | Default | Description |
|---|---|---|
| `KOBLIBS_DIR` | `../koblibs` | Path to the koblibs sibling checkout |
| `KOBLOAD_DOWNLOADS` | `./data/downloads` | Host directory with your download client's completed files |
| `KOBLOAD_API_KEY` | *(none)* | Shared secret callers present to this API |
| `KOBLOAD_PUBLIC_URL` | `http://localhost:7080` | Public base URL of this service |
| `POSTGRES_PASSWORD` | `kobload` | Database password |
| `NATS_TOKEN` | `changeme` | Auth token for the bundled NATS |
| `ADMIN_EMAIL` / `ADMIN_PASSWORD` | `admin@localhost` / *(generated)* | First-boot admin account |
| `PROWLARR_BASE_URL` / `PROWLARR_API_KEY` | *(none)* | Optional Prowlarr aggregator |
| `KOBLOAD_PORT` / `KOBLOAD_WEB_PORT` | `7080` / `5174` | Host ports |

Indexers, download clients and quality profiles are configured in the web UI under Settings, not through environment variables.

## API overview

Everything is under `/api/v1`. Authenticate with an API key (`X-API-Key`) or an admin session.

**Downloads** — `POST /downloads`, `GET /downloads`, `GET /downloads/{id}`, `DELETE /downloads/{id}`, `GET /downloads/{id}/file`, `GET /downloads/{id}/files/{file_id}`, `GET /downloads/stream` (SSE)

**Search and want** — `POST /search`, `POST /want` (search now + create a watch pattern), `POST /want/bulk`

**Watch patterns** — `GET/POST /watch`, `PUT /watch/{id}`, `DELETE /watch/{source_system}/{source_id}`

**Interpretation** — `POST /interpret`, `POST /interpret/stream`

**Indexers and profiles** — `GET/POST /indexer-sources`, `GET/POST/DELETE /indexer-category-mappings/{source_id}`, `GET /quality-profiles`

**Admin** — `/admin/download-clients`, `/admin/indexer-handlers`, `/admin/test-indexer`, `/admin/webhooks`, `/admin/users`, `/admin/api-keys`, `/admin/definitions`, `/admin/quality-profiles`

The full Kobbler integration flow is written up in [docs/kobbler-usage.md](docs/kobbler-usage.md).

## Running behind a reverse proxy

The web container's nginx proxies `/api/*` to the API container, and the frontend uses relative API paths, so no path rewrites are needed:

```
Browser → your reverse proxy (443) → kobload-web:80 → / (SPA) and /api/* → kobload-api:7080
```

Point the proxy at `kobload-web:80` (host port `5174` by default) and set `KOBLOAD_PUBLIC_URL` to the public URL so callbacks and file links are generated correctly.

## Project layout

| Path | What |
|---|---|
| `api/` | Rust API server (axum, sqlx, PostgreSQL) |
| `api/migrations/` | SQL migrations |
| `cardigann/` | Cardigann YAML indexer definition engine |
| `web/` | SvelteKit admin UI |
| `docker/` | Base tooling image, bitmagnet classifier, DB init |
| `scripts/` | Dev helper scripts |
| `docker-compose.yml` | Example production-shaped stack |
| `docker-compose.dev.yml` | Hot-reload dev stack |
| `docker-compose.test.yml` | Throwaway test database |

## Building and testing

```bash
make        # fmt, lint, check, build, tests — the full gate
make quick  # same without tests
make web    # frontend only
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for details, including the test database and the sqlx offline cache.
