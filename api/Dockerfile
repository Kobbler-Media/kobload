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

FROM base AS builder-usenet
# kob-usenet is touched so cargo always recompiles it: BuildKit can reuse an older koblibs
# layer whose mtimes predate an rlib the shared target cache built from newer sources, and
# cargo would then link that stale engine. Only src/ is touched, so rapidyenc's cmake build
# stays cached.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,id=kobload-target,target=/app/target \
    find /koblibs/kob-usenet/src -name '*.rs' -exec touch {} + && \
    cargo build --release -p kobload-usenet && \
    cp /app/target/release/kobload-usenet /app/kobload-usenet-binary

FROM kobload-tools:latest AS prod-usenet
LABEL org.opencontainers.image.source="https://github.com/Kobbler-Media/kobload"
WORKDIR /app
COPY --from=koblibs kob-usenet/src/vendor/LICENSE-infinidysk /usr/share/doc/kobload-usenet/LICENSE-infinidysk
COPY --from=koblibs kob-usenet/rapidyenc/README.md /usr/share/doc/kobload-usenet/LICENSE-rapidyenc-CC0.md
COPY --from=koblibs kob-usenet/rapidyenc/crcutil-1.0/LICENSE /usr/share/doc/kobload-usenet/LICENSE-crcutil-Apache-2.0
RUN cp /usr/share/doc/zlib1g/copyright /usr/share/doc/kobload-usenet/LICENSE-zlib
COPY --from=builder-usenet /app/kobload-usenet-binary /app/kobload-usenet
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
