---
id: db-driven-scheduling
title: Database-Driven Scheduling
---

# Database-Driven Scheduling

Invokr has no separate scheduler process. PostgreSQL handles scheduling through two mechanisms: the `pg_cron` extension for CRON materialization, and transaction-based pickup for all job types.

## No separate scheduler process

Traditional job scheduling systems require a dedicated scheduler process that polls for due jobs, materializes CRON ticks, and promotes delayed jobs. Invokr replaces all of this with database-native mechanisms:

| Concern | Traditional Approach | Invokr Approach |
|---------|---------------------|-----------------|
| CRON tick materialization | Scheduler loop (every 1s) | pg_cron extension (native to PostgreSQL) |
| Delayed job promotion | Promoter loop (every 500ms) | Transaction-based pickup (worker reads PENDING directly) |
| Stuck execution recovery | Reclaimer loop (every 30s) | Not needed (executions are transactional) |
| Leader election | Required for scheduler HA | Not needed (pg_cron + SKIP LOCKED handle coordination) |

This removes a class of failure modes: scheduler crashes, missed ticks, double-materialization, and leader election complexity.

## pg_cron extension

The `pg_cron` extension is a PostgreSQL extension that provides cron-based job scheduling natively within the database. Invokr uses it to materialize CRON job executions.

### Installation

The extension is installed via the `20260322000001_pg_cron.sql` migration:

```sql
CREATE EXTENSION IF NOT EXISTS pg_cron;
```

### CRON job registration

When a CRON job is created via `POST /v1/jobs { trigger: CRON }`, the API registers it with pg_cron using `cron.schedule()`:

```sql
SELECT cron.schedule(
    'invokr_{schema_name}_{job_id}',
    '{cron_expression}',
    '{insert_command}'
);
```

The scheduled command inserts a new `QUEUED` execution on each tick:

```sql
INSERT INTO {schema}.executions
    (job_id, endpoint, endpoint_type, idempotency_key, status, input, run_at, max_attempts)
SELECT j.job_id, j.endpoint, j.endpoint_type,
       'cron_' || j.job_id || '_' || (EXTRACT(EPOCH FROM now()) * 1000)::BIGINT,
       'QUEUED', j.input, now(),
       COALESCE((e.retry_policy->>'max_attempts')::BIGINT, 1)
FROM {schema}.jobs j
JOIN {schema}.endpoints e ON e.name = j.endpoint
WHERE j.job_id = '{job_id}' AND j.status = 'ACTIVE'
ON CONFLICT (job_id, idempotency_key) WHERE idempotency_key IS NOT NULL DO NOTHING;
```

### Idempotency key for CRON ticks

Each CRON tick generates an idempotency key with the format `cron_{job_id}_{epoch_ms}`. Combined with the `ON CONFLICT DO NOTHING` clause on the `idx_executions_cron_dedup` unique partial index, this key means that even if pg_cron fires twice in the same millisecond, only one execution is created.

### Existing CRON job migration

The pg_cron migration also migrates existing active CRON jobs to pg_cron. For each active workspace, it iterates all active CRON jobs and registers them with `cron.schedule()`:

```sql
DO $$ DECLARE
    ws RECORD;
    job RECORD;
BEGIN
    FOR ws IN SELECT schema_name FROM public.workspaces WHERE status = 'ACTIVE' LOOP
        FOR job IN EXECUTE format(
            'SELECT job_id, cron_expression, endpoint FROM %I.jobs
             WHERE trigger_type = ''CRON'' AND status = ''ACTIVE''',
            ws.schema_name
        ) LOOP
            PERFORM cron.schedule(
                'invokr_' || ws.schema_name || '_' || job.job_id,
                job.cron_expression,
                '{insert_command}'
            );
        END LOOP;
    END LOOP;
END $$;
```

## Transaction-based pickup

All three trigger types flow through the same pickup mechanism. The worker's claim query handles `QUEUED`, `RETRYING`, and `PENDING` statuses in a single index scan.

### The pickup index

The `idx_executions_pickup` index carries worker claims. It is a partial index that covers only actionable statuses:

```sql
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_executions_pickup
    ON executions (status, run_at ASC)
    WHERE status IN ('QUEUED', 'RETRYING', 'PENDING');
```

:::note
Including `PENDING` is what lets workers pick up delayed jobs directly, without a promoter loop. It used to be added by the `20260322000000_txn_based_pickup.sql` migration, back when `executions` was a single global table; that migration is now a no-op and the widened index is created per workspace by `workspace_v1.sql`.
:::

### SELECT FOR UPDATE SKIP LOCKED

Workers claim executions using `SELECT FOR UPDATE SKIP LOCKED` within a scoped transaction. This gives three properties:

1. **No double-execution**: Once a worker claims an execution, the row is locked until the transaction commits. Other workers skip it.
2. **No blocking**: With `SKIP LOCKED`, workers don't wait for locks; they immediately try the next available row.
3. **Atomicity**: The execution state change (`PENDING/QUEUED/RETRYING` → `RUNNING`) and all subsequent pipeline operations (attempt recording, finalization) happen within the same transaction.

```sql
UPDATE executions
SET status = 'RUNNING',
    worker_id = $1,
    started_at = now(),
    attempt_count = attempt_count + 1
WHERE execution_id = (
    SELECT execution_id
    FROM executions
    WHERE status IN ('QUEUED', 'RETRYING', 'PENDING')
      AND run_at <= now()
    ORDER BY run_at ASC
    LIMIT 1
    FOR UPDATE SKIP LOCKED
)
RETURNING execution_id, job_id, endpoint, endpoint_type, input, attempt_count, max_attempts;
```

## Trigger type flow

All three trigger types converge on the same pickup mechanism:

### IMMEDIATE jobs

1. API creates job + execution as `QUEUED` with `run_at = now()` in a single transaction
2. Worker picks it up immediately (next poll cycle, ~200ms)
3. No scheduling is involved; the execution is immediately actionable

### DELAYED jobs

1. API creates job + execution as `PENDING` with `run_at = {specified_time}`
2. Worker's claim query includes `PENDING` status with `run_at <= now()` condition
3. When `run_at` arrives, the execution becomes claimable — no promoter needed
4. The worker effectively "promotes" the job by claiming it directly from `PENDING` to `RUNNING`

### CRON jobs

1. API creates job (`ACTIVE`) and registers with pg_cron via `cron.schedule()`
2. On each CRON tick, pg_cron inserts a new `QUEUED` execution with idempotency key `cron_{job_id}_{epoch_ms}`
3. Worker picks up the `QUEUED` execution via normal `SKIP LOCKED` path
4. Repeats until the job is cancelled or its `cron_ends_at` window expires

```
IMMEDIATE:  API → QUEUED execution → Worker claims → RUNNING
DELAYED:    API → PENDING execution (run_at) → Worker claims when run_at <= now() → RUNNING
CRON:       API → pg_cron schedule → pg_cron tick → QUEUED execution → Worker claims → RUNNING
```

## Multi-instance safety

Multiple worker instances can run simultaneously without coordination:

| Mechanism | Safety Guarantee |
|-----------|-----------------|
| `SKIP LOCKED` | Only one worker claims each execution |
| pg_cron + `ON CONFLICT DO NOTHING` | Only one execution per CRON tick, even if pg_cron fires twice |
| Transactional state changes | Execution status changes are atomic — no partial states |

No leader election, distributed locks, or coordination service is required. The database handles concurrency.

:::tip
`FOR UPDATE SKIP LOCKED` is a well-established approach for work queues in PostgreSQL. Workers never block each other: they skip locked rows and try the next one.
:::

## Related pages

- [Worker Pipeline](./worker-pipeline) — How the poller claims and processes executions
- [Exactly-Once Guarantees](./exactly-once) — How idempotency keys and unique constraints prevent duplicates
- [Database Schema](./database-schema) — Full schema layout including the pickup index
- [Reaper](./reaper) — How expired CRON jobs are retired and unscheduled
