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
