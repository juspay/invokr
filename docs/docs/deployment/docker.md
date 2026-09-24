---
id: docker
title: Docker
---

# Docker

Invokr ships with a multi-stage Dockerfile and a set of Docker Compose files for development and production-like environments. This page covers building custom images, the development compose stack, and the custom PostgreSQL image with `pg_cron`.

## Multi-stage Dockerfile

The `Dockerfile` at the repository root uses four stages to produce a minimal runtime image:

| Stage | Base Image | Purpose |
|-------|-----------|---------|
| `dashboard-builder` | `rust:bookworm` | Builds the dashboard WASM bundle (skipped unless `INCLUDE_DASHBOARD=true`) |
| `chef` | `lukemathwalker/cargo-chef:latest-rust-bookworm` | Base for dependency caching via `cargo-chef` |
| `planner` | `chef` | Generates a `recipe.json` from `Cargo.toml` / `Cargo.lock` |
| `builder` | `chef` | Compiles the selected binary using cached dependencies |
| Runtime | `debian:bookworm-slim` | Slim runtime image with only `ca-certificates`, `curl`, and `libssl3` |

### Build arguments

| Arg | Default | Description |
|-----|---------|-------------|
| `BINARY` | *(required)* | Which binary to build: `invokr-api`, `invokr-worker`, or `invokr-mock-server` |
| `FEATURES` | *(empty)* | Cargo feature flags to enable (e.g. `kafka`, `redis-stream`, `kms`) |
| `INCLUDE_DASHBOARD` | `false` | When `true`, builds the dashboard WASM bundle and copies it into the runtime image |

### Dependency caching with cargo-chef

The `planner` stage runs `cargo chef prepare` to create a `recipe.json` that captures the dependency graph. The `builder` stage runs `cargo chef cook` to compile dependencies **before** the application source is copied. Dependency rebuilds then happen only when `Cargo.toml` or `Cargo.lock` change, not on every source edit.

Build caches are mounted via `--mount=type=cache` for both the Cargo registry and the `target/` directory, enabling fast incremental builds across CI runs.

### Runtime image

The final stage is `debian:bookworm-slim` with `ca-certificates`, `curl`, and `libssl3` installed. The compiled binary is placed at `/usr/local/bin/app` and set as the `ENTRYPOINT`. When `INCLUDE_DASHBOARD=true`, the dashboard WASM bundle is copied to `/app/dashboard-dist` and `INVOKR_DASHBOARD_DIST_DIR` is set accordingly.

## Building custom images

### API server (default)

```bash
docker build -t invokr-api --build-arg BINARY=invokr-api .
```

### Worker with Kafka and Redis Stream support

```bash
docker build -t invokr-worker \
  --build-arg BINARY=invokr-worker \
  --build-arg FEATURES=kafka,redis-stream \
  .
```

### API server with KMS and dashboard

```bash
docker build -t invokr-api-full \
  --build-arg BINARY=invokr-api \
  --build-arg FEATURES=kms \
  --build-arg INCLUDE_DASHBOARD=true \
  .
```

:::note
The `INCLUDE_DASHBOARD` build adds the `wasm32-unknown-unknown` target, `wasm-pack`, and `tailwindcss` to the build stage, which increases build time. Only enable it when you need the dashboard served from the API container.
:::

## Development compose stack

The `docker-compose.yml` at the repository root provides all infrastructure services for local development. Services are organized with Docker Compose profiles so you can start only what you need.

### Core service: PostgreSQL

PostgreSQL is the only service that starts by default (no profile needed):

```bash
docker compose up -d postgres
```

It uses a custom image (see [Custom PostgreSQL image](#custom-postgresql-image)) that includes the `pg_cron` extension. The container maps host port **5434** to container port **5432** and creates a database named `invokr_db` with user `invokr`.

```yaml
services:
  postgres:
    build: docker/postgres
    ports:
      - "5434:5432"
    environment:
      POSTGRES_USER: invokr
      POSTGRES_PASSWORD: invokr
      POSTGRES_DB: invokr_db
    command:
      - "postgres"
      - "-c"
      - "shared_preload_libraries=pg_cron"
      - "-c"
      - "cron.database_name=invokr_db"
    volumes:
      - postgres-data:/var/lib/postgresql/data
```

### Optional services (profile-gated)

| Service | Profile | Port | Purpose |
|---------|---------|------|---------|
| `kafka` | `kafka` | 9092 | Bitnami Kafka 3.7 (controller+broker in one node) for Kafka dispatcher testing |
| `redis` | `redis` | 6379 | Redis 7 Alpine for Redis Stream dispatcher testing |
| `localstack` | `kms` | 4566 | LocalStack KMS for local KMS encryption testing |
| `prometheus` | `monitoring` | 9099 | Prometheus metrics scraper (5s interval, 7d retention) |
| `grafana` | `monitoring` | 3001 | Grafana with pre-provisioned dashboards (admin/invokr) |

### Starting optional services

```bash
# Kafka only
docker compose --profile kafka up -d

# Redis only
docker compose --profile redis up -d

# LocalStack KMS only
docker compose --profile kms up -d localstack

# Monitoring stack (Prometheus + Grafana)
docker compose --profile monitoring up -d

# Everything at once
docker compose --profile kafka --profile redis --profile monitoring up -d
```

Or use the `just` shortcuts:

```bash
just infra-up          # DB + Kafka + Redis
just monitoring-up     # Prometheus + Grafana
just all-up            # Everything
```

### Prometheus configuration

The Prometheus container mounts `monitoring/prometheus.yml` from the repository. It scrapes three targets via `host.docker.internal`:

| Job name | Target | Metrics path |
|----------|--------|-------------|
| `invokr-api` | `host.docker.internal:8080` | `/metrics` |
| `invokr-worker` | `host.docker.internal:9090` | `/metrics` |
| `invokr-scheduler` | `host.docker.internal:9091` | `/metrics` |

:::tip
When using a path prefix (e.g. `INVOKR_PATH_PREFIX=/invokr`), update `metrics_path` in `monitoring/prometheus.yml` from `/metrics` to `/invokr/metrics`.
:::

### Grafana dashboards

Grafana is pre-provisioned with:
- **Provisioning:** `monitoring/grafana/provisioning/` (mounted read-only)
- **Dashboards:** `monitoring/grafana/dashboards/` (mounted read-only)
- A pre-built platform dashboard at `monitoring/grafana/dashboards/invokr-platform.json`

Access Grafana at `http://localhost:3001` with credentials `admin` / `invokr`.

## Custom PostgreSQL image

The `docker/postgres/Dockerfile` extends the official `postgres:16` image with the `pg_cron` extension:

```dockerfile
FROM postgres:16

RUN apt-get update \
    && apt-get install -y --no-install-recommends postgresql-16-cron \
    && rm -rf /var/lib/apt/lists/*
```

`pg_cron` is loaded as a shared preload library via the container command:

```
postgres -c shared_preload_libraries=pg_cron -c cron.database_name=invokr_db
```

This is required because Invokr delegates all CRON job materialization to `pg_cron` — there is no separate scheduler process for CRON tick insertion. See [Database-driven scheduling](../architecture/db-driven-scheduling) for details.

:::warning
The `pg_cron` extension must be preloaded at server startup. If you use a different PostgreSQL image, ensure `shared_preload_libraries=pg_cron` is set in your PostgreSQL configuration before starting Invokr.
:::

## Running the full dev stack with Docker

For a production-like Docker setup that builds all Invokr services and runs them with KMS encryption, see [Production Deployment](./production). For local development without Docker (using `nix develop` + `just dev`), see [Development Setup](../development/setup).

## See also

- [Production Deployment](./production) — prod-like Docker Compose with KMS
- [AWS KMS Integration](./kms) — encrypting sensitive env vars
- [Dashboard](./dashboard) — building and serving the WASM dashboard
- [Environment Variables](../configuration/environment-variables) — full configuration reference
