---
id: building
title: Building
---

# Building

This page covers build commands, feature flags, and Docker builds for Invokr.

## Build commands

All build commands are available via the `just` task runner:

```bash
just build          # Build all Rust crates (debug)
just build-release  # Build all Rust crates (release)
just check          # Type-check without producing binaries (cargo check)
just lint           # Run clippy with -D warnings
just fmt            # Format all code (cargo fmt)
just fmt-check      # Check formatting without modifying (for CI)
```

### Direct cargo commands

You can also use `cargo` directly:

```bash
cargo build --workspace                    # Debug build
cargo build --workspace --release          # Release build
cargo check --workspace                    # Type check only
cargo clippy --workspace -- -D warnings    # Lint
cargo fmt --all                            # Format
cargo fmt --all -- --check                 # Format check
```

## Feature flags

Invokr uses Cargo feature flags to conditionally compile optional functionality. Features are defined per-crate and can be enabled selectively.

| Feature | Crate | Dependency | Description |
|---------|-------|------------|-------------|
| `kafka` | `invokr-worker` | `rdkafka` | Kafka dispatcher — enables dispatching jobs to Kafka topics |
| `redis-stream` | `invokr-worker` | `redis` | Redis Stream dispatcher — enables dispatching jobs to Redis Streams |
| `kms` | `invokr-common` | `aws-sdk-kms` | AWS KMS integration — enables transparent decryption of sensitive env vars |
| `pg_cron` | `invokr-common` | `pg_cron` extension | pg_cron extension support — required for CRON job scheduling (enabled by default in the database) |

### Building with features

```bash
# Build with Kafka support
cargo build --workspace --features invokr-worker/kafka

# Build with Redis Stream support
cargo build --workspace --features invokr-worker/redis-stream

# Build with KMS support
cargo build --workspace --features kms

# Build with all worker features
cargo build --workspace --features invokr-worker/kafka,invokr-worker/redis-stream

# Build worker with Kafka + KMS
cargo build -p invokr-worker --features kafka,kms

# Build API with KMS + dashboard
cargo build -p invokr-api --features kms
```

:::note
Feature flags use `crate-name/feature-name` syntax when targeting a specific crate. The `kms` feature is defined in `invokr-common` but can be enabled from the workspace level since it propagates to dependent crates.
:::

### Starting infrastructure for features

Kafka and Redis features require their respective infrastructure running for testing:

```bash
# Kafka
docker compose --profile kafka up -d
# Or: just infra-up (starts DB + Kafka + Redis)

# Redis
docker compose --profile redis up -d
# Or: just infra-up

# LocalStack KMS
just kms-up
```

## Docker build

### Multi-stage Dockerfile

The `Dockerfile` uses four stages:

1. **`dashboard-builder`** (conditional) — Builds the dashboard WASM bundle when `INCLUDE_DASHBOARD=true`
2. **`planner`** — Generates a `cargo-chef` recipe from `Cargo.toml` / `Cargo.lock`
3. **`builder`** — Compiles the binary with cached dependencies
4. **Runtime** — Slim Debian image with only runtime dependencies

See [Docker](../deployment/docker) for full details on the Dockerfile stages and build args.

### Build arguments

| Arg | Default | Description |
|-----|---------|-------------|
| `BINARY` | *(required)* | Which binary to build: `invokr-api`, `invokr-worker`, or `invokr-mock-server` |
| `FEATURES` | *(empty)* | Cargo feature flags (e.g. `kafka`, `redis-stream`, `kms`) |
| `INCLUDE_DASHBOARD` | `false` | When `true`, builds the dashboard WASM bundle |

### Building Docker images

```bash
# API server (no features)
docker build --build-arg BINARY=invokr-api -t invokr-api .

# Worker with Kafka + Redis Stream
docker build \
  --build-arg BINARY=invokr-worker \
  --build-arg FEATURES=kafka,redis-stream \
  -t invokr-worker .

# API with KMS + dashboard
docker build \
  --build-arg BINARY=invokr-api \
  --build-arg FEATURES=kms \
  --build-arg INCLUDE_DASHBOARD=true \
  -t invokr-api-full .

# Mock server
docker build --build-arg BINARY=invokr-mock-server -t invokr-mock-server .
```

### cargo-chef dependency caching

The Dockerfile uses [cargo-chef](https://github.com/LukeMathWalker/cargo-chef) to cache Rust dependencies separately from application source:

1. The `planner` stage runs `cargo chef prepare` to create a `recipe.json` capturing the dependency graph
2. The `builder` stage runs `cargo chef cook` to compile dependencies before the source is copied
3. Build caches are mounted via `--mount=type=cache` for the Cargo registry and `target/` directory

Dependency rebuilds only happen when `Cargo.toml` or `Cargo.lock` change, not on every source edit or CI run.

## Dashboard build

The dashboard is a Leptos 0.7 WASM application that requires `wasm-pack` and the `wasm32-unknown-unknown` target.

### Setup

```bash
just dashboard-setup
```

Installs:
- `wasm32-unknown-unknown` Rust target via `rustup`
- `wasm-pack` via `cargo install`

:::tip
The Nix development shell (`nix develop`) already includes `wasm-pack` and the `wasm32-unknown-unknown` target. Skip `just dashboard-setup` if using Nix.
:::

### Building

```bash
# Release build (optimized, smaller WASM)
just dashboard-build

# Dev build (faster compilation, larger WASM)
just dashboard-build-dev
```

The build process runs two commands:

1. **WASM compilation:**
   ```bash
   cd crates/dashboard && wasm-pack build --target web --release -- --features hydrate
   ```

2. **CSS generation:**
   ```bash
   cd crates/dashboard && tailwindcss -i input.css -o pkg/tailwind-output.css --minify
   ```

The output is placed in `crates/dashboard/pkg/`. Point `INVOKR_DASHBOARD_DIST_DIR` to this directory when running the API server in `both` mode.

### Building with path prefix

The dashboard uses compile-time environment variables baked into the WASM binary. Set them before building:

```bash
INVOKR_DASHBOARD_PATH_PREFIX=/dashboard \
INVOKR_API_BASE_URL=http://localhost:8080/invokr \
  just dashboard-build
```

See [Dashboard](../deployment/dashboard) for more details.

## Cleaning build artifacts

```bash
just clean
```

Removes:
- `target/` directory (Rust build artifacts)
- `smithy/build/` directory (Smithy codegen output)
- `cli/node_modules/` and `cli/dist/` (TypeScript CLI)
- `crates/dashboard/pkg/` (dashboard WASM bundle)

## See also

- [Development Setup](./setup) — prerequisites and environment setup
- [Testing](./testing) — running tests with various features
- [Docker](../deployment/docker) — detailed Dockerfile documentation
- [Dashboard](../deployment/dashboard) — dashboard build and configuration
- [Environment Variables](../configuration/environment-variables) — all configuration variables
