FROM kobload-tools:latest AS base

# koblibs is a sibling checkout referenced by path in Cargo.toml; compose passes
# it as the `koblibs` build context. It must land at /koblibs so that
# `../../koblibs/...` resolves from /app/api.
COPY --from=koblibs . /koblibs

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY .sqlx ./.sqlx
COPY api ./api
COPY cardigann ./cardigann
COPY usenet ./usenet
COPY usenet-proto ./usenet-proto

FROM base AS dev
ENV SQLX_OFFLINE=true
RUN cargo fetch
ENV CARGO_TARGET_DIR=/target
WORKDIR /app/api
CMD ["cargo", "watch", "-x", "run"]

FROM base AS builder
ENV SQLX_OFFLINE=true
# Cache mounts persist across builds (BuildKit). Target cache is ephemeral to
# this RUN, so copy binaries out before the step ends.
# The target mount's id keeps it apart from kobbler's, which mounts the same path with a
# different toolchain and FFmpeg, so a shared dir would rebuild for whichever ran last.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,id=kobload-target,target=/app/target \
    cargo build --release -p kobload-api && \
    cp /app/target/release/kobload-api /app/kobload-api-binary && \
    cp /app/target/release/backfill-hash-index-se /app/backfill-hash-index-se-binary

# The engine image is built from its own crates only, so an api or cardigann change leaves
# every layer below cached and a deploy does not recreate the running engine.
FROM kobload-tools:latest AS usenet-src
COPY --from=koblibs kob-usenet /koblibs/kob-usenet
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY usenet ./usenet
COPY usenet-proto ./usenet-proto
RUN sed -i 's/^members = .*/members = ["usenet", "usenet-proto"]/' Cargo.toml

FROM usenet-src AS builder-usenet
# kob-usenet is touched so cargo always recompiles it: BuildKit can reuse an older koblibs
# layer whose mtimes predate an rlib the shared target cache built from newer sources, and
# cargo would then link that stale engine. Only src/ is touched, so rapidyenc's cmake build
# stays cached. The binary's mtime is pinned so a rebuild of the same source gives the same
# runtime layer.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,id=kobload-usenet-target,target=/app/target \
    find /koblibs/kob-usenet/src -name '*.rs' -exec touch {} + && \
    cargo build --release -p kobload-usenet && \
    cp /app/target/release/kobload-usenet /app/kobload-usenet-binary && \
    touch -d @0 /app/kobload-usenet-binary

# Pinned by digest so the same source gives the same image; bump it for security updates.
# unrar is RARLAB's (non-free): unrar-free cannot open many RAR5 sets. It runs as a separate
# binary, so its licence does not reach the AGPL code.
FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2 AS prod-usenet
LABEL org.opencontainers.image.source="https://github.com/Kobbler-Media/kobload"
RUN sed -i 's/^Components: main$/Components: main non-free/' /etc/apt/sources.list.d/debian.sources \
    && apt-get update \
    && apt-get install -y --no-install-recommends 7zip ca-certificates curl libstdc++6 unrar \
    && rm -rf /var/lib/apt/lists/* /var/cache/apt/* /var/log/apt /var/log/dpkg.log \
    && groupadd --system --gid 10001 kobload \
    && useradd --system --uid 10001 --gid kobload --home-dir /data --no-create-home kobload \
    && install -d -o kobload -g kobload -m 0750 /data /downloads
COPY --link --from=koblibs kob-usenet/src/vendor/LICENSE-infinidysk /usr/share/doc/kobload-usenet/LICENSE-infinidysk
COPY --link --from=koblibs kob-usenet/rapidyenc/README.md /usr/share/doc/kobload-usenet/LICENSE-rapidyenc-CC0.md
COPY --link --from=koblibs kob-usenet/rapidyenc/crcutil-1.0/LICENSE /usr/share/doc/kobload-usenet/LICENSE-crcutil-Apache-2.0
COPY --link --from=koblibs kob-usenet/LICENSE-zlib /usr/share/doc/kobload-usenet/LICENSE-zlib
COPY --link --from=builder-usenet --chmod=0755 /app/kobload-usenet-binary /app/kobload-usenet
WORKDIR /app
USER kobload
ENV USENET_DATA_DIR=/data
EXPOSE 7090
CMD ["/app/kobload-usenet"]

FROM debian:trixie-slim AS definitions
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*
RUN curl -sL https://github.com/Prowlarr/Indexers/archive/refs/heads/master.tar.gz \
    | tar xz --strip-components=3 -C /tmp 'Indexers-master/definitions/v11/' \
    && mkdir -p /definitions \
    && find /tmp -name '*.yml' -exec cp {} /definitions/ \;

FROM kobload-tools:latest AS prod
LABEL org.opencontainers.image.source="https://github.com/Kobbler-Media/kobload"
WORKDIR /app
COPY --from=builder /app/kobload-api-binary /app/kobload-api
# Bundle one-off ops tools next to the main binary so they're runnable in prod
# via `docker compose exec kobload /app/<tool>` without needing a dev image.
COPY --from=builder /app/backfill-hash-index-se-binary /app/backfill-hash-index-se
COPY --from=definitions /definitions /app/definitions
ENV CARDIGANN_DEFINITIONS_DIR=/app/definitions
CMD ["/app/kobload-api"]
