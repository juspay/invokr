---
id: database-schema
title: Database Schema
---

# Database Schema

Invokr uses a schema-per-tenant architecture backed by PostgreSQL. Each workspace gets its own isolated schema with a complete set of tables for job scheduling and execution. Shared tables that manage tenant discovery live in the `public` schema.

## Schema-per-tenant architecture

```
public schema:        organizations, workspaces
tenant schema:        payload_specs, configs, secrets, endpoints,
(org_workspace):      jobs, executions, attempts, execution_logs
```

### Schema naming

Schema names are derived from the org ID and workspace slug:

```rust
pub fn build_schema_name(org_id: &str, workspace_slug: &str) -> String {
    format!(
        "{}_{}",
        org_id.replace('-', "_"),
        workspace_slug.replace('-', "_")
    )
}
```

Hyphens are replaced with underscores because PostgreSQL schema names cannot contain hyphens. Slug validation ensures slugs are lowercase alphanumeric with interior hyphens only:

```rust
pub fn validate_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > MAX_SLUG_LEN { return false; }  // MAX_SLUG_LEN = 25
    if slug.starts_with('-') || slug.ends_with('-') { return false; }
    slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}
```

## Public schema tables

The `public` schema contains two tables for multi-tenant management:

### organizations

```sql
CREATE TABLE public.organizations (
    org_id      TEXT        NOT NULL DEFAULT gen_random_uuid()::TEXT,
    name        TEXT        NOT NULL,
    slug        TEXT        NOT NULL UNIQUE,
    status      TEXT        NOT NULL DEFAULT 'ACTIVE',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_organizations PRIMARY KEY (org_id),
    CONSTRAINT chk_org_status CHECK (status IN ('ACTIVE', 'SUSPENDED', 'DELETED'))
);
```

### workspaces

```sql
CREATE TABLE public.workspaces (
    workspace_id    TEXT        NOT NULL DEFAULT gen_random_uuid()::TEXT,
    org_id          TEXT        NOT NULL,
    name            TEXT        NOT NULL,
    slug            TEXT        NOT NULL,
    schema_name     TEXT        NOT NULL UNIQUE,
    status          TEXT        NOT NULL DEFAULT 'ACTIVE',
    schema_version  BIGINT      NOT NULL DEFAULT 0,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_workspaces PRIMARY KEY (workspace_id),
    CONSTRAINT fk_workspaces_org FOREIGN KEY (org_id) REFERENCES public.organizations (org_id),
    CONSTRAINT uq_workspace_slug UNIQUE (org_id, slug),
    CONSTRAINT chk_ws_status CHECK (status IN ('ACTIVE', 'SUSPENDED', 'DELETED'))
);
```

The `schema_name` column stores the PostgreSQL schema name for the workspace. The `SchemaRegistry` queries this table to discover active schemas:

```sql
SELECT schema_name FROM public.workspaces WHERE status = 'ACTIVE'
```

## Per-workspace schema tables

Each workspace schema contains seven tables. The schema below shows the unprefixed version (table prefix empty). When a prefix is used, all table names and constraint names are prefixed (see [Table Prefix System](#table-prefix-system)).

### payload_specs

Defines JSON Schema input contracts for endpoints:

```sql
CREATE TABLE payload_specs (
    name          TEXT        NOT NULL,
    schema_json   JSONB       NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_payload_specs PRIMARY KEY (name)
);
```

### configs

Key-value static variables resolved at execution time via `{{config.*}}`:

```sql
CREATE TABLE configs (
    name          TEXT        NOT NULL,
    values_json   JSONB       NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_configs PRIMARY KEY (name)
);
```

### secrets

Encrypted secret values, never returned in API responses:

```sql
CREATE TABLE secrets (
    name              TEXT        NOT NULL,
    encrypted_value   BYTEA       NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_secrets PRIMARY KEY (name)
);
```

### endpoints

Registered delivery targets (HTTP, Kafka, Redis Stream, Internal):

```sql
CREATE TABLE endpoints (
    name              TEXT        NOT NULL,
    endpoint_type     TEXT        NOT NULL,
    payload_spec_ref  TEXT,
    config_ref        TEXT,
    spec              JSONB       NOT NULL,
    retry_policy      JSONB,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_endpoints PRIMARY KEY (name),
    CONSTRAINT fk_endpoints_payload_spec FOREIGN KEY (payload_spec_ref) REFERENCES payload_specs (name),
    CONSTRAINT fk_endpoints_config FOREIGN KEY (config_ref) REFERENCES configs (name),
    CONSTRAINT chk_endpoint_type CHECK (endpoint_type IN ('HTTP', 'KAFKA', 'REDIS_STREAM', 'INTERNAL'))
);

CREATE INDEX idx_endpoints_type ON endpoints (endpoint_type);
```

:::note
The `INTERNAL` endpoint type is only available in per-workspace schemas (via `workspace_v1.sql`). The initial migration (`20260317000000_initial.sql`) does not include it.
:::

### jobs

Job definitions with full CRON versioning support:

```sql
CREATE TABLE jobs (
    job_id                TEXT        NOT NULL DEFAULT gen_random_uuid()::TEXT,
    endpoint              TEXT        NOT NULL,
    endpoint_type         TEXT        NOT NULL,
    trigger_type          TEXT        NOT NULL,
    status                TEXT        NOT NULL DEFAULT 'ACTIVE',
    version               BIGINT      NOT NULL DEFAULT 1,
    previous_version_id   TEXT,
    replaced_by_id        TEXT,
    idempotency_key       TEXT,
    input                 JSONB,
    run_at                TIMESTAMPTZ,
    cron_expression       TEXT,
    cron_timezone         TEXT,
    cron_starts_at        TIMESTAMPTZ,
    cron_ends_at          TIMESTAMPTZ,
    cron_next_run_at      TIMESTAMPTZ,
    cron_last_tick_at     TIMESTAMPTZ,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    retired_at            TIMESTAMPTZ,
    CONSTRAINT pk_jobs PRIMARY KEY (job_id),
    CONSTRAINT fk_jobs_endpoint FOREIGN KEY (endpoint) REFERENCES endpoints (name),
    CONSTRAINT chk_trigger_type CHECK (trigger_type IN ('IMMEDIATE', 'DELAYED', 'CRON')),
    CONSTRAINT chk_job_status CHECK (status IN ('ACTIVE', 'RETIRED')),
    CONSTRAINT chk_job_endpoint_type CHECK (endpoint_type IN ('HTTP', 'KAFKA', 'REDIS_STREAM', 'INTERNAL'))
);
```

### executions

Individual execution instances with full lifecycle tracking:

```sql
CREATE TABLE executions (
    execution_id    TEXT        NOT NULL DEFAULT gen_random_uuid()::TEXT,
    job_id          TEXT        NOT NULL,
    endpoint        TEXT        NOT NULL,
    endpoint_type   TEXT        NOT NULL,
    idempotency_key TEXT,
    status          TEXT        NOT NULL DEFAULT 'PENDING',
    input           JSONB,
    output          JSONB,
    attempt_count   BIGINT      NOT NULL DEFAULT 0,
    max_attempts    BIGINT      NOT NULL DEFAULT 1,
    worker_id       TEXT,
    run_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at      TIMESTAMPTZ,
    completed_at    TIMESTAMPTZ,
    duration_ms     BIGINT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_executions PRIMARY KEY (execution_id),
    CONSTRAINT fk_executions_job FOREIGN KEY (job_id) REFERENCES jobs (job_id),
    CONSTRAINT chk_exec_status CHECK (status IN (
        'PENDING', 'QUEUED', 'RUNNING', 'RETRYING', 'SUCCESS', 'FAILED', 'CANCELLED'
    ))
);
```

### attempts

Individual retry attempts within an execution:

```sql
CREATE TABLE attempts (
    attempt_id      TEXT        NOT NULL DEFAULT gen_random_uuid()::TEXT,
    execution_id    TEXT        NOT NULL,
    attempt_number  BIGINT      NOT NULL,
    status          TEXT        NOT NULL,
    started_at      TIMESTAMPTZ NOT NULL,
    completed_at    TIMESTAMPTZ,
    duration_ms     BIGINT,
    output          JSONB,
    error           JSONB,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_attempts PRIMARY KEY (attempt_id),
    CONSTRAINT fk_attempts_execution FOREIGN KEY (execution_id) REFERENCES executions (execution_id),
    CONSTRAINT uq_attempts_exec_number UNIQUE (execution_id, attempt_number),
    CONSTRAINT chk_attempt_status CHECK (status IN ('SUCCESS', 'FAILED'))
);
```

### execution_logs

Structured execution logs for observability:

```sql
CREATE TABLE execution_logs (
    log_id          TEXT        NOT NULL DEFAULT gen_random_uuid()::TEXT,
    execution_id    TEXT        NOT NULL,
    attempt_number  BIGINT      NOT NULL,
    level           TEXT        NOT NULL,
    message         TEXT        NOT NULL,
    logged_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_execution_logs PRIMARY KEY (log_id),
    CONSTRAINT fk_logs_execution FOREIGN KEY (execution_id) REFERENCES executions (execution_id),
    CONSTRAINT chk_log_level CHECK (level IN ('DEBUG', 'INFO', 'WARN', 'ERROR'))
);
```

## Table prefix system

When Invokr tables share a PostgreSQL schema with other application tables, a table prefix prevents name collisions. The `tbl()` function in `DbContext` applies the prefix to all table references.

### How it works

The `workspace_v1.sql` template uses `{p}` as a placeholder:

```sql
CREATE TABLE IF NOT EXISTS {p}jobs (
    job_id  TEXT  NOT NULL DEFAULT gen_random_uuid()::TEXT,
    ...
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_{p}jobs_idempotency
    ON {p}jobs (endpoint, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
```

At provisioning time, `{p}` is replaced:

| Prefix | `{p}` Replacement | Example Table Name |
|--------|-------------------|-------------------|
| `""` (empty) | `""` | `jobs` |
| `sched` | `sched_` | `sched_jobs` |

```rust
let p = if prefix.is_empty() { String::new() }
        else { format!("{}_", prefix) };
let ddl = TEMPLATE.replace("{p}", &p);
```

:::info
The table prefix is validated to contain only alphanumeric characters and underscores. An empty prefix is valid and produces unprefixed table names.
:::

## Key indexes

### idx_executions_pickup (worker hot path)

This is the most heavily used index: the worker's `SKIP LOCKED` claim query scans it on every poll cycle.

```sql
CREATE INDEX idx_executions_pickup
    ON executions (status, run_at ASC)
    WHERE status IN ('QUEUED', 'RETRYING', 'PENDING');
```

This is a **partial index**: it only indexes rows in actionable statuses. The `run_at ASC` ordering claims the oldest actionable execution first (FIFO within each status group).

:::warning
Including `PENDING` is what enables transaction-based pickup for delayed jobs. It used to be added by the `20260322000000_txn_based_pickup.sql` migration, back when `executions` was a single global table; that migration is now a no-op and this index ships as part of the per-workspace `workspace_v1.sql` template.
:::

### idx_jobs_idempotency (job dedup)

Prevents duplicate job creation for the same endpoint + idempotency key:

```sql
CREATE UNIQUE INDEX idx_jobs_idempotency
    ON jobs (endpoint, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
```

This is a **unique partial index**: it only enforces uniqueness when `idempotency_key` is not NULL, so jobs without idempotency keys can coexist.

### idx_executions_cron_dedup (CRON tick dedup)

Prevents duplicate CRON tick executions:

```sql
CREATE UNIQUE INDEX idx_executions_cron_dedup
    ON executions (job_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
```

The CRON tick insert uses `ON CONFLICT (job_id, idempotency_key) WHERE idempotency_key IS NOT NULL DO NOTHING` to silently ignore duplicate ticks.

### Other indexes

| Index | Table | Purpose |
|-------|-------|---------|
| `idx_jobs_cron_due` | `jobs` | Find CRON jobs due for tick (`WHERE trigger_type = 'CRON' AND status = 'ACTIVE'`) |
| `idx_jobs_endpoint` | `jobs` | List jobs by endpoint (`endpoint, created_at DESC`) |
| `idx_jobs_status` | `jobs` | List jobs by status (`status, created_at DESC`) |
| `idx_executions_by_job` | `executions` | List executions for a job (`job_id, created_at DESC`) |
| `idx_executions_running` | `executions` | Find running executions (`WHERE status = 'RUNNING'`) |
| `idx_attempts_by_execution` | `attempts` | List attempts for an execution (`execution_id, attempt_number ASC`) |
| `idx_logs_by_execution` | `execution_logs` | List logs for an execution (`execution_id, logged_at ASC`) |
| `idx_logs_by_attempt` | `execution_logs` | List logs by attempt (`execution_id, attempt_number, logged_at ASC`) |

## Migration files

Migrations are applied in order to the `invokr_db` database:

| Migration | Description |
|-----------|-------------|
| `20260317000000_initial.sql` | Initial schema: all core tables (single-tenant), indexes, region tables |
| `20260318000000_multi_tenancy.sql` | Adds `public.organizations` and `public.workspaces` tables |
| `20260322000000_txn_based_pickup.sql` | Superseded — no-op. Widened `idx_executions_pickup` back when `executions` was global; `workspace_v1.sql` now creates it per workspace |
| `20260322000001_pg_cron.sql` | Installs pg_cron extension, migrates existing CRON jobs to pg_cron |
| `workspace_v1.sql` | Template applied per-workspace at creation time (not a migration) |

### Applying migrations

```bash
for f in migrations/20260317000000_initial.sql \
         migrations/20260318000000_multi_tenancy.sql \
         migrations/20260322000000_txn_based_pickup.sql \
         migrations/20260322000001_pg_cron.sql; do
  docker exec -i invokr-postgres-1 psql -U invokr -d invokr_db -v ON_ERROR_STOP=1 < "$f"
done
```

Or via the justfile:

```bash
just db-migrate    # Run migrations
just db-reset      # Drop + recreate + migrate
```

## workspace_v1.sql template

The `workspace_v1.sql` file is a **template**, not a migration. It is applied to each new workspace schema at creation time and contains all seven per-workspace tables with the `{p}` placeholder for table prefixing.

Key differences from the initial migration:

1. **`INTERNAL` endpoint type**: The CHECK constraint includes `INTERNAL` alongside `HTTP`, `KAFKA`, `REDIS_STREAM`
2. **`{p}` placeholder**: All table names and constraint names use `{p}` for prefix support
3. **No region tables**: `region_heartbeats` and `region_status` are only in the initial migration (public schema)

### Scoped connections

Workspace-scoped operations use `scoped_connection` or `scoped_transaction`, which set PostgreSQL's `search_path` to the workspace schema:

```rust
pub async fn scoped_transaction<'a>(
    pool: &'a PgPool,
    schema_name: &str,
) -> Result<sqlx::Transaction<'a, sqlx::Postgres>, sqlx::Error> {
    assert!(validate_schema_name(schema_name));
    let mut tx = pool.begin().await?;
    let set_path = format!("SET search_path TO \"{}\", public", schema_name);
    sqlx::query(&set_path).execute(&mut *tx).await?;
    Ok(tx)
}
```

Within the transaction, unqualified table names then resolve to the workspace schema first, falling back to `public`. Schema names are validated to prevent SQL injection via `search_path` manipulation.

:::danger
Schema names are validated by `validate_schema_name()` which ensures only alphanumeric characters and underscores. This is critical because schema names are interpolated into `SET search_path` commands. Never bypass this validation.
:::

## Related pages

- [Architecture Overview](./overview) — How the schema fits into the overall system
- [Exactly-Once Guarantees](./exactly-once) — How the unique indexes ensure deduplication
- [Database-Driven Scheduling](./db-driven-scheduling) — How the pickup index enables transaction-based scheduling
- [Dual Deployment Modes](./dual-deployment) — How the table prefix system supports embedding
