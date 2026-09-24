---
id: environment-variables
title: Environment Variables
---

# Environment Variables

All Invokr configuration is via environment variables prefixed with `INVOKR_`. This page is a complete reference of every supported variable, grouped by category.

:::info
The `just` task runner has `set dotenv-load` enabled, so variables defined in a `.env` file at the project root are automatically loaded. See `.env.example` for a template.
:::

## Database

| Variable | Default | Description |
|----------|---------|-------------|
| `INVOKR_DATABASE_URL` | *(required)* | PostgreSQL connection string (e.g. `postgresql://user:pass@host:5432/db`). Required for both API and worker. |
| `INVOKR_DB_POOL_SIZE` | `50` | Maximum number of connections in the database connection pool. |
| `INVOKR_TABLE_PREFIX` | *(empty)* | Prefix for all per-workspace Invokr tables. Set to e.g. `sched` to get `sched_jobs`, `sched_executions`, etc. Only alphanumeric and underscore characters allowed. |

:::warning
`INVOKR_DATABASE_URL` is a **sensitive** variable. When KMS is enabled (`INVOKR_KMS_ENABLED=true`), this must contain a base64-encoded KMS-encrypted ciphertext, not a plaintext connection string. See [AWS KMS Integration](../deployment/kms).
:::

## API server

| Variable | Default | Description |
|----------|---------|-------------|
| `INVOKR_LISTEN_ADDR` | `0.0.0.0:8080` | Bind address and port for the API server. |
| `INVOKR_API_KEY` | `dev-api-key` | Bearer token for API authentication. All requests require `Authorization: Bearer <api_key>`. |
| `INVOKR_PATH_PREFIX` | *(empty)* | URL path prefix for all API routes (e.g. `/invokr`). When set, routes become `/invokr/health`, `/invokr/v1/jobs`, etc. `GET /` returns a `302` redirect to `{prefix}/health`. |
| `INVOKR_MODE` | `api` | Server mode: `api` (REST API only), `dashboard` (dashboard only), or `both` (API + dashboard SSR). |

:::warning
`INVOKR_API_KEY` is a **sensitive** variable. When KMS is enabled, this must contain base64-encoded KMS-encrypted ciphertext. In production, always set this to a strong, unique key — never use the default `dev-api-key`.
:::

### Path prefix

When `INVOKR_PATH_PREFIX` is set, the prefix is normalized (leading/trailing slashes stripped, then prepended with `/`). All API routes, health check, and metrics endpoint are served under the prefix:

```bash
INVOKR_PATH_PREFIX=/invokr ./invokr-api

# Routes:
# GET  /invokr/health
# GET  /invokr/metrics
# POST /invokr/v1/jobs
# GET  /invokr/v1/jobs/{id}
# ...
```

When a prefix is configured and `INVOKR_MODE=both` with a dashboard prefix, `GET /` redirects to the dashboard. Otherwise, `GET /` redirects to `{prefix}/health`.

## Dashboard

These variables are **compile-time** env vars baked into the WASM binary. They must be set when building the dashboard (`just dashboard-build`), not just at runtime.

| Variable | Default | Description |
|----------|---------|-------------|
| `INVOKR_DASHBOARD_PATH_PREFIX` | *(empty)* | URL prefix for dashboard routes (e.g. `/dashboard`). |
| `INVOKR_DASHBOARD_DIST_DIR` | `./dashboard-dist` | Filesystem path to the directory containing the built dashboard WASM bundle and assets. Set to `crates/dashboard/pkg` for local dev. |
| `INVOKR_API_BASE_URL` | *(empty)* | Full API base URL including path prefix. Must include `INVOKR_PATH_PREFIX` if set (e.g. `http://localhost:8080/invokr`). |

:::important
`INVOKR_API_BASE_URL` must include the `INVOKR_PATH_PREFIX` if one is set. For example, if `INVOKR_PATH_PREFIX=/invokr`, then `INVOKR_API_BASE_URL` should be `http://localhost:8080/invokr`.
:::

## Worker

| Variable | Default | Description |
|----------|---------|-------------|
| `INVOKR_WORKER_MAX_CONCURRENT` | `50` | Maximum number of concurrent job executions per worker instance. Enforced via a semaphore. |
| `INVOKR_WORKER_POLL_INTERVAL_MS` | `200` | Interval (in milliseconds) between worker database polls for new executions. |
| `INVOKR_WORKER_SHUTDOWN_TIMEOUT_SEC` | `30` | Graceful shutdown timeout. The worker waits this long for in-flight executions to complete before forcing shutdown. |
| `INVOKR_CONFIG_CACHE_TTL_SEC` | `60` | TTL (in seconds) for the config cache in the worker. Configs are cached in a `DashMap` and refreshed after this interval. |
| `INVOKR_SECRET_CACHE_TTL_SEC` | `300` | TTL (in seconds) for the secret cache in the worker. Secrets are decrypted in memory and cached for this duration. |

### Worker concurrency tuning

The `INVOKR_WORKER_MAX_CONCURRENT` semaphore limits how many executions can be in-flight simultaneously. Each poll iteration:

1. Acquires a semaphore permit (if available)
2. Iterates all active workspace schemas
3. Attempts to claim one execution via `SELECT FOR UPDATE SKIP LOCKED`
4. If no work is found, releases the permit and backs off to `INVOKR_WORKER_POLL_INTERVAL_MS`

If all permits are in use, the worker stops polling until one frees up.

:::tip
When scaling workers horizontally, keep `INVOKR_WORKER_MAX_CONCURRENT` per instance low enough that `(instances × max_concurrent)` does not overwhelm your downstream endpoints or exceed PostgreSQL's `max_connections`.
:::

## Encryption

| Variable | Default | Description |
|----------|---------|-------------|
| `INVOKR_ENCRYPTION_KEY` | `0000...0000` (64 zeros) | AES-256-GCM encryption key for secrets, as a hex string (32 bytes = 64 hex chars). Used to encrypt/decrypt secret values at rest. |

:::danger
`INVOKR_ENCRYPTION_KEY` is a **sensitive** variable. When KMS is enabled, this must contain base64-encoded KMS-encrypted ciphertext.

**In production, always set this to a strong, random 32-byte key.** The default all-zeros key provides no security. If the key is rotated, existing secrets encrypted with the old key cannot be decrypted.
:::

:::warning
`INVOKR_ENCRYPTION_KEY` is also a **sensitive** variable subject to KMS decryption. See [AWS KMS Integration](../deployment/kms).
:::

## Metrics

| Variable | Default | Description |
|----------|---------|-------------|
| `INVOKR_METRICS_PORT` | `9090` | Port for the worker's Prometheus metrics HTTP listener. The API server serves metrics at `GET /metrics` (or `{prefix}/metrics`) on its main port. |

## Reaper

| Variable | Default | Description |
|----------|---------|-------------|
| `INVOKR_REAPER_CRON_EXPRESSION` | `*/15 * * * *` | 5-field pg_cron expression controlling how often Invokr's own dogfooded reaper fires per workspace. Baked into each workspace's pg_cron entry at creation time, so changing this only affects workspaces created afterward. |

:::info
The reaper is Invokr's own CRON sweep that retires expired CRON jobs and unschedules their pg_cron entries. The expression is validated at startup as a 5-field `PgCronExpr` — a typo will cause the server to fail fast rather than breaking the first `POST /workspaces` call.
:::

## Scheduler

| Variable | Default | Description |
|----------|---------|-------------|
| `INVOKR_CRON_TICK_INTERVAL_SEC` | `1` | Interval (in seconds) for the scheduler's CRON materializer tick. |
| `INVOKR_CRON_BATCH_SIZE` | `100` | Maximum number of CRON jobs to process per scheduler tick. |
| `INVOKR_PROMOTE_INTERVAL_MS` | `500` | Interval (in milliseconds) for promoting PENDING (delayed) executions to QUEUED. |
| `INVOKR_RECLAIM_INTERVAL_SEC` | `30` | Interval (in seconds) for reclaiming stuck executions (executions in RUNNING status beyond the timeout). |
| `INVOKR_STUCK_EXECUTION_TIMEOUT_SEC` | `300` | Timeout (in seconds) after which a RUNNING execution is considered stuck and eligible for reclaiming. |

## KMS

These variables control AWS KMS integration for encrypting sensitive environment variables at rest.

| Variable | Default | Description |
|----------|---------|-------------|
| `INVOKR_KMS_ENABLED` | `false` | When `true`, `INVOKR_DATABASE_URL`, `INVOKR_API_KEY`, and `INVOKR_ENCRYPTION_KEY` are expected to be base64-encoded KMS-encrypted ciphertext. Requires the `kms` Cargo feature. |
| `AWS_ENDPOINT_URL` | *(unset)* | KMS endpoint URL. Set to `http://localhost:4566` for LocalStack dev. Omit for production AWS. |
| `AWS_REGION` | *(unset)* | AWS region for KMS (e.g. `us-east-1`). |
| `AWS_ACCESS_KEY_ID` | *(unset)* | AWS access key ID. Use `test` for LocalStack. |
| `AWS_SECRET_ACCESS_KEY` | *(unset)* | AWS secret access key. Use `test` for LocalStack. |

:::warning
If `INVOKR_KMS_ENABLED=true` but the binary was compiled without the `kms` feature, the server will fail to start. Build with `cargo build --features kms` or pass `--build-arg FEATURES=kms` for Docker.
:::

See [AWS KMS Integration](../deployment/kms) for setup instructions.

## CLI / Test scripts

These variables are used by the TypeScript CLI and test scripts (in `cli/`). They are **not** read by the Rust binaries.

| Variable | Default | Description |
|----------|---------|-------------|
| `INVOKR_URL` | *(unset)* | Base URL for the Invokr API. Must include `INVOKR_PATH_PREFIX` if set (e.g. `http://localhost:8080/invokr`). |
| `INVOKR_API_KEY` | *(unset)* | API key for authentication (same as `INVOKR_API_KEY`). |
| `INVOKR_ORG_ID` | *(unset)* | Organization UUID, set after creating an org via the API. |
| `INVOKR_WORKSPACE_ID` | *(unset)* | Workspace UUID, set after creating a workspace via the API. |

:::note
`INVOKR_URL` must include `INVOKR_PATH_PREFIX` when the server is configured with one, and `INVOKR_API_KEY` is the same key the server authenticates against — the CLI and the server read the same variable.
:::

## Sensitive variables and KMS

The following variables are treated as **sensitive** by Invokr. When KMS is enabled (`INVOKR_KMS_ENABLED=true`), they are transparently decrypted at startup:

| Variable | Sensitive? | KMS-decrypted? |
|----------|-----------|----------------|
| `INVOKR_DATABASE_URL` | Yes | Yes |
| `INVOKR_API_KEY` | Yes | Yes |
| `INVOKR_ENCRYPTION_KEY` | Yes | Yes |
| All other `INVOKR_*` variables | No | No |

## Path prefix summary

The path prefix affects the API server (runtime), the dashboard (compile-time), and monitoring configuration:

| Component | Variable | When to set | Type |
|-----------|----------|-------------|------|
| API server | `INVOKR_PATH_PREFIX` | Runtime (env var or `.env`) | Runtime |
| Dashboard | `INVOKR_DASHBOARD_PATH_PREFIX` | Build time (`just dashboard-build`) | Compile-time (baked into WASM) |
| Dashboard | `INVOKR_API_BASE_URL` | Build time (`just dashboard-build`) | Compile-time (baked into WASM) |
| Prometheus | `metrics_path` in `prometheus.yml` | Config file | Config file |
| Docker healthcheck | Health check URL in compose file | Config file | Config file |

### Monitoring config updates

When using a path prefix, update these monitoring configs to match:

**Prometheus** (`monitoring/prometheus.yml`):
```yaml
scrape_configs:
  - job_name: "invokr-api"
    metrics_path: /invokr/metrics   # was /metrics
    static_configs:
      - targets: ["host.docker.internal:8080"]
```

**Docker healthchecks** (`docker-compose.prod.yml`):
```yaml
healthcheck:
  test: ["CMD-SHELL", "curl -sf http://localhost:8080/invokr/health"]
  # was: http://localhost:8080/health
```

## Complete .env.example

```bash
# KMS (requires 'kms' feature: cargo build --features kms)
INVOKR_KMS_ENABLED=false
# AWS_ENDPOINT_URL=http://localhost:4566
# AWS_REGION=us-east-1
# AWS_ACCESS_KEY_ID=test
# AWS_SECRET_ACCESS_KEY=test

# Database
INVOKR_DATABASE_URL=postgresql://invokr:invokr@localhost:5434/invokr_db

# API Server
INVOKR_LISTEN_ADDR=0.0.0.0:8080
# INVOKR_PATH_PREFIX=/invokr
INVOKR_DB_POOL_SIZE=20

# Worker
INVOKR_WORKER_MAX_CONCURRENT=50
INVOKR_WORKER_POLL_INTERVAL_MS=200
INVOKR_CONFIG_CACHE_TTL_SEC=60
INVOKR_SECRET_CACHE_TTL_SEC=300
INVOKR_WORKER_SHUTDOWN_TIMEOUT_SEC=30

# Reaper
# INVOKR_REAPER_CRON_EXPRESSION=*/15 * * * *

# Scheduler
INVOKR_CRON_TICK_INTERVAL_SEC=1
INVOKR_CRON_BATCH_SIZE=100
INVOKR_PROMOTE_INTERVAL_MS=500
INVOKR_RECLAIM_INTERVAL_SEC=30
INVOKR_STUCK_EXECUTION_TIMEOUT_SEC=300

# Encryption
INVOKR_ENCRYPTION_KEY=0000000000000000000000000000000000000000000000000000000000000000

# API Key
INVOKR_API_KEY=dev-api-key

# Dashboard (compile-time env vars for WASM build)
# INVOKR_DASHBOARD_PATH_PREFIX=/dashboard
# INVOKR_API_BASE_URL=http://localhost:8080/invokr

# CLI / Test Scripts
# INVOKR_URL=http://localhost:8080
# INVOKR_API_KEY=dev-api-key
# INVOKR_ORG_ID=<org uuid>
# INVOKR_WORKSPACE_ID=<workspace uuid>
```

## See also

- [Docker](../deployment/docker) — Docker build and compose configuration
- [Production Deployment](../deployment/production) — production tuning and scaling
- [AWS KMS Integration](../deployment/kms) — encrypting sensitive variables
- [Dashboard](../deployment/dashboard) — dashboard path prefix configuration
- [Development Setup](../development/setup) — setting up a dev environment
