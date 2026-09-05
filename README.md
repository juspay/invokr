# Invokr

**`setTimeout` and `setInterval` as a service.**

Distributed, durable, retriable, observable delivery of jobs to HTTP endpoints, Kafka topics, and Redis Streams — with type-safety guarantees.

---

## The mental model

If you've written JavaScript, you already know the API.

| What you want | JS | Invokr |
|---|---|---|
| Fire now | `setTimeout(fn, 0)` | `POST /v1/jobs { trigger: IMMEDIATE }` |
| Fire later | `setTimeout(fn, 5000)` | `POST /v1/jobs { trigger: DELAYED, run_at: "..." }` |
| Fire repeatedly | `setInterval(fn, 60000)` | `POST /v1/jobs { trigger: CRON, cron: "* * * * *" }` |
| Cancel | `clearTimeout(id)` | `POST /v1/jobs/{id}/cancel` |

Except: it survives crashes, retries on failure, never fires twice, and every execution is observable.

---

## Architecture

```
                              ┌─────────────────────────┐
                              │        Client / SDK      │
                              └────────────┬────────────┘
                                           │
                                    POST /v1/jobs
                                           │
                              ┌────────────▼────────────┐
                              │   API Server (actix-web) │
                              │   port 8080 + /metrics   │
                              └────────────┬────────────┘
                                           │
                             INSERT job + execution (txn)
                                           │
                 ┌─────────────────────────▼──────────────────────────┐
                 │               PostgreSQL + pg_cron                  │
                 │                                                     │
                 │  Source of truth          CRON scheduling natively  │
                 │  FOR UPDATE SKIP LOCKED   via pg_cron extension    │
                 │  Txn-based job pickup     (no external scheduler)  │
                 └───────┬──────────────────────────────┬─────────────┘
                         │                              │
              ┌──────────▼───────────┐    ┌─────────────▼─────────────┐
              │     Worker Pool      │    │    Dashboard (WASM)       │
              │                      │    │    Leptos + Trunk         │
              │  Semaphore-gated     │    │    port 3000              │
              │  50 concurrent jobs  │    └───────────────────────────┘
              │                      │
              │  ┌────────────────┐  │
              │  │ HTTP  (reqwest)│  │
              │  │ Kafka (rdkafka)│  │
              │  │ Redis (redis)  │  │
              │  └────────────────┘  │
              │  metrics on :9090    │
              └──────────────────────┘
```

### How scheduling works

Invokr uses **PostgreSQL pg_cron** for CRON materialization and **transaction-based pickup** for all job types:

- **IMMEDIATE** jobs: Execution is created as `QUEUED` in the same transaction as the job. Workers pick it up directly.
- **DELAYED** jobs: Execution is created as `PENDING` with a `run_at` timestamp. Workers pick up PENDING executions once `run_at <= now()`.
- **CRON** jobs: Registered with pg_cron at creation time. pg_cron inserts a new `QUEUED` execution on each tick. Workers pick it up directly.

No separate scheduler process is needed. The database handles all scheduling concerns.

### Crates

| Crate | Description |
|-------|-------------|
| `invokr-common` | Shared library — models, DB layer, config, tenant management, caching, metrics |
| `invokr-api` | REST API server (actix-web). CRUD for all resources, job invocation, Prometheus metrics at `/metrics` |
| `invokr-worker` | Execution engine. Polls DB for QUEUED/RETRYING/PENDING executions, resolves templates, dispatches to endpoints. Exposes metrics via HTTP listener |
| `invokr-mock-server` | Test fixture — HTTP server on port 9999 for integration tests |
| `invokr-dashboard` | Web UI — Leptos/WASM, shows jobs, executions, attempts. Excluded from workspace build |

### Multi-tenancy

Invokr uses **schema-per-tenant** isolation. Each workspace gets its own PostgreSQL schema with isolated tables. Shared tables live in the `public` schema.

```
public schema:        organizations, workspaces
tenant schema:        payload_specs, configs, secrets, endpoints,
(org_workspace):      jobs, executions, attempts, execution_logs
```

Tenant-scoped API requests require `X-Org-Id` and `X-Workspace-Id` headers. The worker iterates all active workspace schemas via a cached `SchemaRegistry` (30s TTL).

---

## Quickstart

### Prerequisites

- [Nix](https://nixos.org/download) with flakes enabled
- [Docker](https://docs.docker.com/get-docker/) (for PostgreSQL)

### Setup (just)

```bash
# Enter the dev shell (installs Rust, Node.js, smithy-cli, just, trunk, etc.)
nix develop

# One-time setup: start DB, run migrations, build SDK, install CLI deps
just setup

# Run all services (API + worker + mock-server)
just dev
```

The API is now running at `http://localhost:8080`.

### Setup (manual)

If you'd rather drive each step yourself (e.g. to run with a path prefix and the
dashboard), the flow below mirrors what `just setup`/`just dev` automate. It assumes
the Postgres container from `docker compose` is up and named `invokr-postgres-1`, with
host port **5434** mapped to the container's `5432` (see `docker-compose.yml`).

```bash
# Start PostgreSQL
docker compose up -d postgres
```

**1. (Re)create the database.** Connect to the default `postgres` database and drop/recreate
`invokr_db` for a clean slate:

```bash
docker exec -i invokr-postgres-1 psql -U invokr -d postgres -c \
  "DROP DATABASE IF EXISTS invokr_db WITH (FORCE);"
docker exec -i invokr-postgres-1 psql -U invokr -d postgres -c \
  "CREATE DATABASE invokr_db;"
```

**2. Apply migrations:**

```bash
INVOKR_DB_MIGRATION_MODE=run cargo run -p invokr-api -- migrate
```

The migrations are compiled into the binary, so this is the same code path a
deployment runs. Applied versions are recorded in `_sqlx_migrations`, making
re-runs a no-op. To review the SQL first without applying it:

```bash
INVOKR_DB_MIGRATION_MODE=dry-run cargo run -p invokr-api -- migrate
```

**3. Run the API server** (here in `both` mode, serving the dashboard under `/dashboard`
and the API under `/api`, on port 8090):

```bash
INVOKR_DATABASE_URL="postgres://invokr:invokr@localhost:5434/invokr_db" \
INVOKR_LISTEN_ADDR="0.0.0.0:8090" \
INVOKR_MODE="both" \
INVOKR_PATH_PREFIX="/api" \
INVOKR_DASHBOARD_PATH_PREFIX="/dashboard" \
INVOKR_DASHBOARD_DIST_DIR="crates/dashboard/pkg" \
cargo run -p invokr-api
```

> Building the dashboard bundle first (`just dashboard-build`) is required for `INVOKR_MODE=both`
> to serve `crates/dashboard/pkg`.

**4. Run the worker** in a separate shell:

```bash
INVOKR_DATABASE_URL="postgres://invokr:invokr@localhost:5434/invokr_db" \
INVOKR_METRICS_PORT="9090" \
cargo run -p invokr-worker
```

### Verify

```bash
# `just dev` (root path):
curl http://localhost:8080/health
# OK

# manual setup above (path prefix /api on port 8090):
curl http://localhost:8090/api/health
# OK
```

---

## Usage

All endpoints require `Authorization: Bearer <api_key>` (default: `dev-api-key`).

Tenant-scoped endpoints (everything except orgs/workspaces) also require:
- `X-Org-Id: <org_id>`
- `X-Workspace-Id: <workspace_id>`

### 1. Setup — create an org and workspace first

```bash
# Create an organization
curl -X POST http://localhost:8080/v1/orgs \
  -H "Authorization: Bearer dev-api-key" \
  -H "Content-Type: application/json" \
  -d '{ "name": "My Company", "slug": "my-company" }'

# Create a workspace within the org
curl -X POST http://localhost:8080/v1/orgs/{org_id}/workspaces \
  -H "Authorization: Bearer dev-api-key" \
  -H "Content-Type: application/json" \
  -d '{ "name": "Production", "slug": "production" }'
```

### 2. Define input contracts, configs, and secrets

```bash
# All subsequent requests include tenant headers
HEADERS='-H "Authorization: Bearer dev-api-key" -H "X-Org-Id: <org_id>" -H "X-Workspace-Id: <workspace_id>" -H "Content-Type: application/json"'

# Create a JSON Schema for input validation
curl -X POST http://localhost:8080/v1/payload-specs $HEADERS \
  -d '{
    "name": "order-input",
    "schema": {
      "type": "object",
      "properties": {
        "order_id": { "type": "string" },
        "user_id": { "type": "string" }
      },
      "required": ["order_id"]
    }
  }'

# Create configs (static variables)
curl -X POST http://localhost:8080/v1/configs $HEADERS \
  -d '{
    "name": "email-service",
    "values": {
      "api_base_url": "https://api.myapp.com",
      "sender": "noreply@myapp.com"
    }
  }'

# Create secrets (encrypted at rest, write-only)
curl -X POST http://localhost:8080/v1/secrets $HEADERS \
  -d '{
    "name": "email_api_key",
    "value": "sk-your-api-key"
  }'
```

### 3. Register — tell Invokr where to deliver

```bash
curl -X POST http://localhost:8080/v1/endpoints $HEADERS \
  -d '{
    "name": "send-welcome-email",
    "type": "HTTP",
    "payload_spec": "order-input",
    "config": "email-service",
    "spec": {
      "url": "{{config.api_base_url}}/emails/welcome",
      "method": "POST",
      "headers": {
        "Authorization": "Bearer {{secret.email_api_key}}",
        "Content-Type": "application/json"
      },
      "body_template": {
        "order_id": "{{input.order_id}}",
        "sender": "{{config.sender}}"
      },
      "timeout_ms": 5000,
      "expected_status_codes": [200, 201, 202, 204]
    },
    "retry_policy": {
      "max_attempts": 3,
      "backoff": "exponential",
      "initial_delay_ms": 1000,
      "max_delay_ms": 30000
    }
  }'
```

Endpoint types: `HTTP`, `KAFKA`, `REDIS_STREAM`. Same template resolution, same retry policy, same guarantees — regardless of transport.

### 4. Invoke — fire it

**Immediate** — fires now:
```bash
curl -X POST http://localhost:8080/v1/jobs $HEADERS \
  -d '{
    "endpoint": "send-welcome-email",
    "trigger": "IMMEDIATE",
    "idempotency_key": "order-1234-welcome",
    "input": { "order_id": "order-1234", "user_id": "u_abc" }
  }'
```

**Delayed** — fires at a specific time:
```bash
curl -X POST http://localhost:8080/v1/jobs $HEADERS \
  -d '{
    "endpoint": "send-welcome-email",
    "trigger": "DELAYED",
    "idempotency_key": "order-1234-reminder",
    "run_at": "2026-03-20T18:00:00Z",
    "input": { "order_id": "order-1234" }
  }'
```

**CRON** — fires on a schedule:
```bash
curl -X POST http://localhost:8080/v1/jobs $HEADERS \
  -d '{
    "endpoint": "send-welcome-email",
    "trigger": "CRON",
    "cron": "0 9 * * MON",
    "timezone": "Asia/Kolkata",
    "input": { "order_id": "all" }
  }'
```

### 5. Observe

```bash
# Job details
curl http://localhost:8080/v1/jobs/{job_id} $HEADERS

# Job health status
curl http://localhost:8080/v1/jobs/{job_id}/status $HEADERS

# List executions
curl http://localhost:8080/v1/jobs/{job_id}/executions $HEADERS

# Execution details
curl http://localhost:8080/v1/executions/{execution_id} $HEADERS

# Attempt history
curl http://localhost:8080/v1/executions/{execution_id}/attempts $HEADERS
```

---

## Using the TypeScript SDK

Invokr generates a TypeScript SDK from Smithy models.

```bash
just build-sdk    # Generate and compile the SDK
just cli-install  # Install CLI deps (links to built SDK)
```

```typescript
import { InvokrServiceClient, CreateJobCommand } from "invokr-sdk";

const client = new InvokrServiceClient({
  endpoint: "http://localhost:8080",
  token: { token: "dev-api-key" },
});

const response = await client.send(
  new CreateJobCommand({
    endpoint: "send-welcome-email",
    trigger: "IMMEDIATE",
    idempotency_key: "order-1234-welcome",
    input: { order_id: "order-1234" },
  }),
);

console.log(response.data.job_id);
```

---

## Template resolution

Endpoint specs support three template namespaces, resolved at execution time:

| Namespace | Source | Example |
|---|---|---|
| `{{input.*}}` | Per-job payload | `{{input.user_id}}` → `"u_abc"` |
| `{{config.*}}` | Centrally managed config | `{{config.api_base_url}}` → `"https://api.myapp.com"` |
| `{{secret.*}}` | Encrypted secret store | `{{secret.email_api_key}}` → resolved at runtime, never exposed |

Configs are cached (60s TTL). Secrets are encrypted at rest, decrypted in memory (300s TTL). Template resolution failures reject the execution immediately — no wasted retries.

---

## Execution lifecycle

```
PENDING ──→ RUNNING ──→ SUCCESS
    │          │
    │          ├──→ RETRYING ──→ RUNNING (next attempt)
    │          │
    │          └──→ FAILED (retries exhausted)
    │
    └──→ CANCELLED
```

- **IMMEDIATE** jobs create an execution as `QUEUED`, picked up immediately by workers.
- **DELAYED** jobs create an execution as `PENDING` with `run_at`. Workers pick it up when `run_at <= now()` — no separate promoter needed.
- **CRON** jobs are registered with pg_cron. Each tick inserts a `QUEUED` execution directly into the database.

### Retry policy

Configurable per endpoint with three backoff strategies:

| Strategy | Formula | Use case |
|----------|---------|----------|
| `fixed` | `initial_delay_ms` | Consistent retry interval |
| `linear` | `initial_delay_ms * attempt` | Gradually increasing delay |
| `exponential` | `initial_delay_ms * 2^(attempt-1)` | Back off quickly under pressure |

All strategies apply ±25% jitter and are capped at `max_delay_ms`.

### CRON versioning

CRON jobs are immutable. Updates create a new version and retire the old one. The full version chain is preserved for audit:

```bash
# Update a CRON job (creates new version)
curl -X PUT http://localhost:8080/v1/jobs/{job_id} $HEADERS \
  -d '{ "cron": "0 */2 * * *", "input": { "mode": "v2" } }'

# View version history
curl http://localhost:8080/v1/jobs/{job_id}/versions $HEADERS
```

---

## Guarantees

| Guarantee | How |
|-----------|-----|
| **Exactly-once** | Idempotency keys + DB unique constraints + `SELECT FOR UPDATE SKIP LOCKED` |
| **Durable** | Every job persisted to PostgreSQL before acknowledgment |
| **Retry with backoff** | Configurable per endpoint: fixed, linear, or exponential with jitter |
| **Sub-second** | Immediate: ~300ms. Delayed: within ~200ms of `run_at` (worker poll interval) |
| **Observable** | Every execution has a lifecycle. Every attempt recorded with duration, output, error |
| **Type-safe** | JSON Schema validation on job input at creation time |
| **Multi-tenant** | Schema-per-workspace isolation. Shared nothing between tenants |

---

## Monitoring

Invokr exposes Prometheus metrics. The API serves metrics at `GET /metrics`, the worker exposes metrics via a separate HTTP listener (default port 9090).

```bash
# Start Prometheus + Grafana
just monitoring-up

# Prometheus: http://localhost:9099
# Grafana:    http://localhost:3001  (admin / invokr)
```

A pre-built Grafana dashboard is included at `monitoring/grafana/dashboards/invokr-platform.json`.

### Key metrics

| Metric | Type | Description |
|--------|------|-------------|
| `invokr_jobs_created_total` | Counter | Jobs created, by trigger type, endpoint, schema |
| `invokr_executions_claimed_total` | Counter | Executions claimed by workers |
| `invokr_executions_completed_total` | Counter | Executions completed, by status (SUCCESS/FAILED) |
| `invokr_execution_duration_seconds` | Histogram | End-to-end execution duration |
| `invokr_dispatch_total` | Counter | Dispatch attempts by endpoint type |
| `invokr_dispatch_duration_seconds` | Histogram | Dispatcher-level latency |
| `invokr_worker_inflight_executions` | Gauge | Currently in-flight executions per worker |
| `invokr_worker_poll_idle_total` | Counter | Idle poll cycles (no work found) |
| `invokr_cron_jobs_reaped_total` | Counter | Expired CRON jobs retired by the reaper, by schema |
| `invokr_kafka_messages_produced_total` | Counter | Kafka messages produced, by status |
| `invokr_redis_stream_messages_sent_total` | Counter | Redis Stream messages sent, by status |

---

## API reference

### Organizations & Workspaces
| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/v1/orgs` | Create organization |
| `GET` | `/v1/orgs` | List organizations |
| `GET` | `/v1/orgs/{org_id}` | Get organization |
| `PUT` | `/v1/orgs/{org_id}` | Update organization |
| `POST` | `/v1/orgs/{org_id}/workspaces` | Create workspace |
| `GET` | `/v1/orgs/{org_id}/workspaces` | List workspaces |
| `GET` | `/v1/orgs/{org_id}/workspaces/{id}` | Get workspace |

### Setup (requires `X-Org-Id` + `X-Workspace-Id` headers)
| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/v1/payload-specs` | Create input schema |
| `GET/PUT/DELETE` | `/v1/payload-specs/{name}` | Manage |
| `POST` | `/v1/configs` | Create config |
| `GET/PUT/DELETE` | `/v1/configs/{name}` | Manage |
| `POST` | `/v1/secrets` | Create secret (write-only) |
| `GET/PUT/DELETE` | `/v1/secrets/{name}` | Manage (value never returned) |

### Endpoints
| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/v1/endpoints` | Register HTTP / Kafka / Redis Stream endpoint |
| `GET/PUT/DELETE` | `/v1/endpoints/{name}` | Manage |

### Jobs
| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/v1/jobs` | Create a job (IMMEDIATE / DELAYED / CRON) |
| `GET` | `/v1/jobs` | List jobs (filterable by endpoint, trigger, status) |
| `GET` | `/v1/jobs/{id}` | Get job details |
| `PUT` | `/v1/jobs/{id}` | Update CRON job (new immutable version) |
| `POST` | `/v1/jobs/{id}/cancel` | Cancel job |
| `GET` | `/v1/jobs/{id}/status` | Job health and stats |
| `GET` | `/v1/jobs/{id}/versions` | Version history (CRON) |
| `GET` | `/v1/jobs/{id}/executions` | List executions |

### Executions
| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/v1/executions/{id}` | Execution details |
| `POST` | `/v1/executions/{id}/cancel` | Cancel execution |
| `GET` | `/v1/executions/{id}/attempts` | Attempt history |
| `GET` | `/v1/executions/{id}/logs` | Structured execution logs |

All list endpoints support cursor-based pagination via `?limit=N&cursor=...`.

---

## Configuration

All configuration is via environment variables prefixed with `INVOKR_`:

| Variable | Default | Description |
|----------|---------|-------------|
| `INVOKR_DATABASE_URL` | *required* | PostgreSQL connection string |
| `INVOKR_LISTEN_ADDR` | `0.0.0.0:8080` | API server bind address |
| `INVOKR_API_KEY` | `dev-api-key` | Bearer token for authentication |
| `INVOKR_ENCRYPTION_KEY` | 64 zeros | AES key for secret encryption (hex, 32+ bytes) |
| `INVOKR_DB_POOL_SIZE` | `50` | Database connection pool size |
| `INVOKR_DB_MIGRATION_MODE` | `none` | What `migrate` does: `none`, `run` or `dry-run` |
| `INVOKR_WORKER_MAX_CONCURRENT` | `50` | Max concurrent job executions per worker |
| `INVOKR_WORKER_POLL_INTERVAL_MS` | `200` | Worker DB polling interval |
| `INVOKR_WORKER_SHUTDOWN_TIMEOUT_SEC` | `30` | Graceful shutdown timeout for in-flight work |
| `INVOKR_CONFIG_CACHE_TTL_SEC` | `60` | Config cache TTL in worker |
| `INVOKR_SECRET_CACHE_TTL_SEC` | `300` | Secret cache TTL in worker |
| `INVOKR_METRICS_PORT` | `9090` | Prometheus metrics HTTP listener port (worker) |
| `INVOKR_PATH_PREFIX` | *(empty)* | URL path prefix for the API server (e.g. `/invokr`) |

### Path prefix

Invokr can be hosted under a URL prefix, useful when running behind a reverse proxy alongside other services.

**API server** — set `INVOKR_PATH_PREFIX` at runtime:

```bash
# All routes are now under /invokr: /invokr/health, /invokr/v1/jobs, etc.
INVOKR_PATH_PREFIX=/invokr just dev
```

When a prefix is configured, hitting `GET /` returns a `302` redirect to `{prefix}/health`.

**Dashboard** — uses compile-time env vars (baked into the WASM binary):

| Variable | Default | Description |
|----------|---------|-------------|
| `INVOKR_DASHBOARD_PATH_PREFIX` | *(empty)* | URL prefix for dashboard routes (e.g. `/dashboard`) |
| `INVOKR_API_BASE_URL` | *(empty)* | Full API base URL including prefix (e.g. `http://localhost:8080/invokr`) |

```bash
# Dashboard at http://localhost:3000/dashboard/, API calls go to http://localhost:8080/invokr/v1/...
INVOKR_DASHBOARD_PATH_PREFIX=/dashboard INVOKR_API_BASE_URL=http://localhost:8080/invokr just dashboard
```

Using `just` with `.env` (since the justfile has `set dotenv-load`):

```env
# .env
INVOKR_PATH_PREFIX=/invokr
INVOKR_DASHBOARD_PATH_PREFIX=/dashboard
INVOKR_API_BASE_URL=http://localhost:8080/invokr
```

```bash
just dev        # API at http://localhost:8080/invokr/...
just dashboard  # Dashboard at http://localhost:3000/dashboard/
```

Without these variables, everything works at the root path as before.

**Note:** When using a path prefix, update monitoring and healthcheck configs to match:

- **Prometheus** (`monitoring/prometheus.yml`): change `metrics_path` from `/metrics` to `/{prefix}/metrics` (e.g. `/invokr/metrics`)
- **Docker healthchecks** (`docker-compose.prod.yml`): change healthcheck URLs from `http://localhost:8080/health` to `http://localhost:8080/{prefix}/health` (e.g. `http://localhost:8080/invokr/health`)

---

## Development

### Just recipes

```bash
just                    # List all recipes
just setup              # One-time setup (DB + migrations + SDK + CLI)
just dev                # Run API + worker + mock-server in parallel

# Individual services
just api                # API server (port 8080)
just worker             # Worker (metrics on :9090)
just mock-server        # Mock HTTP server (port 9999)

# Database
just db-up              # Start PostgreSQL
just db-down            # Stop PostgreSQL
just db-migrate         # Run pending migrations
just db-migrate-dry-run # Show pending migrations without applying them
just migrate-new NAME   # Create a new migration file
just db-reset           # Drop + recreate + migrate
just db-shell           # Open psql shell

# SDK
just smithy-build       # Generate from Smithy models
just build-sdk          # Build TypeScript SDK
just sdk-refresh        # Regenerate + rebuild + reinstall CLI
just cli-install        # Install CLI dependencies

# Tests (integration — requires `just dev` running)
just test-immediate     # Test immediate job execution
just test-delayed       # Test delayed job execution
just test-cron          # Test CRON job execution
just test-e2e           # Full integration test (starts services, runs all tests)
just test-haskell       # Run Haskell SDK example

# Tests (unit — dispatcher tests)
just test-http          # HTTP dispatcher tests (requires mock-server)
just test-kafka         # Kafka dispatcher tests (requires Kafka)
just test-redis         # Redis stream dispatcher tests (requires Redis)
just test-dispatchers   # All dispatcher tests

# Load testing
just load-test 50       # Create 50 jobs of each type and track completion
just load-test-nw 50    # Fire-and-forget (no polling)

# Build
just build              # Build all Rust crates
just build-release      # Release build
just check              # Type-check without building
just lint               # Run clippy
just fmt                # Format code

# Monitoring
just monitoring-up      # Start Prometheus + Grafana
just monitoring-down    # Stop monitoring stack
just all-up             # Start all infrastructure + monitoring
just all-down           # Stop everything

# Dashboard
just dashboard          # Run dashboard dev server (port 3000)
just dashboard-build    # Build WASM dashboard
just dashboard-setup    # Install dashboard build tools

# Infrastructure
just infra-up           # Start all infra (DB + Kafka + Redis)
just infra-down         # Stop all infra
```

### Project structure

```
invokr/
├── crates/
│   ├── common/          # Shared: models, DB, config, tenant, cache, metrics
│   ├── api/             # REST API server (actix-web)
│   ├── worker/          # Job execution engine
│   ├── mock-server/     # Test HTTP server
│   └── dashboard/       # Web UI (Leptos/WASM, excluded from workspace)
├── migrations/          # SQL migration files
├── monitoring/
│   ├── prometheus.yml   # Prometheus scrape config
│   └── grafana/         # Grafana provisioning + dashboards
├── smithy/
│   ├── model/           # Smithy IDL definitions
│   └── smithy-build.json
├── cli/                 # TypeScript CLI for testing (uses generated SDK)
├── haskell-example/     # Example Haskell client
├── nix/                 # Custom Nix derivations (smithy-cli)
├── docker-compose.yml   # PostgreSQL, Kafka (opt), Redis (opt), Prometheus (opt), Grafana (opt)
├── flake.nix            # Nix dev environment
└── justfile             # Task runner
```

### Adding a new endpoint type

The worker dispatches to endpoint types via `crates/worker/src/dispatcher/`. Kafka and Redis Stream are behind feature flags:

```bash
# Build with Kafka support
cargo build --workspace --features invokr-worker/kafka

# Build with Redis Stream support
cargo build --workspace --features invokr-worker/redis-stream
```

To start Kafka or Redis for local dev:

```bash
docker compose --profile kafka up -d
docker compose --profile redis up -d
```

---

## How it works internally

### Worker pipeline

When a worker claims an execution (via `SELECT FOR UPDATE SKIP LOCKED` within a transaction):

1. Load endpoint definition
2. Load config (cached 60s) and secrets (cached 300s, encrypted at rest)
3. Resolve `{{input.*}}`, `{{config.*}}`, `{{secret.*}}` templates
4. If no `body`/`body_template` in spec, inject job `input` as the HTTP body
5. Dispatch to endpoint (HTTP / Kafka / Redis)
6. Record attempt (status, duration, output/error)
7. On success: mark execution `SUCCESS`, commit transaction
8. On failure: compute backoff, mark `RETRYING` (or `FAILED` if retries exhausted), commit transaction

Workers use a semaphore to limit concurrency (default 50). Each poll iteration acquires a permit, iterates all active tenant schemas, and attempts to claim one execution. Idle polls back off to the configured interval (200ms).

### Database-driven scheduling

Instead of a separate scheduler process, Invokr delegates scheduling to PostgreSQL:

- **pg_cron extension** handles CRON job materialization. When a CRON job is created, it's registered with `cron.schedule()`. pg_cron inserts a new `QUEUED` execution row on each tick with an idempotency key (`cron_{job_id}_{epoch_ms}`) to prevent duplicates.
- **Transaction-based pickup** handles DELAYED jobs. The worker's claim query includes `PENDING` status with `run_at <= now()`, so delayed jobs are picked up directly when their time arrives — no promoter loop needed.
- The pickup index covers all three statuses: `WHERE status IN ('QUEUED', 'RETRYING', 'PENDING')`.
