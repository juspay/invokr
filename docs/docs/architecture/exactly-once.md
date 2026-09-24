---
id: exactly-once
title: Exactly-Once Guarantees
---

# Exactly-Once Guarantees

Invokr provides exactly-once execution semantics through durability, idempotency keys, database unique constraints, and transaction-based claiming. Every job fires exactly once, with no duplicates and no missed executions, even under crashes and concurrent access.

## Durability

Every job is persisted to PostgreSQL before the API acknowledges the request. The job and its initial execution are inserted in a single database transaction:

```sql
BEGIN;
INSERT INTO jobs (endpoint, endpoint_type, trigger_type, idempotency_key, input)
VALUES ($1, $2, 'IMMEDIATE', $3, $4)
RETURNING job_id;

INSERT INTO executions (job_id, endpoint, endpoint_type, idempotency_key, status, run_at, input, max_attempts)
VALUES ($5, $1, $2, $3, 'QUEUED', now(), $4, $6)
RETURNING execution_id, status, created_at;
COMMIT;
```

If the transaction commits, the job is durable. If the process crashes before the response is sent, the client can retry with the same idempotency key and get the original result. If the transaction rolls back, no partial state exists.

## Exactly-once execution

Exactly-once rests on three layers:

### 1. Idempotency keys + unique constraints

Every job creation requires (or generates) an idempotency key. The `idx_jobs_idempotency` unique partial index prevents duplicate job creation:

```sql
CREATE UNIQUE INDEX IF NOT EXISTS idx_jobs_idempotency
    ON jobs (endpoint, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
```

| Trigger Type | Key Provided By | Example |
|-------------|-----------------|---------|
| `IMMEDIATE` | Client | `order-1234-welcome-email` |
| `DELAYED` | Client | `order-1234-reminder` |
| `CRON` | System | `cron_{job_id}_{epoch_ms}` |

For CRON ticks, the system generates the key as `cron_{job_id}_{epoch_ms}`, where `epoch_ms` is the current Unix timestamp in milliseconds. Each tick therefore produces a unique key, and the unique index prevents duplicate ticks within the same millisecond.

### 2. Execution-level deduplication

The `idx_executions_cron_dedup` unique partial index prevents duplicate executions for the same job + idempotency key combination:

```sql
CREATE UNIQUE INDEX IF NOT EXISTS idx_executions_cron_dedup
    ON executions (job_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
```

The CRON tick insert uses `ON CONFLICT DO NOTHING` to silently ignore duplicate ticks:

```sql
INSERT INTO executions (...)
VALUES (...)
ON CONFLICT (job_id, idempotency_key) WHERE idempotency_key IS NOT NULL DO NOTHING;
```

### 3. SKIP LOCKED for claiming

The worker claims executions using `SELECT FOR UPDATE SKIP LOCKED` within a transaction. Once a worker claims an execution, no other worker can claim it:

- The row lock is held until the transaction commits
- Other workers skip the locked row and try the next one
- The execution transitions from `QUEUED`/`PENDING`/`RETRYING` → `RUNNING` atomically within the claim

## Immutability

### CRON job immutability

CRON jobs are immutable. Updates create a new version and retire the old one, linked via `previous_version_id`:

```
job_abc (v1, ACTIVE)
    └──→ PUT /v1/jobs/job_abc
            └──→ job_def (v2, ACTIVE)  ← previous_version_id: job_abc
                     job_abc (v1, RETIRED)
```

The full version chain is preserved for audit. The `GET /v1/jobs/{job_id}/versions` endpoint returns the complete chain, newest to oldest.

One-shot jobs (`IMMEDIATE`, `DELAYED`) are also immutable — they fire once and complete. Attempting to update them returns `409 JOB_NOT_UPDATABLE`.

### Version chain schema

The `jobs` table tracks the version chain through two columns:

| Column | Type | Description |
|--------|------|-------------|
| `version` | `BIGINT` | Version number (starts at 1, incremented on update) |
| `previous_version_id` | `TEXT` | Job ID of the previous version (NULL for v1) |
| `replaced_by_id` | `TEXT` | Job ID of the new version (NULL if not replaced) |
| `status` | `TEXT` | `ACTIVE` or `RETIRED` (old version is retired when a new one is created) |

## Duplicate request handling

When a client sends a job creation request with an idempotency key that already exists, the API returns the existing entity with `200 OK` instead of `201 Created`:

| Scenario | HTTP Status | Response |
|----------|-------------|----------|
| New job created | `201 Created` | Full job + execution resource |
| Duplicate (same idempotency key) | `200 OK` | Existing job + execution resource |
| Duplicate CRON tick (same epoch_ms) | Silently ignored | `ON CONFLICT DO NOTHING` |

Clients can safely retry on network failures without creating duplicate jobs.

## Transaction boundaries

All execution state changes are atomic. The worker pipeline operates within a single scoped transaction per execution:

```
BEGIN (scoped to workspace schema)
  → Claim execution (SKIP LOCKED)         — status: QUEUED → RUNNING
  → Load endpoint
  → Load config (cached)
  → Load secrets (cached, decrypt)
  → Resolve templates
  → Dispatch to endpoint
  → Record attempt
  → Finalize execution                    — status: RUNNING → SUCCESS / RETRYING / FAILED
COMMIT
```

If any step fails, the entire transaction can roll back — the execution stays in its previous state and can be retried. If the transaction commits, all state changes (claim, attempt record, execution finalization, execution logs) are applied atomically.

:::info
The reaper also operates within this transaction boundary. When the reaper retires expired CRON jobs and unschedules their pg_cron entries, those changes commit atomically with the reaper execution's outcome. See [Reaper](./reaper) for details.
:::

## Guarantee summary

| Guarantee | Mechanism |
|-----------|----------|
| **Durability** | Every job + execution persisted to PostgreSQL before API acknowledgment |
| **Exactly-once creation** | `idx_jobs_idempotency` unique partial index on `(endpoint, idempotency_key)` |
| **Exactly-once CRON tick** | `idx_executions_cron_dedup` unique partial index on `(job_id, idempotency_key)` + `ON CONFLICT DO NOTHING` |
| **Exactly-once execution** | `SELECT FOR UPDATE SKIP LOCKED` within a transaction |
| **Atomic state transitions** | All execution state changes within a single transaction |
| **Immutability** | CRON jobs updated via new versions, old versions retired |
| **Safe retries** | Duplicate requests return existing entity with `200 OK` |
| **Crash recovery** | Uncommitted transactions roll back; committed jobs survive |

## Related pages

- [Database-Driven Scheduling](./db-driven-scheduling) — How pg_cron and SKIP LOCKED enable scheduling without a separate process
- [Worker Pipeline](./worker-pipeline) — The execution pipeline that processes claimed executions
- [Database Schema](./database-schema) — Full schema layout including all unique indexes
