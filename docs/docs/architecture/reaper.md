---
id: reaper
title: Reaper
---

# Reaper

The reaper is Invokr's internal garbage collector for expired CRON jobs. It retires CRON jobs whose `cron_ends_at` window has passed and unschedules their pg_cron entries, preventing them from firing no-op inserts forever.

## What Is the Reaper?

pg_cron drives CRON execution materialization, but it has no concept of a job's `cron_ends_at` window — left alone it fires forever. While the `cron_ends_at` guard in the pg_cron command stops *new executions* past the window (the `WHERE j.status = 'ACTIVE'` clause in the insert command returns no rows), the reaper handles the *lifecycle*: it periodically flips expired CRON jobs to `RETIRED` and removes their pg_cron entry so they stop firing no-op inserts entirely.

## Dogfooded as a Invokr INTERNAL CRON Job

The reaper is itself a Invokr job. Instead of running as a hidden tokio interval task inside each worker pod, it runs through the same execution pipeline as any user-created job:

1. Each workspace is provisioned at creation time with an `INTERNAL` endpoint named `invokr.reaper` and a CRON job
2. The CRON schedule is controlled by `INVOKR_REAPER_CRON_EXPRESSION` (default: `*/15 * * * *`)
3. pg_cron ticks materialize an execution into the workspace's own `executions` table
4. The worker claims it via the normal `SKIP LOCKED` path
5. The `INTERNAL` dispatcher's `reaper` task calls `reap_schema()`

This means the reaper gets for free:

- **Attempts**: Every sweep is recorded as an attempt with status, duration, and output
- **Retries**: Failed sweeps retry per the endpoint's retry policy
- **Metrics**: Reaper executions emit the same Prometheus metrics as any other execution
- **Dashboard rows**: Reaper executions are visible in the workspace's dashboard
- **Execution logs**: Structured logs for each sweep

## Configuration

The reaper's schedule is read from `INVOKR_REAPER_CRON_EXPRESSION` at workspace creation time and baked into the workspace's pg_cron entry:

| Variable | Default | Description |
|----------|---------|-------------|
| `INVOKR_REAPER_CRON_EXPRESSION` | `*/15 * * * *` | 5-field pg_cron expression controlling how often the reaper fires per workspace |

:::warning
Changing `INVOKR_REAPER_CRON_EXPRESSION` only affects workspaces created **after** the change. Existing workspaces keep their original reaper schedule. To update an existing workspace's reaper schedule, you would need to unschedule and reschedule the pg_cron entry manually.
:::

## Baked into Workspace Creation

When a workspace is provisioned, the reaper's `INTERNAL` endpoint and CRON job are created as part of the workspace setup. This is done by `db::workspaces::provision_reaper()`, which:

1. Creates an `INTERNAL` endpoint named `invokr.reaper` with a `task: "reaper"` spec
2. Creates a `CRON` job targeting that endpoint with the configured schedule
3. Registers the job with pg_cron via `cron.schedule()`

The reaper is therefore always present in every workspace — no manual setup required.

## Runs Inside the Scoped Transaction

The reaper's `reap_schema()` function runs on the caller's database connection — the same scoped transaction that the execution pipeline uses for the reaper execution's outcome. This means:

- Retire expired CRON jobs
- Unschedule their pg_cron entries
- Record the attempt
- Finalize the execution (SUCCESS/FAILED)

All of these operations commit (or roll back) together. If the unschedule fails, the entire batch rolls back and retries on the next sweep. This prevents a critical failure mode: committing a `RETIRED` job while leaving its pg_cron entry scheduled forever (a permanent leak, since future sweeps only look at `ACTIVE` jobs).

```rust
pub async fn reap_schema(
    conn: &mut PgConnection,
    prefix: &str,
    schema_name: &str,
) -> Result<Vec<String>, sqlx::Error> {
    let retired = db::jobs::retire_expired_cron_jobs(conn, prefix).await?;

    for job_id in &retired {
        db::jobs::unschedule_pg_cron_conn(conn, schema_name, job_id).await?;
    }

    if !retired.is_empty() {
        metrics::counter!(m::CRON_JOBS_REAPED_TOTAL, "schema" => schema_name.to_string())
            .increment(retired.len() as u64);
    }

    Ok(retired)
}
```

The unschedule is existence-guarded — an already-removed pg_cron entry is a no-op rather than an error.

## INTERNAL Endpoint Type

The reaper uses a special `INTERNAL` endpoint type. This type is distinct from `HTTP`, `KAFKA`, and `REDIS_STREAM`:

```sql
-- workspace_v1.sql includes INTERNAL in the CHECK constraint:
CONSTRAINT chk_{p}endpoint_type CHECK (endpoint_type IN ('HTTP', 'KAFKA', 'REDIS_STREAM', 'INTERNAL'))
```

:::note
The initial migration (`20260317000000_initial.sql`) only allows `HTTP`, `KAFKA`, and `REDIS_STREAM`. The `INTERNAL` type was added in the workspace schema template (`workspace_v1.sql`) for per-workspace schemas.
:::

### The `task` Discriminator

`INTERNAL` endpoints carry a `task` field in their spec that selects which in-process routine to run. The dispatcher matches on this field:

```rust
match task {
    "reaper" => match reaper::reap_schema(conn, prefix, schema_name).await {
        Ok(retired) => DispatchResult::Success {
            output: serde_json::json!({
                "task": "reaper",
                "schema": schema_name,
                "retired_count": retired.len(),
                "retired_job_ids": retired,
            }),
        },
        Err(e) => DispatchResult::Failure { /* ... */ },
    },
    other => DispatchResult::Failure { /* INTERNAL_TASK_UNKNOWN */ },
}
```

Today, `"reaper"` is the only `INTERNAL` task. The architecture is extensible — new internal tasks can be added by implementing a new function and adding a match arm.

### API Guards

User-created jobs cannot target `INTERNAL` endpoints. The API rejects requests that specify an `INTERNAL` endpoint type with a `422` error. This prevents users from invoking internal routines directly.

## Coordination Across Worker Pods

Multiple worker pods can run simultaneously. Coordination is implicit:

1. pg_cron inserts exactly one execution per tick per schema
2. `claim()` uses `FOR UPDATE SKIP LOCKED`, so exactly one pod claims each reaper execution
3. The reaper runs within the claimed execution's transaction

No advisory locks or leader election needed. The previous implementation used a tokio interval task with an advisory lock in each worker pod — this was replaced by the dogfooded approach for better observability and simplicity.

## Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `invokr_cron_jobs_reaped_total` | Counter | CRON jobs retired by the reaper, by schema |

Each reaper execution also emits the standard execution metrics (`invokr_executions_claimed_total`, `invokr_executions_completed_total`, `invokr_execution_duration_seconds`).

## Related Pages

- [Database-Driven Scheduling](./db-driven-scheduling) — How pg_cron drives CRON job materialization
- [Worker Pipeline](./worker-pipeline) — The pipeline through which reaper executions flow
- [Delivery Guarantees](./exactly-once) — How SKIP LOCKED ensures only one pod runs each sweep
