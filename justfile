# Invokr — Distributed Job Scheduling and Execution Engine

set dotenv-load

# Default: list all recipes
default:
    @just --list

# ─── Environment ──────────────────────────────────────────────

export INVOKR_DATABASE_URL := env("INVOKR_DATABASE_URL", "postgresql://invokr:invokr@localhost:5434/invokr_db")
export INVOKR_API_KEY := env("INVOKR_API_KEY", "dev-api-key")
export INVOKR_ENCRYPTION_KEY := env("INVOKR_ENCRYPTION_KEY", "0000000000000000000000000000000000000000000000000000000000000000")

# ─── Setup ────────────────────────────────────────────────────

# One-time project setup: start DB, run migrations, build SDK, install CLI deps
setup: db-up db-migrate build-sdk cli-install
    @echo "Setup complete. Run 'just dev' to start all services."

# Create .env from example if it doesn't exist
init-env:
    @[ -f .env ] || cp .env.example .env && echo ".env ready"

# ─── Database ─────────────────────────────────────────────────

# Start PostgreSQL
db-up:
    docker compose up -d postgres
    @echo "Waiting for PostgreSQL to be ready..."
    @sleep 3

# Stop PostgreSQL
db-down:
    docker compose down

# Deliberately the same code path the deployment runs, rather than a shell loop
# over migrations/*.sql — otherwise developers exercise a different mechanism
# from production, which is how a migration passes locally and fails on deploy.

# Run pending migrations
db-migrate:
    INVOKR_DB_MIGRATION_MODE=run cargo run --quiet -p invokr-api -- migrate

# Print the SQL a migration run would execute, without applying it. Also how an
# existing database is baselined: keep the _sqlx_migrations INSERTs for
# migrations already applied by hand, discard the DDL.

# Show pending migrations without applying them
db-migrate-dry-run:
    INVOKR_DB_MIGRATION_MODE=dry-run cargo run --quiet -p invokr-api -- migrate

# Create a new migration with a correctly formatted, ordered filename
migrate-new name:
    sqlx migrate add --source migrations {{name}}

# Reset database (drop + recreate + migrate)
db-reset:
    sqlx database drop --database-url "$INVOKR_DATABASE_URL" -y || true
    sqlx database create --database-url "$INVOKR_DATABASE_URL"
    just db-migrate

# Open a SQL shell
db-shell:
    psql "$INVOKR_DATABASE_URL"

# ─── Build ────────────────────────────────────────────────────

# Build all Rust crates
build:
    cargo build --workspace

# Build in release mode
build-release:
    cargo build --workspace --release

# Check all crates compile
check:
    cargo check --workspace

# ─── Smithy + SDK ─────────────────────────────────────────────

export SMITHY_MAVEN_REPOS := "https://repo.maven.apache.org/maven2|https://sandbox.assets.juspay.in/smithy/m2"

# Run before regeneration to surface model errors with clean messages
# instead of cryptic codegen failures.

# Validate Smithy models
smithy-validate:
    cd smithy && smithy validate

# Edit smithy/model/* → run this → commit the resulting diff
# (model + crates/client/) in the same PR.

# Regenerate from Smithy models and sync the committed Rust SDK at crates/client/
smithy-build: smithy-validate
    # `smithy build` writes into smithy/build/ without clearing it, so renaming
    # a shape leaves the old artifact behind alongside the new one, and
    # `build-sdk` compiles the whole directory into the published package.
    rm -rf smithy/build
    cd smithy && smithy build
    rm -rf crates/client
    cp -R smithy/build/smithy/source/rust-client-codegen crates/client
    # Restore the tracked README.md (DO NOT EDIT warning) that the wipe removed
    git checkout -- crates/client/README.md
    @echo "Regenerated crates/client. Review with: git diff -- crates/client"

# Build the generated TypeScript SDK (npm install + compile)
build-sdk: smithy-build
    cd smithy/build/smithy/source/typescript-client-codegen && npm install && npm run build

# Install CLI dependencies (links to built SDK)
cli-install: build-sdk
    cd cli && npm install

# Regenerate SDK and reinstall CLI (after Smithy model changes)
sdk-refresh: build-sdk cli-install

# ─── Run Services ─────────────────────────────────────────────

# Run the API server (port 8080)
api:
    cargo run -p invokr-api

# Run the worker
worker:
    cargo run -p invokr-worker

# Run the mock HTTP server (port 9999)
mock-server:
    cargo run -p invokr-mock-server

# Run the library-mode example (requires `just setup` + `just mock-server`)
example-library-mode:
    cargo run -p library-mode-example

# Run all services in parallel (API + worker + scheduler + mock-server)
dev:
    #!/usr/bin/env bash
    set -e
    trap 'kill 0' EXIT

    echo "Starting all Invokr services..."
    echo "  API:       http://localhost:8080  (metrics at /metrics)"
    echo "  Worker:    metrics on :9090"
    echo "  Mock:      http://localhost:9999"

    cargo run -p invokr-api &
    INVOKR_METRICS_PORT=9090 cargo run -p invokr-worker &
    cargo run -p invokr-mock-server &

    echo "All services starting. Press Ctrl+C to stop all."
    wait

# ─── Test ─────────────────────────────────────────────────────

# Run HTTP dispatcher tests (requires mock-server running)
test-http:
    cargo test -p invokr-worker --lib dispatcher::http::tests

# Run Kafka dispatcher tests (requires: docker compose --profile kafka up -d)
test-kafka:
    cargo test -p invokr-worker --features kafka --lib dispatcher::kafka::tests -- --test-threads=1

# Run Redis stream dispatcher tests (requires: docker compose --profile redis up -d)
test-redis:
    cargo test -p invokr-worker --features redis-stream --lib dispatcher::redis_stream::tests -- --test-threads=1

# Run all dispatcher tests (requires kafka, redis, and mock-server)
test-dispatchers: test-http test-kafka test-redis

# Load test: create N jobs of each type (IMMEDIATE, DELAYED, CRON) and track completion
# Usage: just load-test 50
load-test N="10":
    cd cli && npx tsx src/load-test.ts {{N}}

# Load test (fire-and-forget, no polling)
load-test-nw N="10":
    cd cli && npx tsx src/load-test.ts {{N}} --no-wait

# Run the immediate execution end-to-end test
test-immediate:
    cd cli && npx tsx src/test-immediate.ts

# Run the delayed execution end-to-end test (requires worker running)
test-delayed:
    cd cli && npx tsx src/test-delayed.ts

# Run the CRON job end-to-end test (requires worker running; pg_cron materializes ticks)
test-cron:
    cd cli && npx tsx src/test-cron.ts

# API-only — no worker required.

# Verify public API guards for INTERNAL jobs/endpoints (dogfooded reaper)
test-internal-guards:
    cd cli && npx tsx src/test-internal-guards.ts

# Full integration test: setup → dev services → run test
test-e2e: build
    #!/usr/bin/env bash
    set -e
    trap 'kill 0' EXIT

    echo "Starting services for e2e test..."

    cargo run -p invokr-api &
    API_PID=$!

    cargo run -p invokr-worker &
    WORKER_PID=$!

    cargo run -p invokr-mock-server &
    MOCK_PID=$!

    # Wait for services to be ready
    echo "Waiting for services to start..."
    for i in $(seq 1 30); do
        if curl -sf http://localhost:8080/health > /dev/null 2>&1 && \
           curl -sf http://localhost:9999/health > /dev/null 2>&1; then
            echo "Services ready."
            break
        fi
        if [ "$i" -eq 30 ]; then
            echo "ERROR: Services failed to start within 30s"
            exit 1
        fi
        sleep 1
    done

    cd cli && npx tsx src/test-immediate.ts && npx tsx src/test-delayed.ts && npx tsx src/test-cron.ts && npx tsx src/test-internal-guards.ts
    EXIT_CODE=$?

    echo "Shutting down services..."
    exit $EXIT_CODE

# Run the Haskell SDK example (requires db-up, db-migrate)
test-haskell: build
    #!/usr/bin/env bash
    set -e
    trap 'kill 0' EXIT

    echo "Starting services for Haskell e2e test..."

    cargo run -p invokr-api &
    cargo run -p invokr-worker &
    cargo run -p invokr-mock-server &

    echo "Waiting for services to start..."
    for i in $(seq 1 30); do
        if curl -sf http://localhost:8080/health > /dev/null 2>&1 && \
           curl -sf http://localhost:9999/health > /dev/null 2>&1; then
            echo "Services ready."
            break
        fi
        if [ "$i" -eq 30 ]; then
            echo "ERROR: Services failed to start within 30s"
            exit 1
        fi
        sleep 1
    done

    echo "Building and running Haskell example..."
    cd haskell-example && nix-shell \
        -p "haskell.packages.ghc96.ghcWithPackages (p: with p; [aeson text network-uri http-client http-types bytestring mtl time containers http-date case-insensitive])" \
        cabal-install \
        --run "cabal run invokr-example 2>&1"
    EXIT_CODE=$?

    echo "Shutting down services..."
    exit $EXIT_CODE

# ─── KMS (LocalStack) ────────────────────────────────────────

# Start LocalStack for KMS dev testing
kms-up:
    docker compose --profile kms up -d localstack
    @echo "Waiting for LocalStack to be ready..."
    @sleep 3
    @echo "LocalStack KMS ready at http://localhost:4566"

# Stop LocalStack
kms-down:
    docker compose --profile kms down

# Create a KMS key on LocalStack + encrypt DB URL → .env.kms
kms-init: kms-up
    unset INVOKR_DATABASE_URL INVOKR_API_KEY INVOKR_ENCRYPTION_KEY && ./scripts/kms-init.sh

# Encrypt a plaintext value with the LocalStack KMS key
kms-encrypt VALUE:
    @./scripts/kms-encrypt.sh "{{VALUE}}"

# Run API + worker with KMS feature enabled (uses .env.kms)
kms-dev:
    #!/usr/bin/env bash
    set -e
    if [ ! -f .env.kms ]; then
        echo "Error: .env.kms not found. Run 'just kms-init' first." >&2
        exit 1
    fi
    # cp .env.kms .env
    trap 'kill 0' EXIT
    echo "Starting KMS-enabled dev services..."
    cargo run --features kms -p invokr-api &
    INVOKR_METRICS_PORT=9090 RUST_LOG=info cargo run --features kms -p invokr-worker &
    cargo run -p invokr-mock-server &
    echo "All services starting with KMS. Press Ctrl+C to stop."
    wait

# ─── Docker Prod ─────────────────────────────────────────────

# Start full prod-like Docker environment (postgres + KMS + all services)
docker-prod-up:
    ./scripts/docker-prod.sh

# Stop and clean up prod-like Docker environment
docker-prod-down:
    ./scripts/docker-prod-down.sh

# ─── Cargo utilities ─────────────────────────────────────────

# Run clippy lints
lint:
    cargo clippy --workspace -- -D warnings

# Format code
fmt:
    cargo fmt --all

# Format check (CI)
fmt-check:
    cargo fmt --all -- --check

# ─── Docker ──────────────────────────────────────────────────

# Start all infrastructure (DB + Kafka + Redis)
infra-up:
    docker compose --profile kafka --profile redis up -d

# Stop all infrastructure
infra-down:
    docker compose --profile kafka --profile redis down

# ─── Monitoring ─────────────────────────────────────────────

# Start Prometheus + Grafana (Grafana at http://localhost:3001, admin/invokr)
monitoring-up:
    docker compose --profile monitoring up -d
    @echo "Prometheus: http://localhost:9099"
    @echo "Grafana:    http://localhost:3001  (admin / invokr)"

# Stop monitoring stack
monitoring-down:
    docker compose --profile monitoring down

# Start everything: infra + monitoring
all-up:
    docker compose --profile kafka --profile redis --profile monitoring up -d
    @echo "All infrastructure started."
    @echo "Prometheus: http://localhost:9099"
    @echo "Grafana:    http://localhost:3001  (admin / invokr)"

# Stop everything
all-down:
    docker compose --profile kafka --profile redis --profile monitoring down

# ─── Dashboard ──────────────────────────────────────────────

# Build the dashboard WASM hydration bundle (requires wasm-pack and wasm32 target)
dashboard-build:
    cd crates/dashboard && wasm-pack build --target web --release -- --features hydrate
    cd crates/dashboard && tailwindcss -i input.css -o pkg/tailwind-output.css --minify

# Build dashboard in dev mode (faster, no optimizations)
dashboard-build-dev:
    cd crates/dashboard && wasm-pack build --target web --dev -- --features hydrate
    cd crates/dashboard && tailwindcss -i input.css -o pkg/tailwind-output.css

# Run the dashboard via the API server (SSR mode)
dashboard:
    INVOKR_MODE=both INVOKR_DASHBOARD_DIST_DIR=crates/dashboard/pkg cargo run -p invokr-api

# Install dashboard build tools
dashboard-setup:
    rustup target add wasm32-unknown-unknown
    cargo install wasm-pack

# ─── Cleanup ─────────────────────────────────────────────────

# Clean all build artifacts
clean:
    cargo clean
    rm -rf smithy/build
    rm -rf cli/node_modules cli/dist
    rm -rf crates/dashboard/pkg

# ─── Documentation (Docusaurus) ──────────────────────────────

# Start Docusaurus dev server (hot reload)
docs-dev:
    cd docs && npm start

# Build the documentation site for production
docs-build:
    cd docs && npm run build

# Serve the built documentation locally
docs-serve:
    cd docs && npm run serve

# Regenerate API reference from OpenAPI spec (run after `just smithy-build`)
docs-gen-api:
    cp smithy/build/smithy/source/openapi/InvokrService.openapi.json docs/api/invokr-openapi.json
    cd docs && npx docusaurus clean-api-docs all || true
    cd docs && npx docusaurus gen-api-docs all

# Install documentation dependencies (first-time setup)
docs-install:
    cd docs && npm install
