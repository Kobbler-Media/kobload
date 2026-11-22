# Built-in usenet engine

Kobload ships an optional usenet engine, `kobload-usenet`, that talks NNTP to your providers itself. It does two jobs over one connection pool:

- **Streaming.** A usenet search result plays straight away over HTTP Range, the same way a cached debrid link does. Only the articles the player asks for are fetched, so one episode of a season pack costs that episode's bytes.
- **Downloads.** It downloads NZBs to disk for the library, with PAR2 verify and repair and unpacking. It can replace SABnzbd as the usenet download client.

It runs as its own container next to the API. Deploying the API does not cut streams, and the engine's CPU (yEnc decoding, PAR2, unpacking) stays within its own container limits.

## Setup

### 1. Secrets

The API and the engine share three secrets. Each must be at least 32 bytes long, and all three must differ, otherwise both services refuse to start. Generate each one with `openssl rand -hex 32`.

| Variable | Used for |
|---|---|
| `USENET_CONTROL_KEY` | Bearer key the API uses to drive the engine (imports, jobs), and the engine uses to pull its server list from the API |
| `USENET_STREAM_SECRET` | HMAC key for the signed stream URLs the API hands to Kobbler; the engine checks every stream request against it |
| `USENET_METRICS_KEY` | Bearer key for the engine's `/metrics`. Without it `/metrics` stays closed |

### 2. Compose profile

The engine is the `usenet` service in `docker-compose.yml`, behind the `usenet` profile. In `.env`:

```bash
COMPOSE_PROFILES=usenet
USENET_CONTROL_KEY=<openssl rand -hex 32>
USENET_STREAM_SECRET=<openssl rand -hex 32>
USENET_METRICS_KEY=<openssl rand -hex 32>
# Where the API reaches the engine (the compose service name works here).
USENET_ENGINE_URL=http://usenet:7090
# Where Kobbler reaches the engine. Signed stream URLs start with it, so it must be an
# address the Kobbler host can open, normally this host's LAN IP.
USENET_STREAM_BASE_URL=http://192.168.1.20:7090
# Publish the engine's port on that LAN address only.
USENET_BIND=192.168.1.20
```

Then `docker compose up -d`. The API turns usenet streaming on only when `USENET_ENGINE_URL`, `USENET_STREAM_BASE_URL`, `USENET_CONTROL_KEY` and `USENET_STREAM_SECRET` are all set. With only some of them set it refuses to start, so a typo can't leave it running half-configured.

The engine reports healthy (`GET /healthz`) once it has a server list. It pulls that list from the API every minute and keeps the last one on disk, so a restart serves streams at once even while the API is down.

### 3. Exposure and firewall

The stream port (7090) serves anyone who holds a valid signed URL. URLs expire after six hours and name one file of one release, but treat the port as internal anyway:

- Publish it on a LAN address only (`USENET_BIND`). It defaults to `127.0.0.1`. Never publish it on `0.0.0.0` on a host that faces the internet.
- If Kobbler runs on another host, allow that one host through your firewall to the port, for example `ufw allow from 192.168.1.30 to any port 7090 proto tcp`.
- There is no reason to route the port through a reverse proxy or expose it publicly. Kobbler fetches the bytes and serves them to players itself.

### 4. Downloads mount and user

Finished downloads land in `<USENET_DOWNLOAD_DIR>/<category>`, where the category is the one set on the usenet download client (`kobload` by default). The API imports a finished download by the absolute path the engine reports. **The engine and the API must therefore mount the downloads directory at the same path.** The base compose file mounts `KOBLOAD_DOWNLOADS` at `/downloads` in both containers, and `USENET_DOWNLOAD_DIR` defaults to `/downloads`. If you mount your downloads elsewhere (for example `/mnt/download` in both containers), set `USENET_DOWNLOAD_DIR` to a directory under that path, such as `/mnt/download/nzb/complete`.

Unfinished downloads are staged in `.incomplete` inside the category folder, so the final move is a rename on the same filesystem.

The engine image runs as uid/gid `10001`. The downloads directory must be writable by the user the engine runs as, and the finished files must be readable by the API:

- either set `USENET_UID` / `USENET_GID` to the owner of the downloads directory (for example the account SABnzbd writes as),
- or `chown -R 10001:10001` a dedicated directory and point `USENET_DOWNLOAD_DIR` at it.

When you change the uid, also chown the engine's `usenet_data` volume (NZB cache, release layouts, job state) to match.

| Variable | Default | Description |
|---|---|---|
| `USENET_DOWNLOAD_DIR` | `/downloads` | Absolute path inside the container where finished downloads go, one folder per category |
| `USENET_FREE_SPACE_FLOOR_GB` | `1` | Downloads pause while the download filesystem has less free space than this, and resume on their own |
| `USENET_ACTIVE_JOBS` | `2` | Downloads fetched at once; the rest wait in the queue |
| `USENET_UID` / `USENET_GID` | `10001` | User the engine runs as |
| `USENET_MIN_CONNECTIONS` | `4` | NNTP connections kept open and logged in, so a stream's first read doesn't wait on a fresh login |
| `USENET_PORT` | `7090` | Host port the engine is published on |
| `USENET_BIND` | `127.0.0.1` | Host address the engine is published on |
| `USENET_KOBLOAD_URL` | `http://api:7080` | Where the engine reaches the API to pull its servers |
| `USENET_RUST_LOG` | `info` | Engine log filter |

The compose service also sets `net.ipv4.tcp_slow_start_after_idle=0` inside the engine's network namespace. Without it, every idle pooled connection restarts TCP slow start, and on a high-latency provider a stream's first article takes several round trips longer.

### 5. Servers

Add your provider accounts under **Settings → Usenet Servers**. The engine picks up changes within a minute. **Test** logs in over NNTP the way the engine does.

## Server tiers and backbones

Each server has a **priority tier**. Lower tiers are tried first.

- Within a tier, requests go to the least busy server.
- A higher tier (a backup or block account) is asked for an article **only when every lower tier has answered that it is missing it** (NNTP 430), or when no lower tier's retention covers the post. A busy, slow or unreachable primary never sends traffic to a backup, so block accounts are spent only on articles the primaries really lack.
- Servers in the same **backbone group** share their memory of missing articles. Once an article is missing on one server of a backbone, the others on that backbone aren't asked for it.
- **Retention (days)**: a server is skipped for posts older than its retention.
- **Account expiry**: a server stops being used after this date.
- **Block quota (GB)**: the engine stops using a block account once it has downloaded this much.

A typical setup is one or two unlimited accounts on different backbones in tier 0, and a block account in tier 99.

## Connection caps

Providers cap how many connections one login may hold, and some ban accounts that go over. For each server:

- **Connections** is how many connections the engine opens to it.
- **Account connection cap** is the provider's limit for the login, summed over every server row with the same host and username. Kobload refuses to save a server that would take an account over its cap, and logs a warning at startup if one already does.

The engine shares its pool between streaming and downloads, so you don't split connections between them. Playback always comes first: while something is playing, downloads leave a few connections free and pipeline less deeply. After playback come the stream's cache-ahead reads, then library downloads, then prefetching and health checks.

**Running SABnzbd beside the engine.** SABnzbd keeps its own connections to the same accounts. While both run, the engine's connections plus SABnzbd's must stay within each account's cap. If a SABnzbd download client is configured in Kobload, its servers are read from SABnzbd and counted against the caps of the accounts they share with the engine. Lower SABnzbd's connection counts (or the engine's) until the sum fits. Once SABnzbd is idle or stopped it holds no connections, and the engine can take the full count.

## Streaming vs downloads

| | Streaming | Downloads |
|---|---|---|
| Enabled by | a debrid provider of type **Usenet** (**Settings → Debrid**; it needs no API key) | a download client of type **Usenet (built-in engine)** |
| What is fetched | only the byte ranges the player reads, plus Kobbler's cache-ahead | every article of the NZB |
| Damage | missing or corrupt articles end the stream with an error and are never served as zeros. The release is marked dead for later searches | repaired from PAR2 where the recovery blocks suffice; otherwise the job fails |
| Archives | loose files and stored (uncompressed) RAR/7z sets stream directly | compressed, solid or encrypted archives are unpacked with `unrar` / `7z` |
| Passwords | from the NZB's `password` meta; a protected archive without one is refused | from the NZB's `password` meta or the indexer; a protected archive without one fails the job |

**Streaming.** On a tap, Kobload fetches the NZB from the indexer, but only when the engine doesn't already have it. The engine reads just the first article of each file to learn the layout, and the API returns signed URLs for the chosen file: one for the player, one for Kobbler's background cache-ahead. In the background Kobload then STATs a 5% sample of the release's articles. If the sample finds articles missing and the post has no PAR2 files, or more are missing than PAR2 would usually repair, the release is ranked down the same way as a failed playback. Prewarming fetches an NZB ahead of a tap only for the result a search would auto-play, so ranking walks don't spend your indexer's grab limit.

**Downloads.** Before fetching the bulk of a job, the engine STATs a sample of its articles and fails the job early when the damage is beyond what the post's PAR2 blocks can repair. A failure caused by the release itself (missing articles, a bad archive, a wrong password) fails the download, and Kobload searches for another release. A failure caused by infrastructure (providers unreachable, a full disk) is retried with backoff and never counts against the release.

## Making the engine the default usenet client

1. Add a download client of type **Usenet (built-in engine)** under **Settings → Clients**. It needs only a category. **Test** checks that the engine answers.
2. To try it on a few grabs first, leave your existing usenet client as the default and pick the engine per grab.
3. Mark the engine client as **default**. Every later usenet grab goes to it.
4. To retire SABnzbd, disable its download client once its last jobs have finished. A disabled client takes no new grabs, but the jobs it already has are still polled, imported and cleaned up. You can leave SABnzbd itself installed as a fallback. Switching back is just re-enabling it and making it the default again.

## Monitoring

`GET /metrics` (with `Authorization: Bearer $USENET_METRICS_KEY`) serves Prometheus text:

- `kobload_usenet_jobs{state}`: download jobs by state. `kobload_usenet_paused_for_space` is 1 while downloads wait for free space.
- `kobload_usenet_body_attempts_total{provider,workload,outcome}` and `kobload_usenet_body_bytes_total{provider,workload}`: article fetches per provider, split by workload (`stream`, `cache`, `queue`, `warm`, `health`).
- `kobload_usenet_provider_connections`, `kobload_usenet_provider_busy`, `kobload_usenet_connect_failures_total`, `kobload_usenet_breaker_trips_total`: per-provider pool state.
- `kobload_usenet_stream_failures_total{code}`: stream reads refused or cut, by reason (`missing_articles`, `corrupt_articles`, `timeout`, `providers_unavailable`).

Usenet stream attempts also show in the web UI's **Debrid Log**.
