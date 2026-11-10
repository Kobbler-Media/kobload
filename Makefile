# Ports and connection strings used by the test/sqlx targets. Override on the
# command line or in your environment, e.g. `make test KOBLOAD_TEST_DB_PORT=15434`.
KOBLOAD_TEST_DB_PORT ?= 5434
TEST_DATABASE_URL ?= postgres://kobload:kobload@localhost:$(KOBLOAD_TEST_DB_PORT)/kobload_test
# Exported so docker-compose.test.yml sees the same port.
export KOBLOAD_TEST_DB_PORT

.PHONY: all quick web install up fmt fmt-api fmt-web lint lint-api lint-web check check-api check-web sqlx-check sqlx-prepare sqlx-migrate build build-api build-web test test-api test-db-up test-db-down docker-push tools sweep

# sqlx-cli is needed only by the sqlx-* targets. Fail with instructions rather
# than cargo's opaque "no such subcommand".
define require_sqlx_cli
	@cargo sqlx --version >/dev/null 2>&1 || { \
		echo "sqlx-cli not installed. Run:"; \
		echo "  cargo install sqlx-cli --no-default-features --features postgres,rustls"; \
		exit 1; }
endef

all: sweep fmt lint check build test

# Quick build check - skips slow backend tests
quick: sweep fmt lint check build

# Web-only checks - for frontend-only changes
web: fmt-web lint-web check-web build-web

fmt: fmt-api fmt-web
lint: lint-api lint-web
check: check-api check-web sqlx-check
build: build-api build-web

fmt-api:
	cargo fmt --manifest-path api/Cargo.toml

lint-api:
	SQLX_OFFLINE=true cargo clippy --manifest-path api/Cargo.toml --all-targets --all-features -- -D warnings

check-api:
	SQLX_OFFLINE=true cargo check --manifest-path api/Cargo.toml

# Verify .sqlx cache matches code. If stale, run: make sqlx-prepare
# `-- --all-targets` so test-only query macros (api/tests/) are included; without
# it, `lint-api` (clippy --all-targets) surfaces them as SQLX_OFFLINE cache misses.
# `--all` so koblibs' nats-jobs (a dep, not a workspace member) is force-recompiled and
# re-emits its job_status queries. Without it those nine entries appear or vanish
# depending only on whether cargo happened to rebuild the dep.
sqlx-check: test-db-up sqlx-migrate
	$(require_sqlx_cli)
	DATABASE_URL=$(TEST_DATABASE_URL) cargo sqlx prepare --workspace --all --check -- --all-targets

# Regenerate .sqlx cache from current code + DB schema
sqlx-prepare: test-db-up sqlx-migrate
	$(require_sqlx_cli)
	DATABASE_URL=$(TEST_DATABASE_URL) cargo sqlx prepare --workspace --all -- --all-targets

# Apply migrations to the test database
sqlx-migrate: test-db-up
	$(require_sqlx_cli)
	DATABASE_URL=$(TEST_DATABASE_URL) cargo sqlx migrate run --source api/migrations

build-api:
	SQLX_OFFLINE=true RUSTFLAGS="-D warnings" cargo build --manifest-path api/Cargo.toml

# Install web dependencies. Needs the koblibs sibling checkout — web/package.json
# pulls @koblibs/ui from file:../../koblibs.
install:
	npm --prefix web ci

# The web targets no-op with a notice on a fresh clone so `make` still works
# before `make install`. Real npm failures still fail the build.
fmt-web:
	@if [ -d web/node_modules ]; then npm --prefix web run format; else echo "web: skipping format (run 'make install')"; fi

lint-web:
	@if [ -d web/node_modules ]; then npm --prefix web run lint; else echo "web: skipping lint (run 'make install')"; fi

check-web:
	@if [ -d web/node_modules ]; then npm --prefix web run check; else echo "web: skipping check (run 'make install')"; fi

build-web:
	@if [ -d web/node_modules ]; then npm --prefix web run build; else echo "web: skipping build (run 'make install')"; fi

test: test-api

test-db-up:
	docker compose -f docker-compose.test.yml up -d --wait

test-db-down:
	docker compose -f docker-compose.test.yml down

test-api: test-db-up
	@DATABASE_URL=$(TEST_DATABASE_URL) \
	CARGO_INCREMENTAL=0 \
	cargo test --manifest-path api/Cargo.toml -- --test-threads=4; \
	EXIT_CODE=$$?; \
	$(MAKE) test-db-down; \
	exit $$EXIT_CODE

# Publish release images. Maintainers only — needs push rights on the registry.
# Same variable docker-compose.yml uses, so a fork can set KOBLOAD_REGISTRY once
# in .env and it applies to both.
KOBLOAD_REGISTRY ?= ghcr.io/kobbler-media
ifdef REGISTRY
export KOBLOAD_REGISTRY := $(REGISTRY)
endif
REGISTRY ?= $(KOBLOAD_REGISTRY)

docker-push: tools
	docker compose build api web
	docker compose push api web
	@echo ""
	@echo "=== Pushed digests ==="
	@docker inspect $(REGISTRY)/kobload:latest --format '  api: {{index .RepoDigests 0}}' 2>/dev/null || true
	@docker inspect $(REGISTRY)/kobload-web:latest --format '  web: {{index .RepoDigests 0}}' 2>/dev/null || true
	@echo ""
	@echo "Pruning old build artifacts..."
	docker image prune -f --filter "dangling=true"

# Optional disk-space housekeeping. Needs `cargo install cargo-sweep`; skipped
# silently when it isn't installed so a fresh clone can still run `make`.
sweep:
	@command -v cargo-sweep >/dev/null 2>&1 && cargo sweep --maxsize 10GB . || \
		echo "sweep: cargo-sweep not installed, skipping (cargo install cargo-sweep)"

# Base image with FFmpeg, chromaprint and the archive tools the API links
# against. Required before any `docker compose ... --build` of the api service
# — skip it and the build fails with a "pull access denied" error that reads
# like a registry auth problem rather than a missing local image.
tools:
	docker build -f docker/tools.Dockerfile -t kobload-tools:latest .

# Builds the base image first if missing, then starts the stack.
up:
	@docker image inspect kobload-tools:latest >/dev/null 2>&1 || $(MAKE) tools
	docker compose up -d --build
