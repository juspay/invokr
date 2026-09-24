---
id: cron-jobs
title: CRON Jobs
---

# CRON Jobs

CRON jobs run on a recurring schedule. When you create a CRON job, Invokr registers it with PostgreSQL's `pg_cron` extension. On each scheduled tick, pg_cron inserts a new `QUEUED` execution directly into the database, which workers pick up and dispatch. No external scheduler process is required.

## Creating a CRON job

```bash
curl -X POST http://localhost:8080/v1/jobs \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
  -d '{
    "endpoint": "send-welcome-email",
    "trigger": "CRON",
    "cron": "0 9 * * MON",
    "timezone": "Asia/Kolkata",
    "input": { "order_id": "all" }
  }'
```

Response (`201 Created`):

```json
{
  "job_id": "job_c72f...",
  "endpoint": "send-welcome-email",
  "endpoint_type": "HTTP",
  "trigger": "CRON",
  "status": "ACTIVE",
  "version": 1,
  "cron": "0 9 * * MON",
  "timezone": "Asia/Kolkata",
  "next_run_at": "2026-03-16T09:00:00+05:30",
  "input": { "order_id": "all" },
  "created_at": "2026-03-15T10:00:00Z"
}
```

## CRON expression format

Invokr uses standard **5-field cron expressions**:

```
┌───────────── minute (0-59)
│ ┌───────────── hour (0-23)
│ │ ┌───────────── day-of-month (1-31)
│ │ │ ┌───────────── month (1-12 or JAN-DEC)
│ │ │ │ ┌───────────── day-of-week (0-6 or SUN-SAT)
│ │ │ │ │
* * * * *
```

| Field | Allowed values | Special characters |
|-------|---------------|-------------------|
| Minute | `0-59` | `*`, `,`, `-`, `/` |
| Hour | `0-23` | `*`, `,`, `-`, `/` |
| Day of month | `1-31` | `*`, `,`, `-`, `/` |
| Month | `1-12` or `JAN-DEC` | `*`, `,`, `-`, `/` |
| Day of week | `0-6` or `SUN-SAT` | `*`, `,`, `-`, `/` |

Common examples:

| Expression | Meaning |
|-----------|---------|
| `* * * * *` | Every minute |
| `*/5 * * * *` | Every 5 minutes |
| `0 * * * *` | Every hour at minute 0 |
| `0 9 * * *` | Every day at 09:00 |
| `0 9 * * MON` | Every Monday at 09:00 |
| `0 */2 * * *` | Every 2 hours |
| `0 0 1 * *` | First day of every month at midnight |
| `30 23 * * FRI` | Every Friday at 23:30 |

:::warning
Invalid cron expressions return `422 INVALID_CRON`.
:::

## Timezone support

CRON jobs evaluate their schedule in a specified IANA timezone. Set the `timezone` field (or `cron_timezone` in the database) to control when ticks fire:

```json
{
  "cron": "0 9 * * MON",
  "timezone": "Asia/Kolkata"
}
```

The `next_run_at` in the response reflects the timezone offset:

```json
{
  "next_run_at": "2026-03-16T09:00:00+05:30"
}
```

:::info
Use full IANA timezone identifiers (e.g. `Asia/Kolkata`, `America/New_York`, `Europe/London`, `UTC`). The timezone affects when pg_cron fires each tick.
:::

## Start and end boundaries

CRON jobs support optional time boundaries:

| Field | Description | Default |
|-------|-------------|---------|
| `cron_starts_at` | Earliest time the CRON schedule is active | Job creation time (`now()`) |
| `cron_ends_at` | Time after which no new executions are created | `null` (indefinite) |

Example with boundaries:

```json
{
  "endpoint": "send-welcome-email",
  "trigger": "CRON",
  "cron": "0 9 * * MON",
  "timezone": "Asia/Kolkata",
  "starts_at": "2026-03-16T00:00:00Z",
  "ends_at": "2026-12-31T23:59:59Z",
  "input": { "order_id": "all" }
}
```

:::note
When `cron_ends_at` is reached, the CRON job stops generating new executions. In-flight executions continue to completion. The job status remains `ACTIVE` — the reaper handles cleanup of expired CRON jobs.
:::

## How pg_cron materializes executions

Invokr delegates CRON scheduling to PostgreSQL's `pg_cron` extension. No separate scheduler process is needed.

### Registration

When a CRON job is created, the API server calls `cron.schedule()` to register the schedule with pg_cron:

```sql
SELECT cron.schedule(
  'invokr_job_{job_id}',
  '{cron_expression}',
  $$SELECT materialize_cron_execution('{job_id}')$$
);
```

### Each tick

On each scheduled tick, pg_cron executes the registered function, which:

1. Finds the CRON job by `job_id`.
2. Checks that `status = 'ACTIVE'` and `cron_ends_at` has not passed.
3. Inserts a new `QUEUED` execution with an idempotency key.
4. Advances `cron_next_run_at` to the next scheduled time.

```sql
INSERT INTO executions (job_id, endpoint, endpoint_type, idempotency_key, status, input, run_at, max_attempts)
VALUES ($1, $2, $3, 'cron_' || $1 || '_' || extract(epoch from $4)::TEXT, 'QUEUED', $5, $4, $6)
ON CONFLICT (job_id, idempotency_key) DO NOTHING;
```

Workers then pick up the `QUEUED` execution via `SELECT FOR UPDATE SKIP LOCKED`, just like immediate jobs.

### CRON tick idempotency

Each CRON tick generates an idempotency key in the format:

```
cron_{job_id}_{epoch_ms}
```

This key, combined with the unique index on `(job_id, idempotency_key)`, ensures that even if pg_cron fires twice (e.g. during recovery), the `ON CONFLICT DO NOTHING` clause prevents duplicate executions.

:::tip
The `cron_next_run_at` column is advanced via a compare-and-swap (`UPDATE ... WHERE cron_next_run_at = $current_tick`), which prevents double-ticking when multiple scheduler instances run.
:::

## Getting job status

```bash
curl http://localhost:8080/v1/jobs/{job_id}/status \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>"
```

Response:

```json
{
  "job_id": "job_c72f...",
  "endpoint": "generate-weekly-report",
  "endpoint_type": "HTTP",
  "trigger": "CRON",
  "health": "HEALTHY",
  "version": 1,
  "last_execution": {
    "execution_id": "exec_8f3a...",
    "status": "SUCCESS",
    "started_at": "2026-03-15T10:30:00Z",
    "completed_at": "2026-03-15T10:30:01Z",
    "attempt_number": 1
  },
  "active_executions": {
    "pending": 2,
    "running": 1,
    "total": 3
  },
  "cron": {
    "expression": "0 9 * * MON",
    "next_run_at": "2026-03-16T09:00:00+05:30",
    "last_tick_at": "2026-03-09T09:00:00+05:30"
  },
  "stats": {
    "last_24h": {
      "total": 142,
      "succeeded": 139,
      "failed": 3,
      "avg_duration_ms": 340,
      "p99_duration_ms": 1200
    }
  }
}
```

### Health states

| Health | Condition |
|--------|-----------|
| `HEALTHY` | Recent executions mostly succeeding |
| `DEGRADED` | Elevated failure rate, but some succeeding |
| `FAILING` | Most recent executions failing |
| `IDLE` | No executions in the recent window |

## Listing executions

```bash
curl "http://localhost:8080/v1/jobs/{job_id}/executions?limit=10" \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>"
```

Each CRON tick creates a separate execution. Use [pagination](./pagination) to iterate through all executions for a CRON job.

## Cancelling a CRON job

Cancelling a CRON job unschedules it from pg_cron and stops future executions:

```bash
curl -X POST http://localhost:8080/v1/jobs/{job_id}/cancel \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>"
```

Response (`200 OK`):

```json
{
  "job_id": "job_c72f...",
  "status": "RETIRED",
  "retired_at": "2026-03-15T12:00:00Z"
}
```

When a CRON job is cancelled:

1. The job's `status` is set to `RETIRED`.
2. The pg_cron schedule is unscheduled (`cron.unschedule()`).
3. No new executions are created.
4. **In-flight executions run to completion** — they are not killed.

:::info
Cancelling is idempotent. Cancelling an already-`RETIRED` job is a no-op.
:::

## Reaper cleanup

The reaper is a background process that cleans up expired CRON jobs. When a CRON job's `cron_ends_at` has passed, the reaper:

1. Unschedules the job from pg_cron (if still scheduled).
2. Sets the job status to `RETIRED`.
3. Waits for any in-flight executions to complete.

CRON jobs with an `ends_at` boundary are cleaned up this way even if the cancel API is never called.

## CRON catch-up behavior

If the database or API was down and missed ticks, pg_cron materializes all missed executions sequentially. The `cron_next_run_at` is computed from the **current tick**, not `now()`:

```
CRON job: every minute. System was down from 09:00 to 09:10.

Iteration 1:  next_run_at = 09:00 (due). Creates execution. Advances to 09:01.
Iteration 2:  next_run_at = 09:01 (due). Creates execution. Advances to 09:02.
...
Iteration 11: next_run_at = 09:10 (due). Creates execution. Advances to 09:11.
Iteration 12: next_run_at = 09:11 (not yet due). Stops.
```

:::warning
If your CRON job is high-frequency and the system was down for a long period, catch-up can create a large burst of executions. Consider setting `cron_ends_at` or using a less frequent schedule if this is a concern.
:::

## CRON jobs are immutable

CRON jobs cannot be modified in place. To update a CRON job (e.g. change the schedule or input), use `PUT /v1/jobs/{job_id}`, which creates a new version and retires the old one. See [Job Versioning](./versioning) for details.

```bash
curl -X PUT http://localhost:8080/v1/jobs/{job_id} \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
  -d '{
    "cron": "0 */2 * * *",
    "input": { "mode": "v2" }
  }'
```

## Testing CRON jobs

For a quick end-to-end test, use a 1-minute cron expression and the mock HTTP server:

```bash
# Start the mock server
just mock-server

# Create a CRON job that fires every minute
curl -X POST http://localhost:8080/v1/jobs \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
  -d '{
    "endpoint": "send-welcome-email",
    "trigger": "CRON",
    "cron": "* * * * *",
    "timezone": "UTC",
    "input": { "order_id": "cron-test" }
  }'

# Wait ~60 seconds, then list executions
curl http://localhost:8080/v1/jobs/{job_id}/executions \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>"
```

Run the CRON integration test:

```bash
just test-cron
```

## See also

- [Job versioning](./versioning) — updating CRON jobs creates new versions
- [Delayed jobs](./delayed-jobs) — one-shot scheduled execution
- [Pagination](./pagination) — listing executions with cursors
- [Monitoring](./monitoring) — tracking CRON job health metrics
