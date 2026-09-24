---
id: quickstart
title: Quickstart
---

# Quickstart

This guide gets Invokr running locally and fires your first job end to end. It uses **service mode** — Invokr as a standalone REST API. To embed Invokr directly in a Rust application instead, see [Library Mode Setup](./deployment/library-mode).

---

## Prerequisites

- [Nix](https://nixos.org/download) with flakes enabled
- [Docker](https://docs.docker.com/get-docker/) (for PostgreSQL)

:::tip
No Nix? You can still run everything by hand — see [manual setup](#setup-manual) below. We lean on Nix because it pins every dependency for you, so the dev shell comes out the same on every machine.
:::

---

## Setup with `just`

The quickest path is the `just` task runner inside the Nix dev shell:

```bash
# Enter the dev shell (installs Rust, Node.js, smithy-cli, just, trunk, etc.)
nix develop

# One-time setup: start DB, run migrations, build SDK, install CLI deps
just setup

# Run all services (API + worker + mock-server)
just dev
```

The API is now running at `http://localhost:8080`.

---

## Setup (manual) {#setup-manual}

If you'd rather drive each step yourself (e.g. to run with a path prefix and the dashboard), the flow below mirrors what `just setup`/`just dev` automate. It assumes the Postgres container from `docker compose` is up and named `invokr-postgres-1`, with host port **5434** mapped to the container's `5432` (see `docker-compose.yml`).

```bash
# Start PostgreSQL
docker compose up -d postgres
```

**1. (Re)create the database.** Connect to the default `postgres` database and drop/recreate `invokr_db` for a clean slate:

```bash
docker exec -i invokr-postgres-1 psql -U invokr -d postgres -c \
  "DROP DATABASE IF EXISTS invokr_db WITH (FORCE);"
docker exec -i invokr-postgres-1 psql -U invokr -d postgres -c \
  "CREATE DATABASE invokr_db;"
```

**2. Apply migrations** in order:

```bash
for f in migrations/20260317000000_initial.sql \
         migrations/20260318000000_multi_tenancy.sql \
         migrations/20260322000000_txn_based_pickup.sql \
         migrations/20260322000001_pg_cron.sql; do
  echo ">> applying $f"
  docker exec -i invokr-postgres-1 psql -U invokr -d invokr_db -v ON_ERROR_STOP=1 < "$f"
done
```

**3. Run the API server** (here in `both` mode, serving the dashboard under `/dashboard` and the API under `/api`, on port 8090):

```bash
INVOKR_DATABASE_URL="postgres://invokr:invokr@localhost:5434/invokr_db" \
INVOKR_LISTEN_ADDR="0.0.0.0:8090" \
INVOKR_MODE="both" \
INVOKR_PATH_PREFIX="/api" \
INVOKR_DASHBOARD_PATH_PREFIX="/dashboard" \
INVOKR_DASHBOARD_DIST_DIR="crates/dashboard/pkg" \
cargo run -p invokr-api
```

:::warning
Building the dashboard bundle first (`just dashboard-build`) is required for `INVOKR_MODE=both` to serve `crates/dashboard/pkg`.
:::

**4. Run the worker** in a separate shell:

```bash
INVOKR_DATABASE_URL="postgres://invokr:invokr@localhost:5434/invokr_db" \
INVOKR_METRICS_PORT="9090" \
cargo run -p invokr-worker
```

---

## Verify

```bash
# `just dev` (root path):
curl http://localhost:8080/health
# OK

# manual setup above (path prefix /api on port 8090):
curl http://localhost:8090/api/health
# OK
```

---

## End-to-end example

All endpoints require `Authorization: Bearer <api_key>` (default: `dev-api-key`).

Tenant-scoped endpoints (everything except orgs/workspaces) also require:
- `X-Org-Id: <org_id>`
- `X-Workspace-Id: <workspace_id>`

### 1. Create an organization and workspace

```bash
# Create an organization
curl -X POST http://localhost:8080/v1/orgs \
  -H "Authorization: Bearer dev-api-key" \
  -H "Content-Type: application/json" \
  -d '{ "name": "My Company", "slug": "my-company" }'
```

Response (`201 Created`):

```json
{
  "org_id": "550e8400-e29b-41d4-a716-446655440000",
  "name": "My Company",
  "slug": "my-company",
  "created_at": "2026-03-15T10:00:00Z"
}
```

```bash
# Create a workspace within the org
curl -X POST http://localhost:8080/v1/orgs/{org_id}/workspaces \
  -H "Authorization: Bearer dev-api-key" \
  -H "Content-Type: application/json" \
  -d '{ "name": "Production", "slug": "production" }'
```

Response (`201 Created`):

```json
{
  "workspace_id": "660e8400-e29b-41d4-a716-446655440000",
  "org_id": "550e8400-e29b-41d4-a716-446655440000",
  "name": "Production",
  "slug": "production",
  "schema_name": "my_company_production",
  "created_at": "2026-03-15T10:00:01Z"
}
```

:::note
Save the `org_id` and `workspace_id` from these responses — you'll need them for all subsequent requests via the `X-Org-Id` and `X-Workspace-Id` headers.
:::

### 2. Define input contracts, configs, and secrets

For all subsequent requests, include tenant headers:

```bash
-H "Authorization: Bearer dev-api-key"
-H "X-Org-Id: <org_id>"
-H "X-Workspace-Id: <workspace_id>"
-H "Content-Type: application/json"
```

**Create a payload spec** (JSON Schema for input validation):

```bash
curl -X POST http://localhost:8080/v1/payload-specs \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
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
```

Response (`201 Created`):

```json
{
  "name": "order-input",
  "schema": {
    "type": "object",
    "properties": {
      "order_id": { "type": "string" },
      "user_id": { "type": "string" }
    },
    "required": ["order_id"]
  },
  "created_at": "2026-03-15T10:00:00Z",
  "updated_at": "2026-03-15T10:00:00Z"
}
```

**Create a config** (static variables):

```bash
curl -X POST http://localhost:8080/v1/configs \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "email-service",
    "values": {
      "api_base_url": "https://api.myapp.com",
      "sender": "noreply@myapp.com"
    }
  }'
```

Response (`201 Created`):

```json
{
  "name": "email-service",
  "values": {
    "api_base_url": "https://api.myapp.com",
    "sender": "noreply@myapp.com"
  },
  "created_at": "2026-03-15T10:00:00Z",
  "updated_at": "2026-03-15T10:00:00Z"
}
```

**Create a secret** (encrypted at rest, write-only):

```bash
curl -X POST http://localhost:8080/v1/secrets \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "email_api_key",
    "value": "sk-your-api-key"
  }'
```

Response (`201 Created` — value is never returned):

```json
{
  "name": "email_api_key",
  "created_at": "2026-03-15T10:00:00Z",
  "updated_at": "2026-03-15T10:00:00Z"
}
```

### 3. Register an HTTP endpoint

Tell Invokr where to deliver:

```bash
curl -X POST http://localhost:8080/v1/endpoints \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
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

Response (`201 Created`):

```json
{
  "name": "send-welcome-email",
  "type": "HTTP",
  "payload_spec": "order-input",
  "config": "email-service",
  "spec": { ... },
  "retry_policy": { ... },
  "created_at": "2026-03-15T10:00:00Z",
  "updated_at": "2026-03-15T10:00:00Z"
}
```

:::info
Whatever the transport, `HTTP`, `KAFKA`, and `REDIS_STREAM` endpoints share the same template resolution, retry policy, and delivery guarantees.
:::

### 4. Fire an immediate job

```bash
curl -X POST http://localhost:8080/v1/jobs \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
  -d '{
    "endpoint": "send-welcome-email",
    "trigger": "IMMEDIATE",
    "idempotency_key": "order-1234-welcome",
    "input": { "order_id": "order-1234", "user_id": "u_abc" }
  }'
```

Response (`201 Created`):

```json
{
  "job_id": "job_8f3a...",
  "endpoint": "send-welcome-email",
  "endpoint_type": "HTTP",
  "trigger": "IMMEDIATE",
  "status": "ACTIVE",
  "version": 1,
  "idempotency_key": "order-1234-welcome",
  "input": { "order_id": "order-1234", "user_id": "u_abc" },
  "execution": {
    "execution_id": "exec_2b7c...",
    "status": "QUEUED",
    "created_at": "2026-03-15T10:00:00Z"
  },
  "created_at": "2026-03-15T10:00:00Z"
}
```

### 5. Observe the execution

```bash
# Job details
curl http://localhost:8080/v1/jobs/{job_id} \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>"

# Job health status
curl http://localhost:8080/v1/jobs/{job_id}/status \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>"

# List executions
curl http://localhost:8080/v1/jobs/{job_id}/executions \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>"

# Execution details
curl http://localhost:8080/v1/executions/{execution_id} \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>"

# Attempt history
curl http://localhost:8080/v1/executions/{execution_id}/attempts \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>"
```

:::tip
For local testing without an external API, use the bundled mock HTTP server (`just mock-server`, port 9999). Set your endpoint's `url` to `http://localhost:9999/success` to simulate a successful response, or `http://localhost:9999/fail` to trigger a 500.
:::

---

## Next steps

- [Core Concepts](./core-concepts/overview) — understand the three-step workflow
- [Jobs](./core-concepts/jobs) — trigger types and the job lifecycle
- [Executions](./core-concepts/executions) — execution state machine and retries
- [API Reference](./api/invokr/invokr-api) — full API documentation
